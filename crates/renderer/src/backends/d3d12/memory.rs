//! Explicit memory snapshots. Native requirements, logical bytes and OS process
//! usage are different observations; none proves future allocation or residency.

use windows_core::Interface;

use super::{D3D12Runtime, D3D12RuntimeError, bindings::*, device::DeviceOwner};

/// Native allocation requirements for live Renderer-created committed resources.
/// One committed resource has one implicit heap. Driver/descriptor/pipeline and
/// swapchain allocations are excluded; OS process usage is reported separately.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct D3D12CommittedHeapStats {
    pub resource_count: u64,
    /// Sum of per-resource `GetResourceAllocationInfo::SizeInBytes` results.
    pub allocation_bytes: u64,
    pub alignment_4k_count: u64,
    pub alignment_64k_count: u64,
    pub alignment_4m_count: u64,
    pub alignment_other_count: u64,
}

/// Device-wide Renderer accounting, including resources retained by escaped work
/// or managed handles and by other clients of the same native Device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct D3D12CommittedMemoryStats {
    pub default_heap: D3D12CommittedHeapStats,
    pub upload_heap: D3D12CommittedHeapStats,
    pub readback_heap: D3D12CommittedHeapStats,
    pub overflowed: bool,
}

/// OS accounting for this process, adapter and memory segment at query time.
/// Budget is dynamic and reservations are hints, not allocation guarantees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct D3D12VideoMemoryInfo {
    pub budget_bytes: u64,
    pub current_usage_bytes: u64,
    pub available_for_reservation_bytes: u64,
    pub current_reservation_bytes: u64,
}

/// Prepared transfer arena capacity, separate from native committed size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct D3D12StagingMemoryStats {
    pub capacity_bytes: u64,
    pub slots: usize,
    pub bytes_per_slot: u64,
    /// Leased arenas leave the pool; this baseline does not instrument their
    /// live byte usage. Unavailable observations are not reported as zero.
    pub used_bytes: Option<u64>,
    pub peak_used_bytes: Option<u64>,
}

/// An on-demand diagnostic snapshot, outside normal frame processing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct D3D12MemoryStats {
    /// Packed logical bytes in this runtime's canonical registry.
    pub registered_logical_bytes: u64,
    pub retained_logical_bytes: u64,
    pub cached_logical_bytes: u64,
    pub upload: D3D12StagingMemoryStats,
    pub readback: D3D12StagingMemoryStats,
    /// Present only with `memory-profiling`; dedicated creation/destruction
    /// metadata and native requirement queries are compiled out otherwise.
    pub committed: Option<D3D12CommittedMemoryStats>,
    /// None means this adapter does not expose IDXGIAdapter3. Query failures
    /// after successful interface discovery are returned as errors.
    pub process_local: Option<D3D12VideoMemoryInfo>,
    pub process_non_local: Option<D3D12VideoMemoryInfo>,
}

impl D3D12Runtime {
    /// Query logical registry bytes, prepared transfer capacity, optional native
    /// resource requirements, and DXGI process usage/budget on the same adapter.
    /// This creates temporary DXGI query interfaces and can allocate; call it at
    /// explicit observation boundaries, never inside an allocation-free frame.
    /// Concurrent native creation/destruction may make the independent readings
    /// describe slightly different instants. No GPU wait or residency change is
    /// issued. Physical page residency is not measured.
    pub fn memory_stats(&self) -> Result<D3D12MemoryStats, D3D12RuntimeError> {
        self.device.ensure_active()?;
        let [registered, retained, cached] = self
            .pool
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .logical_memory_bytes();
        let capacity = self.pool.capacity();
        let arena = |slots: usize, bytes_per_slot: u64| D3D12StagingMemoryStats {
            capacity_bytes: bytes_per_slot.saturating_mul(slots as u64),
            slots,
            bytes_per_slot,
            used_bytes: None,
            peak_used_bytes: None,
        };
        let (process_local, process_non_local) = process_memory(&self.device)?;
        Ok(D3D12MemoryStats {
            registered_logical_bytes: registered,
            retained_logical_bytes: retained,
            cached_logical_bytes: cached,
            upload: arena(capacity.executions, capacity.upload_bytes),
            readback: arena(capacity.results, capacity.readback_bytes),
            #[cfg(feature = "memory-profiling")]
            committed: Some(
                *self
                    .device
                    .committed_memory
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            ),
            #[cfg(not(feature = "memory-profiling"))]
            committed: None,
            process_local,
            process_non_local,
        })
    }
}

fn process_memory(
    owner: &DeviceOwner,
) -> Result<(Option<D3D12VideoMemoryInfo>, Option<D3D12VideoMemoryInfo>), D3D12RuntimeError> {
    // Keep factory and adapter refs local so final native validation runs after
    // these query interfaces have been released, even in strict configurations.
    // SAFETY: Supported factory IID, no debug requirement or state mutation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory2(0) }
        .map_err(|error| owner.native("CreateDXGIFactory2(memory query)", error))?;
    for index in 0.. {
        // SAFETY: Live owned factory; each successful result owns its COM ref.
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(owner.native("EnumAdapters1(memory query)", error)),
        };
        let mut desc = DXGI_ADAPTER_DESC1::default();
        // SAFETY: Correctly sized output for the retained live adapter.
        unsafe { adapter.GetDesc1(&mut desc) }
            .ok()
            .map_err(|error| owner.native("GetDesc1(memory query)", error))?;
        let mut luid = [0; 8];
        luid[..4].copy_from_slice(&desc.AdapterLuid.LowPart.to_le_bytes());
        luid[4..].copy_from_slice(&desc.AdapterLuid.HighPart.to_le_bytes());
        if luid != owner.info.luid {
            continue;
        }
        let adapter: IDXGIAdapter3 = match adapter.cast() {
            Ok(adapter) => adapter,
            Err(error) if error.code().0 as u32 == 0x8000_4002 => return Ok((None, None)),
            Err(error) => return Err(owner.native("QueryInterface(IDXGIAdapter3)", error)),
        };
        let query = |segment| {
            let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
            // SAFETY: This runtime uses single-node adapter zero; local and
            // nonlocal are valid segment groups and output is correctly sized.
            unsafe { adapter.QueryVideoMemoryInfo(0, segment, &mut info) }
                .ok()
                .map_err(|error| owner.native("IDXGIAdapter3::QueryVideoMemoryInfo", error))?;
            Ok(D3D12VideoMemoryInfo {
                budget_bytes: info.Budget,
                current_usage_bytes: info.CurrentUsage,
                available_for_reservation_bytes: info.AvailableForReservation,
                current_reservation_bytes: info.CurrentReservation,
            })
        };
        return Ok((
            Some(query(DXGI_MEMORY_SEGMENT_GROUP_LOCAL)?),
            Some(query(DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL)?),
        ));
    }
    Err(D3D12RuntimeError::unsupported(
        "runtime adapter is no longer enumerated for the DXGI memory query",
    ))
}

#[cfg(feature = "memory-profiling")]
pub(super) struct CommittedAllocation {
    accounting: std::sync::Arc<std::sync::Mutex<D3D12CommittedMemoryStats>>,
    heap: D3D12_HEAP_TYPE,
    bytes: u64,
    alignment: u64,
}

#[cfg(feature = "memory-profiling")]
impl D3D12CommittedMemoryStats {
    fn heap(&mut self, heap: D3D12_HEAP_TYPE) -> &mut D3D12CommittedHeapStats {
        match heap {
            D3D12_HEAP_TYPE_DEFAULT => &mut self.default_heap,
            D3D12_HEAP_TYPE_UPLOAD => &mut self.upload_heap,
            D3D12_HEAP_TYPE_READBACK => &mut self.readback_heap,
            _ => unreachable!("only the three supported committed heap types are created"),
        }
    }
}

#[cfg(feature = "memory-profiling")]
impl D3D12CommittedHeapStats {
    fn alignment_count(&mut self, alignment: u64) -> &mut u64 {
        match alignment {
            4096 => &mut self.alignment_4k_count,
            65_536 => &mut self.alignment_64k_count,
            4_194_304 => &mut self.alignment_4m_count,
            _ => &mut self.alignment_other_count,
        }
    }
}

#[cfg(feature = "memory-profiling")]
impl CommittedAllocation {
    /// Called only after successful committed creation. Native requirements are
    /// queried from the exact created descriptor, rather than packed byte math.
    pub(super) fn new(
        owner: &DeviceOwner,
        heap: D3D12_HEAP_TYPE,
        desc: &D3D12_RESOURCE_DESC,
    ) -> Result<Self, D3D12RuntimeError> {
        // SAFETY: The exact descriptor already succeeded on this live single-
        // node Device; the slice holds one correctly sized native descriptor.
        let native = unsafe { owner.raw.GetResourceAllocationInfo(0, &[*desc]) };
        if native.SizeInBytes == u64::MAX || native.Alignment == 0 {
            return Err(D3D12RuntimeError::classified(
                super::D3D12RuntimeErrorKind::Native,
                "GetResourceAllocationInfo failed for a created committed resource",
            ));
        }
        Self::record(
            std::sync::Arc::clone(&owner.committed_memory),
            heap,
            native.SizeInBytes,
            native.Alignment,
        )
    }

    fn record(
        accounting: std::sync::Arc<std::sync::Mutex<D3D12CommittedMemoryStats>>,
        heap: D3D12_HEAP_TYPE,
        bytes: u64,
        alignment: u64,
    ) -> Result<Self, D3D12RuntimeError> {
        let mut stats = accounting.lock().unwrap_or_else(|error| error.into_inner());
        let group = stats.heap(heap);
        let updated = group
            .resource_count
            .checked_add(1)
            .zip(group.allocation_bytes.checked_add(bytes))
            .zip(group.alignment_count(alignment).checked_add(1));
        let Some(((count, allocation_bytes), alignment_count)) = updated else {
            stats.overflowed = true;
            return Err(D3D12RuntimeError::invalid(
                "committed resource memory accounting overflow",
            ));
        };
        group.resource_count = count;
        group.allocation_bytes = allocation_bytes;
        *group.alignment_count(alignment) = alignment_count;
        drop(stats);
        Ok(Self {
            accounting,
            heap,
            bytes,
            alignment,
        })
    }
}

#[cfg(feature = "memory-profiling")]
impl Drop for CommittedAllocation {
    fn drop(&mut self) {
        let mut stats = self
            .accounting
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let group = stats.heap(self.heap);
        group.resource_count -= 1;
        group.allocation_bytes -= self.bytes;
        *group.alignment_count(self.alignment) -= 1;
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "memory-profiling")]
    #[test]
    fn committed_accounting_follows_final_resource_ownership_without_heap_rounding() {
        use super::*;
        use std::sync::{Arc, Mutex};
        let accounting = Arc::new(Mutex::new(D3D12CommittedMemoryStats::default()));
        let first = CommittedAllocation::record(
            Arc::clone(&accounting),
            D3D12_HEAP_TYPE_DEFAULT,
            131_072,
            65_536,
        )
        .unwrap();
        let second = CommittedAllocation::record(
            Arc::clone(&accounting),
            D3D12_HEAP_TYPE_UPLOAD,
            4096,
            4096,
        )
        .unwrap();
        let snapshot = *accounting.lock().unwrap();
        assert_eq!(snapshot.default_heap.resource_count, 1);
        assert_eq!(snapshot.default_heap.allocation_bytes, 131_072);
        assert_eq!(snapshot.default_heap.alignment_64k_count, 1);
        assert_eq!(snapshot.upload_heap.allocation_bytes, 4096);
        assert_eq!(snapshot.upload_heap.alignment_4k_count, 1);
        drop(first);
        assert_eq!(accounting.lock().unwrap().default_heap.resource_count, 0);
        drop(second);
        assert_eq!(
            *accounting.lock().unwrap(),
            D3D12CommittedMemoryStats::default()
        );
    }

    #[test]
    #[ignore = "explicit D3D12 hardware memory queries; isolated strict/shipping process"]
    fn d3d12_memory_stats_hardware() {
        use super::*;
        use crate::d3d12::D3D12RuntimeConfig;
        let config = D3D12RuntimeConfig {
            capacity: super::super::frame::D3D12FrameCapacity {
                executions: 1,
                results: 1,
                resources: 8,
                resource_capacity: 8,
                operations: 8,
                buffers: 2,
                buffer_bytes: 4,
                upload_bytes: 256,
                readback_bytes: 256,
                scratch_bytes: 4,
                ..Default::default()
            },
            ..Default::default()
        };
        let runtime = D3D12Runtime::new(config).expect("hardware runtime");
        let monitor = runtime.validation_monitor();
        let before = runtime.memory_stats().expect("same adapter memory queries");
        assert_eq!(before.registered_logical_bytes, 0);
        assert_eq!(before.retained_logical_bytes, 0);
        assert_eq!(before.cached_logical_bytes, 8);
        assert_eq!(before.upload.capacity_bytes, 256);
        assert_eq!(before.readback.capacity_bytes, 256);
        assert!(before.upload.used_bytes.is_none());
        assert!(before.process_local.expect("IDXGIAdapter3").budget_bytes > 0);
        assert!(before.process_non_local.is_some());
        #[cfg(feature = "memory-profiling")]
        {
            let committed = before.committed.unwrap();
            assert!(!committed.overflowed);
            assert_eq!(committed.default_heap.resource_count, 3);
            assert_eq!(committed.upload_heap.resource_count, 1);
            assert_eq!(committed.readback_heap.resource_count, 1);
            assert!(committed.default_heap.allocation_bytes >= 12);
            let buffer = super::super::resources::DeviceBuffer::storage(&runtime.device, 17)
                .expect("DEFAULT resource outside Graph registry");
            let with_buffer = runtime.memory_stats().unwrap();
            assert_eq!(with_buffer.registered_logical_bytes, 0);
            assert_eq!(
                with_buffer.committed.unwrap().default_heap.resource_count,
                committed.default_heap.resource_count + 1
            );
            drop(buffer);
            assert_eq!(runtime.memory_stats().unwrap().committed, before.committed);
        }
        #[cfg(not(feature = "memory-profiling"))]
        assert!(before.committed.is_none());
        println!("D3D12_MEMORY_STATS {before:?}");
        #[cfg(feature = "memory-profiling")]
        let accounting = std::sync::Arc::clone(&runtime.device.committed_memory);
        runtime.shutdown().unwrap();
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:?}");
        #[cfg(feature = "development-validation")]
        assert!(report.teardown_checked);
        #[cfg(feature = "memory-profiling")]
        assert_eq!(
            *accounting.lock().unwrap(),
            D3D12CommittedMemoryStats::default()
        );
    }
}
