//! Prepared Graph operands, staging leases and native dependency planning.
//! Recording and completion are implemented by the private `record` child.

use std::collections::BTreeMap;
#[cfg(feature = "vulkan-diagnostics")]
use std::ffi::CString;
use std::sync::Arc;

use ash::vk;

use super::super::memory::MemoryOwner;
use super::super::transfer::staging::{SharedStaging, StagingRange};
use super::metrics;
use super::provenance::{BufferProvenance, IndexError};
use super::shaders::{ShaderBindings, ShaderPrograms};
use super::{NativeResource, Physical, PhysicalLease, RuntimeError, Snapshot};
use crate::graph::{
    AccessPurpose, AttachmentOps, CompiledGraph, ComputeTransform, MeshUpload, Operation,
    RasterDrawOperands, RasterParameters, ResourceDesc,
};

#[cfg(feature = "graph-diagnostics")]
pub(super) mod diagnostics;
mod record;

impl Snapshot {
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
        draws: Arc<[RasterDrawOperands]>,
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

impl TryFrom<&Operation> for PreparedOperation {
    type Error = RuntimeError;

    fn try_from(operation: &Operation) -> Result<Self, Self::Error> {
        Ok(match *operation {
            Operation::UploadBuffer {
                dst,
                offset,
                ref source,
                ..
            } => Self::UploadBuffer {
                dst,
                offset,
                size: source.len() as u64,
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
                draws: Arc::clone(draws),
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
            Operation::Culled | Operation::Declared(_) => {
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

fn provenance_requirements(graph: &CompiledGraph) -> (Vec<bool>, Vec<bool>) {
    // Every canonical buffer may become indices in a future graph, even when
    // this graph does not export it again. Transient vertex/transfer-only data
    // needs no CPU provenance. Mark sources of copies feeding required buffers.
    let mut retained = graph
        .resources
        .iter()
        .map(|resource| {
            resource.import.is_some() && matches!(resource.desc, ResourceDesc::Buffer(_))
        })
        .collect::<Vec<_>>();
    for &resource in &graph.exports {
        if matches!(graph.resources[resource].desc, ResourceDesc::Buffer(_)) {
            retained[resource] = true;
        }
    }
    let mut needed = retained.clone();
    for &pass in &graph.order {
        match &graph.passes[pass].operation {
            Operation::Raster { indices, .. } => needed[*indices] = true,
            Operation::RasterBatch { draws, .. } => {
                for draw in draws.iter() {
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
    (needed, retained)
}

struct PreparedPass {
    #[cfg(feature = "vulkan-diagnostics")]
    name: CString,
    operation: PreparedOperation,
    buffers: Vec<vk::BufferMemoryBarrier2<'static>>,
    images: Vec<vk::ImageMemoryBarrier2<'static>>,
    staging: Option<usize>,
    descriptor: Option<vk::DescriptorSet>,
}

pub(super) struct GraphWork {
    owner: Arc<MemoryOwner>,
    physical: Vec<Option<PhysicalLease>>,
    pub(super) final_states: Vec<Option<Snapshot>>,
    serial: u64,
    passes: Vec<PreparedPass>,
    uploads: Vec<StagingRange>,
    pub(super) readbacks: Vec<(usize, Option<StagingRange>)>,
    programs: Option<Arc<ShaderPrograms>>,
    bindings: Option<ShaderBindings>,
    pub(super) profile: Option<metrics::WorkProfile>,
}

impl GraphWork {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare(
        owner: Arc<MemoryOwner>,
        graph: &CompiledGraph,
        serial: u64,
        physical: Vec<Option<PhysicalLease>>,
        programs: Option<Arc<ShaderPrograms>>,
        mut bindings: Option<ShaderBindings>,
        upload: &SharedStaging,
        readback: &SharedStaging,
        #[cfg(feature = "graph-diagnostics")] mut diagnostic: Option<&mut String>,
    ) -> Result<Self, RuntimeError> {
        let mut states = physical
            .iter()
            .map(|value| value.as_ref().map(|value| value.lock().scheduled.clone()))
            .collect::<Vec<_>>();
        let (provenance_needed, retain_provenance) = provenance_requirements(graph);
        let mut passes = Vec::new();
        let mut uploads = Vec::new();
        let mut readbacks = Vec::new();
        for &index in &graph.order {
            let pass = &graph.passes[index];
            #[cfg(feature = "graph-diagnostics")]
            if let Some(output) = diagnostic.as_deref_mut() {
                diagnostics::pass(output, index, pass);
            }
            let mut prepared = PreparedPass {
                #[cfg(feature = "vulkan-diagnostics")]
                name: CString::new(pass.name.as_str())
                    .map_err(|_| RuntimeError::invalid("pass debug name contains a NUL byte"))?,
                operation: PreparedOperation::try_from(&pass.operation)?,
                buffers: Vec::new(),
                images: Vec::new(),
                staging: None,
                descriptor: None,
            };
            // Aggregate one pass's uses before modifying previous-access state;
            // disjoint self-copies must not appear to depend on themselves.
            let mut uses: BTreeMap<usize, Use> = BTreeMap::new();
            for access in &pass.accesses {
                let current = usage(access.purpose);
                let target = uses.entry(access.resource).or_default();
                target.stage |= current.stage;
                target.access |= current.access;
                target.writes |= access.write;
                if current.layout != vk::ImageLayout::UNDEFINED {
                    target.layout = current.layout;
                }
            }
            for (resource, current) in uses {
                let native = physical[resource].as_ref().unwrap();
                let previous = states[resource].as_mut().unwrap();
                let layout_change = matches!(native.native, NativeResource::Image(_))
                    && previous.layout != current.layout;
                let (source_stage, source_access) = source_scope(previous, current, layout_change);
                let dependency = !source_stage.is_empty();
                match &native.native {
                    NativeResource::Buffer(buffer) if dependency => {
                        prepared.buffers.push(
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
                        prepared.images.push(
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
                    let buffer = prepared.buffers.last().filter(|barrier| {
                        dependency
                            && matches!(&native.native, NativeResource::Buffer(buffer)
                            if buffer.handle == barrier.buffer)
                    });
                    let image = prepared.images.last().filter(|barrier| {
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
            (|| -> Result<(), RuntimeError> {
                match &pass.operation {
                    Operation::UploadBuffer {
                        dst,
                        offset,
                        bytes,
                        source,
                        mesh,
                    } => {
                        let slice = &bytes[source.clone()];
                        let mut range = upload.allocate(slice.len() as u64)?;
                        range.write(slice)?;
                        prepared.staging = Some(uploads.len());
                        uploads.push(range);
                        if provenance_needed[*dst] {
                            let target = *offset..*offset + slice.len() as u64;
                            let provenance = &mut states[*dst].as_mut().unwrap().provenance;
                            match mesh {
                                Some(MeshUpload::Vertices) => {
                                    *provenance = BufferProvenance::default()
                                }
                                Some(MeshUpload::Indices { byte_len, maximum }) => {
                                    provenance.write_mesh_indices(target, *byte_len, *maximum);
                                }
                                None => provenance.write_slice(
                                    target,
                                    Arc::clone(bytes),
                                    source.clone(),
                                ),
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
                            let contents = states[*src]
                                .as_ref()
                                .unwrap()
                                .provenance
                                .copied(*src_offset..*src_offset + *size, *dst_offset);
                            states[*dst]
                                .as_mut()
                                .unwrap()
                                .provenance
                                .replace(*dst_offset..*dst_offset + *size, contents);
                        }
                    }
                    Operation::UploadImage { bytes, .. } => {
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
                        for (draw_index, draw) in draws.iter().enumerate() {
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
                                    index_error(error).context(format!("draw {draw_index}"))
                                })?;
                        }
                    }
                    Operation::CopyImage { .. } | Operation::ClearAttachments { .. } => {}
                    Operation::Culled | Operation::Declared(_) => {
                        return Err(RuntimeError::unsupported(
                            "declaration-only pass is not executable",
                        ));
                    }
                }
                Ok(())
            })()
            .map_err(|error| error.context(format!("pass {:?}", pass.name)))?;
            passes.push(prepared);
        }
        for (index, state) in states.iter_mut().enumerate() {
            if let Some(state) = state {
                state.serial = serial;
                state.initialized = graph.final_contents[index].clone();
                // Transient index validation has finished. Only canonical imports
                // and exports can acquire a future index role in another graph.
                if !retain_provenance[index] {
                    state.provenance = BufferProvenance::default();
                }
            }
        }
        #[cfg(feature = "graph-diagnostics")]
        if let Some(output) = diagnostic {
            diagnostics::finish(output, graph, &states, !readbacks.is_empty());
        }
        Ok(Self {
            owner,
            physical,
            final_states: states,
            serial,
            passes,
            uploads,
            readbacks,
            programs,
            bindings,
            profile: None,
        })
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
    } else if previous.stage == current.stage && previous.access == current.access {
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
        let mut graph = Graph::new();
        let buffer = graph.buffer("slice", BufferDesc { size: 4 }).unwrap();
        let upload = graph
            .upload_buffer_slice("upload", buffer, 0, input, 5..9)
            .unwrap();
        graph.readback_buffer("read", buffer, 0, 4).unwrap();
        let plan = graph.compile().unwrap();
        let operation = PreparedOperation::try_from(&plan.passes[upload.index].operation).unwrap();
        assert!(matches!(
            &operation,
            PreparedOperation::UploadBuffer {
                offset: 0,
                size: 4,
                ..
            }
        ));
        drop(graph);
        drop(plan);
        assert!(
            weak.upgrade().is_none(),
            "native recording operands must not retain CPU upload input"
        );
    }

    #[test]
    fn provenance_follows_index_copy_sources_and_persistent_imports_only() {
        let mut graph = Graph::new();
        let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
        graph
            .upload_buffer("vertices.upload", vertices, 0, vec![0; 84])
            .unwrap();
        let source = graph.buffer("source", BufferDesc { size: 12 }).unwrap();
        let middle = graph.buffer("middle", BufferDesc { size: 12 }).unwrap();
        let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
        graph
            .upload_buffer("index.upload", source, 0, vec![0; 12])
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
                    initialized: std::iter::once(0..12).collect(),
                },
            )
            .unwrap();
        graph
            .readback_buffer("persistent.read", imported, 0, 12)
            .unwrap();
        let (needed, retained) = provenance_requirements(&graph.compile().unwrap());
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
            })
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
            let mut graph = Graph::new();
            let source = graph.image("source", ImageDesc::rgba8(2, 2)).unwrap();
            graph.upload_image("source.upload", source, pixels).unwrap();
            graph.export_image(source).unwrap();
            let mut producer = runtime.submit(&graph.compile().unwrap()).unwrap();
            let exported = producer.export_image(source).unwrap();

            let mut graph = Graph::new();
            let source = graph.import_image("source", &exported).unwrap();
            let mut reads = Vec::new();
            for index in 0..3 {
                let output = graph
                    .image(format!("output.{index}"), ImageDesc::rgba8(2, 2))
                    .unwrap();
                graph
                    .composite(format!("sample.{index}"), source, output)
                    .unwrap();
                reads.push(
                    graph
                        .readback_image(format!("read.{index}"), output)
                        .unwrap(),
                );
            }
            let mut consumer = runtime.submit(&graph.compile().unwrap()).unwrap();
            let mut result = consumer.wait(Duration::from_secs(10)).unwrap().unwrap();
            for read in reads {
                assert_eq!(result.readback(read).unwrap(), pixels);
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
            })
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
                let mut graph = Graph::new();
                let buffer = graph.buffer("transient", BufferDesc { size: 16 }).unwrap();
                let image = graph
                    .image("transient-image", ImageDesc::rgba8(2, 2))
                    .unwrap();
                graph
                    .upload_buffer_shared("buffer.upload", buffer, 0, buffer_bytes)
                    .unwrap();
                graph
                    .upload_image_shared("image.upload", image, image_bytes)
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
                result.work.passes.is_empty(),
                "completed output retains no recording plan"
            );
            assert_eq!(result.readback(buffer_read).unwrap(), [17u8; 16]);
            assert_eq!(result.readback(image_read).unwrap(), [29u8; 16]);
            drop((pending, result));

            // The initial-transition ordering also covers earlier copies in other
            // submissions on the same queue. Only C is waited before reading A/B.
            let mut producer = Graph::new();
            let buffer = producer
                .buffer("pending-buffer", BufferDesc { size: 16 })
                .unwrap();
            producer
                .upload_buffer("pending.upload", buffer, 0, vec![17; 16])
                .unwrap();
            producer.export_buffer(buffer).unwrap();
            let mut first = runtime.submit(&producer.compile().unwrap()).unwrap();
            let buffer = first.export_buffer(buffer).unwrap();
            let mut images = Graph::new();
            // A distinct extent requires a fresh native image with UNDEFINED layout.
            let image = images.image("new-image", ImageDesc::rgba8(3, 1)).unwrap();
            images
                .upload_image("new-image.upload", image, vec![29; 12])
                .unwrap();
            images.export_image(image).unwrap();
            let mut second = runtime.submit(&images.compile().unwrap()).unwrap();
            let image = second.export_image(image).unwrap();
            let mut consumer = Graph::new();
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
            assert_eq!(result.readback(buffer_read).unwrap(), [17; 16]);
            assert_eq!(result.readback(image_read).unwrap(), [29; 12]);
            second.wait(Duration::from_secs(10)).unwrap().unwrap();
            first.wait(Duration::from_secs(10)).unwrap().unwrap();
            runtime.delete(buffer.key()).unwrap();
            runtime.delete(image.key()).unwrap();
            drop((first, second, third, result));

            // A generic exported buffer can first acquire an index role in a later
            // graph. Updating that import without re-export must preserve provenance.
            let (initial, indices) = {
                let mut graph = Graph::new();
                let indices = graph
                    .buffer("future-indices", BufferDesc { size: 12 })
                    .unwrap();
                graph
                    .upload_buffer("indices.upload", indices, 0, vec![0; 12])
                    .unwrap();
                graph.export_buffer(indices).unwrap();
                (graph.compile().unwrap(), indices)
            };
            let mut first = runtime.submit(&initial).unwrap();
            let exported = first.export_buffer(indices).unwrap();
            drop(initial);
            let mut update = Graph::new();
            let indices = update
                .import_buffer("persistent-indices", &exported)
                .unwrap();
            let patch = update.buffer("patch", BufferDesc { size: 4 }).unwrap();
            update
                .upload_buffer("patch.upload", patch, 0, 1u32.to_le_bytes().to_vec())
                .unwrap();
            update
                .copy_buffer("patch.copy", patch, 0, indices, 4, 4)
                .unwrap();
            let mut second = runtime.submit(&update.compile().unwrap()).unwrap();
            second.wait(Duration::from_secs(10)).unwrap().unwrap();
            first.wait(Duration::from_secs(10)).unwrap().unwrap();
            let current = runtime.refresh_buffer(exported.key()).unwrap();
            let mut draw = Graph::new();
            let indices = draw.import_buffer("indices", &current).unwrap();
            let vertices = draw.buffer("vertices", BufferDesc { size: 84 }).unwrap();
            draw.upload_buffer("vertices.upload", vertices, 0, vec![0; 84])
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
            assert_eq!(result.readback(read).unwrap(), [255, 0, 0, 255]);
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
