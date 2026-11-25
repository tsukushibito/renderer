#[cfg(all(windows, feature = "d3d12"))]
use crate::capacity::ResourceClass;
use crate::capacity::WorkingSet;
#[cfg(sashiko_gpu_backend)]
use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};
use crate::graph::GraphCapacity;

#[cfg(sashiko_gpu_backend)]
use super::RuntimeInner;
use super::{ExecutionError, Runtime};

/// Selects the GPU backend and its physical preparation limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeConfig {
    #[cfg(sashiko_webgpu)]
    WebGpu(WebGpuConfig),
    #[cfg(feature = "vulkan")]
    Vulkan(VulkanConfig),
    #[cfg(all(feature = "d3d12", windows))]
    D3D12(D3D12Config),
}

impl RuntimeConfig {
    /// Raise physical limits for the declared working set. Transfer sizes are per
    /// submission and include texture row/placement padding. The staging pool
    /// on Vulkan is shared by concurrent submissions.
    #[cfg_attr(not(sashiko_gpu_backend), allow(unused_variables))]
    pub fn fit_preparation(
        &mut self,
        preparation: &crate::capacity::Preparation,
        upload_bytes: u64,
        readback_bytes: u64,
    ) -> Result<(), crate::graph::GraphError> {
        #[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
        let req = preparation.graph_requirements();
        match *self {
            #[cfg(sashiko_webgpu)]
            Self::WebGpu(ref mut settings) => {
                let _ = preparation;
                settings.upload_bytes = settings.upload_bytes.max(upload_bytes);
                settings.readback_bytes = settings.readback_bytes.max(readback_bytes);
                Ok(())
            }
            #[cfg(feature = "vulkan")]
            Self::Vulkan(ref mut settings) => {
                let overflow =
                    || crate::graph::GraphError::invalid_input("Vulkan preparation limit overflow");
                let frames = preparation.frames_in_flight().get();
                let staging = upload_bytes
                    .max(readback_bytes)
                    .checked_mul(frames as u64)
                    .ok_or_else(overflow)?;
                let ranges = req.passes.checked_mul(frames).ok_or_else(overflow)?;
                // A shared operand range uses separate native sets in passes with
                // and without depth. Each recorded draw uses at most one variant.
                let raster_sets = req.draws.min(req.bindings.saturating_mul(2));
                let sets = req
                    .compute_dispatches
                    .checked_add(raster_sets)
                    .ok_or_else(overflow)?;
                let sets = u32::try_from(sets).map_err(|_| overflow())?;
                settings.max_shader_bindings = settings.max_shader_bindings.max(sets);
                settings.staging_bytes = settings.staging_bytes.max(staging);
                settings.staging_ranges = settings.staging_ranges.max(ranges);
                Ok(())
            }
            #[cfg(all(windows, feature = "d3d12"))]
            Self::D3D12(ref mut settings) => {
                for class in preparation.working_set().resources {
                    if let ResourceClass::Buffer { desc, .. } = class {
                        settings.max_buffer_bytes = settings.max_buffer_bytes.max(desc.size);
                    }
                }
                settings.buffer_count = settings.buffer_count.max(req.resources);
                settings.upload_bytes = settings.upload_bytes.max(upload_bytes);
                settings.readback_bytes = settings.readback_bytes.max(readback_bytes);
                Ok(())
            }
        }
    }
}

/// WebGPU transfer capacity in bytes per execution and per retained readback.
#[cfg(sashiko_webgpu)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WebGpuConfig {
    pub upload_bytes: u64,
    pub readback_bytes: u64,
}

/// Initial physical Vulkan limits for ordinary Graph execution.
#[cfg(feature = "vulkan")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VulkanConfig {
    pub validation: crate::vulkan::ValidationMode,
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
        let config = crate::vulkan::RuntimeConfig::default();
        Self {
            validation: crate::vulkan::ValidationMode::default(),
            staging_bytes: config.staging_bytes,
            staging_ranges: config.staging_ranges,
            max_shader_bindings: config.max_shader_bindings,
        }
    }
}

/// Initial physical D3D12 limits. Transfer bytes include the
/// native padding of placed texture footprints.
#[cfg(all(feature = "d3d12", windows))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3D12Config {
    pub validation: crate::d3d12::D3D12ValidationMode,
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
        let capacity = crate::d3d12::D3D12FrameCapacity::default();
        Self {
            validation: crate::d3d12::D3D12ValidationMode::default(),
            buffer_count: capacity.buffers,
            max_buffer_bytes: capacity.buffer_bytes,
            upload_bytes: capacity.upload_bytes,
            readback_bytes: capacity.readback_bytes,
            scratch_bytes: capacity.scratch_bytes,
        }
    }
}

/// Logical limits shared by headless and windowed Graph runtimes. The caller
/// allocates Graph storage separately and selects concurrency at startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    pub(super) fn vulkan_capacity(self) -> Result<crate::vulkan::FrameCapacity, ExecutionError> {
        Ok(crate::vulkan::FrameCapacity {
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
    ) -> Result<crate::vulkan::RuntimeConfig, ExecutionError> {
        Ok(crate::vulkan::RuntimeConfig {
            execution_slots: slot_total(self.frames, presentation_slots)?,
            resource_capacity: self.resource_capacity,
            staging_bytes: settings.staging_bytes,
            staging_ranges: settings.staging_ranges,
            max_shader_bindings: settings.max_shader_bindings,
        })
    }

    #[cfg(all(feature = "d3d12", windows))]
    fn d3d12_config(self, settings: D3D12Config) -> crate::d3d12::D3D12RuntimeConfig {
        crate::d3d12::D3D12RuntimeConfig {
            validation: settings.validation,
            capacity: crate::d3d12::D3D12FrameCapacity {
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

#[cfg_attr(
    not(sashiko_gpu_backend),
    expect(
        unused_variables,
        reason = "without a backend, the dispatcher has no runtime variants"
    )
)]
impl Runtime {
    /// Check the fixed physical limits before replacing a working set or attaching
    /// a prepared subsystem to this device. No native state changes on this path.
    pub fn check_transfer_and_binding_limits(
        &self,
        preparation: &crate::capacity::Preparation,
        upload: u64,
        readback: u64,
    ) -> Result<(), ExecutionError> {
        #[cfg(sashiko_gpu_backend)]
        {
            let mut required = self.config;
            required
                .fit_preparation(preparation, upload, readback)
                .map_err(|_| ExecutionError::invalid("backend capacity overflow"))?;
            if required != self.config {
                return Err(ExecutionError::with_capacity(
                    "preparation exceeds fixed backend limits",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::WorkingSet,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                ));
            }
            Ok(())
        }
        #[cfg(not(sashiko_gpu_backend))]
        match self.config {}
    }

    /// Create a headless runtime with a complete prepared execution working set.
    pub fn new(
        config: RuntimeConfig,
        limits: RuntimeLimits,
        working_set: WorkingSet<'_>,
    ) -> Result<Self, ExecutionError> {
        limits.validate()?;
        match config {
            #[cfg(sashiko_webgpu)]
            RuntimeConfig::WebGpu(_) => {
                let _ = working_set;
                Err(ExecutionError::webgpu(
                    super::ExecutionErrorKind::Unsupported,
                    "create the WebGPU runtime after requesting the browser device with from_webgpu_device",
                ))
            }
            #[cfg(feature = "vulkan")]
            RuntimeConfig::Vulkan(settings) => {
                let runtime = crate::vulkan::VulkanRuntime::new_with_validation(
                    limits.vulkan_config(settings, 0)?,
                    settings.validation,
                )?;
                Self::prepare_vulkan_runtime(runtime, settings, limits, working_set)
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeConfig::D3D12(settings) => Self::with_inner(
                RuntimeInner::D3D12(new_d3d12(settings, limits, working_set)?),
                limits,
                config,
            ),
        }
    }

    /// Create a runtime on the device and queue acquired by the browser host.
    /// The caller retains its canvas and drives completion from the event loop.
    #[cfg(sashiko_webgpu)]
    pub async fn from_webgpu_device(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        config: WebGpuConfig,
        limits: RuntimeLimits,
        working_set: WorkingSet<'_>,
    ) -> Result<Self, ExecutionError> {
        limits.validate()?;
        let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let engine = crate::webgpu::engine::Engine::new(
            device.clone(),
            queue.clone(),
            limits,
            working_set,
            config.upload_bytes,
            config.readback_bytes,
        );
        // Pop the entire stack before yielding. Other canvas users can issue
        // commands on this device while the browser resolves our error scopes.
        let validation = validation.pop();
        let internal = internal.pop();
        let memory = memory.pop();
        if let Some(error) = validation.await.or(internal.await).or(memory.await) {
            return Err(ExecutionError::webgpu(
                super::ExecutionErrorKind::Native,
                error.to_string(),
            ));
        }
        Self::with_inner(
            RuntimeInner::WebGpu(engine?),
            limits,
            RuntimeConfig::WebGpu(config),
        )
    }

    /// Use a host's existing Vulkan device/queue for ordinary Graph execution.
    /// Set `settings.validation` to `Disabled`; the host owns layer configuration
    /// and observes diagnostics throughout the shared instance's lifetime.
    ///
    /// # Safety
    /// Uphold the device ownership, enabled features, queue synchronization and
    /// capacity requirements of [`crate::vulkan::VulkanRuntime::from_host_device`].
    #[cfg(feature = "vulkan")]
    pub unsafe fn from_vulkan_device(
        host: crate::vulkan::VulkanHostDevice,
        settings: VulkanConfig,
        limits: RuntimeLimits,
        working_set: WorkingSet<'_>,
    ) -> Result<Self, ExecutionError> {
        limits.validate()?;
        if settings.validation != crate::vulkan::ValidationMode::Disabled {
            return Err(ExecutionError::invalid(
                "an adopted Vulkan instance uses host-configured validation",
            ));
        }
        // SAFETY: The caller accepts the native sharing contract.
        let runtime = unsafe {
            crate::vulkan::VulkanRuntime::from_host_device(
                limits.vulkan_config(settings, host.presentation_slots)?,
                host,
            )
        }?;
        Self::prepare_vulkan_runtime(runtime, settings, limits, working_set)
    }

    #[cfg(feature = "vulkan")]
    fn prepare_vulkan_runtime(
        mut runtime: crate::vulkan::VulkanRuntime,
        settings: VulkanConfig,
        limits: RuntimeLimits,
        working_set: WorkingSet<'_>,
    ) -> Result<Self, ExecutionError> {
        runtime.set_frame_limit(limits.frames)?;
        runtime.prepare_graph_working_set(Some(limits.vulkan_capacity()?), working_set)?;
        Self::with_inner(
            RuntimeInner::Vulkan(runtime),
            limits,
            RuntimeConfig::Vulkan(settings),
        )
    }

    /// Run Graphs on a game engine's existing D3D12 DIRECT queue (node zero).
    /// QueryInterface and GetDevice retain independent COM references. The host
    /// may release its original references or continue to submit unrelated work.
    /// Order accesses to shared resources using their explicit ownership handoff;
    /// use the runtime's queue lock for multi-operation host queue transactions.
    /// Required diagnostics observe an already-enabled Debug Layer and GBV;
    /// this constructor never enables or disables either on the host Device.
    #[cfg(all(feature = "d3d12", windows))]
    pub fn from_d3d12_queue(
        queue: &windows_core::IUnknown,
        config: D3D12Config,
        limits: RuntimeLimits,
        working_set: WorkingSet<'_>,
    ) -> Result<Self, ExecutionError> {
        limits.validate()?;
        let classes = D3D12Classes::new(working_set.resources)?;
        let mut runtime = crate::d3d12::D3D12Runtime::from_queue(
            queue,
            limits.d3d12_config(config),
            &classes.buffers,
            &classes.images,
            working_set.raster,
            working_set.compute,
        )?;
        runtime.prepare_samplers(working_set.samplers)?;
        Self::with_inner(
            RuntimeInner::D3D12(runtime),
            limits,
            RuntimeConfig::D3D12(config),
        )
    }

    /// Prepare new classes and Program variants at an explicit change boundary.
    ///
    /// Return Graph execution, result and submission export metadata leases
    /// before replacing the set. Registered buffers and images keep their keys,
    /// scheduled contents and import snapshots, including snapshots in built Graphs.
    /// Unused idle resources from the previous set are released when the
    /// replacement commits. Failed replacements preserve the previous set.
    pub fn prepare_working_set(
        &mut self,
        working_set: WorkingSet<'_>,
    ) -> Result<(), ExecutionError> {
        self.collect_released_images()?;
        match self.inner {
            #[cfg(sashiko_webgpu)]
            RuntimeInner::WebGpu(ref mut runtime) => {
                let settings = match self.config {
                    RuntimeConfig::WebGpu(settings) => settings,
                    #[cfg(feature = "vulkan")]
                    _ => unreachable!("runtime owns its backend configuration"),
                };
                runtime.prepare(
                    self.limits,
                    working_set,
                    settings.upload_bytes,
                    settings.readback_bytes,
                )
            }
            #[cfg(feature = "vulkan")]
            RuntimeInner::Vulkan(ref mut runtime) => {
                Ok(runtime.prepare_graph_working_set(None, working_set)?)
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeInner::D3D12(ref mut runtime) => {
                let classes = D3D12Classes::new(working_set.resources)?;
                runtime.prepare_samplers(working_set.samplers)?;
                Ok(runtime.prepare_graph_working_set(
                    Some(&classes.buffers),
                    &classes.images,
                    working_set.raster,
                    working_set.compute,
                )?)
            }
        }
    }

    /// Resize this device's recording storage and replace its prepared classes at
    /// an explicit load boundary. The queue, registered resources and snapshots
    /// keep their identities. Frame concurrency stays under the host's control.
    pub fn prepare_fitted_working_set(
        &mut self,
        preparation: &crate::capacity::Preparation,
        upload_bytes: u64,
        readback_bytes: u64,
    ) -> Result<(), ExecutionError> {
        self.collect_released_images()?;
        let limits = preparation
            .replacement_limits(self.limits)
            .map_err(|_| ExecutionError::invalid("working set limits overflow"))?;
        if limits.frames != self.limits.frames || limits.results != self.limits.results {
            return Err(ExecutionError::invalid(
                "new working set exceeds runtime concurrency",
            ));
        }
        let mut config = self.config;
        config
            .fit_preparation(preparation, upload_bytes, readback_bytes)
            .map_err(|_| ExecutionError::invalid("backend capacity overflow"))?;
        let set = preparation.working_set();
        #[cfg(sashiko_gpu_backend)]
        self.image_releases
            .reserve_capacity(limits.resource_capacity)?;
        match (&mut self.inner, config) {
            #[cfg(sashiko_webgpu)]
            (RuntimeInner::WebGpu(runtime), RuntimeConfig::WebGpu(settings)) => {
                runtime.prepare(limits, set, settings.upload_bytes, settings.readback_bytes)?;
            }
            #[cfg(feature = "vulkan")]
            (RuntimeInner::Vulkan(runtime), RuntimeConfig::Vulkan(settings)) => {
                runtime.grow_graph_working_set(
                    limits.vulkan_capacity()?,
                    settings,
                    limits.resource_capacity,
                    set,
                )?;
            }
            #[cfg(all(feature = "d3d12", windows))]
            (RuntimeInner::D3D12(runtime), RuntimeConfig::D3D12(settings)) => {
                let classes = D3D12Classes::new(set.resources)?;
                runtime.prepare_samplers(set.samplers)?;
                runtime.prepare_graph_with_capacity(
                    limits.d3d12_config(settings).capacity,
                    Some(&classes.buffers),
                    &classes.images,
                    set.raster,
                    set.compute,
                )?;
            }
            #[cfg(all(
                feature = "vulkan",
                any(all(windows, feature = "d3d12"), sashiko_webgpu)
            ))]
            _ => unreachable!("runtime owns its backend configuration"),
            #[cfg(not(sashiko_gpu_backend))]
            _ => match config {},
        }
        #[cfg(sashiko_gpu_backend)]
        {
            self.image_releases.set_limit(limits.resource_capacity);
            self.config = config;
            self.limits = limits;
            Ok(())
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
) -> Result<crate::d3d12::D3D12Runtime, ExecutionError> {
    let classes = D3D12Classes::new(set.resources)?;
    let mut runtime = crate::d3d12::D3D12Runtime::new_with_working_set(
        limits.d3d12_config(settings),
        &classes.buffers,
        &classes.images,
        set.raster,
        set.compute,
    )?;
    runtime.prepare_samplers(set.samplers)?;
    Ok(runtime)
}

#[cfg(all(feature = "d3d12", windows))]
struct D3D12Classes {
    buffers: Vec<crate::d3d12::D3D12BufferClass>,
    images: Vec<crate::d3d12::D3D12ImageClass>,
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
                ResourceClass::Buffer { desc, count } => {
                    buffers.push(crate::d3d12::D3D12BufferClass {
                        size: desc.size,
                        count,
                    })
                }
                ResourceClass::Image { desc, count } => {
                    images.push(crate::d3d12::D3D12ImageClass { desc, count });
                }
            }
        }
        Ok(Self { buffers, images })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
    use super::*;

    #[cfg(feature = "vulkan")]
    #[test]
    fn fitted_descriptor_pool_accounts_for_shared_depth_variants() {
        use crate::capacity::{GraphRequirements, Preparation};
        let mut prep = Preparation::new(std::num::NonZeroUsize::MIN);
        prep.add_graph_requirements(GraphRequirements {
            draws: 200,
            bindings: 80,
            compute_dispatches: 3,
            ..Default::default()
        })
        .unwrap();
        let mut config = RuntimeConfig::Vulkan(VulkanConfig::default());
        config.fit_preparation(&prep, 0, 0).unwrap();
        assert!(
            matches!(config, RuntimeConfig::Vulkan(settings) if settings.max_shader_bindings >= 163)
        );
    }

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
