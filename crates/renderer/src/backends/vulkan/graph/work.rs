//! Prepared Graph operands, staging leases and native dependency planning.
//! Recording and completion are implemented by the private `record` child.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use std::ops::{Deref, DerefMut, Range};
use std::sync::Arc;

#[cfg(test)]
use crate::graph::RasterParameters;
use ash::vk;

use super::super::memory::MemoryOwner;
use super::super::program::{RasterProgramSlot, ShaderBindings, ShaderPrograms};
use super::super::transfer::staging::{SharedStaging, StagingError};
use super::compute::{ComputeDispatch, ComputeLease};
use super::metrics;
use super::provenance::IndexError;
use super::storage::{StorageLease, StoragePool};
use super::{NativeResource, Physical, RuntimeError, Snapshot};
use crate::graph::{
    AccessPurpose, CompiledGraph, MeshUpload, Operation, ResourceDesc, SampleCount,
};

#[cfg(feature = "graph-diagnostics")]
pub(super) mod diagnostics;
mod record;
mod storage;
pub(super) use storage::{ResultData, WorkData};

/// Resolve immutable logical operands after the Work has leased its physical
/// table and established sampled-view suitability. Buffer Device limits are
/// checked by ShaderBindings when the descriptor set is first prepared.
fn native_raster_binding(
    operand: &crate::graph::BindingOperands,
    physical: &[Option<super::PhysicalLease>],
    sampler_for: impl FnOnce(crate::program::SamplerDescriptor) -> vk::Sampler,
) -> super::super::program::RasterBinding {
    use super::super::program::RasterBinding;
    use crate::graph::BindingOperands;
    match operand {
        BindingOperands::Buffer { resource, range } => RasterBinding::Buffer {
            buffer: physical[*resource].as_ref().unwrap().buffer().handle,
            offset: range.offset,
            size: range.size,
        },
        BindingOperands::ImageView(view) => RasterBinding::Image(
            physical[view.image().index]
                .as_ref()
                .unwrap()
                .image()
                .view_for_mips(view.mip_level(), view.mip_level_count())
                .expect("validated prepared sampled view range"),
        ),
        BindingOperands::Sampler(desc) => RasterBinding::Sampler(sampler_for(*desc)),
    }
}

pub(super) fn validate_sampled_operands(
    layout: &[crate::program::BindingLayout],
    operands: &[crate::graph::BindingOperands],
    physical: &[Option<super::PhysicalLease>],
    raster_pairs: Option<&[crate::program::SampledImageSamplerPair]>,
) -> Result<(), RuntimeError> {
    use crate::graph::BindingOperands;
    use crate::program::BindingType;
    for (index, (slot, operand)) in layout.iter().zip(operands).enumerate() {
        if let BindingOperands::ImageView(view) = operand {
            let image = physical[view.image().index].as_ref().unwrap().image();
            if matches!(
                slot.kind,
                BindingType::StorageImageRead(_) | BindingType::StorageImageWrite(_)
            ) && image.mip_levels != 1
            {
                return Err(RuntimeError::unsupported(
                    "native storage image bindings require a single-mip image",
                ));
            }
            if image
                .view_for_mips(view.mip_level(), view.mip_level_count())
                .is_none()
            {
                return Err(RuntimeError::unsupported(
                    "native image bindings support one mip or the complete mip chain",
                ));
            }
            if slot.kind == BindingType::SampledImage
                && image_requires_linear_filtering(layout, operands, raster_pairs, index)
                && !image.supports_linear_filtering
            {
                return Err(RuntimeError::unsupported(
                    "sampled image format does not support linear filtering",
                ));
            }
        }
    }
    Ok(())
}

/// Raster declares every actual pair at the trusted asset boundary. Compute
/// keeps its existing conservative contract: any linear sampler requires every
/// sampled image to support linear filtering.
fn image_requires_linear_filtering(
    layout: &[crate::program::BindingLayout],
    operands: &[crate::graph::BindingOperands],
    raster_pairs: Option<&[crate::program::SampledImageSamplerPair]>,
    image_index: usize,
) -> bool {
    use crate::graph::BindingOperands;
    let Some(pairs) = raster_pairs else {
        return operands.iter().any(
            |operand| matches!(operand, BindingOperands::Sampler(policy) if policy.is_linear()),
        );
    };
    let image = layout[image_index];
    pairs.iter().any(|pair| {
        if pair.image_group != image.group || pair.image_binding != image.binding {
            return false;
        }
        // The unsafe immutable Program contract guarantees valid kinds and
        // complete pairs. Graph construction canonicalizes these operands in
        // the same layout order and prevents reusing them with another Program.
        let sampler_index = layout
            .iter()
            .position(|slot| {
                slot.group == pair.sampler_group && slot.binding == pair.sampler_binding
            })
            .expect("trusted Raster sampling pair references a sampler slot");
        match operands[sampler_index] {
            BindingOperands::Sampler(policy) => policy.is_linear(),
            _ => unreachable!("trusted Raster sampling pair kind"),
        }
    })
}

fn staging_capacity_context(
    error: StagingError,
    alignment: u64,
    mut own_reservations: impl Iterator<Item = u64>,
) -> RuntimeError {
    // Only run after a failed allocation. Every range held by this Graph must
    // coexist until submission/retirement; retrying cannot release those ranges.
    let capacity = match &error {
        StagingError::CapacityExhausted {
            capacity,
            requested,
            ..
        } => {
            let own = own_reservations.try_fold(0u64, u64::checked_add);
            let padded = requested
                .checked_add(alignment - 1)
                .map(|value| value / alignment * alignment);
            let required = own.and_then(|own| padded.and_then(|padded| own.checked_add(padded)));
            Some(RuntimeCapacityError::new(
                RuntimeCapacityKind::StagingBytes,
                if required.is_none_or(|required| required > *capacity) {
                    CapacityRecovery::Reconfigure
                } else {
                    CapacityRecovery::ReleaseOwners
                },
                Some(*capacity),
                required,
            ))
        }
        StagingError::RangeCapacityExhausted { capacity } => {
            let required = (own_reservations.count() as u64).checked_add(1);
            Some(RuntimeCapacityError::new(
                RuntimeCapacityKind::StagingRanges,
                if required.is_none_or(|required| required > *capacity as u64) {
                    CapacityRecovery::Reconfigure
                } else {
                    CapacityRecovery::ReleaseOwners
                },
                Some(*capacity as u64),
                required,
            ))
        }
        _ => None,
    };
    let error = RuntimeError::from(error);
    match capacity {
        Some(capacity) => error.with_capacity_details(capacity),
        None => error,
    }
}

impl Snapshot {
    // Only read-only consumers without a layout transition may reuse this
    // visibility. Exact equality deliberately avoids inferring stage/access
    // pairings from independently accumulated masks.
    pub(super) fn has_read_visibility(
        &self,
        stage: vk::PipelineStageFlags2,
        access: vk::AccessFlags2,
    ) -> bool {
        self.read_stage == stage && self.read_access == access
    }

    pub(super) fn record_read(
        &mut self,
        stage: vk::PipelineStageFlags2,
        access: vk::AccessFlags2,
        layout_change: bool,
    ) {
        if layout_change {
            self.layout_transition_stage = stage;
            self.stage = stage;
            self.access = access;
        } else {
            self.stage |= stage;
            self.access |= access;
        }
        // This pair describes one real dependency destination, never the
        // cross-product of independently accumulated stage/access masks.
        self.read_stage = stage;
        self.read_access = access;
    }

    fn record_access(&mut self, current: Use, layout_change: bool, serial: u64) {
        if current.writes {
            if layout_change {
                self.layout_transition_stage = current.stage;
            }
            self.stage = current.stage;
            self.access = current.access;
            self.writer_stage = current.stage;
            self.writer_access = current.access;
            self.read_stage = vk::PipelineStageFlags2::NONE;
            self.read_access = vk::AccessFlags2::NONE;
            self.revision = serial;
        } else {
            self.record_read(current.stage, current.access, layout_change);
        }
    }
}

/// Recording operands contain no CPU upload payload. The shared input's last
/// owner can disappear as soon as staging/provenance no longer needs it.
enum PreparedOperation {
    UploadBuffer {
        dst: usize,
        offset: u64,
        size: u64,
    },
    CopyBuffer {
        src: usize,
        dst: usize,
        src_offset: u64,
        dst_offset: u64,
        size: u64,
    },
    UploadImage {
        dst: usize,
    },
    CopyImage {
        src: usize,
        dst: usize,
    },
    ReadbackBuffer {
        src: usize,
        offset: u64,
        size: u64,
    },
    ReadbackImage {
        src: usize,
    },
    Dispatch {
        workgroups: [u32; 3],
    },
    Raster {
        batches: Range<usize>,
        colors: Range<usize>,
        depth: Option<usize>,
        sample_count: SampleCount,
    },
    ClearAttachments {
        colors: Range<usize>,
        depth: Option<usize>,
    },
}

impl PreparedOperation {
    fn from_graph(graph: &CompiledGraph, operation: &Operation) -> Result<Self, RuntimeError> {
        Ok(match *operation {
            Operation::UploadBuffer { dst, offset, .. } => Self::UploadBuffer {
                dst,
                offset,
                size: graph.upload_data(operation).len() as u64,
            },
            Operation::CopyBuffer {
                src,
                dst,
                src_offset,
                dst_offset,
                size,
            } => Self::CopyBuffer {
                src,
                dst,
                src_offset,
                dst_offset,
                size,
            },
            Operation::UploadImage { dst, .. } => Self::UploadImage { dst },
            Operation::CopyImage { src, dst } => Self::CopyImage { src, dst },
            Operation::ReadbackBuffer { src, offset, size } => {
                Self::ReadbackBuffer { src, offset, size }
            }
            Operation::ReadbackImage { src } => Self::ReadbackImage { src },
            Operation::Dispatch { workgroups, .. } => Self::Dispatch { workgroups },
            Operation::Raster {
                ref batches,
                ref colors,
                depth,
                ..
            } => Self::Raster {
                batches: batches.clone(),
                colors: colors.clone(),
                depth,
                sample_count: super::attachment_samples(graph, colors, depth),
            },
            Operation::AttachmentPass {
                ref colors, depth, ..
            } => Self::ClearAttachments {
                colors: colors.clone(),
                depth,
            },
            Operation::Declared(_) => {
                return Err(RuntimeError::unsupported(
                    "pass has no supported native operation",
                ));
            }
        })
    }
}

fn index_error(error: IndexError) -> RuntimeError {
    match error {
        IndexError::Capacity(error) => RuntimeError::capacity_with(
            "generic index range summary capacity exhausted",
            RuntimeCapacityError::new(
                RuntimeCapacityKind::GraphMetadata,
                CapacityRecovery::Reconfigure,
                Some(error.capacity as u64),
                Some(error.required as u64),
            ),
        ),
        IndexError::UnknownContents => {
            RuntimeError::unsupported("Raster index range lacks a valid bounds proof")
        }
        IndexError::VertexBounds {
            index,
            vertex_count,
        } => RuntimeError::invalid(format!(
            "raster index {index} exceeds declared vertex count {vertex_count}",
        )),
        IndexError::DeviceLimit { index, maximum } => RuntimeError::unsupported(format!(
            "raster index {index} exceeds Device maxDrawIndexedIndexValue {maximum}",
        )),
    }
}

fn provenance_requirements(graph: &CompiledGraph, needed: &mut [bool], retained: &mut [bool]) {
    // Every canonical buffer may become indices in a future graph, even when
    // this graph does not export it again. Transient vertex/transfer-only data
    // needs no CPU provenance. Mark sources of copies feeding required buffers.
    for (index, resource) in graph.resources.iter().enumerate() {
        retained[index] =
            resource.import.is_some() && matches!(resource.desc, ResourceDesc::Buffer(_));
    }
    for &resource in &graph.exports {
        if matches!(graph.resources[resource].desc, ResourceDesc::Buffer(_)) {
            retained[resource] = true;
        }
    }
    let resources = graph.resources.len();
    needed[..resources].copy_from_slice(&retained[..resources]);
    for &pass in &graph.order {
        if let Operation::Raster { draws, .. } = &graph.passes[pass].operation {
            for draw in graph.raster_draws(draws) {
                if let Some(indexed) = &draw.indexed {
                    needed[indexed.indices] = true;
                }
            }
        }
    }
    for &pass in graph.order.iter().rev() {
        if let Operation::CopyBuffer { src, dst, .. } = graph.passes[pass].operation
            && needed[dst]
        {
            needed[src] = true;
        }
    }
}

struct PreparedPass {
    #[cfg(feature = "vulkan-diagnostics")]
    name: Range<usize>,
    operation: PreparedOperation,
    buffers: Range<usize>,
    images: Range<usize>,
    staging: Option<usize>,
    compute: Option<ComputeDispatch>,
    raster_programs: Range<usize>,
    raster_descriptors: Range<usize>,
}

pub(super) struct GraphWork {
    owner: Arc<MemoryOwner>,
    data: Option<StorageLease<WorkData>>,
    serial: u64,
    programs: Option<Arc<ShaderPrograms>>,
    bindings: Option<ShaderBindings>,
    compute: Option<ComputeLease>,
    plan: Option<CompiledGraph>,
    pub(super) profile: Option<metrics::WorkProfile>,
    // Return the result slot last: an idle result pool must also imply that
    // optional timing aliases owned by this result have already been released.
    pub(super) result: StorageLease<ResultData>,
}
impl Deref for GraphWork {
    type Target = WorkData;
    fn deref(&self) -> &WorkData {
        self.data.as_ref().expect("recorded Graph storage")
    }
}
impl DerefMut for GraphWork {
    fn deref_mut(&mut self) -> &mut WorkData {
        self.data.as_mut().expect("recorded Graph storage")
    }
}

impl GraphWork {
    pub(super) fn acquire(
        owner: Arc<MemoryOwner>,
        work: &StoragePool<WorkData>,
        results: &StoragePool<ResultData>,
        graph: &CompiledGraph,
        serial: u64,
    ) -> Result<Self, RuntimeError> {
        let mut data = work.acquire()?;
        let limit = data.limit.graph;
        let draws = graph
            .order
            .iter()
            .filter_map(|&pass| match &graph.passes[pass].operation {
                Operation::Raster { draws, .. } => Some(draws.len()),
                _ => None,
            })
            .try_fold(0usize, usize::checked_add);
        if draws.is_none_or(|draws| draws > limit.draws)
            || graph.resources.len() > limit.resources
            || graph.binding_count() > limit.bindings
            || graph.order.len() > limit.passes
            || graph
                .order
                .iter()
                .map(|&pass| graph.accesses(pass).len())
                .sum::<usize>()
                > limit.accesses
        {
            return Err(RuntimeError::capacity_with(
                "prepared Graph work capacity exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::GraphMetadata,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            ));
        }
        let result = results.acquire().map_err(|error| {
            error.capacity_context(
                RuntimeCapacityKind::ResultSlots,
                CapacityRecovery::ReleaseResults,
            )
        })?;
        // Set before any logical resource or partial preparation can retain a
        // lease/state. Every failure then resets precisely this active prefix.
        data.active_resources = graph.resources.len();
        Ok(Self {
            owner,
            data: Some(data),
            result,
            serial,
            programs: None,
            bindings: None,
            compute: None,
            plan: None,
            profile: None,
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare(
        mut self,
        graph: &CompiledGraph,
        programs: Option<Arc<ShaderPrograms>>,
        mut bindings: Option<ShaderBindings>,
        mut compute: Option<ComputeLease>,
        upload: &SharedStaging,
        readback: &SharedStaging,
        #[cfg(feature = "graph-diagnostics")] mut diagnostic: Option<&mut String>,
    ) -> Result<Self, RuntimeError> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.vulkan.graph.prepare"),
                0,
            )
        });
        let owner = Arc::clone(&self.owner);
        let serial = self.serial;
        let data = self.data.as_mut().unwrap().deref_mut();
        let WorkData {
            physical,
            final_states: states,
            passes,
            buffers,
            images,
            uploads,
            uses,
            touched,
            used,
            provenance_needed,
            retain_provenance,
            copy_scratch,
            raster_programs,
            raster_descriptors,
            raster_bindings,
            ..
        } = data;
        #[cfg(feature = "vulkan-diagnostics")]
        let labels = &mut data.labels;
        let readbacks = &mut self.result.readbacks;
        #[cfg(feature = "tracy")]
        let snapshots = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.vulkan.graph.snapshot_states"),
                0,
            )
        });
        for (index, physical) in physical.iter().enumerate().take(graph.resources.len()) {
            if let Some(physical) = physical {
                states[index]
                    .as_mut()
                    .unwrap()
                    .copy_from(&physical.lock().scheduled)?;
            }
        }
        #[cfg(feature = "tracy")]
        drop(snapshots);
        provenance_requirements(graph, provenance_needed, retain_provenance);
        let mut raster_program_cursor = 0;
        for &index in &graph.order {
            #[cfg(feature = "tracy")]
            let _span = tracy_client::Client::running().map(|client| {
                client.span(
                    tracy_client::span_location!("renderer.vulkan.graph.prepare_pass"),
                    0,
                )
            });
            let pass = &graph.passes[index];
            #[cfg(feature = "graph-diagnostics")]
            if let Some(output) = diagnostic.as_deref_mut() {
                diagnostics::pass(output, graph, index);
            }
            let mut prepared = PreparedPass {
                #[cfg(feature = "vulkan-diagnostics")]
                name: {
                    let name = graph.pass_name(index).as_bytes();
                    if name.contains(&0) {
                        return Err(RuntimeError::invalid("pass debug name contains a NUL byte"));
                    }
                    if name.len().saturating_add(1) > labels.capacity().saturating_sub(labels.len())
                    {
                        return Err(RuntimeError::capacity_with(
                            "prepared debug label capacity exhausted",
                            RuntimeCapacityError::new(
                                RuntimeCapacityKind::DiagnosticStorage,
                                CapacityRecovery::Reconfigure,
                                None,
                                None,
                            ),
                        ));
                    }
                    let start = labels.len();
                    labels.extend_from_slice(name);
                    labels.push(0);
                    start..labels.len()
                },
                operation: PreparedOperation::from_graph(graph, &pass.operation)?,
                buffers: buffers.len()..buffers.len(),
                images: images.len()..images.len(),
                staging: None,
                compute: None,
                raster_programs: raster_program_cursor..raster_program_cursor,
                raster_descriptors: raster_descriptors.len()..raster_descriptors.len(),
            };
            // Aggregate one pass's uses before modifying previous-access state;
            // disjoint self-copies must not appear to depend on themselves.
            touched.clear();
            for access in graph.accesses(index) {
                let current = usage(access.purpose, access.stages);
                if !used[access.resource] {
                    used[access.resource] = true;
                    touched.push(access.resource);
                }
                let target = &mut uses[access.resource];
                // Transfer purposes also describe buffers. Their layout hints
                // must not reject a legal disjoint same-buffer copy.
                if matches!(
                    graph.resources[access.resource].desc,
                    ResourceDesc::Image(_)
                ) && target.layout != vk::ImageLayout::UNDEFINED
                    && current.layout != vk::ImageLayout::UNDEFINED
                    && target.layout != current.layout
                {
                    return Err(RuntimeError::unsupported(
                        "one native pass cannot use different layouts of the same image",
                    ));
                }
                target.stage |= current.stage;
                target.access |= current.access;
                target.writes |= access.write;
                if current.layout != vk::ImageLayout::UNDEFINED {
                    target.layout = current.layout;
                }
            }
            touched.sort_unstable();
            for &resource in touched.iter() {
                let current = uses[resource];
                used[resource] = false;
                uses[resource] = Use::default();
                let native = physical[resource].as_ref().unwrap();
                let previous = states[resource].as_mut().unwrap();
                let layout_change = matches!(native.native, NativeResource::Image(_))
                    && previous.layout != current.layout;
                let (source_stage, source_access) = source_scope(previous, current, layout_change);
                let dependency = !source_stage.is_empty();
                match &native.native {
                    NativeResource::Buffer(buffer) if dependency => {
                        buffers.push(
                            vk::BufferMemoryBarrier2::default()
                                .src_stage_mask(source_stage)
                                .src_access_mask(source_access)
                                .dst_stage_mask(current.stage)
                                .dst_access_mask(current.access)
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .buffer(buffer.handle)
                                .offset(0)
                                .size(buffer.size),
                        );
                    }
                    NativeResource::Image(image) if dependency || layout_change => {
                        images.push(
                            vk::ImageMemoryBarrier2::default()
                                .src_stage_mask(source_stage)
                                .src_access_mask(source_access)
                                .dst_stage_mask(current.stage)
                                .dst_access_mask(current.access)
                                .old_layout(previous.layout)
                                .new_layout(current.layout)
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .image(image.image)
                                .subresource_range(
                                    vk::ImageSubresourceRange::default()
                                        .aspect_mask(image.aspect)
                                        .level_count(image.mip_levels)
                                        .layer_count(1),
                                ),
                        );
                    }
                    _ => {}
                }
                #[cfg(feature = "graph-diagnostics")]
                if let Some(output) = diagnostic.as_deref_mut() {
                    let buffer = buffers[prepared.buffers.start..].last().filter(|barrier| {
                        dependency
                            && matches!(&native.native, NativeResource::Buffer(buffer)
                            if buffer.handle == barrier.buffer)
                    });
                    let image = images[prepared.images.start..].last().filter(|barrier| {
                        (dependency || layout_change)
                            && matches!(&native.native, NativeResource::Image(image)
                                if image.image == barrier.image)
                    });
                    diagnostics::dependency(
                        output,
                        resource,
                        previous,
                        current,
                        layout_change,
                        buffer,
                        image,
                    );
                }
                previous.record_access(current, layout_change, serial);
                if matches!(native.native, NativeResource::Image(_)) {
                    previous.layout = current.layout;
                }
            }
            prepared.buffers.end = buffers.len();
            prepared.images.end = images.len();
            (|| -> Result<(), RuntimeError> {
                match &pass.operation {
                    Operation::UploadBuffer {
                        dst, offset, mesh, ..
                    } => {
                        let slice = graph.upload_data(&pass.operation);
                        let mut range = upload.allocate(slice.len() as u64).map_err(|error| {
                            staging_capacity_context(
                                error,
                                upload.stats().alignment,
                                uploads.iter().map(|range| range.reserved_size()),
                            )
                        })?;
                        range.write(slice)?;
                        prepared.staging = Some(uploads.len());
                        uploads.push(range);
                        if provenance_needed[*dst] {
                            let target = *offset..*offset + slice.len() as u64;
                            let provenance = &mut states[*dst].as_mut().unwrap().provenance;
                            match mesh {
                                Some(MeshUpload::Vertices) => provenance.clear(),
                                Some(MeshUpload::Indices { byte_len, maximum }) => {
                                    provenance.write_mesh_indices(target, *byte_len, *maximum);
                                }
                                None => provenance
                                    .write_graph_slice(target, graph, index, 0..slice.len())
                                    .map_err(|_| {
                                        RuntimeError::capacity_with(
                                            "prepared provenance capacity exhausted",
                                            RuntimeCapacityError::new(
                                                RuntimeCapacityKind::GraphMetadata,
                                                CapacityRecovery::Reconfigure,
                                                None,
                                                None,
                                            ),
                                        )
                                    })?,
                            }
                        }
                    }
                    Operation::CopyBuffer {
                        src,
                        dst,
                        src_offset,
                        dst_offset,
                        size,
                    } => {
                        if provenance_needed[*dst] {
                            states[*src]
                                .as_ref()
                                .unwrap()
                                .provenance
                                .copy_into(
                                    *src_offset..*src_offset + *size,
                                    *dst_offset,
                                    copy_scratch,
                                )
                                .map_err(|_| {
                                    RuntimeError::capacity_with(
                                        "prepared provenance copy capacity exhausted",
                                        RuntimeCapacityError::new(
                                            RuntimeCapacityKind::GraphMetadata,
                                            CapacityRecovery::Reconfigure,
                                            None,
                                            None,
                                        ),
                                    )
                                })?;
                            states[*dst]
                                .as_mut()
                                .unwrap()
                                .provenance
                                .replace(*dst_offset..*dst_offset + *size, copy_scratch)
                                .map_err(|_| {
                                    RuntimeError::capacity_with(
                                        "prepared provenance replacement capacity exhausted",
                                        RuntimeCapacityError::new(
                                            RuntimeCapacityKind::GraphMetadata,
                                            CapacityRecovery::Reconfigure,
                                            None,
                                            None,
                                        ),
                                    )
                                })?;
                        }
                    }
                    Operation::UploadImage { .. } => {
                        let bytes = graph.upload_data(&pass.operation);
                        let mut range = upload.allocate(bytes.len() as u64).map_err(|error| {
                            staging_capacity_context(
                                error,
                                upload.stats().alignment,
                                uploads.iter().map(|range| range.reserved_size()),
                            )
                        })?;
                        range.write(bytes)?;
                        prepared.staging = Some(uploads.len());
                        uploads.push(range);
                    }
                    Operation::ReadbackBuffer { size, .. } => {
                        prepared.staging = Some(readbacks.len());
                        let range = readback.allocate(*size).map_err(|error| {
                            staging_capacity_context(
                                error,
                                readback.stats().alignment,
                                readbacks
                                    .iter()
                                    .filter_map(|(_, range)| range.as_ref())
                                    .map(|range| range.reserved_size()),
                            )
                        })?;
                        readbacks.push((index, Some(range)));
                    }
                    Operation::ReadbackImage { src } => {
                        let ResourceDesc::Image(desc) = graph.resources[*src].desc else {
                            unreachable!()
                        };
                        prepared.staging = Some(readbacks.len());
                        let range =
                            readback
                                .allocate(desc.byte_len().unwrap())
                                .map_err(|error| {
                                    staging_capacity_context(
                                        error,
                                        readback.stats().alignment,
                                        readbacks
                                            .iter()
                                            .filter_map(|(_, range)| range.as_ref())
                                            .map(|range| range.reserved_size()),
                                    )
                                })?;
                        readbacks.push((index, Some(range)));
                    }
                    Operation::Dispatch {
                        program,
                        bindings,
                        workgroups,
                    } => {
                        prepared.compute = Some(compute.as_mut().unwrap().prepare(
                            *program,
                            graph.raster_bindings(bindings),
                            physical,
                            *workgroups,
                        )?);
                        // Shader writes invalidate any prior CPU index proof,
                        // including proofs retained by imported/exported buffers.
                        for access in graph.accesses(index) {
                            if access.write
                                && provenance_needed[access.resource]
                                && matches!(
                                    graph.resources[access.resource].desc,
                                    ResourceDesc::Buffer(_)
                                )
                            {
                                states[access.resource]
                                    .as_mut()
                                    .unwrap()
                                    .provenance
                                    .clear_range(access.range.clone())
                                    .map_err(|_| {
                                        RuntimeError::capacity_with(
                                            "Compute provenance capacity exhausted",
                                            RuntimeCapacityError::new(
                                                RuntimeCapacityKind::GraphMetadata,
                                                CapacityRecovery::Reconfigure,
                                                None,
                                                None,
                                            ),
                                        )
                                    })?;
                            }
                        }
                    }
                    Operation::Raster {
                        batches,
                        draws,
                        depth,
                        ..
                    } => {
                        #[cfg(feature = "tracy")]
                        let _span = tracy_client::Client::running().map(|client| {
                            client.span(
                                tracy_client::span_location!("renderer.vulkan.raster.prepare"),
                                0,
                            )
                        });
                        let programs = programs.as_ref().unwrap();
                        for batch in graph.raster_batches(batches) {
                            let slot = raster_programs[raster_program_cursor];
                            raster_program_cursor += 1;
                            let native_program = programs.raster_program(slot);
                            let program = &native_program.raster_asset;
                            let binding_layout = (!program.bindings().is_empty()).then(|| {
                                let RasterProgramSlot::Managed(key) = slot else {
                                    unreachable!("fixed Raster Program has no bindings")
                                };
                                key
                            });
                            for (draw_index, draw) in
                                graph.raster_draws(&batch.draws).iter().enumerate()
                            {
                                #[cfg(feature = "tracy")]
                                let _span = tracy_client::Client::running().map(|client| {
                                    client.span(
                                        tracy_client::span_location!(
                                            "renderer.vulkan.raster.prepare_draw"
                                        ),
                                        0,
                                    )
                                });
                                if let Some(indexed) = &draw.indexed {
                                    let vertex_count =
                                        graph.resources[indexed.vertices].desc.units()
                                            / u64::from(program.vertex_stride());
                                    states[indexed.indices]
                                        .as_mut()
                                        .unwrap()
                                        .provenance
                                        .validate_index_range(
                                            indexed.first_index,
                                            indexed.index_count,
                                            vertex_count,
                                            owner.device.max_draw_indexed_index_value(),
                                        )
                                        .map_err(|error| {
                                            let draw_index =
                                                batch.draws.start - draws.start + draw_index;
                                            index_error(error)
                                                .context(format_args!("draw {draw_index}"))
                                        })?;
                                }
                                let sets = if let Some(key) = binding_layout {
                                    raster_bindings.resolve(
                                        &draw.bindings,
                                        depth.is_some(),
                                        key,
                                        |validate_sampled| {
                                            if validate_sampled {
                                                validate_sampled_operands(
                                                    program.bindings(),
                                                    graph.raster_bindings(&draw.bindings),
                                                    physical,
                                                    Some(program.sampling_pairs()),
                                                )?;
                                            }
                                            #[cfg(feature = "tracy")]
                                            let _span =
                                                tracy_client::Client::running().map(|client| {
                                                    client.span(
                                                        tracy_client::span_location!(
                                                            "renderer.vulkan.raster.descriptors"
                                                        ),
                                                        0,
                                                    )
                                                });
                                            let operands = program
                                                .bindings()
                                                .iter()
                                                .copied()
                                                .zip(graph.raster_bindings(&draw.bindings))
                                                .map(|(slot, operand)| {
                                                    (
                                                        slot,
                                                        native_raster_binding(
                                                            operand,
                                                            physical,
                                                            |desc| native_program.sampler_for(desc),
                                                        ),
                                                    )
                                                });
                                            bindings.as_mut().unwrap().raster(key, operands)
                                        },
                                    )?
                                } else {
                                    [vk::DescriptorSet::null();
                                        crate::program::MAX_RASTER_BIND_GROUPS]
                                };
                                raster_descriptors.push(sets);
                            }
                        }
                        prepared.raster_programs.end = raster_program_cursor;
                        prepared.raster_descriptors.end = raster_descriptors.len();
                    }
                    Operation::CopyImage { .. } | Operation::AttachmentPass { .. } => {}
                    Operation::Declared(_) => {
                        return Err(RuntimeError::unsupported(
                            "pass has no supported native operation",
                        ));
                    }
                }
                Ok(())
            })()
            .map_err(|error| error.context(format_args!("pass {:?}", graph.pass_name(index))))?;
            passes.push(prepared);
        }
        for (index, state) in states.iter_mut().enumerate().take(graph.resources.len()) {
            if physical[index].is_some()
                && let Some(state) = state
            {
                state.serial = serial;
                state.set_initialized(graph.final_contents(index))?;
                // Transient index validation has finished. Only canonical imports
                // and exports can acquire a future index role in another graph.
                if !retain_provenance[index] {
                    state.provenance.clear();
                }
            }
        }
        #[cfg(feature = "graph-diagnostics")]
        if let Some(output) = diagnostic {
            diagnostics::finish(
                output,
                graph,
                &states[..graph.resources.len()],
                !readbacks.is_empty(),
            );
        }
        self.programs = programs;
        self.bindings = bindings;
        self.compute = compute;
        self.plan = Some(graph.clone());
        Ok(self)
    }

    fn resource(&self, index: usize) -> &Physical {
        self.physical[index].as_ref().unwrap()
    }
}

#[derive(Clone, Copy)]
struct Use {
    stage: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
    layout: vk::ImageLayout,
    writes: bool,
}
impl Default for Use {
    fn default() -> Self {
        Self {
            stage: vk::PipelineStageFlags2::NONE,
            access: vk::AccessFlags2::NONE,
            layout: vk::ImageLayout::UNDEFINED,
            writes: false,
        }
    }
}

fn usage(purpose: AccessPurpose, stages: crate::program::ShaderStages) -> Use {
    let mut shader_stage = vk::PipelineStageFlags2::NONE;
    for (logical, native) in [
        (
            crate::program::ShaderStages::VERTEX,
            vk::PipelineStageFlags2::VERTEX_SHADER,
        ),
        (
            crate::program::ShaderStages::FRAGMENT,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
        ),
        (
            crate::program::ShaderStages::COMPUTE,
            vk::PipelineStageFlags2::COMPUTE_SHADER,
        ),
    ] {
        if stages.contains(logical) {
            shader_stage |= native;
        }
    }
    use AccessPurpose::*;
    let (stage, access, layout) = match purpose {
        CopySource => (
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        ),
        CopyDestination => (
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_WRITE,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        ),
        Uniform => (
            shader_stage,
            vk::AccessFlags2::UNIFORM_READ,
            vk::ImageLayout::UNDEFINED,
        ),
        StorageRead => (
            shader_stage,
            vk::AccessFlags2::SHADER_STORAGE_READ,
            vk::ImageLayout::GENERAL,
        ),
        StorageWrite => (
            shader_stage,
            vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::ImageLayout::GENERAL,
        ),
        StorageReadWrite => (
            shader_stage,
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::ImageLayout::GENERAL,
        ),
        Sampled => (
            shader_stage,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        ),
        Vertex => (
            vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
            vk::AccessFlags2::VERTEX_ATTRIBUTE_READ,
            vk::ImageLayout::UNDEFINED,
        ),
        Index => (
            vk::PipelineStageFlags2::INDEX_INPUT,
            vk::AccessFlags2::INDEX_READ,
            vk::ImageLayout::UNDEFINED,
        ),
        Indirect => (
            vk::PipelineStageFlags2::DRAW_INDIRECT,
            vk::AccessFlags2::INDIRECT_COMMAND_READ,
            vk::ImageLayout::UNDEFINED,
        ),
        ColorAttachment | ColorResolve => (
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_READ | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        ),
        DepthAttachment => (
            vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
            vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_READ
                | vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
            vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
        ),
    };
    Use {
        stage,
        access,
        layout,
        writes: false,
    }
}

fn source_scope(
    previous: &Snapshot,
    current: Use,
    layout_change: bool,
) -> (vk::PipelineStageFlags2, vk::AccessFlags2) {
    if current.writes || layout_change {
        let scope = (
            previous.stage | previous.writer_stage | previous.layout_transition_stage,
            previous.access | previous.writer_access,
        );
        if layout_change && previous.layout == vk::ImageLayout::UNDEFINED && scope.0.is_empty() {
            // A fresh, unaliased image permits an empty source scope by the API.
            // On the tested stack an earlier, unrelated buffer copy lost data
            // with an empty transition source; ordering COPY prevented this
            // (renderer-j17). The driver-internal cause remains unconfirmed.
            // Order prior copies, including pending graphs on this same queue,
            // before that transition. This is execution-only, with no host wait
            // or claim that fresh images require a source memory dependency.
            (vk::PipelineStageFlags2::COPY, vk::AccessFlags2::NONE)
        } else {
            scope
        }
    } else if previous.has_read_visibility(current.stage, current.access) {
        // A prior dependency already exposed this writer to this exact read
        // scope. A pipeline barrier's second scope also
        // includes later commands on the same queue; no intervening write or
        // layout transition has invalidated that visibility. Equality keeps
        // this deliberately narrow and does not infer stage/access pairings
        // from independently accumulated masks. Keep the earlier writer and
        // layout transition for other reads, writes and layout changes.
        (vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE)
    } else {
        // Expose the last writer to each new reader scope. Layout transitions
        // can also write memory and make those writes available automatically.
        // Chain through the transition's destination stage so a reader using
        // a different stage still waits for the transition and sees its writes.
        (
            previous.writer_stage | previous.layout_transition_stage,
            previous.writer_access,
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn generic_index_prefix_capacity_reports_reconfiguration() {
        let error = super::index_error(crate::graph::provenance::IndexError::Capacity(
            crate::graph::provenance::ProvenanceCapacityError {
                required: 3,
                capacity: 2,
            },
        ));
        assert_eq!(
            error.kind(),
            crate::backends::vulkan::graph::RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(
            error.capacity(),
            Some(crate::capacity::RuntimeCapacityError::new(
                crate::capacity::RuntimeCapacityKind::GraphMetadata,
                crate::capacity::CapacityRecovery::Reconfigure,
                Some(2),
                Some(3),
            ))
        );
    }

    #[test]
    fn raster_filter_requirements_follow_only_declared_sampling_pairs() {
        use crate::graph::{BindingOperands, Graph, ImageDesc};
        use crate::program::{
            BindingLayout, BindingType, SampledImageSamplerPair, SamplerDescriptor, ShaderStages,
        };
        let slot = |binding, kind| BindingLayout {
            group: 0,
            binding,
            kind,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        };
        let layout = [
            slot(0, BindingType::SampledImage),
            slot(1, BindingType::SampledImage),
            slot(2, BindingType::Sampler),
            slot(3, BindingType::Sampler),
        ];
        let mut graph = Graph::test_new();
        let color = graph.image("color", ImageDesc::rgba8(1, 1)).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(1, 1)).unwrap();
        let operands = [
            BindingOperands::ImageView(graph.image_view(color, 0, 0).unwrap()),
            BindingOperands::ImageView(graph.image_view(depth, 0, 0).unwrap()),
            BindingOperands::Sampler(SamplerDescriptor::LinearClamp),
            BindingOperands::Sampler(SamplerDescriptor::NearestClamp),
        ];
        let pair = |image_binding, sampler_binding| SampledImageSamplerPair {
            image_group: 0,
            image_binding,
            sampler_group: 0,
            sampler_binding,
        };
        // D32 sampled through nearest is legal even if its format cannot filter.
        let pairs = [pair(0, 2), pair(1, 3)];
        assert!(image_requires_linear_filtering(
            &layout,
            &operands,
            Some(&pairs),
            0
        ));
        assert!(!image_requires_linear_filtering(
            &layout,
            &operands,
            Some(&pairs),
            1
        ));
        // The same depth image needs support when its actual sampler changes.
        let pairs = [pair(0, 2), pair(1, 2)];
        assert!(image_requires_linear_filtering(
            &layout,
            &operands,
            Some(&pairs),
            1
        ));
        // One image can use multiple samplers; a linear pair cannot be hidden.
        let pairs = [pair(1, 3), pair(1, 2)];
        assert!(image_requires_linear_filtering(
            &layout,
            &operands,
            Some(&pairs),
            1
        ));
        // Load/texelFetch-only Raster images need no filtering capability.
        assert!(!image_requires_linear_filtering(
            &layout,
            &operands,
            Some(&[]),
            1
        ));
        // Compute deliberately keeps the prior conservative contract.
        assert!(image_requires_linear_filtering(&layout, &operands, None, 1));
        for policy in [
            SamplerDescriptor::LinearRepeat,
            SamplerDescriptor::LinearRepeatUClampV,
        ] {
            let mut operands = operands;
            operands[2] = BindingOperands::Sampler(policy);
            assert!(image_requires_linear_filtering(
                &layout,
                &operands,
                Some(&pairs),
                1
            ));
        }
    }

    use super::super::{RuntimeConfig, VulkanRuntime};
    use super::*;
    use crate::graph::{
        Attachment, BufferDesc, Graph, ImageDesc, ImportBuffer, LoadOp, RasterAttachments,
        RasterDraw, ResourceKey, StoreOp,
    };
    use std::time::Duration;
    #[test]
    fn cumulative_staging_requests_cannot_be_recovered_by_polling_other_frames() {
        let byte_error = || StagingError::CapacityExhausted {
            capacity: 4096,
            requested: 3072,
            reserved: 3072,
        };
        let own = staging_capacity_context(byte_error(), 256, [3072].into_iter());
        assert_eq!(
            own.capacity(),
            Some(RuntimeCapacityError::new(
                RuntimeCapacityKind::StagingBytes,
                CapacityRecovery::Reconfigure,
                Some(4096),
                Some(6144)
            ))
        );
        let external = staging_capacity_context(byte_error(), 256, [].into_iter());
        assert_eq!(
            external.capacity(),
            Some(RuntimeCapacityError::new(
                RuntimeCapacityKind::StagingBytes,
                CapacityRecovery::ReleaseOwners,
                Some(4096),
                Some(3072)
            ))
        );
        let own = staging_capacity_context(
            StagingError::RangeCapacityExhausted { capacity: 1 },
            256,
            [512].into_iter(),
        );
        assert_eq!(
            own.capacity().unwrap().recovery,
            CapacityRecovery::Reconfigure
        );
        assert_eq!(own.capacity().unwrap().required, Some(2));
        let external = staging_capacity_context(
            StagingError::RangeCapacityExhausted { capacity: 1 },
            256,
            [].into_iter(),
        );
        assert_eq!(
            external.capacity().unwrap().recovery,
            CapacityRecovery::ReleaseOwners
        );
        let padded = staging_capacity_context(
            StagingError::CapacityExhausted {
                capacity: 1024,
                requested: 513,
                reserved: 512,
            },
            256,
            [512].into_iter(),
        );
        assert_eq!(padded.capacity().unwrap().required, Some(1280));
        assert_eq!(
            padded.capacity().unwrap().recovery,
            CapacityRecovery::Reconfigure
        );
    }

    #[test]
    #[ignore = "explicit real GPU Graph staging-capacity regression; run shipping and strict separately"]
    fn vulkan_graph_cumulative_staging_capacity_is_not_owner_backpressure() {
        use super::super::ResourceClass;
        for (bytes, ranges, size, resource) in [
            (4096, 4, 3072, RuntimeCapacityKind::StagingBytes),
            (8192, 1, 512, RuntimeCapacityKind::StagingRanges),
        ] {
            let mut runtime = VulkanRuntime::new(RuntimeConfig {
                execution_slots: 1,
                resource_capacity: 2,
                staging_bytes: bytes,
                staging_ranges: ranges,
                max_shader_bindings: 1,
            })
            .unwrap();
            assert_ne!(
                runtime.owner.device.device_type(),
                vk::PhysicalDeviceType::CPU,
                "real GPU required"
            );
            let monitor = runtime.validation_monitor();
            #[cfg(feature = "development-validation")]
            crate::backends::vulkan::assert_real_validation(&runtime.diagnostics);
            #[cfg(not(feature = "development-validation"))]
            assert_eq!(
                monitor.report().requested,
                crate::vulkan::ValidationMode::Disabled
            );
            super::super::tests::prepare_scenario(
                &mut runtime,
                2,
                2,
                &[ResourceClass::Buffer {
                    desc: BufferDesc { size },
                    count: 2,
                }],
            );
            let mut graph = Graph::test_new();
            let payload = vec![7; size as usize];
            for name in ["first", "second"] {
                let buffer = graph.buffer(name, BufferDesc { size }).unwrap();
                graph.upload_buffer(name, buffer, 0, &payload).unwrap();
                graph.export_buffer(buffer).unwrap();
            }
            let plan = graph.compile().unwrap();
            let error = runtime.submit(&plan).err().unwrap();
            let capacity = error.capacity().unwrap();
            assert_eq!(capacity.resource, resource);
            assert_eq!(capacity.recovery, CapacityRecovery::Reconfigure);
            assert!(capacity.required.unwrap() > capacity.limit.unwrap());
            assert_eq!(runtime.stats().upload_live_ranges, 0);
            drop((plan, runtime));
            assert!(monitor.report().is_clean(), "{:?}", monitor.report());
        }
    }
    #[test]
    fn shader_resource_scopes_follow_the_program_and_fixed_function_fetch_is_separate() {
        use crate::program::ShaderStages;
        for stages in [
            ShaderStages::VERTEX,
            ShaderStages::FRAGMENT,
            ShaderStages::COMPUTE,
            ShaderStages::VERTEX.union(ShaderStages::FRAGMENT),
        ] {
            let expected = if stages == ShaderStages::VERTEX {
                vk::PipelineStageFlags2::VERTEX_SHADER
            } else if stages == ShaderStages::FRAGMENT {
                vk::PipelineStageFlags2::FRAGMENT_SHADER
            } else if stages == ShaderStages::COMPUTE {
                vk::PipelineStageFlags2::COMPUTE_SHADER
            } else {
                vk::PipelineStageFlags2::VERTEX_SHADER | vk::PipelineStageFlags2::FRAGMENT_SHADER
            };
            assert_eq!(usage(AccessPurpose::Uniform, stages).stage, expected);
            assert_eq!(usage(AccessPurpose::Sampled, stages).stage, expected);
            assert_eq!(usage(AccessPurpose::StorageRead, stages).stage, expected);
        }
        assert_eq!(
            usage(AccessPurpose::Uniform, ShaderStages::VERTEX).access,
            vk::AccessFlags2::UNIFORM_READ
        );
        assert_eq!(
            usage(AccessPurpose::Sampled, ShaderStages::COMPUTE).access,
            vk::AccessFlags2::SHADER_SAMPLED_READ
        );
        assert_eq!(
            usage(AccessPurpose::Vertex, ShaderStages::EMPTY).stage,
            vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT
        );
        assert_eq!(
            usage(AccessPurpose::Index, ShaderStages::EMPTY).stage,
            vk::PipelineStageFlags2::INDEX_INPUT
        );
    }

    #[test]
    fn prepared_upload_uses_slice_length_without_retaining_source_bytes() {
        let input: Arc<[u8]> = vec![7; 64].into();
        let weak = Arc::downgrade(&input);
        let mut graph = Graph::test_new();
        let buffer = graph.buffer("slice", BufferDesc { size: 4 }).unwrap();
        let upload = graph
            .upload_buffer_slice("upload", buffer, 0, input, 5..9)
            .unwrap();
        graph.readback_buffer("read", buffer, 0, 4).unwrap();
        let plan = graph.compile().unwrap();
        let operation =
            PreparedOperation::from_graph(&plan, &plan.passes[upload.index].operation).unwrap();
        assert!(matches!(
            &operation,
            PreparedOperation::UploadBuffer {
                offset: 0,
                size: 4,
                ..
            }
        ));
        drop(plan);
        assert!(
            weak.upgrade().is_none(),
            "native recording operands must not retain CPU upload input"
        );
    }

    #[test]
    fn provenance_follows_index_copy_sources_and_persistent_imports_only() {
        let mut graph = Graph::test_new();
        let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
        graph
            .upload_buffer("vertices.upload", vertices, 0, &[0; 84])
            .unwrap();
        let source = graph.buffer("source", BufferDesc { size: 12 }).unwrap();
        let middle = graph.buffer("middle", BufferDesc { size: 12 }).unwrap();
        let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
        graph
            .upload_buffer("index.upload", source, 0, &[0; 12])
            .unwrap();
        graph
            .copy_buffer("index.copy1", source, 0, middle, 0, 12)
            .unwrap();
        graph
            .copy_buffer("index.copy2", middle, 0, indices, 0, 12)
            .unwrap();
        let color = graph.image("color", ImageDesc::rgba8(1, 1)).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(1, 1)).unwrap();
        graph
            .raster(
                "mesh",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[Attachment {
                        view: graph.image_view(color, 0, 0).unwrap(),
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    depth: Some(Attachment {
                        view: graph.image_view(depth, 0, 0).unwrap(),
                        load: LoadOp::Clear(1.0),
                        store: StoreOp::Store,
                    }),
                },
                |raster| {
                    raster.draw(
                        &crate::program::ManagedRasterProgram::mesh(),
                        [RasterDraw {
                            bindings: None,
                            vertices,
                            indices,
                            first_index: 0,
                            index_count: 3,
                            instance_count: 1,
                            parameters: RasterParameters::default().into(),
                        }],
                    )
                },
            )
            .unwrap();
        graph.readback_image("color.read", color).unwrap();
        let imported = graph
            .import_buffer(
                "persistent",
                &ImportBuffer::new(
                    ResourceKey {
                        owner: 1,
                        index: 0,
                        generation: 1,
                    },
                    1,
                    BufferDesc { size: 12 },
                    crate::graph::ImportRanges::test_capture(std::slice::from_ref(&(0..12))),
                )
                .unwrap(),
            )
            .unwrap();
        graph
            .readback_buffer("persistent.read", imported, 0, 12)
            .unwrap();
        let plan = graph.compile().unwrap();
        let mut needed = vec![false; plan.resources.len()];
        let mut retained = vec![false; plan.resources.len()];
        provenance_requirements(&plan, &mut needed, &mut retained);
        assert!(
            !needed[vertices.index],
            "vertex-only input needs no CPU copy"
        );
        for buffer in [source, middle, indices] {
            assert!(
                needed[buffer.index],
                "all index copy sources must remain known"
            );
            assert!(
                !retained[buffer.index],
                "transient data need not survive prepare"
            );
        }
        assert!(
            needed[imported.index] && retained[imported.index],
            "registration persists without another explicit export"
        );
    }

    #[test]
    fn writer_visibility_survives_different_read_stages_and_import_snapshots() {
        let previous = Snapshot {
            stage: vk::PipelineStageFlags2::COPY | vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::TRANSFER_READ | vk::AccessFlags2::SHADER_STORAGE_READ,
            writer_stage: vk::PipelineStageFlags2::COPY,
            writer_access: vk::AccessFlags2::TRANSFER_WRITE,
            ..Snapshot::default()
        };
        let snapshot = previous.clone();
        for purpose in [
            AccessPurpose::Vertex,
            AccessPurpose::Index,
            AccessPurpose::Sampled,
        ] {
            let (stage, access) =
                source_scope(&snapshot, usage(purpose, purpose.default_stages()), false);
            assert!(stage.contains(vk::PipelineStageFlags2::COPY));
            assert!(access.contains(vk::AccessFlags2::TRANSFER_WRITE));
        }
        let mut writer = usage(
            AccessPurpose::CopyDestination,
            AccessPurpose::CopyDestination.default_stages(),
        );
        writer.writes = true;
        let (stage, access) = source_scope(&previous, writer, false);
        assert!(
            stage.contains(vk::PipelineStageFlags2::COPY | vk::PipelineStageFlags2::COMPUTE_SHADER)
        );
        assert!(
            access
                .contains(vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::TRANSFER_WRITE)
        );
    }

    #[test]
    fn initial_image_transition_orders_prior_copies_without_source_memory_access() {
        let initial = Snapshot::default();
        for purpose in [
            AccessPurpose::CopyDestination,
            AccessPurpose::StorageWrite,
            AccessPurpose::ColorAttachment,
            AccessPurpose::DepthAttachment,
        ] {
            assert_eq!(
                source_scope(&initial, usage(purpose, purpose.default_stages()), true),
                (vk::PipelineStageFlags2::COPY, vk::AccessFlags2::NONE)
            );
        }
        assert_eq!(
            source_scope(
                &initial,
                usage(
                    AccessPurpose::CopyDestination,
                    AccessPurpose::CopyDestination.default_stages()
                ),
                false
            ),
            (vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE),
            "new buffer uploads do not need the image scheduling workaround"
        );
        let previous = Snapshot {
            layout: vk::ImageLayout::GENERAL,
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_WRITE,
            writer_stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            writer_access: vk::AccessFlags2::SHADER_STORAGE_WRITE,
            ..Snapshot::default()
        };
        assert_eq!(
            source_scope(
                &previous,
                usage(
                    AccessPurpose::Sampled,
                    AccessPurpose::Sampled.default_stages()
                ),
                true
            ),
            (
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE
            ),
            "later transitions preserve their actual writer without extra COPY scope"
        );
    }

    #[test]
    fn readonly_scopes_preserve_content_revision_and_order_later_writes() {
        let mut state = Snapshot {
            revision: 7,
            stage: vk::PipelineStageFlags2::COPY,
            access: vk::AccessFlags2::TRANSFER_WRITE,
            writer_stage: vk::PipelineStageFlags2::COPY,
            writer_access: vk::AccessFlags2::TRANSFER_WRITE,
            ..Snapshot::default()
        };
        state.record_access(
            usage(
                AccessPurpose::CopySource,
                AccessPurpose::CopySource.default_stages(),
            ),
            false,
            8,
        );
        state.record_access(
            usage(
                AccessPurpose::StorageRead,
                AccessPurpose::StorageRead.default_stages(),
            ),
            false,
            9,
        );
        state.record_access(
            usage(
                AccessPurpose::Sampled,
                AccessPurpose::Sampled.default_stages(),
            ),
            true,
            10,
        );
        assert_eq!(state.revision, 7, "reads/layout changes preserve contents");
        let mut write = usage(
            AccessPurpose::CopyDestination,
            AccessPurpose::CopyDestination.default_stages(),
        );
        write.writes = true;
        let (stages, access) = source_scope(&state, write, true);
        assert!(stages.contains(vk::PipelineStageFlags2::FRAGMENT_SHADER));
        assert!(stages.contains(vk::PipelineStageFlags2::COPY));
        assert!(access.contains(vk::AccessFlags2::SHADER_SAMPLED_READ));
        assert!(access.contains(vk::AccessFlags2::TRANSFER_WRITE));
        state.record_access(write, true, 11);
        assert_eq!(state.revision, 11);
    }

    #[test]
    fn repeated_read_scope_reuses_visibility_without_losing_the_writer() {
        let mut state = Snapshot {
            revision: 7,
            stage: vk::PipelineStageFlags2::COPY,
            access: vk::AccessFlags2::TRANSFER_WRITE,
            writer_stage: vk::PipelineStageFlags2::COPY,
            writer_access: vk::AccessFlags2::TRANSFER_WRITE,
            layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            ..Snapshot::default()
        };
        let sampled = usage(
            AccessPurpose::Sampled,
            AccessPurpose::Sampled.default_stages(),
        );
        let mut dependencies = 0;
        for reader in 0..8 {
            let transition = state.layout != sampled.layout;
            let source = source_scope(&state, sampled, transition);
            dependencies += usize::from(!source.0.is_empty());
            state.record_access(sampled, transition, 8 + reader);
            state.layout = sampled.layout;
        }
        assert_eq!(
            dependencies, 1,
            "one writer-to-reader dependency covers all eight reads"
        );
        assert_eq!(state.revision, 7);
        assert_eq!(
            source_scope(&state.clone(), sampled, false),
            (vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE),
            "an accepted read-only import snapshot keeps the same visibility"
        );
        for different_scope in [
            usage(
                AccessPurpose::Sampled,
                crate::program::ShaderStages::COMPUTE,
            ),
            Use {
                access: vk::AccessFlags2::SHADER_STORAGE_READ,
                ..sampled
            },
        ] {
            assert_eq!(
                source_scope(&state, different_scope, false),
                (
                    vk::PipelineStageFlags2::COPY | vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::TRANSFER_WRITE
                ),
                "a new reader scope needs both the writer and the layout transition"
            );
        }
        assert!(
            !source_scope(&state, sampled, true).0.is_empty(),
            "layout transitions remain ordered"
        );
        let mut write = usage(
            AccessPurpose::CopyDestination,
            AccessPurpose::CopyDestination.default_stages(),
        );
        write.writes = true;
        let (stage, access) = source_scope(&state, write, false);
        assert!(
            stage
                .contains(vk::PipelineStageFlags2::FRAGMENT_SHADER | vk::PipelineStageFlags2::COPY)
        );
        assert!(
            access
                .contains(vk::AccessFlags2::SHADER_SAMPLED_READ | vk::AccessFlags2::TRANSFER_WRITE)
        );
        state.record_access(write, false, 16);
        assert_eq!(state.revision, 16);
        assert!(
            !source_scope(&state, sampled, false).0.is_empty(),
            "a later write invalidates read visibility"
        );
    }

    #[test]
    fn read_write_access_does_not_establish_visibility_for_a_later_read() {
        let mut state = Snapshot::default();
        let mut read_write = usage(
            AccessPurpose::StorageReadWrite,
            AccessPurpose::StorageReadWrite.default_stages(),
        );
        read_write.writes = true;
        state.record_access(read_write, false, 1);
        assert_eq!(
            source_scope(&state, read_write, false),
            (read_write.stage, read_write.access),
            "identical read/write scopes still need a write dependency"
        );
        assert_eq!(
            source_scope(
                &state,
                usage(
                    AccessPurpose::StorageRead,
                    AccessPurpose::StorageRead.default_stages()
                ),
                false
            ),
            (
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE
            )
        );
    }

    #[test]
    fn buffer_read_visibility_reuses_exact_pairs_and_keeps_all_reader_scopes() {
        let mut state = Snapshot::default();
        let mut write = usage(
            AccessPurpose::CopyDestination,
            AccessPurpose::CopyDestination.default_stages(),
        );
        write.writes = true;
        state.record_access(write, false, 7);
        let copied = usage(
            AccessPurpose::CopySource,
            AccessPurpose::CopySource.default_stages(),
        );
        let mut dependencies = 0;
        for reader in 0..8 {
            dependencies += usize::from(!source_scope(&state, copied, false).0.is_empty());
            state.record_access(copied, false, 8 + reader);
        }
        assert_eq!(dependencies, 1);
        assert_eq!(state.revision, 7);
        assert!(state.access.contains(vk::AccessFlags2::TRANSFER_WRITE));
        assert!(source_scope(&state.clone(), copied, false).0.is_empty());

        let storage = usage(
            AccessPurpose::StorageRead,
            AccessPurpose::StorageRead.default_stages(),
        );
        assert_eq!(
            source_scope(&state, storage, false),
            (write.stage, write.access)
        );
        state.record_access(storage, false, 16);
        assert!(source_scope(&state, storage, false).0.is_empty());
        let combined = Use {
            stage: copied.stage | storage.stage,
            access: copied.access | storage.access,
            ..copied
        };
        assert!(
            !source_scope(&state, combined, false).0.is_empty(),
            "independently accumulated stage/access masks do not prove another exact pair"
        );
        let (stages, accesses) = source_scope(&state, write, false);
        assert!(stages.contains(copied.stage | storage.stage));
        assert!(accesses.contains(write.access | copied.access | storage.access));
        state.record_access(write, false, 17);
        assert_eq!(state.revision, 17);
        assert_eq!(
            source_scope(&state, storage, false),
            (write.stage, write.access)
        );
    }

    #[test]
    #[ignore = "explicit real GPU repeated reader visibility; run strict and shipping separately"]
    fn vulkan_graph_repeated_reader_visibility() {
        let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
        tracing::dispatcher::with_default(&dispatch, || {
            let mut runtime = VulkanRuntime::new(RuntimeConfig {
                execution_slots: 3,
                resource_capacity: 8,
                staging_bytes: 4096,
                max_shader_bindings: 4,
                staging_ranges: 256,
            })
            .unwrap();
            runtime
                .prepare_working_set(
                    super::super::FrameCapacity::default(),
                    &[
                        super::super::ResourceClass::Image {
                            desc: ImageDesc::rgba8(2, 2),
                            count: 1,
                        },
                        super::super::ResourceClass::Buffer {
                            desc: BufferDesc { size: 16 },
                            count: 4,
                        },
                    ],
                )
                .unwrap();
            runtime
                .prepare_compute_programs(&[crate::program::ManagedProgram::sampled_readback()])
                .unwrap();
            let diagnostics = runtime.diagnostics.clone();
            #[cfg(feature = "development-validation")]
            crate::backends::vulkan::assert_real_validation(&diagnostics);
            assert_ne!(
                runtime.owner.device.device_type(),
                vk::PhysicalDeviceType::CPU
            );
            let pixels = [
                11, 23, 37, 255, 41, 53, 67, 255, 71, 83, 97, 255, 101, 113, 127, 255,
            ];
            let mut graph = Graph::test_new();
            let source = graph.image("source", ImageDesc::rgba8(2, 2)).unwrap();
            graph
                .upload_image("source.upload", source, &pixels)
                .unwrap();
            graph.export_image(source).unwrap();
            let buffer = graph.buffer("buffer", BufferDesc { size: 16 }).unwrap();
            graph
                .upload_buffer("buffer.upload", buffer, 0, &pixels)
                .unwrap();
            graph.export_buffer(buffer).unwrap();
            let mut producer = runtime.submit(&graph.compile().unwrap()).unwrap();
            let exported = producer.export_image(source).unwrap();
            let exported_buffer = producer.export_buffer(buffer).unwrap();

            let mut graph = Graph::test_new();
            let source = graph.import_image("source", &exported).unwrap();
            let buffer = graph.import_buffer("buffer", &exported_buffer).unwrap();
            let mut reads = Vec::new();
            for index in 0..3 {
                let output = graph
                    .buffer(&format!("output.{index}"), BufferDesc { size: 16 })
                    .unwrap();
                super::super::tests::sample_rgba8(
                    &mut graph,
                    &format!("sample.{index}"),
                    source,
                    output,
                );
                reads.push(
                    graph
                        .readback_buffer(&format!("read.{index}"), output, 0, 16)
                        .unwrap(),
                );
            }
            let mut buffer_reads = Vec::new();
            for range in [0..16, 4..8, 8..16] {
                buffer_reads.push((
                    graph
                        .readback_buffer(
                            "buffer.read",
                            buffer,
                            range.start,
                            range.end - range.start,
                        )
                        .unwrap(),
                    range,
                ));
            }
            let mut consumer = runtime.submit(&graph.compile().unwrap()).unwrap();

            // A later pending writer must order all earlier readers, including
            // the reads whose identical writer dependencies were elided.
            let mut graph = Graph::test_new();
            let buffer = graph.import_buffer("buffer", &exported_buffer).unwrap();
            graph
                .upload_buffer("buffer.patch", buffer, 4, &[3, 5, 7, 11])
                .unwrap();
            let patched = graph
                .readback_buffer("buffer.patched", buffer, 0, 16)
                .unwrap();
            let mut writer = runtime.submit(&graph.compile().unwrap()).unwrap();
            let mut result = consumer.wait(Duration::from_secs(10)).unwrap().unwrap();
            for read in reads {
                assert_eq!(result.test_readback(read).unwrap(), pixels);
            }
            for (read, range) in buffer_reads {
                assert_eq!(
                    result.test_readback(read).unwrap(),
                    pixels[range.start as usize..range.end as usize]
                );
            }
            let mut written = writer.wait(Duration::from_secs(10)).unwrap().unwrap();
            let mut expected = pixels;
            expected[4..8].copy_from_slice(&[3, 5, 7, 11]);
            assert_eq!(written.test_readback(patched).unwrap(), expected);
            producer.wait(Duration::from_secs(10)).unwrap().unwrap();
            runtime.delete(exported.key()).unwrap();
            runtime.delete(exported_buffer.key()).unwrap();
            drop((producer, consumer, writer, result, written, runtime));
            assert!(
                diagnostics.snapshot().is_clean(),
                "{:#?}",
                diagnostics.snapshot()
            );
        });
    }
    #[test]
    #[ignore = "explicit real GPU upload ownership and future index import test; requires Core/SyncVal"]
    fn vulkan_graph_upload_input_retirement_and_future_index_import() {
        let dispatch = crate::backends::vulkan::real_vulkan_dispatch();
        tracing::dispatcher::with_default(&dispatch, || {
            let mut runtime = VulkanRuntime::new(RuntimeConfig {
                execution_slots: 3,
                resource_capacity: 8,
                staging_bytes: 4096,
                max_shader_bindings: 4,
                staging_ranges: 256,
            })
            .unwrap();
            runtime
                .prepare_working_set(
                    super::super::FrameCapacity::default(),
                    &[
                        super::super::ResourceClass::Buffer {
                            desc: BufferDesc { size: 16 },
                            count: 1,
                        },
                        super::super::ResourceClass::Buffer {
                            desc: BufferDesc { size: 12 },
                            count: 1,
                        },
                        super::super::ResourceClass::Buffer {
                            desc: BufferDesc { size: 4 },
                            count: 1,
                        },
                        super::super::ResourceClass::Buffer {
                            desc: BufferDesc { size: 84 },
                            count: 1,
                        },
                        super::super::ResourceClass::Image {
                            desc: ImageDesc::rgba8(2, 2),
                            count: 1,
                        },
                        super::super::ResourceClass::Image {
                            desc: ImageDesc::rgba8(3, 1),
                            count: 1,
                        },
                        super::super::ResourceClass::Image {
                            desc: ImageDesc::rgba8(1, 1),
                            count: 1,
                        },
                        super::super::ResourceClass::Image {
                            desc: ImageDesc::depth32(1, 1),
                            count: 1,
                        },
                    ],
                )
                .unwrap();
            let diagnostics = runtime.diagnostics.clone();
            crate::backends::vulkan::assert_real_validation(&diagnostics);
            assert_ne!(
                runtime.owner.device.device_type(),
                vk::PhysicalDeviceType::CPU
            );

            // Recording owns staging, not the original transient input. This holds
            // while GPU work is still pending, and also for unread completed output.
            let (plan, buffer_read, image_read, buffer_input, image_input) = {
                let buffer_bytes: Arc<[u8]> = Arc::from([17u8; 16]);
                let image_bytes: Arc<[u8]> = Arc::from([29u8; 16]);
                let buffer_input = Arc::downgrade(&buffer_bytes);
                let image_input = Arc::downgrade(&image_bytes);
                let mut graph = Graph::test_new();
                let buffer = graph.buffer("transient", BufferDesc { size: 16 }).unwrap();
                let image = graph
                    .image("transient-image", ImageDesc::rgba8(2, 2))
                    .unwrap();
                graph
                    .upload_buffer("buffer.upload", buffer, 0, &buffer_bytes)
                    .unwrap();
                graph
                    .upload_image("image.upload", image, &image_bytes)
                    .unwrap();
                let buffer_read = graph.readback_buffer("buffer.read", buffer, 0, 16).unwrap();
                let image_read = graph.readback_image("image.read", image).unwrap();
                (
                    graph.compile().unwrap(),
                    buffer_read,
                    image_read,
                    buffer_input,
                    image_input,
                )
            };
            let mut pending = runtime.submit(&plan).unwrap();
            drop(plan);
            assert!(buffer_input.upgrade().is_none());
            assert!(image_input.upgrade().is_none());
            let mut result = pending.wait(Duration::from_secs(10)).unwrap().unwrap();
            assert!(
                result.work.data.is_none(),
                "completed output retains no recording plan"
            );
            assert_eq!(result.test_readback(buffer_read).unwrap(), [17u8; 16]);
            assert_eq!(result.test_readback(image_read).unwrap(), [29u8; 16]);
            drop((pending, result));

            // The initial-transition ordering also covers earlier copies in other
            // submissions on the same queue. Only C is waited before reading A/B.
            let mut producer = Graph::test_new();
            let buffer = producer
                .buffer("pending-buffer", BufferDesc { size: 16 })
                .unwrap();
            producer
                .upload_buffer("pending.upload", buffer, 0, &[17; 16])
                .unwrap();
            producer.export_buffer(buffer).unwrap();
            let mut first = runtime.submit(&producer.compile().unwrap()).unwrap();
            let buffer = first.export_buffer(buffer).unwrap();
            let mut images = Graph::test_new();
            // A distinct extent requires a fresh native image with UNDEFINED layout.
            let image = images.image("new-image", ImageDesc::rgba8(3, 1)).unwrap();
            images
                .upload_image("new-image.upload", image, &[29; 12])
                .unwrap();
            images.export_image(image).unwrap();
            let mut second = runtime.submit(&images.compile().unwrap()).unwrap();
            let image = second.export_image(image).unwrap();
            let mut consumer = Graph::test_new();
            let input_buffer = consumer.import_buffer("pending-buffer", &buffer).unwrap();
            let input_image = consumer.import_image("pending-image", &image).unwrap();
            let buffer_read = consumer
                .readback_buffer("pending-buffer.read", input_buffer, 0, 16)
                .unwrap();
            let image_read = consumer
                .readback_image("pending-image.read", input_image)
                .unwrap();
            let mut third = runtime.submit(&consumer.compile().unwrap()).unwrap();
            let mut result = third.wait(Duration::from_secs(10)).unwrap().unwrap();
            assert_eq!(result.test_readback(buffer_read).unwrap(), [17; 16]);
            assert_eq!(result.test_readback(image_read).unwrap(), [29; 12]);
            second.wait(Duration::from_secs(10)).unwrap().unwrap();
            first.wait(Duration::from_secs(10)).unwrap().unwrap();
            runtime.delete(buffer.key()).unwrap();
            runtime.delete(image.key()).unwrap();
            drop((first, second, third, result));

            // A generic exported buffer can first acquire an index role in a later
            // graph. Updating that import without re-export must preserve provenance.
            let (initial, indices) = {
                let mut graph = Graph::test_new();
                let indices = graph
                    .buffer("future-indices", BufferDesc { size: 12 })
                    .unwrap();
                graph
                    .upload_buffer("indices.upload", indices, 0, &[0; 12])
                    .unwrap();
                graph.export_buffer(indices).unwrap();
                (graph.compile().unwrap(), indices)
            };
            let mut first = runtime.submit(&initial).unwrap();
            let exported = first.export_buffer(indices).unwrap();
            drop(initial);
            let mut update = Graph::test_new();
            let indices = update
                .import_buffer("persistent-indices", &exported)
                .unwrap();
            let patch = update.buffer("patch", BufferDesc { size: 4 }).unwrap();
            update
                .upload_buffer("patch.upload", patch, 0, &1u32.to_le_bytes())
                .unwrap();
            update
                .copy_buffer("patch.copy", patch, 0, indices, 4, 4)
                .unwrap();
            let mut second = runtime.submit(&update.compile().unwrap()).unwrap();
            second.wait(Duration::from_secs(10)).unwrap().unwrap();
            first.wait(Duration::from_secs(10)).unwrap().unwrap();
            let current = runtime.refresh_buffer(exported.key()).unwrap();
            let mut draw = Graph::test_new();
            let indices = draw.import_buffer("indices", &current).unwrap();
            let vertices = draw.buffer("vertices", BufferDesc { size: 84 }).unwrap();
            draw.upload_buffer("vertices.upload", vertices, 0, &[0; 84])
                .unwrap();
            let color = draw.image("color", ImageDesc::rgba8(1, 1)).unwrap();
            let depth = draw.image("depth", ImageDesc::depth32(1, 1)).unwrap();
            draw.raster(
                "degenerate-mesh",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[Attachment {
                        view: draw.image_view(color, 0, 0).unwrap(),
                        load: LoadOp::Clear([1.0, 0.0, 0.0, 1.0]),
                        store: StoreOp::Store,
                    }],
                    depth: Some(Attachment {
                        view: draw.image_view(depth, 0, 0).unwrap(),
                        load: LoadOp::Clear(1.0),
                        store: StoreOp::Store,
                    }),
                },
                |raster| {
                    raster.draw(
                        &crate::program::ManagedRasterProgram::mesh(),
                        [RasterDraw {
                            bindings: None,
                            vertices,
                            indices,
                            first_index: 0,
                            index_count: 3,
                            instance_count: 1,
                            parameters: RasterParameters::default().into(),
                        }],
                    )
                },
            )
            .unwrap();
            let read = draw.readback_image("color.read", color).unwrap();
            let mut third = runtime.submit(&draw.compile().unwrap()).unwrap();
            let mut result = third.wait(Duration::from_secs(10)).unwrap().unwrap();
            assert_eq!(result.test_readback(read).unwrap(), [255, 0, 0, 255]);
            runtime.delete(exported.key()).unwrap();
            drop((first, second, third, result, runtime));
            assert!(
                diagnostics.snapshot().is_clean(),
                "{:#?}",
                diagnostics.snapshot()
            );
        });
    }
}
