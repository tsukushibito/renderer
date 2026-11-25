//! Prepared legacy transition descriptions; temporary COM references never escape a flush.

use std::mem::ManuallyDrop;

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use super::super::{D3D12RuntimeError, bindings::*};

pub(in crate::backends::d3d12) struct TransitionBatch {
    barriers: Vec<D3D12_RESOURCE_BARRIER>,
    maximum: usize,
    #[cfg(test)]
    pub(in crate::backends::d3d12) recorded_calls: usize,
    #[cfg(test)]
    pub(in crate::backends::d3d12) recorded_transitions: usize,
}

fn capacity_error(limit: Option<usize>, required: Option<usize>) -> D3D12RuntimeError {
    D3D12RuntimeError::capacity_with(
        "prepared transition batch capacity exhausted",
        RuntimeCapacityError::new(
            RuntimeCapacityKind::GraphMetadata,
            CapacityRecovery::Reconfigure,
            limit.and_then(|value| u64::try_from(value).ok()),
            required.and_then(|value| u64::try_from(value).ok()),
        ),
    )
}

pub(in crate::backends::d3d12) fn common_capacity(
    buffers: usize,
    images: usize,
) -> Result<usize, D3D12RuntimeError> {
    let required = buffers.checked_add(1).and_then(|n| n.checked_add(images));
    required
        .filter(|&count| u32::try_from(count).is_ok())
        .ok_or_else(|| capacity_error(Some(u32::MAX as usize), required))
}

impl TransitionBatch {
    pub(in crate::backends::d3d12) fn new(maximum: usize) -> Result<Self, D3D12RuntimeError> {
        let mut result = Self {
            barriers: Vec::new(),
            maximum: 0,
            #[cfg(test)]
            recorded_calls: 0,
            #[cfg(test)]
            recorded_transitions: 0,
        };
        result.prepare_capacity(maximum)?;
        Ok(result)
    }

    /// Explicit working-set preparation only; never called from frame recording.
    pub(in crate::backends::d3d12) fn prepare_capacity(
        &mut self,
        maximum: usize,
    ) -> Result<(), D3D12RuntimeError> {
        if u32::try_from(maximum).is_err() {
            return Err(capacity_error(Some(u32::MAX as usize), Some(maximum)));
        }
        self.clear();
        if self.barriers.capacity() < maximum {
            self.barriers.try_reserve_exact(maximum).map_err(|_| {
                D3D12RuntimeError::capacity_with(
                    "transition batch reservation failed",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::HostMemory,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?;
        }
        self.maximum = maximum;
        Ok(())
    }

    pub(in crate::backends::d3d12) fn clear(&mut self) {
        for barrier in &mut self.barriers {
            // SAFETY: Only push_transition constructs entries, always with the
            // Transition tag and one owned optional COM reference. Generated
            // union fields suppress Drop; release each occupied entry once.
            unsafe { ManuallyDrop::drop(&mut (*barrier.Anonymous.Transition).pResource) };
        }
        self.barriers.clear();
    }

    pub(in crate::backends::d3d12) fn push_transition(
        &mut self,
        resource: &ID3D12Resource,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> Result<(), D3D12RuntimeError> {
        self.push(Some(resource), before, after)
    }

    fn push(
        &mut self,
        resource: Option<&ID3D12Resource>,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> Result<(), D3D12RuntimeError> {
        if before == after {
            return Ok(());
        }
        if self.barriers.len() == self.maximum {
            return Err(capacity_error(
                Some(self.maximum),
                self.maximum.checked_add(1),
            ));
        }
        // Move a fresh description. The generated native Clone is a bitwise
        // copy and must never duplicate ownership of these union payloads.
        self.barriers.push(D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: ManuallyDrop::new(resource.cloned()),
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        });
        Ok(())
    }

    pub(in crate::backends::d3d12) fn record(
        &mut self,
        list: &ID3D12GraphicsCommandList,
        required: usize,
        fill: impl FnOnce(&mut Self) -> Result<(), D3D12RuntimeError>,
    ) -> Result<(), D3D12RuntimeError> {
        self.record_with(required, fill, |barriers| {
            #[cfg(feature = "tracy")]
            let _span = tracy_client::Client::running().map(|client| {
                client.span(
                    tracy_client::span_location!("renderer.d3d12.graph.common_barrier_call"),
                    0,
                )
            });
            // SAFETY: All descriptions retain live resources through this
            // call. Exclusive recording supplies their tracked prior states;
            // prepared capacity fits UINT and preserves the supplied order.
            unsafe { list.ResourceBarrier(barriers) };
        })
    }

    fn record_with(
        &mut self,
        required: usize,
        fill: impl FnOnce(&mut Self) -> Result<(), D3D12RuntimeError>,
        record: impl FnOnce(&[D3D12_RESOURCE_BARRIER]),
    ) -> Result<(), D3D12RuntimeError> {
        self.clear();
        #[cfg(test)]
        {
            self.recorded_calls = 0;
            self.recorded_transitions = 0;
        }
        if required > self.maximum {
            return Err(capacity_error(Some(self.maximum), Some(required)));
        }
        let cleanup = ClearOnDrop(self);
        fill(cleanup.0)?;
        if !cleanup.0.barriers.is_empty() {
            #[cfg(test)]
            {
                cleanup.0.recorded_calls += 1;
                cleanup.0.recorded_transitions = cleanup.0.barriers.len();
            }
            record(&cleanup.0.barriers);
        }
        Ok(())
    }
}

struct ClearOnDrop<'a>(&'a mut TransitionBatch);
impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.clear();
    }
}
impl Drop for TransitionBatch {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn common_batch_limits_include_scratch_images_and_native_count() {
        assert_eq!(common_capacity(2, 3).unwrap(), 6);
        assert_eq!(common_capacity(0, 0).unwrap(), 1);
        assert!(common_capacity(usize::MAX, 0).is_err());
        assert!(common_capacity(0, usize::MAX).is_err());
        assert!(common_capacity(u32::MAX as usize, 0).is_err());
    }

    #[test]
    fn mixed_transitions_preserve_order_in_one_call_and_reuse_prepared_storage() {
        let mut batch = TransitionBatch::new(4).unwrap();
        let allocation = batch.barriers.as_ptr();
        let capacity = batch.barriers.capacity();
        for _ in 0..3 {
            let recorded = Cell::new(false);
            batch
                .record_with(
                    4,
                    |batch| {
                        for before in [
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                            D3D12_RESOURCE_STATE_COMMON,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                            D3D12_RESOURCE_STATE_RENDER_TARGET,
                        ] {
                            // CPU fixture substitutes only the native resource pointer;
                            // these descriptions are inspected, never sent to D3D12.
                            batch.push(None, before, D3D12_RESOURCE_STATE_COMMON)?;
                        }
                        Ok(())
                    },
                    |barriers| {
                        assert!(!recorded.replace(true));
                        let observed: Vec<_> = barriers
                            .iter()
                            .map(|barrier| {
                                assert_eq!(barrier.Type, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION);
                                // SAFETY: Production construction supplied the checked Transition tag.
                                let transition = unsafe { &*barrier.Anonymous.Transition };
                                assert_eq!(transition.StateAfter, D3D12_RESOURCE_STATE_COMMON);
                                assert_eq!(
                                    transition.Subresource,
                                    D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES
                                );
                                transition.StateBefore
                            })
                            .collect();
                        assert_eq!(
                            observed,
                            [
                                D3D12_RESOURCE_STATE_COPY_SOURCE,
                                D3D12_RESOURCE_STATE_COPY_DEST,
                                D3D12_RESOURCE_STATE_RENDER_TARGET
                            ]
                        );
                    },
                )
                .unwrap();
            assert!(recorded.get());
            assert_eq!((batch.recorded_calls, batch.recorded_transitions), (1, 3));
            assert!(batch.barriers.is_empty());
            assert_eq!(
                (batch.barriers.as_ptr(), batch.barriers.capacity()),
                (allocation, capacity)
            );
        }
        batch
            .record_with(
                4,
                |batch| {
                    batch.push(
                        None,
                        D3D12_RESOURCE_STATE_COMMON,
                        D3D12_RESOURCE_STATE_COMMON,
                    )
                },
                |_| panic!("empty native call"),
            )
            .unwrap();
        assert_eq!(batch.recorded_calls, 0);
    }

    #[test]
    fn capacity_and_fill_failures_do_not_record_or_leave_pending_descriptions() {
        let mut batch = TransitionBatch::new(1).unwrap();
        let called = Cell::new(false);
        assert!(
            batch
                .record_with(
                    2,
                    |_| {
                        called.set(true);
                        Ok(())
                    },
                    |_| unreachable!()
                )
                .is_err()
        );
        assert!(
            !called.get(),
            "capacity is established before recording/operand cloning"
        );
        assert!(
            batch
                .record_with(
                    1,
                    |batch| {
                        batch.push(
                            None,
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                            D3D12_RESOURCE_STATE_COMMON,
                        )?;
                        batch.push(
                            None,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                            D3D12_RESOURCE_STATE_COMMON,
                        )
                    },
                    |_| unreachable!()
                )
                .is_err()
        );
        assert!(batch.barriers.is_empty());
        assert_eq!(batch.recorded_calls, 0);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = batch.record_with(
                1,
                |batch| {
                    batch.push(
                        None,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                        D3D12_RESOURCE_STATE_COMMON,
                    )
                },
                |_| panic!("injected recording unwind"),
            );
        }));
        assert!(failed.is_err());
        assert!(batch.barriers.is_empty());
    }

    #[test]
    #[ignore = "requires real D3D12 hardware; root serializes native acceptance"]
    fn d3d12_transition_batch_releases_temporary_com_references_on_every_exit() {
        use super::super::super::resources::DeviceBuffer;
        use crate::d3d12::{D3D12Runtime, D3D12RuntimeConfig, D3D12ValidationMode};
        use windows_core::{IUnknown, Interface};
        fn references(resource: &ID3D12Resource) -> u32 {
            // SAFETY: IUnknown is the prefix of this real resource's COM
            // vtable. Balance the probe reference immediately; no native
            // commands or other thread accesses this isolated fixture resource.
            unsafe {
                let table = resource.assume_vtable::<IUnknown>();
                (table.AddRef)(resource.as_raw());
                (table.Release)(resource.as_raw())
            }
        }
        let runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: if cfg!(feature = "d3d12-diagnostics") {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            ..Default::default()
        })
        .unwrap();
        let monitor = runtime.validation_monitor();
        let buffer = DeviceBuffer::new(&runtime.device, 4, D3D12_HEAP_TYPE_DEFAULT).unwrap();
        let resource = buffer.raw();
        let baseline = references(resource);
        let mut batch = TransitionBatch::new(1).unwrap();
        batch
            .push_transition(
                resource,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            )
            .unwrap();
        assert_eq!(references(resource), baseline + 1);
        batch.clear();
        assert_eq!(references(resource), baseline);
        batch
            .record_with(
                1,
                |batch| {
                    batch.push_transition(
                        resource,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                        D3D12_RESOURCE_STATE_COMMON,
                    )
                },
                |_| {
                    assert_eq!(references(resource), baseline + 1);
                },
            )
            .unwrap();
        assert_eq!(references(resource), baseline);
        assert!(
            batch
                .record_with(
                    1,
                    |batch| {
                        batch.push_transition(
                            resource,
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                            D3D12_RESOURCE_STATE_COMMON,
                        )?;
                        batch.push_transition(
                            resource,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                            D3D12_RESOURCE_STATE_COMMON,
                        )
                    },
                    |_| unreachable!()
                )
                .is_err()
        );
        assert_eq!(references(resource), baseline);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = batch.record_with(
                1,
                |batch| {
                    batch.push_transition(
                        resource,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                        D3D12_RESOURCE_STATE_COMMON,
                    )
                },
                |_| panic!("injected recording unwind"),
            );
        }));
        assert!(failed.is_err());
        assert_eq!(references(resource), baseline);
        batch
            .push_transition(
                resource,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            )
            .unwrap();
        drop(batch);
        assert_eq!(references(resource), baseline);
        drop(buffer);
        runtime.shutdown().unwrap();
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(
            report.teardown_checked,
            cfg!(feature = "d3d12-diagnostics"),
            "{report:?}"
        );
        if cfg!(feature = "d3d12-diagnostics") {
            assert!(
                report.debug_layer_enabled && report.gpu_based_validation_enabled,
                "{report:?}"
            );
        }
    }
}
