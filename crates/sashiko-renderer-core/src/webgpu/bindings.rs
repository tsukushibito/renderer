//! Explicit shader layouts and bind groups shared by compute and raster recording.
use std::{collections::HashMap, num::NonZeroU64};

use super::engine::Resources;
use crate::{
    ImageFormat,
    capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind},
    graph::BindingOperands,
    program::{
        BindingLayout, BindingType, MAX_RASTER_BIND_GROUPS, SampledImageSamplerPair, ShaderStages,
    },
    runtime::ExecutionError,
};

pub(super) fn texture_format(format: ImageFormat) -> wgpu::TextureFormat {
    use wgpu::TextureFormat as F;
    match format {
        ImageFormat::Rgba8Unorm => F::Rgba8Unorm,
        ImageFormat::Rgba8Srgb => F::Rgba8UnormSrgb,
        ImageFormat::Rgba16Float => F::Rgba16Float,
        ImageFormat::Depth32Float => F::Depth32Float,
        ImageFormat::Rgba32Float => F::Rgba32Float,
        ImageFormat::Bc1RgbaUnorm => F::Bc1RgbaUnorm,
        ImageFormat::Bc1RgbaSrgb => F::Bc1RgbaUnormSrgb,
        ImageFormat::Bc3RgbaUnorm => F::Bc3RgbaUnorm,
        ImageFormat::Bc3RgbaSrgb => F::Bc3RgbaUnormSrgb,
        ImageFormat::Bc5RgUnorm => F::Bc5RgUnorm,
        ImageFormat::Bc6hRgbUfloat => F::Bc6hRgbUfloat,
        ImageFormat::Bc7RgbaUnorm => F::Bc7RgbaUnorm,
        ImageFormat::Bc7RgbaSrgb => F::Bc7RgbaUnormSrgb,
    }
}

fn stages(scope: ShaderStages) -> wgpu::ShaderStages {
    let mut result = wgpu::ShaderStages::empty();
    for (stage, native) in [
        (ShaderStages::VERTEX, wgpu::ShaderStages::VERTEX),
        (ShaderStages::FRAGMENT, wgpu::ShaderStages::FRAGMENT),
        (ShaderStages::COMPUTE, wgpu::ShaderStages::COMPUTE),
    ] {
        if scope.contains(stage) {
            result |= native;
        }
    }
    result
}

/// Bytes of a per-draw constant record in the dynamic uniform arena.
#[derive(Clone, Copy)]
pub(super) struct ImmediateBinding {
    pub size: u64,
    pub stages: ShaderStages,
}

const IMMEDIATE_SLOT: u32 = 255;
type Groups = [Option<wgpu::BindGroup>; MAX_RASTER_BIND_GROUPS];

struct Slot {
    binding: u32,
    operand: Option<usize>,
    min_offset_alignment: u64,
    max_buffer_size: u64,
}

struct Group {
    layout: wgpu::BindGroupLayout,
    slots: Vec<Slot>,
}

pub(super) struct ProgramLayout {
    pipeline: wgpu::PipelineLayout,
    groups: Vec<Option<Group>>,
    immediate_size: Option<NonZeroU64>,
}

impl ProgramLayout {
    pub(super) fn new(
        device: &wgpu::Device,
        slots: &[BindingLayout],
        sampling_pairs: &[SampledImageSamplerPair],
        nonfiltering_samplers: &[(u32, u32)],
        immediate: Option<ImmediateBinding>,
    ) -> Result<Self, ExecutionError> {
        let limits = device.limits();
        let group_count = slots
            .iter()
            .map(|s| u64::from(s.group) + 1)
            .max()
            .unwrap_or(0)
            .max(u64::from(immediate.is_some()));
        if group_count > u64::from(limits.max_bind_groups)
            || group_count > MAX_RASTER_BIND_GROUPS as u64
        {
            return Err(ExecutionError::invalid(
                "WebGPU program exceeds bind group limit",
            ));
        }
        let immediate_size = immediate
            .map(|item| {
                item.size
                    .checked_next_multiple_of(16)
                    .and_then(NonZeroU64::new)
                    .ok_or_else(|| ExecutionError::invalid("invalid WebGPU inline uniform size"))
            })
            .transpose()?;
        let mut groups = Vec::with_capacity(group_count as usize);
        let compute_has_samplers = slots
            .iter()
            .any(|s| s.stages.contains(ShaderStages::COMPUTE) && s.kind == BindingType::Sampler);
        for group in 0..group_count as u32 {
            let mut entries = Vec::new();
            let mut operands = Vec::new();
            for (index, slot) in slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| slot.group == group)
            {
                let is_nonfiltering =
                    |group, binding| nonfiltering_samplers.contains(&(group, binding));
                // A Load-only texture and a texture used exclusively with nearest-only
                // samplers can bind nonfilterable formats, including depth-as-float.
                let filterable = compute_has_samplers
                    || sampling_pairs.iter().any(|pair| {
                        (pair.image_group, pair.image_binding) == (slot.group, slot.binding)
                            && !is_nonfiltering(pair.sampler_group, pair.sampler_binding)
                    });
                let ty = match slot.kind {
                    BindingType::Uniform
                    | BindingType::StorageRead
                    | BindingType::StorageWrite
                    | BindingType::StorageReadWrite => wgpu::BindingType::Buffer {
                        ty: if slot.kind == BindingType::Uniform {
                            wgpu::BufferBindingType::Uniform
                        } else {
                            wgpu::BufferBindingType::Storage {
                                read_only: slot.kind == BindingType::StorageRead,
                            }
                        },
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(slot.min_size),
                    },
                    BindingType::ComparisonSampler => {
                        wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison)
                    }
                    BindingType::SampledDepth => wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    BindingType::SampledImage => wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    BindingType::StorageImageRead(format)
                    | BindingType::StorageImageWrite(format) => wgpu::BindingType::StorageTexture {
                        access: if matches!(slot.kind, BindingType::StorageImageRead(_)) {
                            wgpu::StorageTextureAccess::ReadOnly
                        } else {
                            wgpu::StorageTextureAccess::WriteOnly
                        },
                        format: texture_format(format),
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    BindingType::Sampler => {
                        wgpu::BindingType::Sampler(if is_nonfiltering(slot.group, slot.binding) {
                            wgpu::SamplerBindingType::NonFiltering
                        } else {
                            wgpu::SamplerBindingType::Filtering
                        })
                    }
                };
                let (alignment, limit) = match slot.kind {
                    BindingType::Uniform => (
                        limits.min_uniform_buffer_offset_alignment,
                        limits.max_uniform_buffer_binding_size,
                    ),
                    BindingType::StorageRead
                    | BindingType::StorageWrite
                    | BindingType::StorageReadWrite => (
                        limits.min_storage_buffer_offset_alignment,
                        limits.max_storage_buffer_binding_size,
                    ),
                    _ => (1, 0),
                };
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding: slot.binding,
                    visibility: stages(slot.stages),
                    ty,
                    count: None,
                });
                operands.push(Slot {
                    binding: slot.binding,
                    operand: Some(index),
                    min_offset_alignment: u64::from(alignment),
                    max_buffer_size: limit,
                });
            }
            if group == 0
                && let Some(immediate) = immediate
            {
                if entries.iter().any(|e| e.binding == IMMEDIATE_SLOT) {
                    return Err(ExecutionError::invalid(
                        "WebGPU inline uniform conflicts with shader binding 255",
                    ));
                }
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding: IMMEDIATE_SLOT,
                    visibility: stages(immediate.stages),
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: immediate_size,
                    },
                    count: None,
                });
                operands.push(Slot {
                    binding: IMMEDIATE_SLOT,
                    operand: None,
                    min_offset_alignment: 1,
                    max_buffer_size: limits.max_uniform_buffer_binding_size,
                });
            }
            groups.push((!entries.is_empty()).then(|| Group {
                layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: None,
                    entries: &entries,
                }),
                slots: operands,
            }));
        }
        let native: Vec<_> = groups
            .iter()
            .map(|group| group.as_ref().map(|g| &g.layout))
            .collect();
        let pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &native,
            immediate_size: 0,
        });
        Ok(Self {
            pipeline,
            groups,
            immediate_size,
        })
    }
    pub(super) fn pipeline(&self) -> &wgpu::PipelineLayout {
        &self.pipeline
    }
}

// Keys retain device handles, so a recycled logical name or a new view of the
// same texture cannot hit a stale group. Dynamic offsets live on the draw.
#[derive(Clone, Eq, Hash, PartialEq)]
enum Atom {
    Layout(wgpu::BindGroupLayout),
    Buffer(wgpu::Buffer, u64, NonZeroU64),
    Image(wgpu::TextureView),
    Sampler(wgpu::Sampler),
}

pub(super) struct BindGroups {
    cache: HashMap<Box<[Atom]>, wgpu::BindGroup>,
    key: Vec<Atom>,
    capacity: usize,
}

impl BindGroups {
    pub(super) fn new(capacity: usize) -> Result<Self, ExecutionError> {
        #[allow(
            clippy::mutable_key_type,
            reason = "wgpu handles hash their immutable identity, independently of buffer map state"
        )]
        let mut cache = HashMap::new();
        cache
            .try_reserve(capacity)
            .map_err(|_| ExecutionError::invalid("could not reserve WebGPU bind group cache"))?;
        Ok(Self {
            cache,
            key: Vec::new(),
            capacity,
        })
    }

    pub(super) fn clear(&mut self) {
        self.cache.clear();
        self.key.clear();
    }

    pub(super) fn resolve(
        &mut self,
        device: &wgpu::Device,
        layout: &ProgramLayout,
        operands: &[BindingOperands],
        resources: &Resources<'_>,
        immediate: Option<&wgpu::Buffer>,
    ) -> Result<Groups, ExecutionError> {
        let mut bound = std::array::from_fn(|_| None);
        for (group_index, group) in layout.groups.iter().enumerate() {
            let Some(group) = group else { continue };
            self.key.clear();
            self.key.push(Atom::Layout(group.layout.clone()));
            for slot in &group.slots {
                let atom = match slot.operand.map(|i| &operands[i]) {
                    Some(BindingOperands::Buffer { resource, range }) => {
                        let size = NonZeroU64::new(range.size).ok_or_else(|| {
                            ExecutionError::invalid("empty WebGPU buffer binding")
                        })?;
                        Atom::Buffer(resources.buffer(*resource)?.clone(), range.offset, size)
                    }
                    Some(BindingOperands::ImageView(view)) => {
                        Atom::Image(resources.view(*view)?.clone())
                    }
                    Some(BindingOperands::Sampler(desc)) => {
                        Atom::Sampler(resources.sampler(*desc)?.clone())
                    }
                    None => Atom::Buffer(
                        immediate
                            .ok_or_else(|| {
                                ExecutionError::invalid("missing WebGPU inline uniform arena")
                            })?
                            .clone(),
                        0,
                        layout
                            .immediate_size
                            .expect("prepared dynamic uniform binding"),
                    ),
                };
                self.key.push(atom);
            }
            if let Some(group) = self.cache.get(self.key.as_slice()) {
                bound[group_index] = Some(group.clone());
                continue;
            }
            if self.cache.len() == self.capacity {
                return Err(ExecutionError::with_capacity(
                    "WebGPU bind group cache is full",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Descriptors,
                        CapacityRecovery::Reconfigure,
                        Some(self.capacity as u64),
                        Some(self.capacity as u64 + 1),
                    ),
                ));
            }
            let entries: Vec<_> = group
                .slots
                .iter()
                .zip(&self.key[1..])
                .map(|(slot, atom)| {
                    let resource = match atom {
                        Atom::Buffer(buffer, offset, size) => {
                            if !offset.is_multiple_of(slot.min_offset_alignment)
                                || size.get() > slot.max_buffer_size
                            {
                                return Err(ExecutionError::invalid(
                                    "WebGPU buffer binding exceeds device size or offset alignment",
                                ));
                            }
                            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer,
                                offset: *offset,
                                size: Some(*size),
                            })
                        }
                        Atom::Image(view) => wgpu::BindingResource::TextureView(view),
                        Atom::Sampler(sampler) => wgpu::BindingResource::Sampler(sampler),
                        Atom::Layout(_) => unreachable!("group layout occupies the header"),
                    };
                    Ok(wgpu::BindGroupEntry {
                        binding: slot.binding,
                        resource,
                    })
                })
                .collect::<Result<_, ExecutionError>>()?;
            let native = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &group.layout,
                entries: &entries,
            });
            self.cache
                .insert(self.key.clone().into_boxed_slice(), native.clone());
            bound[group_index] = Some(native);
        }
        Ok(bound)
    }
}
