//! Prepared Graph operands, staging leases and native dependency planning.
//! Recording and completion are implemented by the private `record` child.

use std::ops::{Deref, DerefMut, Range};
use std::sync::Arc;

use ash::vk;

use super::super::memory::MemoryOwner;
use super::super::program::{ShaderBindings, ShaderPrograms};
use super::super::transfer::staging::SharedStaging;
use super::metrics;
use super::provenance::IndexError;
use super::storage::{StorageLease, StoragePool};
use super::{NativeResource, Physical, RuntimeError, Snapshot};
use crate::graph::{
    AccessPurpose, AttachmentOps, CompiledGraph, ComputeTransform, MeshUpload, Operation,
    RasterParameters, ResourceDesc,
};

#[cfg(feature = "graph-diagnostics")]
pub(super) mod diagnostics;
mod record;
mod storage;
pub(super) use storage::{ResultData, WorkData};

impl Snapshot {
    // Only read-only consumers without a layout transition may reuse this
    // visibility. Exact equality deliberately avoids inferring stage/access
    // pairings from independently accumulated masks.
    pub(super) fn has_read_visibility(
        &self,
        stage: vk::PipelineStageFlags2,
        access: vk::AccessFlags2,
    ) -> bool {
        self.stage == stage && self.access == access
    }

    fn record_access(&mut self, current: Use, layout_change: bool, serial: u64) {
        if !current.writes && !layout_change {
            self.stage |= current.stage;
            self.access |= current.access;
        } else {
            self.stage = current.stage;
            self.access = current.access;
        }
        if current.writes {
            self.writer_stage = current.stage;
            self.writer_access = current.access;
            self.revision = serial;
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
    Compute {
        dst: usize,
        transform: ComputeTransform,
    },
    Raster {
        vertices: usize,
        indices: usize,
        index_count: u32,
        color: usize,
        depth: usize,
        color_ops: AttachmentOps,
        depth_ops: AttachmentOps,
        clear_color: [f32; 4],
        clear_depth: f32,
        parameters: RasterParameters,
    },
    RasterBatch {
        draws: Range<usize>,
        color: usize,
        depth: usize,
        clear_color: [f32; 4],
        clear_depth: f32,
    },
    ClearAttachments {
        color: usize,
        depth: usize,
        clear_color: [f32; 4],
        clear_depth: f32,
    },
    Composite {
        dst: usize,
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
            Operation::Compute { dst, transform, .. } => Self::Compute { dst, transform },
            Operation::Raster {
                vertices,
                indices,
                index_count,
                color,
                depth,
                color_ops,
                depth_ops,
                clear_color,
                clear_depth,
                parameters,
            } => Self::Raster {
                vertices,
                indices,
                index_count,
                color,
                depth,
                color_ops,
                depth_ops,
                clear_color,
                clear_depth,
                parameters,
            },
            Operation::RasterBatch {
                ref draws,
                color,
                depth,
                clear_color,
                clear_depth,
            } => Self::RasterBatch {
                draws: draws.clone(),
                color,
                depth,
                clear_color,
                clear_depth,
            },
            Operation::ClearAttachments {
                color,
                depth,
                clear_color,
                clear_depth,
            } => Self::ClearAttachments {
                color,
                depth,
                clear_color,
                clear_depth,
            },
            Operation::Composite { dst, .. } => Self::Composite { dst },
            Operation::Declared(_) => {
                return Err(RuntimeError::unsupported(
                    "pass has no executable operation",
                ));
            }
        })
    }
}

fn index_error(error: IndexError) -> RuntimeError {
    match error {
        IndexError::UnknownContents => {
            RuntimeError::unsupported("raster indices must have CPU-known upload/copy contents")
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
    needed.copy_from_slice(retained);
    for &pass in &graph.order {
        match &graph.passes[pass].operation {
            Operation::Raster { indices, .. } => needed[*indices] = true,
            Operation::RasterBatch { draws, .. } => {
                for draw in graph.raster_draws(draws) {
                    needed[draw.indices] = true;
                }
            }
            _ => {}
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
    descriptor: Option<vk::DescriptorSet>,
}

pub(super) struct GraphWork {
    owner: Arc<MemoryOwner>,
    data: Option<StorageLease<WorkData>>,
    pub(super) result: StorageLease<ResultData>,
    serial: u64,
    programs: Option<Arc<ShaderPrograms>>,
    bindings: Option<ShaderBindings>,
    plan: Option<CompiledGraph>,
    pub(super) profile: Option<metrics::WorkProfile>,
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
        let data = work.acquire()?;
        let limit = data.limit.graph;
        if graph.resources.len() > limit.resources
            || graph.order.len() > limit.passes
            || graph
                .order
                .iter()
                .map(|&pass| graph.accesses(pass).len())
                .sum::<usize>()
                > limit.accesses
        {
            return Err(RuntimeError::capacity(
                "prepared Graph work capacity exhausted",
            ));
        }
        let result = results.acquire()?;
        Ok(Self {
            owner,
            data: Some(data),
            result,
            serial,
            programs: None,
            bindings: None,
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
        upload: &SharedStaging,
        readback: &SharedStaging,
        #[cfg(feature = "graph-diagnostics")] mut diagnostic: Option<&mut String>,
    ) -> Result<Self, RuntimeError> {
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
            ..
        } = data;
        #[cfg(feature = "vulkan-diagnostics")]
        let labels = &mut data.labels;
        let readbacks = &mut self.result.readbacks;
        for (index, physical) in physical.iter().enumerate().take(graph.resources.len()) {
            if let Some(physical) = physical {
                states[index]
                    .as_mut()
                    .unwrap()
                    .copy_from(&physical.lock().scheduled)?;
            }
        }
        provenance_requirements(graph, provenance_needed, retain_provenance);
        for &index in &graph.order {
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
                        return Err(RuntimeError::capacity(
                            "prepared debug label capacity exhausted",
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
                descriptor: None,
            };
            // Aggregate one pass's uses before modifying previous-access state;
            // disjoint self-copies must not appear to depend on themselves.
            touched.clear();
            for access in graph.accesses(index) {
                let current = usage(access.purpose);
                if !used[access.resource] {
                    used[access.resource] = true;
                    touched.push(access.resource);
                }
                let target = &mut uses[access.resource];
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
                                        .level_count(1)
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
                        let mut range = upload.allocate(slice.len() as u64)?;
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
                                        RuntimeError::capacity(
                                            "prepared provenance capacity exhausted",
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
                                    RuntimeError::capacity(
                                        "prepared provenance copy capacity exhausted",
                                    )
                                })?;
                            states[*dst]
                                .as_mut()
                                .unwrap()
                                .provenance
                                .replace(*dst_offset..*dst_offset + *size, copy_scratch)
                                .map_err(|_| {
                                    RuntimeError::capacity(
                                        "prepared provenance replacement capacity exhausted",
                                    )
                                })?;
                        }
                    }
                    Operation::UploadImage { .. } => {
                        let bytes = graph.upload_data(&pass.operation);
                        let mut range = upload.allocate(bytes.len() as u64)?;
                        range.write(bytes)?;
                        prepared.staging = Some(uploads.len());
                        uploads.push(range);
                    }
                    Operation::ReadbackBuffer { size, .. } => {
                        prepared.staging = Some(readbacks.len());
                        readbacks.push((index, Some(readback.allocate(*size)?)));
                    }
                    Operation::ReadbackImage { src } => {
                        let ResourceDesc::Image(desc) = graph.resources[*src].desc else {
                            unreachable!()
                        };
                        prepared.staging = Some(readbacks.len());
                        readbacks.push((index, Some(readback.allocate(desc.byte_len().unwrap())?)));
                    }
                    Operation::Compute { src, dst, .. } => {
                        prepared.descriptor = Some(bindings.as_mut().unwrap().compute(
                            physical[*src].as_ref().unwrap().image(),
                            physical[*dst].as_ref().unwrap().image(),
                        )?);
                    }
                    Operation::Composite { src, .. } => {
                        prepared.descriptor = Some(
                            bindings
                                .as_mut()
                                .unwrap()
                                .composite(physical[*src].as_ref().unwrap().image())?,
                        );
                    }
                    Operation::Raster {
                        vertices,
                        indices,
                        index_count,
                        ..
                    } => {
                        let vertex_count = graph.resources[*vertices].desc.units() / 28;
                        states[*indices]
                            .as_mut()
                            .unwrap()
                            .provenance
                            .validate_indices(
                                *index_count,
                                vertex_count,
                                owner.device.max_draw_indexed_index_value(),
                            )
                            .map_err(index_error)?;
                    }
                    Operation::RasterBatch { draws, .. } => {
                        for (draw_index, draw) in graph.raster_draws(draws).iter().enumerate() {
                            let vertex_count = graph.resources[draw.vertices].desc.units() / 28;
                            states[draw.indices]
                                .as_mut()
                                .unwrap()
                                .provenance
                                .validate_indices(
                                    draw.index_count,
                                    vertex_count,
                                    owner.device.max_draw_indexed_index_value(),
                                )
                                .map_err(|error| {
                                    index_error(error).context(format_args!("draw {draw_index}"))
                                })?;
                        }
                    }
                    Operation::CopyImage { .. } | Operation::ClearAttachments { .. } => {}
                    Operation::Declared(_) => {
                        return Err(RuntimeError::unsupported(
                            "declaration-only pass is not executable",
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

fn usage(purpose: AccessPurpose) -> Use {
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
        StorageRead => (
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ,
            vk::ImageLayout::GENERAL,
        ),
        StorageWrite => (
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::ImageLayout::GENERAL,
        ),
        StorageReadWrite => (
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::ImageLayout::GENERAL,
        ),
        Sampled => (
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
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
        ColorAttachment => (
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
            previous.stage | previous.writer_stage,
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
        // A prior read-only layout transition already exposed this writer to
        // this exact destination scope. A pipeline barrier's second scope also
        // includes later commands on the same queue; no intervening write or
        // layout transition has invalidated that visibility. Equality keeps
        // this deliberately narrow and does not infer stage/access pairings
        // from independently accumulated masks. Keep the writer below for a
        // different reader scope and for subsequent writes/layout changes.
        (vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE)
    } else {
        // A COPY read does not by itself make an older COPY write visible to
        // vertex/index/shader reads. Preserve the last writer across readers,
        // including imports, and expose it to each actual destination access.
        (previous.writer_stage, previous.writer_access)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{RuntimeConfig, VulkanRuntime};
    use super::*;
    use crate::graph::{
        Attachment, BufferDesc, Graph, ImageDesc, ImportBuffer, LoadOp, RasterAttachments,
        RasterDraw, ResourceKey, StoreOp,
    };
    use std::time::Duration;
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
                RasterDraw {
                    vertices,
                    indices,
                    index_count: 3,
                    parameters: RasterParameters::default(),
                },
                RasterAttachments {
                    color: Attachment {
                        image: color,
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    },
                    depth: Attachment {
                        image: depth,
                        load: LoadOp::Clear(1.0),
                        store: StoreOp::Store,
                    },
                },
            )
            .unwrap();
        graph.readback_image("color.read", color).unwrap();
        let imported = graph
            .import_buffer(
                "persistent",
                &ImportBuffer {
                    key: ResourceKey {
                        owner: 1,
                        index: 0,
                        generation: 1,
                    },
                    revision: 1,
                    desc: BufferDesc { size: 12 },
                    initialized: crate::graph::ImportRanges::test_capture(std::slice::from_ref(
                        &(0..12),
                    )),
                },
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
            let (stage, access) = source_scope(&snapshot, usage(purpose), false);
            assert!(stage.contains(vk::PipelineStageFlags2::COPY));
            assert!(access.contains(vk::AccessFlags2::TRANSFER_WRITE));
        }
        let mut writer = usage(AccessPurpose::CopyDestination);
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
                source_scope(&initial, usage(purpose), true),
                (vk::PipelineStageFlags2::COPY, vk::AccessFlags2::NONE)
            );
        }
        assert_eq!(
            source_scope(&initial, usage(AccessPurpose::CopyDestination), false),
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
            source_scope(&previous, usage(AccessPurpose::Sampled), true),
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
        state.record_access(usage(AccessPurpose::CopySource), false, 8);
        state.record_access(usage(AccessPurpose::StorageRead), false, 9);
        state.record_access(usage(AccessPurpose::Sampled), true, 10);
        assert_eq!(state.revision, 7, "reads/layout changes preserve contents");
        let mut write = usage(AccessPurpose::CopyDestination);
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
        let sampled = usage(AccessPurpose::Sampled);
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
            usage(AccessPurpose::StorageRead),
            Use {
                access: vk::AccessFlags2::SHADER_STORAGE_READ,
                ..sampled
            },
        ] {
            assert_eq!(
                source_scope(&state, different_scope, false),
                (
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE
                ),
                "a new reader stage or access type still needs writer visibility"
            );
        }
        assert!(
            !source_scope(&state, sampled, true).0.is_empty(),
            "layout transitions remain ordered"
        );
        let mut write = usage(AccessPurpose::CopyDestination);
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
        let mut read_write = usage(AccessPurpose::StorageReadWrite);
        read_write.writes = true;
        state.record_access(read_write, false, 1);
        assert_eq!(
            source_scope(&state, read_write, false),
            (read_write.stage, read_write.access),
            "identical read/write scopes still need a write dependency"
        );
        assert_eq!(
            source_scope(&state, usage(AccessPurpose::StorageRead), false),
            (
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE
            )
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
                    &[super::super::ResourceClass::Image {
                        desc: ImageDesc::rgba8(2, 2),
                        count: 4,
                    }],
                )
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
            let mut producer = runtime.submit(&graph.compile().unwrap()).unwrap();
            let exported = producer.export_image(source).unwrap();

            let mut graph = Graph::test_new();
            let source = graph.import_image("source", &exported).unwrap();
            let mut reads = Vec::new();
            for index in 0..3 {
                let output = graph
                    .image(&format!("output.{index}"), ImageDesc::rgba8(2, 2))
                    .unwrap();
                graph
                    .composite(&format!("sample.{index}"), source, output)
                    .unwrap();
                reads.push(
                    graph
                        .readback_image(&format!("read.{index}"), output)
                        .unwrap(),
                );
            }
            let mut consumer = runtime.submit(&graph.compile().unwrap()).unwrap();
            let mut result = consumer.wait(Duration::from_secs(10)).unwrap().unwrap();
            for read in reads {
                assert_eq!(result.test_readback(read).unwrap(), pixels);
            }
            producer.wait(Duration::from_secs(10)).unwrap().unwrap();
            runtime.delete(exported.key()).unwrap();
            drop((producer, consumer, result, runtime));
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
                RasterDraw {
                    vertices,
                    indices,
                    index_count: 3,
                    parameters: RasterParameters::default(),
                },
                RasterAttachments {
                    color: Attachment {
                        image: color,
                        load: LoadOp::Clear([1.0, 0.0, 0.0, 1.0]),
                        store: StoreOp::Store,
                    },
                    depth: Attachment {
                        image: depth,
                        load: LoadOp::Clear(1.0),
                        store: StoreOp::Store,
                    },
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
