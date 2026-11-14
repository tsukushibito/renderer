//! Trusted M2 shaders, pipelines and bounded descriptor leases.
//!
//! The graph executor owns synchronization. Every object referenced by recorded
//! commands, including a descriptor lease, stays alive through proven retirement.

#[cfg(feature = "vulkan-diagnostics")]
use std::ffi::CStr;
#[cfg(feature = "vulkan-diagnostics")]
use std::ffi::CString;
use std::sync::{Arc, Mutex, OnceLock};

use ash::vk;

use super::super::error::RuntimeError;
use super::super::memory::MemoryOwner;
use super::super::resources::DeviceImage;
use crate::graph::RasterParameters;

fn native(owner: &MemoryOwner, operation: &str, result: vk::Result) -> RuntimeError {
    owner.device.observe_result(result);
    RuntimeError::native(operation, owner.device.name(), result)
}

#[cfg(feature = "vulkan-diagnostics")]
fn name_object<H: vk::Handle>(
    owner: &MemoryOwner,
    handle: H,
    name: &CStr,
) -> Result<(), RuntimeError> {
    let info = vk::DebugUtilsObjectNameInfoEXT::default()
        .object_handle(handle)
        .object_name(name);
    // SAFETY: Every caller owns a live handle on this Device; the name remains
    // live through this immediate call and debug_utils is required at startup.
    unsafe {
        owner
            .device
            .debug_utils()
            .set_debug_utils_object_name(&info)
    }
    .map_err(|r| native(owner, "vkSetDebugUtilsObjectNameEXT(Graph)", r))
}

struct ShaderModule {
    owner: Arc<MemoryOwner>,
    module: vk::ShaderModule,
}

impl ShaderModule {
    fn new(owner: Arc<MemoryOwner>, bytes: &[u8]) -> Result<Self, RuntimeError> {
        let words = spirv_words(bytes)?;
        // SAFETY: Only fixed, validated, repository-owned shaders enter here;
        // words are aligned and stay live through synchronous module creation.
        let module = unsafe {
            owner
                .device
                .raw()
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
        }
        .map_err(|result| native(&owner, "vkCreateShaderModule(Graph)", result))?;
        Ok(Self { owner, module })
    }
}

fn spirv_words(bytes: &[u8]) -> Result<Vec<u32>, RuntimeError> {
    if !bytes.len().is_multiple_of(4) || bytes.len() < 20 {
        return Err(RuntimeError::invalid("invalid embedded SPIR-V word length"));
    }
    let words: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect();
    if words[0] != 0x0723_0203 {
        return Err(RuntimeError::invalid("invalid embedded SPIR-V magic"));
    }
    Ok(words)
}

impl Drop for ShaderModule {
    fn drop(&mut self) {
        // SAFETY: Module creation use ends with synchronous pipeline creation;
        // pipelines do not retain a lifetime dependency on ShaderModule.
        unsafe {
            self.owner
                .device
                .raw()
                .destroy_shader_module(self.module, None)
        };
    }
}

struct DescriptorArena {
    owner: Arc<MemoryOwner>,
    pool: vk::DescriptorPool,
}

impl DescriptorArena {
    fn new(
        owner: Arc<MemoryOwner>,
        max_bindings: u32,
        #[cfg_attr(not(feature = "vulkan-diagnostics"), allow(unused_variables))] ordinal: usize,
    ) -> Result<Self, RuntimeError> {
        let sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_IMAGE,
                descriptor_count: max_bindings * 2,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: max_bindings,
            },
        ];
        let info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(max_bindings)
            .pool_sizes(&sizes);
        // SAFETY: Counts checked at ShaderPrograms creation; this arena is
        // exclusively leased, and its pool remains live through recorded use.
        let pool = unsafe { owner.device.raw().create_descriptor_pool(&info, None) }
            .map_err(|result| native(&owner, "vkCreateDescriptorPool(Graph)", result))?;
        let result = Self { owner, pool };
        #[cfg(feature = "vulkan-diagnostics")]
        {
            let name = CString::new(format!("m2-descriptor-arena-{ordinal}"))
                .expect("numeric name has no NUL");
            name_object(&result.owner, pool, &name)?;
        }
        Ok(result)
    }
}

impl Drop for DescriptorArena {
    fn drop(&mut self) {
        // SAFETY: A checked-out arena cannot reach Drop before its executor
        // proves retirement (or cancellation); unknown completion retains it.
        unsafe {
            self.owner
                .device
                .raw()
                .destroy_descriptor_pool(self.pool, None)
        };
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ShaderStats {
    pub(super) descriptor_arenas_created: usize,
    pub(super) descriptor_arenas_reused: usize,
    pub(super) descriptor_arenas_leased: usize,
}

#[derive(Default)]
struct DescriptorCache {
    idle: Vec<DescriptorArena>,
    stats: ShaderStats,
}

/// The live operations of one graph. Unused programs impose no capabilities or
/// native creation work; adding another kind never replaces a published program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ShaderNeeds {
    pub(super) compute: bool,
    pub(super) raster: bool,
    pub(super) composite: bool,
}

impl ShaderNeeds {
    pub(super) fn any(self) -> bool {
        self.compute || self.raster || self.composite
    }
}

#[derive(Clone, Copy, Debug)]
enum ProgramKind {
    Compute,
    Raster,
    Composite,
}

impl ProgramKind {
    #[cfg(feature = "vulkan-diagnostics")]
    fn name(self) -> &'static CStr {
        match self {
            Self::Compute => c"m2-image-compute",
            Self::Raster => c"m2-indexed-mesh-depth",
            Self::Composite => c"m2-sampled-composite",
        }
    }
}

/// Validate the ABI used by this operation before any of its native objects are
/// created. The query boundary also lets CPU tests model an adapter lacking D32
/// without substituting a fake implementation of the Vulkan object API.
fn validate_program(
    kind: ProgramKind,
    limits: &vk::PhysicalDeviceLimits,
    mut query: impl FnMut(vk::Format) -> vk::FormatProperties,
) -> Result<(), RuntimeError> {
    let supported = match kind {
        ProgramKind::Compute => {
            limits.max_compute_work_group_size[0] >= 8
                && limits.max_compute_work_group_size[1] >= 8
                && limits.max_compute_work_group_size[2] >= 1
                && limits.max_compute_work_group_invocations >= 64
                && limits.max_push_constants_size >= 4
                && limits.max_per_stage_descriptor_storage_images >= 2
                && limits.max_descriptor_set_storage_images >= 2
        }
        ProgramKind::Raster => {
            limits.max_push_constants_size >= 80
                && limits.max_vertex_input_attributes >= 2
                && limits.max_vertex_input_bindings >= 1
                && limits.max_vertex_input_binding_stride >= 28
                && limits.max_vertex_input_attribute_offset >= 12
                && limits.max_color_attachments >= 1
        }
        ProgramKind::Composite => {
            limits.max_per_stage_descriptor_samplers >= 1
                && limits.max_per_stage_descriptor_sampled_images >= 1
                && limits.max_descriptor_set_samplers >= 1
                && limits.max_descriptor_set_sampled_images >= 1
                && limits.max_color_attachments >= 1
        }
    };
    if !supported {
        return Err(RuntimeError::unsupported(format!(
            "adapter limits do not support the fixed {kind:?} shader ABI"
        )));
    }
    let mut require = |format, features: vk::FormatFeatureFlags, buffer| {
        let properties = query(format);
        let available = if buffer {
            properties.buffer_features
        } else {
            properties.optimal_tiling_features
        };
        if available.contains(features) {
            Ok(())
        } else {
            Err(RuntimeError::unsupported(format!(
                "{kind:?} requires format {format:?} features {features:?}; available {available:?}"
            )))
        }
    };
    match kind {
        ProgramKind::Compute => require(
            vk::Format::R8G8B8A8_UNORM,
            vk::FormatFeatureFlags::STORAGE_IMAGE,
            false,
        ),
        ProgramKind::Raster => {
            require(
                vk::Format::R8G8B8A8_UNORM,
                vk::FormatFeatureFlags::COLOR_ATTACHMENT,
                false,
            )?;
            // D32 is not mandatory on every Vulkan 1.3 adapter. Checking only
            // DeviceImage creation misses graphs that do not even use depth.
            require(
                vk::Format::D32_SFLOAT,
                vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT,
                false,
            )?;
            for format in [
                vk::Format::R32G32B32_SFLOAT,
                vk::Format::R32G32B32A32_SFLOAT,
            ] {
                require(format, vk::FormatFeatureFlags::VERTEX_BUFFER, true)?;
            }
            Ok(())
        }
        ProgramKind::Composite => require(
            vk::Format::R8G8B8A8_UNORM,
            vk::FormatFeatureFlags::COLOR_ATTACHMENT | vk::FormatFeatureFlags::SAMPLED_IMAGE,
            false,
        ),
    }
}

/// One immutable program owns its complete native dependency chain. Null fields
/// permit immediate ownership and cleanup of partially initialized programs.
struct Program {
    owner: Arc<MemoryOwner>,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    sampler: vk::Sampler,
}

impl Program {
    fn new(
        owner: Arc<MemoryOwner>,
        kind: ProgramKind,
        limits: &vk::PhysicalDeviceLimits,
    ) -> Result<Self, RuntimeError> {
        validate_program(kind, limits, |format| {
            // SAFETY: The retained Device owns this live adapter and Instance.
            unsafe {
                owner
                    .device
                    .instance_raw()
                    .get_physical_device_format_properties(owner.device.physical_device(), format)
            }
        })?;
        let mut result = Self {
            owner,
            set_layout: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            sampler: vk::Sampler::null(),
        };
        result.initialize(kind)?;
        Ok(result)
    }

    fn initialize(&mut self, kind: ProgramKind) -> Result<(), RuntimeError> {
        let device = self.owner.device.raw();
        let compute_bindings = [0, 1].map(|binding| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(binding)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        });
        let composite_bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let bindings: &[_] = match kind {
            ProgramKind::Compute => &compute_bindings,
            ProgramKind::Raster => &[],
            ProgramKind::Composite => &composite_bindings,
        };
        // SAFETY: Capability checks precede native creation. Every successful
        // handle is immediately owned here, including on failure or unwind.
        unsafe {
            if !bindings.is_empty() {
                self.set_layout = device
                    .create_descriptor_set_layout(
                        &vk::DescriptorSetLayoutCreateInfo::default().bindings(bindings),
                        None,
                    )
                    .map_err(|r| native(&self.owner, "vkCreateDescriptorSetLayout(Graph)", r))?;
            }
            let layouts = [self.set_layout];
            let pushes = [vk::PushConstantRange {
                stage_flags: if matches!(kind, ProgramKind::Raster) {
                    vk::ShaderStageFlags::VERTEX
                } else {
                    vk::ShaderStageFlags::COMPUTE
                },
                offset: 0,
                size: if matches!(kind, ProgramKind::Raster) {
                    80
                } else {
                    4
                },
            }];
            let mut layout_info = vk::PipelineLayoutCreateInfo::default();
            if !bindings.is_empty() {
                layout_info = layout_info.set_layouts(&layouts);
            }
            if matches!(kind, ProgramKind::Compute | ProgramKind::Raster) {
                layout_info = layout_info.push_constant_ranges(&pushes);
            }
            self.layout = device
                .create_pipeline_layout(&layout_info, None)
                .map_err(|r| native(&self.owner, "vkCreatePipelineLayout(Graph)", r))?;
            if matches!(kind, ProgramKind::Composite) {
                self.sampler = device
                    .create_sampler(
                        &vk::SamplerCreateInfo::default()
                            .mag_filter(vk::Filter::NEAREST)
                            .min_filter(vk::Filter::NEAREST)
                            .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                            .max_lod(0.0),
                        None,
                    )
                    .map_err(|r| native(&self.owner, "vkCreateSampler(Graph)", r))?;
            }
        }
        self.pipeline = match kind {
            ProgramKind::Compute => self.compute_pipeline()?,
            ProgramKind::Raster => self.graphics_pipeline(true)?,
            ProgramKind::Composite => self.graphics_pipeline(false)?,
        };
        #[cfg(feature = "vulkan-diagnostics")]
        {
            let name = kind.name();
            name_object(&self.owner, self.pipeline, name)?;
            let layout_name = CString::new(format!("{}-layout", name.to_string_lossy()))
                .expect("fixed program names contain no NUL");
            name_object(&self.owner, self.layout, &layout_name)?;
            if !bindings.is_empty() {
                let set_name = CString::new(format!("{}-set-layout", name.to_string_lossy()))
                    .expect("fixed program names contain no NUL");
                name_object(&self.owner, self.set_layout, &set_name)?;
            }
            if matches!(kind, ProgramKind::Composite) {
                name_object(&self.owner, self.sampler, c"m2-composite-nearest-sampler")?;
            }
        }
        Ok(())
    }

    fn compute_pipeline(&self) -> Result<vk::Pipeline, RuntimeError> {
        let compute = ShaderModule::new(
            Arc::clone(&self.owner),
            include_bytes!("../../../../shaders/image.comp.spv"),
        )?;
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(compute.module)
            .name(c"main");
        let info = [vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(self.layout)];
        // SAFETY: Fixed module and layout ABI agree. Local arrays remain live
        // through creation. Partial pipeline successes are destroyed on failure.
        match unsafe {
            self.owner
                .device
                .raw()
                .create_compute_pipelines(vk::PipelineCache::null(), &info, None)
        } {
            Ok(pipelines) => Ok(pipelines[0]),
            Err((partial, error)) => {
                for pipeline in partial {
                    // SAFETY: Returned partial pipelines have no recorded uses.
                    unsafe { self.owner.device.raw().destroy_pipeline(pipeline, None) };
                }
                Err(native(
                    &self.owner,
                    "vkCreateComputePipelines(Graph)",
                    error,
                ))
            }
        }
    }
    fn graphics_pipeline(&self, mesh: bool) -> Result<vk::Pipeline, RuntimeError> {
        let vertex = ShaderModule::new(
            Arc::clone(&self.owner),
            if mesh {
                include_bytes!("../../../../shaders/mesh.vert.spv")
            } else {
                include_bytes!("../../../../shaders/composite.vert.spv")
            },
        )?;
        let fragment = ShaderModule::new(
            Arc::clone(&self.owner),
            if mesh {
                include_bytes!("../../../../shaders/mesh.frag.spv")
            } else {
                include_bytes!("../../../../shaders/composite.frag.spv")
            },
        )?;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vertex.module)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment.module)
                .name(c"main"),
        ];
        let bindings = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: 28,
            input_rate: vk::VertexInputRate::VERTEX,
        }];
        let attributes = [
            vk::VertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: vk::Format::R32G32B32_SFLOAT,
                offset: 0,
            },
            vk::VertexInputAttributeDescription {
                location: 1,
                binding: 0,
                format: vk::Format::R32G32B32A32_SFLOAT,
                offset: 12,
            },
        ];
        let mut vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        if mesh {
            vertex_input = vertex_input
                .vertex_binding_descriptions(&bindings)
                .vertex_attribute_descriptions(&attributes);
        }
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let depth = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(mesh)
            .depth_write_enable(mesh)
            .depth_compare_op(vk::CompareOp::LESS);
        let color_attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&color_attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let formats = [vk::Format::R8G8B8A8_UNORM];
        let mut rendering = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&formats)
            .depth_attachment_format(if mesh {
                vk::Format::D32_SFLOAT
            } else {
                vk::Format::UNDEFINED
            });
        let infos = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(self.layout)
            .push_next(&mut rendering)];
        // SAFETY: All pointers refer to local live arrays and the fixed shader
        // ABI; Dynamic Rendering1.3 and the queried attachment formats are used.
        match unsafe {
            self.owner.device.raw().create_graphics_pipelines(
                vk::PipelineCache::null(),
                &infos,
                None,
            )
        } {
            Ok(pipelines) => Ok(pipelines[0]),
            Err((partial, error)) => {
                for pipeline in partial {
                    // SAFETY: Returned partial pipelines have no recorded uses.
                    unsafe { self.owner.device.raw().destroy_pipeline(pipeline, None) };
                }
                Err(native(
                    &self.owner,
                    "vkCreateGraphicsPipelines(Graph)",
                    error,
                ))
            }
        }
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        // SAFETY: Published programs are owned by ShaderPrograms until all work
        // retires. Unpublished partial programs have never been recorded.
        unsafe {
            let device = self.owner.device.raw();
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
            device.destroy_sampler(self.sampler, None);
        }
    }
}

/// Programs are published once by kind and retained for the runtime lifetime.
/// A short initialization lock allows adding a new kind while prior submissions
/// retain immutable programs; ordinary recording reads them without locking.
pub(super) struct ShaderPrograms {
    owner: Arc<MemoryOwner>,
    compute: OnceLock<Program>,
    raster: OnceLock<Program>,
    composite: OnceLock<Program>,
    initialization: Mutex<()>,
    slots: usize,
    max_bindings: u32,
    limits: vk::PhysicalDeviceLimits,
    descriptors: Mutex<DescriptorCache>,
}

impl ShaderPrograms {
    pub(super) fn new(
        owner: Arc<MemoryOwner>,
        concurrent_slots: usize,
        max_bindings: u32,
    ) -> Result<Arc<Self>, RuntimeError> {
        owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if concurrent_slots == 0 || max_bindings == 0 || max_bindings > u32::MAX / 2 {
            return Err(RuntimeError::invalid(
                "Graph shader capacities must be nonzero and descriptor counts representable",
            ));
        }
        let limits = *owner.device.limits();
        Ok(Arc::new(Self {
            owner,
            compute: OnceLock::new(),
            raster: OnceLock::new(),
            composite: OnceLock::new(),
            initialization: Mutex::new(()),
            slots: concurrent_slots,
            max_bindings,
            limits,
            descriptors: Mutex::new(DescriptorCache::default()),
        }))
    }

    pub(super) fn ensure(&self, needs: ShaderNeeds) -> Result<(), RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if (!needs.compute || self.compute.get().is_some())
            && (!needs.raster || self.raster.get().is_some())
            && (!needs.composite || self.composite.get().is_some())
        {
            return Ok(());
        }
        let _initialization = self
            .initialization
            .lock()
            .map_err(|_| RuntimeError::invalid("Graph program initialization lock poisoned"))?;
        for (needed, slot, kind) in [
            (needs.compute, &self.compute, ProgramKind::Compute),
            (needs.raster, &self.raster, ProgramKind::Raster),
            (needs.composite, &self.composite, ProgramKind::Composite),
        ] {
            if needed && slot.get().is_none() {
                // Cache only successful creation; a transient OOM can be retried.
                let program = Program::new(Arc::clone(&self.owner), kind, &self.limits)?;
                slot.set(program).map_err(|_| {
                    RuntimeError::invalid(
                        "Graph program was unexpectedly published during initialization",
                    )
                })?;
            }
        }
        Ok(())
    }

    fn compute(&self) -> &Program {
        self.compute
            .get()
            .expect("Compute program prepared before recording")
    }
    fn raster(&self) -> &Program {
        self.raster
            .get()
            .expect("Raster program prepared before recording")
    }
    fn composite(&self) -> &Program {
        self.composite
            .get()
            .expect("Composite program prepared before recording")
    }

    #[cfg(test)]
    pub(super) fn handles(&self) -> [vk::Pipeline; 3] {
        [&self.compute, &self.raster, &self.composite].map(|slot| {
            slot.get()
                .map_or(vk::Pipeline::null(), |program| program.pipeline)
        })
    }

    pub(super) fn acquire_bindings(self: &Arc<Self>) -> Result<ShaderBindings, RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let mut cache = self
            .descriptors
            .lock()
            .map_err(|_| RuntimeError::invalid("Graph descriptor cache lock poisoned"))?;
        if cache.stats.descriptor_arenas_leased == self.slots {
            return Err(RuntimeError::capacity(
                "Graph descriptor arena capacity exhausted",
            ));
        }
        let arena = if let Some(arena) = cache.idle.pop() {
            cache.stats.descriptor_arenas_reused += 1;
            arena
        } else {
            let arena = DescriptorArena::new(
                Arc::clone(&self.owner),
                self.max_bindings,
                cache.stats.descriptor_arenas_created,
            )?;
            cache.stats.descriptor_arenas_created += 1;
            arena
        };
        cache.stats.descriptor_arenas_leased += 1;
        Ok(ShaderBindings {
            programs: Arc::clone(self),
            arena: Some(arena),
            used: 0,
        })
    }

    pub(super) fn stats(&self) -> ShaderStats {
        self.descriptors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stats
    }

    /// # Safety
    /// `command` is recording outside rendering. Bindings/images/programs are
    /// retained through GPU retirement; prior dependencies establish GENERAL
    /// src/dst layouts and shader read/write visibility. `mode` is 0 or 1.
    pub(super) unsafe fn record_compute(
        &self,
        command: vk::CommandBuffer,
        set: vk::DescriptorSet,
        destination: &DeviceImage,
        mode: u32,
    ) {
        let device = self.owner.device.raw();
        // SAFETY: The executor satisfies the documented retention/layout/ABI
        // contract; validated descriptor set is immutable during command use.
        unsafe {
            device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::COMPUTE,
                self.compute().pipeline,
            );
            device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::COMPUTE,
                self.compute().layout,
                0,
                &[set],
                &[],
            );
            device.cmd_push_constants(
                command,
                self.compute().layout,
                vk::ShaderStageFlags::COMPUTE,
                0,
                &mode.to_ne_bytes(),
            );
            device.cmd_dispatch(
                command,
                destination.extent.width.div_ceil(8),
                destination.extent.height.div_ceil(8),
                1,
            );
        }
    }

    /// # Safety
    /// Command records outside rendering; all supplied resources and programs
    /// survive retirement. Vertex/index ranges obey the fixed ABI and validated
    /// indices. Color/depth are distinct, equal extents and in attachment layouts
    /// after dependencies, with valid contents for LOAD. Attachments are whole.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn record_raster(
        &self,
        command: vk::CommandBuffer,
        vertices: vk::Buffer,
        indices: vk::Buffer,
        index_count: u32,
        color: &DeviceImage,
        depth: &DeviceImage,
        color_load: vk::AttachmentLoadOp,
        color_store: vk::AttachmentStoreOp,
        clear_color: [f32; 4],
        depth_load: vk::AttachmentLoadOp,
        depth_store: vk::AttachmentStoreOp,
        clear_depth: f32,
        parameters: &RasterParameters,
    ) {
        // SAFETY: The caller supplies a validated draw and retained attachments.
        unsafe {
            self.record_rasters(
                command,
                std::iter::once((vertices, indices, index_count, parameters)),
                color,
                depth,
                color_load,
                color_store,
                clear_color,
                depth_load,
                depth_store,
                clear_depth,
            );
        }
    }

    /// # Safety
    /// All fixed-pipeline draws are validated and retained through retirement.
    /// External dependencies precede this whole scope. No shader storage writes
    /// occur; draw order preserves primitive-order attachment operations.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn record_raster_batch<'a>(
        &self,
        command: vk::CommandBuffer,
        draws: impl IntoIterator<Item = (vk::Buffer, vk::Buffer, u32, &'a RasterParameters)>,
        color: &DeviceImage,
        depth: &DeviceImage,
        clear_color: [f32; 4],
        clear_depth: f32,
    ) {
        // SAFETY: The graph batch declares the full clear and every buffer read;
        // the caller validates indices and establishes all attachment layouts.
        unsafe {
            self.record_rasters(
                command,
                draws,
                color,
                depth,
                vk::AttachmentLoadOp::CLEAR,
                vk::AttachmentStoreOp::STORE,
                clear_color,
                vk::AttachmentLoadOp::CLEAR,
                vk::AttachmentStoreOp::STORE,
                clear_depth,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn record_rasters<'a>(
        &self,
        command: vk::CommandBuffer,
        draws: impl IntoIterator<Item = (vk::Buffer, vk::Buffer, u32, &'a RasterParameters)>,
        color: &DeviceImage,
        depth: &DeviceImage,
        color_load: vk::AttachmentLoadOp,
        color_store: vk::AttachmentStoreOp,
        clear_color: [f32; 4],
        depth_load: vk::AttachmentLoadOp,
        depth_store: vk::AttachmentStoreOp,
        clear_depth: f32,
    ) {
        let color_attachments = [vk::RenderingAttachmentInfo::default()
            .image_view(color.view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(color_load)
            .store_op(color_store)
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: clear_color,
                },
            })];
        let depth_attachment = vk::RenderingAttachmentInfo::default()
            .image_view(depth.view)
            .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
            .load_op(depth_load)
            .store_op(depth_store)
            .clear_value(vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: clear_depth,
                    stencil: 0,
                },
            });
        let info = vk::RenderingInfo::default()
            .render_area(vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: color.extent,
            })
            .layer_count(1)
            .color_attachments(&color_attachments)
            .depth_attachment(&depth_attachment);
        let device = self.owner.device.raw();
        // SAFETY: Caller establishes outside-scope dependencies and valid
        // resource/range/binding/layout state. No barrier occurs in this scope.
        unsafe {
            device.cmd_begin_rendering(command, &info);
            device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.raster().pipeline,
            );
            self.viewport(command, color.extent);
            for (vertices, indices, index_count, parameters) in draws {
                let mut bytes = [0_u8; 80];
                for (value, output) in parameters
                    .transform
                    .iter()
                    .flatten()
                    .chain(&parameters.tint)
                    .zip(bytes.as_chunks_mut::<4>().0)
                {
                    output.copy_from_slice(&value.to_ne_bytes());
                }
                device.cmd_push_constants(
                    command,
                    self.raster().layout,
                    vk::ShaderStageFlags::VERTEX,
                    0,
                    &bytes,
                );
                device.cmd_bind_vertex_buffers(command, 0, &[vertices], &[0]);
                device.cmd_bind_index_buffer(command, indices, 0, vk::IndexType::UINT32);
                device.cmd_draw_indexed(command, index_count, 1, 0, 0, 0);
            }
            device.cmd_end_rendering(command);
        }
    }

    /// # Safety
    /// Recording outside rendering; descriptor source is SHADER_READ_ONLY and
    /// destination COLOR_ATTACHMENT_OPTIMAL with required dependencies. Both
    /// images, immutable descriptor set and programs survive retirement; extents
    /// match, source and destination differ. Fullscreen draw initializes all pixels.
    pub(super) unsafe fn record_composite(
        &self,
        command: vk::CommandBuffer,
        set: vk::DescriptorSet,
        destination: &DeviceImage,
    ) {
        let colors = [vk::RenderingAttachmentInfo::default()
            .image_view(destination.view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::DONT_CARE)
            .store_op(vk::AttachmentStoreOp::STORE)];
        let info = vk::RenderingInfo::default()
            .render_area(vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: destination.extent,
            })
            .layer_count(1)
            .color_attachments(&colors);
        let device = self.owner.device.raw();
        // SAFETY: The caller guarantees the documented synchronization and
        // descriptor/resource retention. Fixed triangle covers every pixel.
        unsafe {
            device.cmd_begin_rendering(command, &info);
            device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.composite().pipeline,
            );
            self.viewport(command, destination.extent);
            device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.composite().layout,
                0,
                &[set],
                &[],
            );
            device.cmd_draw(command, 3, 1, 0, 0);
            device.cmd_end_rendering(command);
        }
    }

    unsafe fn viewport(&self, command: vk::CommandBuffer, extent: vk::Extent2D) {
        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: extent.width as f32,
            height: extent.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let scissor = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent,
        };
        // SAFETY: Called only inside our rendering scope with queried dimensions
        // and dynamically enabled viewport/scissor state.
        unsafe {
            self.owner
                .device
                .raw()
                .cmd_set_viewport(command, 0, &[viewport]);
            self.owner
                .device
                .raw()
                .cmd_set_scissor(command, 0, &[scissor]);
        }
    }
}

impl Drop for ShaderPrograms {
    fn drop(&mut self) {
        // Checked-out descriptor leases retain self. Clear cached descriptor
        // pools before the separately owned programs and their layouts drop.
        self.descriptors
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .idle
            .clear();
    }
}

/// Exclusive arena. Executor may release only for canceled/proven-unsubmitted
/// commands or after actual GPU retirement. Unknown completion must retain it.
pub(super) struct ShaderBindings {
    programs: Arc<ShaderPrograms>,
    arena: Option<DescriptorArena>,
    used: u32,
}

impl ShaderBindings {
    fn allocate(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Result<vk::DescriptorSet, RuntimeError> {
        if self.used >= self.programs.max_bindings {
            return Err(RuntimeError::capacity(
                "Graph descriptor binding capacity exhausted",
            ));
        }
        let layouts = [layout];
        let info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.arena.as_ref().expect("live descriptor lease").pool)
            .set_layouts(&layouts);
        // SAFETY: This lease has exclusive host access to its pool. A freshly
        // allocated set has no command references and can be updated once.
        let sets = unsafe {
            self.programs
                .owner
                .device
                .raw()
                .allocate_descriptor_sets(&info)
        }
        .map_err(|r| native(&self.programs.owner, "vkAllocateDescriptorSets(Graph)", r))?;
        self.used += 1;
        Ok(sets[0])
    }

    pub(super) fn compute(
        &mut self,
        source: &DeviceImage,
        destination: &DeviceImage,
    ) -> Result<vk::DescriptorSet, RuntimeError> {
        if !source.belongs_to(&self.programs.owner)
            || !destination.belongs_to(&self.programs.owner)
            || source.format != vk::Format::R8G8B8A8_UNORM
            || destination.format != source.format
            || source.image == destination.image
            || source.extent != destination.extent
        {
            return Err(RuntimeError::invalid(
                "Compute bindings require distinct same-Device equal RGBA8 images",
            ));
        }
        if destination.extent.width.div_ceil(8)
            > self.programs.limits.max_compute_work_group_count[0]
            || destination.extent.height.div_ceil(8)
                > self.programs.limits.max_compute_work_group_count[1]
            || self.programs.limits.max_compute_work_group_count[2] < 1
        {
            return Err(RuntimeError::unsupported(
                "Compute dispatch exceeds adapter workgroup counts",
            ));
        }
        let set = self.allocate(self.programs.compute().set_layout)?;
        let source_info = [vk::DescriptorImageInfo::default()
            .image_view(source.view)
            .image_layout(vk::ImageLayout::GENERAL)];
        let destination_info = [vk::DescriptorImageInfo::default()
            .image_view(destination.view)
            .image_layout(vk::ImageLayout::GENERAL)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&source_info),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&destination_info),
        ];
        // SAFETY: Set is fresh/unbound and exclusively owned. Image views belong
        // to Device and match shader ABI; executor retains them until retirement.
        unsafe {
            self.programs
                .owner
                .device
                .raw()
                .update_descriptor_sets(&writes, &[])
        };
        Ok(set)
    }

    pub(super) fn composite(
        &mut self,
        source: &DeviceImage,
    ) -> Result<vk::DescriptorSet, RuntimeError> {
        if !source.belongs_to(&self.programs.owner) || source.format != vk::Format::R8G8B8A8_UNORM {
            return Err(RuntimeError::invalid(
                "Composite binding requires same-Device RGBA8 image",
            ));
        }
        let set = self.allocate(self.programs.composite().set_layout)?;
        let image_info = [vk::DescriptorImageInfo::default()
            .sampler(self.programs.composite().sampler)
            .image_view(source.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let writes = [vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&image_info)];
        // SAFETY: Fresh set, immutable after this update, fixed combined sampler
        // ABI; executor retains image/view and this lease through retirement.
        unsafe {
            self.programs
                .owner
                .device
                .raw()
                .update_descriptor_sets(&writes, &[])
        };
        Ok(set)
    }
}

impl Drop for ShaderBindings {
    fn drop(&mut self) {
        let Some(arena) = self.arena.take() else {
            return;
        };
        let mut cache = self
            .programs
            .descriptors
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.stats.descriptor_arenas_leased -= 1;
        if self.programs.owner.device.ensure_active().is_err() {
            return;
        }
        // SAFETY: Executor releases this lease only after cancellation or proven
        // retirement. No pending command uses these sets; executable commands
        // invalidated by reset are never submitted again and reset before reuse.
        let reset = unsafe {
            self.programs
                .owner
                .device
                .raw()
                .reset_descriptor_pool(arena.pool, vk::DescriptorPoolResetFlags::empty())
        };
        match reset {
            Ok(()) => cache.idle.push(arena),
            Err(result) => {
                self.programs.owner.device.observe_result(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_capabilities_only_require_the_requested_operation() {
        // A compute-only adapter contract has no graphics ABI requirements.
        let compute_limits = vk::PhysicalDeviceLimits {
            max_compute_work_group_size: [8, 8, 1],
            max_compute_work_group_invocations: 64,
            max_push_constants_size: 4,
            max_per_stage_descriptor_storage_images: 2,
            max_descriptor_set_storage_images: 2,
            ..Default::default()
        };
        validate_program(ProgramKind::Compute, &compute_limits, |format| {
            assert_eq!(format, vk::Format::R8G8B8A8_UNORM);
            vk::FormatProperties {
                optimal_tiling_features: vk::FormatFeatureFlags::STORAGE_IMAGE,
                ..Default::default()
            }
        })
        .unwrap();

        // A sampled composite must also work without compute or D32 support.
        let composite_limits = vk::PhysicalDeviceLimits {
            max_per_stage_descriptor_samplers: 1,
            max_per_stage_descriptor_sampled_images: 1,
            max_descriptor_set_samplers: 1,
            max_descriptor_set_sampled_images: 1,
            max_color_attachments: 1,
            ..Default::default()
        };
        validate_program(ProgramKind::Composite, &composite_limits, |format| {
            assert_eq!(format, vk::Format::R8G8B8A8_UNORM);
            vk::FormatProperties {
                optimal_tiling_features: vk::FormatFeatureFlags::COLOR_ATTACHMENT
                    | vk::FormatFeatureFlags::SAMPLED_IMAGE,
                ..Default::default()
            }
        })
        .unwrap();
    }

    #[test]
    fn raster_rejects_missing_depth_or_color_support_before_native_creation() {
        let limits = vk::PhysicalDeviceLimits {
            max_push_constants_size: 80,
            max_vertex_input_attributes: 2,
            max_vertex_input_bindings: 1,
            max_vertex_input_binding_stride: 28,
            max_vertex_input_attribute_offset: 12,
            max_color_attachments: 1,
            ..Default::default()
        };
        for unsupported in [vk::Format::R8G8B8A8_UNORM, vk::Format::D32_SFLOAT] {
            let mut queried = Vec::new();
            let error = validate_program(ProgramKind::Raster, &limits, |format| {
                queried.push(format);
                if format == unsupported {
                    return vk::FormatProperties::default();
                }
                vk::FormatProperties {
                    optimal_tiling_features: vk::FormatFeatureFlags::COLOR_ATTACHMENT
                        | vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT,
                    buffer_features: vk::FormatFeatureFlags::VERTEX_BUFFER,
                    ..Default::default()
                }
            })
            .unwrap_err();
            assert_eq!(error.kind(), super::super::RuntimeErrorKind::Unsupported);
            assert!(error.message.contains(&format!("{unsupported:?}")));
            assert_eq!(queried.last(), Some(&unsupported));
        }
    }

    #[test]
    fn trusted_shader_bytes_are_decoded_without_alignment_assumptions() {
        let bytes = include_bytes!("../../../../shaders/image.comp.spv");
        let mut unaligned = vec![0xff];
        unaligned.extend_from_slice(bytes);
        let words = spirv_words(&unaligned[1..]).unwrap();
        assert_eq!(words[0], 0x0723_0203);
        assert_eq!(
            words[1], 0x0001_0300,
            "fixed SPIR-V1.3 avoids optional maintenance4 LocalSizeId"
        );
        for malformed in [&bytes[..bytes.len() - 1], &bytes[..16], &[0u8; 20][..]] {
            assert!(spirv_words(malformed).is_err());
        }
    }
}
