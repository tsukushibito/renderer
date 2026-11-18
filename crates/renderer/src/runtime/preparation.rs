use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};
use crate::graph::{BufferDesc, GraphCapacity, ImageDesc};
use crate::program::{ManagedProgram, RasterPipelineRequest};

use super::{ExecutionError, Runtime, RuntimeInner};

/// Selects the GPU backend and its physical preparation limits.
#[derive(Clone, Copy, Debug)]
pub enum RuntimeConfig {
    #[cfg(feature = "vulkan")]
    Vulkan(VulkanConfig),
    #[cfg(all(feature = "d3d12", windows))]
    D3D12(D3D12Config),
}

/// Physical Vulkan limits fixed at initialization.
#[cfg(feature = "vulkan")]
#[derive(Clone, Copy, Debug)]
pub struct VulkanConfig {
    pub validation: crate::ValidationMode,
    /// Capacity of each shared upload and readback staging buffer.
    pub staging_bytes: u64,
    /// Retained staging range leases per shared buffer.
    pub staging_ranges: usize,
    /// Descriptor-bearing operations per Graph. Dispatches and distinct
    /// Raster draw binding sets each consume a slot.
    pub max_shader_bindings: u32,
}

#[cfg(feature = "vulkan")]
impl Default for VulkanConfig {
    fn default() -> Self {
        let config = crate::RuntimeConfig::default();
        Self {
            validation: crate::ValidationMode::default(),
            staging_bytes: config.staging_bytes,
            staging_ranges: config.staging_ranges,
            max_shader_bindings: config.max_shader_bindings,
        }
    }
}

/// Physical D3D12 limits fixed at initialization. Transfer bytes include the
/// native padding of placed texture footprints.
#[cfg(all(feature = "d3d12", windows))]
#[derive(Clone, Copy, Debug)]
pub struct D3D12Config {
    pub validation: crate::D3D12ValidationMode,
    /// Buffer operands per Graph, including imported buffers.
    pub buffer_count: usize,
    /// Largest logical buffer operand.
    pub max_buffer_bytes: u64,
    /// Upload staging bytes per execution.
    pub upload_bytes: u64,
    /// Readback staging bytes per retained CPU result.
    pub readback_bytes: u64,
    /// Buffer copy scratch bytes per execution.
    pub scratch_bytes: u64,
}

#[cfg(all(feature = "d3d12", windows))]
impl Default for D3D12Config {
    fn default() -> Self {
        let capacity = crate::D3D12FrameCapacity::default();
        Self {
            validation: crate::D3D12ValidationMode::default(),
            buffer_count: capacity.buffers,
            max_buffer_bytes: capacity.buffer_bytes,
            upload_bytes: capacity.upload_bytes,
            readback_bytes: capacity.readback_bytes,
            scratch_bytes: capacity.scratch_bytes,
        }
    }
}

/// Logical limits shared by headless and windowed Graph runtimes. Bounds are
/// fixed at initialization. Graph storage is allocated separately by the caller.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeLimits {
    pub graph: GraphCapacity,
    /// Accepted Graph submissions whose completion has not been consumed.
    pub frames: usize,
    /// Retained CPU results, including readbacks reserved by pending frames.
    pub results: usize,
    /// Physical registry entries, including registered resources and free classes.
    pub resource_capacity: usize,
    pub range_snapshots: usize,
    pub initialized_ranges_per_resource: usize,
    pub provenance_segments_per_resource: usize,
    pub index_prefixes_per_resource: usize,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            graph: GraphCapacity::default(),
            frames: 4,
            results: 8,
            resource_capacity: 64,
            range_snapshots: 1024,
            initialized_ranges_per_resource: 32,
            provenance_segments_per_resource: 64,
            index_prefixes_per_resource: 64,
        }
    }
}

impl RuntimeLimits {
    pub(super) fn validate(self) -> Result<(), ExecutionError> {
        if self.frames == 0
            || self.results == 0
            || self.resource_capacity == 0
            || self.graph.resources == 0
            || self.graph.passes == 0
            || self.range_snapshots == 0
        {
            return Err(ExecutionError::invalid(
                "Graph, frame, result, resource and range slot limits must be nonzero",
            ));
        }
        Ok(())
    }

    #[cfg(feature = "vulkan")]
    pub(super) fn vulkan_capacity(self) -> Result<crate::FrameCapacity, ExecutionError> {
        Ok(crate::FrameCapacity {
            graph: self.graph,
            work_slots: self.frames,
            result_slots: self.results,
            metadata_slots: slot_total(self.frames, self.results)?,
            range_snapshots: self.range_snapshots,
            initialized_ranges_per_resource: self.initialized_ranges_per_resource,
            provenance_segments_per_resource: self.provenance_segments_per_resource,
            index_prefixes_per_resource: self.index_prefixes_per_resource,
        })
    }

    #[cfg(feature = "vulkan")]
    pub(super) fn vulkan_config(
        self,
        settings: VulkanConfig,
        presentation_slots: usize,
    ) -> Result<crate::RuntimeConfig, ExecutionError> {
        Ok(crate::RuntimeConfig {
            execution_slots: slot_total(self.frames, presentation_slots)?,
            resource_capacity: self.resource_capacity,
            staging_bytes: settings.staging_bytes,
            staging_ranges: settings.staging_ranges,
            max_shader_bindings: settings.max_shader_bindings,
        })
    }

    #[cfg(all(feature = "d3d12", windows))]
    fn d3d12_config(self, settings: D3D12Config) -> crate::D3D12RuntimeConfig {
        crate::D3D12RuntimeConfig {
            validation: settings.validation,
            capacity: crate::D3D12FrameCapacity {
                executions: self.frames,
                results: self.results,
                resources: self.graph.resources,
                resource_capacity: self.resource_capacity,
                // Readback records, graph operations and raster draw records
                // have independent arenas bounded by this value.
                operations: self.graph.passes.max(self.graph.draws),
                initialized_ranges_per_resource: self.initialized_ranges_per_resource,
                range_snapshots: self.range_snapshots,
                provenance_segments_per_resource: self.provenance_segments_per_resource,
                index_prefixes_per_resource: self.index_prefixes_per_resource,
                buffers: settings.buffer_count,
                buffer_bytes: settings.max_buffer_bytes,
                upload_bytes: settings.upload_bytes,
                readback_bytes: settings.readback_bytes,
                scratch_bytes: settings.scratch_bytes,
            },
        }
    }
}

/// A resource class and its globally reserved instance count. The count
/// already includes the caller's needed concurrency across all frames.
#[derive(Clone, Copy, Debug)]
pub enum ResourceClass {
    Buffer { desc: BufferDesc, count: usize },
    Image { desc: ImageDesc, count: usize },
}

/// Resources and exact Program variants to prepare at a startup or change
/// boundary. Registered resources occupy additional physical registry entries.
#[derive(Clone, Copy, Default)]
pub struct WorkingSet<'a> {
    pub resources: &'a [ResourceClass],
    pub raster: &'a [RasterPipelineRequest],
    pub compute: &'a [ManagedProgram],
}

impl Runtime {
    /// Create a headless runtime with a complete prepared execution working set.
    pub fn new(
        config: RuntimeConfig,
        limits: RuntimeLimits,
        working_set: WorkingSet<'_>,
    ) -> Result<Self, ExecutionError> {
        limits.validate()?;
        match config {
            #[cfg(feature = "vulkan")]
            RuntimeConfig::Vulkan(settings) => {
                let capacity = limits.vulkan_capacity()?;
                let mut runtime = crate::VulkanRuntime::new_with_validation(
                    limits.vulkan_config(settings, 0)?,
                    settings.validation,
                )?;
                runtime.set_frame_limit(limits.frames)?;
                runtime.prepare_graph_working_set(Some(capacity), working_set)?;
                Ok(Self {
                    inner: RuntimeInner::Vulkan(runtime),
                })
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeConfig::D3D12(settings) => Ok(Self {
                inner: RuntimeInner::D3D12(new_d3d12(settings, limits, working_set)?),
            }),
        }
    }

    /// Prepare new classes and Program variants at an explicit change boundary.
    ///
    /// Return all Graph execution, result, export, and import-range leases
    /// before replacing the set. Registered resources retain keys and contents;
    /// refresh buffer snapshots with the retained keys after preparation.
    /// Unused idle resources from the previous set are released when the
    /// replacement commits. Failed replacements preserve the previous set.
    pub fn prepare_working_set(
        &mut self,
        working_set: WorkingSet<'_>,
    ) -> Result<(), ExecutionError> {
        match &mut self.inner {
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(runtime) => {
                Ok(runtime.prepare_graph_working_set(None, working_set)?)
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(runtime) => {
                let classes = D3D12Classes::new(working_set.resources)?;
                Ok(runtime.prepare_graph_working_set(
                    Some(&classes.buffers),
                    &classes.images,
                    working_set.raster,
                    working_set.compute,
                )?)
            }
        }
    }
}

#[cfg(feature = "vulkan")]
fn slot_total(frames: usize, other: usize) -> Result<usize, ExecutionError> {
    frames.checked_add(other).ok_or_else(|| {
        ExecutionError::with_capacity(
            "runtime slot count overflow",
            RuntimeCapacityError::new(
                RuntimeCapacityKind::WorkingSet,
                CapacityRecovery::Reconfigure,
                None,
                None,
            ),
        )
    })
}

#[cfg(all(feature = "d3d12", windows))]
pub(super) fn new_d3d12(
    settings: D3D12Config,
    limits: RuntimeLimits,
    set: WorkingSet<'_>,
) -> Result<crate::D3D12Runtime, ExecutionError> {
    let classes = D3D12Classes::new(set.resources)?;
    Ok(crate::D3D12Runtime::new_with_working_set(
        limits.d3d12_config(settings),
        &classes.buffers,
        &classes.images,
        set.raster,
        set.compute,
    )?)
}

#[cfg(all(feature = "d3d12", windows))]
struct D3D12Classes {
    buffers: Vec<crate::D3D12BufferClass>,
    images: Vec<crate::D3D12ImageClass>,
}

#[cfg(all(feature = "d3d12", windows))]
impl D3D12Classes {
    fn new(resources: &[ResourceClass]) -> Result<Self, ExecutionError> {
        let buffer_count = resources
            .iter()
            .filter(|resource| matches!(resource, ResourceClass::Buffer { .. }))
            .count();
        let mut buffers = Vec::new();
        let mut images = Vec::new();
        buffers
            .try_reserve_exact(buffer_count)
            .and_then(|()| images.try_reserve_exact(resources.len() - buffer_count))
            .map_err(|_| {
                ExecutionError::with_capacity(
                    "unable to reserve resource class descriptions",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::HostMemory,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?;
        for resource in resources {
            match *resource {
                ResourceClass::Buffer { desc, count } => buffers.push(crate::D3D12BufferClass {
                    size: desc.size,
                    count,
                }),
                ResourceClass::Image { desc, count } => {
                    images.push(crate::D3D12ImageClass { desc, count });
                }
            }
        }
        Ok(Self { buffers, images })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "vulkan")]
    #[test]
    fn vulkan_presentation_slots_are_additional_and_arithmetic_is_bounded() {
        let limits = RuntimeLimits {
            frames: 3,
            results: 5,
            ..RuntimeLimits::default()
        };
        let tuning = VulkanConfig::default();
        assert_eq!(limits.vulkan_config(tuning, 2).unwrap().execution_slots, 5);
        let capacity = limits.vulkan_capacity().unwrap();
        assert_eq!(capacity.work_slots, 3);
        assert_eq!(capacity.result_slots, 5);
        assert_eq!(capacity.metadata_slots, 8);
        let limits = RuntimeLimits {
            frames: usize::MAX,
            ..limits
        };
        assert_eq!(
            limits
                .vulkan_config(tuning, 2)
                .unwrap_err()
                .capacity()
                .unwrap()
                .recovery,
            CapacityRecovery::Reconfigure
        );
    }

    #[cfg(all(feature = "d3d12", windows))]
    #[test]
    fn d3d12_logical_limits_keep_native_transfer_adjustments_separate() {
        let limits = RuntimeLimits {
            graph: GraphCapacity {
                passes: 7,
                draws: 3,
                ..Default::default()
            },
            frames: 2,
            results: 5,
            ..Default::default()
        };
        let config = limits.d3d12_config(D3D12Config {
            upload_bytes: 512,
            readback_bytes: 1024,
            ..Default::default()
        });
        assert_eq!(config.capacity.executions, limits.frames);
        assert_eq!(config.capacity.results, limits.results);
        assert_eq!(config.capacity.operations, limits.graph.passes);
        assert_eq!(config.capacity.upload_bytes, 512);
        assert_eq!(config.capacity.readback_bytes, 1024);
    }
}
