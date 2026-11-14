//! Transactional construction of copy, fixed shader and declaration-only passes.
//!
//! Each builder validates raw inputs and appends only after all operands succeed.

use std::{collections::BTreeMap, ops::Range, sync::Arc};

use super::accesses::conflicting_accesses;
use super::{
    Access, AccessKind, AccessPurpose, AccessRange, AttachmentOps, BufferHandle, ComputeTransform,
    Graph, GraphError, GraphErrorKind, ImageAspect, ImageDesc, ImageFormat, ImageHandle,
    LoadAction, LoadOp, MeshUpload, Operation, PassHandle, PassKind, RasterAttachments, RasterDraw,
    RasterDrawOperands, RasterParameters, ResourceAccess, ResourceDesc, ResourceHandle, StoreOp,
    ValidatedRasterParameters, overlaps,
};

impl Graph {
    pub fn upload_buffer(
        &mut self,
        name: impl Into<String>,
        dst: BufferHandle,
        offset: u64,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<PassHandle, GraphError> {
        self.upload_buffer_shared(name, dst, offset, bytes.into().into())
    }

    /// Retains shared immutable bytes without copying them during compilation.
    /// Reuse the same allocation when building graphs with the same upload data.
    pub fn upload_buffer_shared(
        &mut self,
        name: impl Into<String>,
        dst: BufferHandle,
        offset: u64,
        bytes: Arc<[u8]>,
    ) -> Result<PassHandle, GraphError> {
        let source = 0..bytes.len();
        self.upload_buffer_slice(name, dst, offset, bytes, source)
    }

    /// Retain a slice of shared immutable input without allocating a chunk copy.
    /// The CPU source offset is independent of the aligned GPU destination offset.
    pub(crate) fn upload_buffer_slice(
        &mut self,
        name: impl Into<String>,
        dst: BufferHandle,
        offset: u64,
        bytes: Arc<[u8]>,
        source: Range<usize>,
    ) -> Result<PassHandle, GraphError> {
        self.upload_buffer_slice_with_mesh(name, dst, offset, bytes, source, None)
    }

    /// The engine bridge supplies a trusted immutable stream and its provenance;
    /// this Graph boundary owns handle/range checks and transactional insertion.
    pub(crate) fn upload_mesh_slice(
        &mut self,
        name: impl Into<String>,
        dst: BufferHandle,
        bytes: &Arc<[u8]>,
        source: Range<usize>,
        mesh: MeshUpload,
    ) -> Result<PassHandle, GraphError> {
        let id = self.buffer_id(dst)?;
        if self.resources[id].desc.units() != bytes.len() as u64
            || !source.start.is_multiple_of(4)
            || !source.end.is_multiple_of(4)
        {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "managed mesh upload must preserve the stream size and aligned byte positions",
                id,
            ));
        }
        self.upload_buffer_slice_with_mesh(
            name,
            dst,
            source.start as u64,
            Arc::clone(bytes),
            source,
            Some(mesh),
        )
    }

    fn upload_buffer_slice_with_mesh(
        &mut self,
        name: impl Into<String>,
        dst: BufferHandle,
        offset: u64,
        bytes: Arc<[u8]>,
        source: Range<usize>,
        mesh: Option<MeshUpload>,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let dst = graph.buffer_id(dst)?;
            let slice = bytes.get(source.clone()).ok_or_else(|| {
                graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    format!("upload source range {source:?} exceeds shared input length {} or is reversed", bytes.len()),
                    dst,
                )
            })?;
            let range = graph.buffer_range(dst, offset, slice.len() as u64, true)?;
            Ok((
                Operation::UploadBuffer { dst, offset, bytes, source, mesh },
                vec![write(dst, range, AccessPurpose::CopyDestination)],
                false,
            ))
        })
    }
    pub fn copy_buffer(
        &mut self,
        name: impl Into<String>,
        src: BufferHandle,
        src_offset: u64,
        dst: BufferHandle,
        dst_offset: u64,
        size: u64,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.buffer_id(src)?;
            let dst = graph.buffer_id(dst)?;
            let source = graph.buffer_range(src, src_offset, size, true)?;
            let target = graph.buffer_range(dst, dst_offset, size, true)?;
            if src == dst && overlaps(&source, &target) {
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "overlapping same-buffer copy",
                        dst,
                    )
                    .with_range(AccessRange::Buffer(target)));
            }
            Ok((
                Operation::CopyBuffer {
                    src,
                    dst,
                    src_offset,
                    dst_offset,
                    size,
                },
                vec![
                    read(src, source, AccessPurpose::CopySource),
                    write(dst, target, AccessPurpose::CopyDestination),
                ],
                false,
            ))
        })
    }
    pub fn readback_buffer(
        &mut self,
        name: impl Into<String>,
        src: BufferHandle,
        offset: u64,
        size: u64,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.buffer_id(src)?;
            let range = graph.buffer_range(src, offset, size, true)?;
            Ok((
                Operation::ReadbackBuffer { src, offset, size },
                vec![read(src, range, AccessPurpose::CopySource)],
                true,
            ))
        })
    }
    pub fn upload_image(
        &mut self,
        name: impl Into<String>,
        dst: ImageHandle,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<PassHandle, GraphError> {
        self.upload_image_shared(name, dst, bytes.into().into())
    }

    /// Retains one complete packed RGBA8 image as shared immutable upload data.
    pub fn upload_image_shared(
        &mut self,
        name: impl Into<String>,
        dst: ImageHandle,
        bytes: Arc<[u8]>,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let dst = graph.image_id(dst)?;
            graph.color(dst)?;
            if graph.image_desc_at(dst)?.byte_len() != Some(bytes.len() as u64) {
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "image upload must provide every packed RGBA8 texel",
                        dst,
                    )
                    .with_range(graph.access_range(dst, 0..1)));
            }
            Ok((
                Operation::UploadImage { dst, bytes },
                vec![write(dst, 0..1, AccessPurpose::CopyDestination)],
                false,
            ))
        })
    }
    pub fn copy_image(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
        dst: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            let dst = graph.image_id(dst)?;
            graph.compatible_color_pair(src, dst)?;
            Ok((
                Operation::CopyImage { src, dst },
                vec![
                    read(src, 0..1, AccessPurpose::CopySource),
                    write(dst, 0..1, AccessPurpose::CopyDestination),
                ],
                false,
            ))
        })
    }
    pub fn readback_image(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            graph.color(src)?;
            Ok((
                Operation::ReadbackImage { src },
                vec![read(src, 0..1, AccessPurpose::CopySource)],
                true,
            ))
        })
    }
    pub fn compute(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
        dst: ImageHandle,
        transform: ComputeTransform,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            let dst = graph.image_id(dst)?;
            graph.compatible_color_pair(src, dst)?;
            Ok((
                Operation::Compute {
                    src,
                    dst,
                    transform,
                },
                vec![
                    read(src, 0..1, AccessPurpose::StorageRead),
                    write(dst, 0..1, AccessPurpose::StorageWrite),
                ],
                false,
            ))
        })
    }
    /// Draw u32 indexed triangles using tightly packed seven-f32 vertices
    /// (`xyz`, then linear `rgba`). The runtime checks CPU-known index values
    /// against the vertex extent and rejects unknown index contents.
    /// Transform and tint are owned values; no scene borrow or arbitrary shader
    /// or native commands are retained. Load/Discard require no clear value.
    pub fn raster(
        &mut self,
        name: impl Into<String>,
        draw: RasterDraw,
        attachments: RasterAttachments,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let draw = graph.raster_draw(
                draw.vertices,
                draw.indices,
                draw.index_count,
                draw.parameters,
            )?;
            let vertices = draw.vertices;
            let indices = draw.indices;
            let color = graph.image_id(attachments.color.image)?;
            let depth = graph.image_id(attachments.depth.image)?;
            graph.color(color)?;
            let c = graph.image_desc_at(color)?;
            let d = graph.image_desc_at(depth)?;
            if d.format != ImageFormat::Depth32Float || (c.width, c.height) != (d.width, d.height) {
                return Err(graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "raster needs matching RGBA8 color and D32 depth extents",
                    depth,
                ));
            }
            let vs = graph.resources[vertices].desc.units();
            let ix = u64::from(draw.index_count) * 4;
            let clear_color = match attachments.color.load {
                LoadOp::Clear(value) => {
                    if value.iter().any(|x| !x.is_finite()) {
                        return Err(GraphError::new(
                            GraphErrorKind::InvalidInput,
                            "invalid attachment clear value",
                        ));
                    }
                    value
                }
                LoadOp::Load | LoadOp::Discard => [0.0; 4],
            };
            let clear_depth = match attachments.depth.load {
                LoadOp::Clear(value) => {
                    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                        return Err(GraphError::new(
                            GraphErrorKind::InvalidInput,
                            "invalid attachment clear value",
                        ));
                    }
                    value
                }
                LoadOp::Load | LoadOp::Discard => 0.0,
            };
            let color_ops = attachments.color.ops();
            let depth_ops = attachments.depth.ops();
            Ok((
                Operation::Raster {
                    vertices,
                    indices,
                    index_count: draw.index_count,
                    color,
                    depth,
                    color_ops,
                    depth_ops,
                    clear_color,
                    clear_depth,
                    parameters: draw.parameters,
                },
                vec![
                    read(vertices, 0..vs, AccessPurpose::Vertex),
                    read(indices, 0..ix, AccessPurpose::Index),
                    attachment(color, color_ops, AccessPurpose::ColorAttachment),
                    attachment(depth, depth_ops, AccessPurpose::DepthAttachment),
                ],
                false,
            ))
        })
    }

    /// Engine-only opaque list: clear once and store both complete targets.
    /// Draw order is retained. External buffer dependencies precede the single
    /// rendering scope; this operation admits no arbitrary shader side effects.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn raster_batch(
        &mut self,
        name: impl Into<String>,
        draws: impl ExactSizeIterator<
            Item = Result<(BufferHandle, BufferHandle, u32, ValidatedRasterParameters), GraphError>,
        >,
        color: ImageHandle,
        depth: ImageHandle,
        clear_color: [f32; 4],
        clear_depth: f32,
    ) -> Result<PassHandle, GraphError> {
        if draws.len() == 0 {
            return self.clear_attachments(name, color, depth, clear_color, clear_depth);
        }
        self.build_pass(name, |graph| {
            let color = graph.image_id(color)?;
            let depth = graph.image_id(depth)?;
            graph.color(color)?;
            let c = graph.image_desc_at(color)?;
            let d = graph.image_desc_at(depth)?;
            if d.format != ImageFormat::Depth32Float || (c.width, c.height) != (d.width, d.height) {
                return Err(graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "raster batch needs matching RGBA8 color and D32 depth extents",
                    depth,
                ));
            }
            if !clear_depth.is_finite()
                || !(0.0..=1.0).contains(&clear_depth)
                || clear_color.iter().any(|x| !x.is_finite())
            {
                return Err(GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "invalid attachment clear value",
                ));
            }
            let mut encoded = Vec::with_capacity(draws.len());
            // Preserve both purposes if a buffer supplies vertices and indices.
            let mut buffers = BTreeMap::<usize, [u64; 2]>::new();
            for draw in draws {
                let (vertices, indices, index_count, parameters) = draw?;
                let (vertices, indices) = graph.raster_buffers(vertices, indices, index_count)?;
                let draw = RasterDrawOperands {
                    vertices,
                    indices,
                    index_count,
                    parameters: parameters.get(),
                };
                let vs = graph.resources[draw.vertices].desc.units();
                let ix = u64::from(draw.index_count) * 4;
                buffers.entry(draw.vertices).or_default()[0] = vs;
                let index_range = &mut buffers.entry(draw.indices).or_default()[1];
                *index_range = (*index_range).max(ix);
                encoded.push(draw);
            }
            let mut accesses = Vec::with_capacity(buffers.len() * 2 + 2);
            for (resource, [vertices, indices]) in buffers {
                if vertices != 0 {
                    accesses.push(read(resource, 0..vertices, AccessPurpose::Vertex));
                }
                if indices != 0 {
                    accesses.push(read(resource, 0..indices, AccessPurpose::Index));
                }
            }
            accesses.push(attachment(
                color,
                AttachmentOps::CLEAR_STORE,
                AccessPurpose::ColorAttachment,
            ));
            accesses.push(attachment(
                depth,
                AttachmentOps::CLEAR_STORE,
                AccessPurpose::DepthAttachment,
            ));
            Ok((
                Operation::RasterBatch {
                    draws: encoded.into(),
                    color,
                    depth,
                    clear_color,
                    clear_depth,
                },
                accesses,
                false,
            ))
        })
    }

    /// Initializes a complete color/depth target even when a view has no draws.
    pub fn clear_attachments(
        &mut self,
        name: impl Into<String>,
        color: ImageHandle,
        depth: ImageHandle,
        clear_color: [f32; 4],
        clear_depth: f32,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let color = graph.image_id(color)?;
            let depth = graph.image_id(depth)?;
            graph.color(color)?;
            let c = graph.image_desc_at(color)?;
            let d = graph.image_desc_at(depth)?;
            if d.format != ImageFormat::Depth32Float || (c.width, c.height) != (d.width, d.height) {
                return Err(graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "clear needs matching RGBA8 color and D32 depth extents",
                    depth,
                ));
            }
            if !clear_depth.is_finite()
                || !(0.0..=1.0).contains(&clear_depth)
                || clear_color.iter().any(|x| !x.is_finite())
            {
                return Err(GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "invalid attachment clear value",
                ));
            }
            Ok((
                Operation::ClearAttachments {
                    color,
                    depth,
                    clear_color,
                    clear_depth,
                },
                vec![
                    attachment(
                        color,
                        AttachmentOps::CLEAR_STORE,
                        AccessPurpose::ColorAttachment,
                    ),
                    attachment(
                        depth,
                        AttachmentOps::CLEAR_STORE,
                        AccessPurpose::DepthAttachment,
                    ),
                ],
                false,
            ))
        })
    }
    pub fn composite(
        &mut self,
        name: impl Into<String>,
        src: ImageHandle,
        dst: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            let dst = graph.image_id(dst)?;
            graph.compatible_color_pair(src, dst)?;
            Ok((
                Operation::Composite { src, dst },
                vec![
                    read(src, 0..1, AccessPurpose::Sampled),
                    write(dst, 0..1, AccessPurpose::ColorAttachment),
                ],
                false,
            ))
        })
    }
    /// Represent analysis-only work. A Vulkan runtime must explicitly reject
    /// live declaration-only operations rather than report successful execution.
    pub fn declare_pass(
        &mut self,
        name: impl Into<String>,
        kind: PassKind,
        accesses: Vec<Access>,
        side_effect: bool,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let mut encoded = Vec::with_capacity(accesses.len());
            for access in accesses {
                let id = match access.resource {
                    ResourceHandle::Buffer(h) => graph.buffer_id(h)?,
                    ResourceHandle::Image(h) => graph.image_id(h)?,
                };
                let range = match (&access.range, graph.resources[id].desc) {
                    (AccessRange::Buffer(r), ResourceDesc::Buffer(_)) => graph.buffer_range(
                        id,
                        r.start,
                        r.end
                            .checked_sub(r.start)
                            .ok_or_else(|| graph.resource_error(GraphErrorKind::InvalidInput, "reversed byte range", id)
                                .with_range(access.range.clone()))?,
                        false,
                    )?,
                    (
                        AccessRange::Image {
                            mip: 0,
                            layer: 0,
                            aspect,
                        },
                        ResourceDesc::Image(d),
                    ) if *aspect
                        == if d.format == ImageFormat::Rgba8Unorm {
                            ImageAspect::Color
                        } else {
                            ImageAspect::Depth
                        } =>
                    {
                        0..1
                    }
                    _ => {
                        return Err(graph.resource_error(GraphErrorKind::Unsupported,
                            "unsupported range: only complete mip 0/layer 0 and its matching aspect are supported",
                            id,
                        ).with_range(access.range.clone()));
                    }
                };
                validate_purpose(kind, graph.resources[id].desc, access.kind, access.purpose)
                    .map_err(|error| graph.resource_context(error, id).with_range(access.range.clone()))?;
                encoded.push(ResourceAccess {
                    resource: id,
                    range,
                    read: access.kind != AccessKind::Write,
                    write: access.kind != AccessKind::Read,
                    initializes: access.kind != AccessKind::Read,
                    discard_after: false,
                    purpose: access.purpose,
                });
            }
            // This public input boundary establishes pass-local disjointness.
            // Rechecking the immutable encoded accesses at submit is unnecessary.
            if let Some((left, _)) = conflicting_accesses(&encoded, |access| {
                (access.resource, access.range.clone(), access.write)
            }) {
                let access = &encoded[left];
                return Err(graph.resource_error(GraphErrorKind::InvalidInput,
                    "overlapping pass accesses must use one read_write declaration",
                    access.resource,
                ).with_range(graph.access_range(access.resource, access.range.clone())));
            }
            Ok((Operation::Declared(kind), encoded, side_effect))
        })
    }
    fn build_pass(
        &mut self,
        name: impl Into<String>,
        build: impl FnOnce(&Self) -> Result<(Operation, Vec<ResourceAccess>, bool), GraphError>,
    ) -> Result<PassHandle, GraphError> {
        let name = name.into();
        let (operation, accesses, side_effect) =
            build(self).map_err(|error| error.with_pass_name(&name))?;
        Ok(self.push(name, operation, accesses, side_effect))
    }

    fn raster_draw(
        &self,
        vertices: BufferHandle,
        indices: BufferHandle,
        index_count: u32,
        parameters: RasterParameters,
    ) -> Result<RasterDrawOperands, GraphError> {
        let (vertices, indices) = self.raster_buffers(vertices, indices, index_count)?;
        let parameters = ValidatedRasterParameters::new(parameters)?;
        Ok(RasterDrawOperands {
            vertices,
            indices,
            index_count,
            parameters: parameters.get(),
        })
    }

    fn raster_buffers(
        &self,
        vertices: BufferHandle,
        indices: BufferHandle,
        index_count: u32,
    ) -> Result<(usize, usize), GraphError> {
        let vertices = self.buffer_id(vertices)?;
        let indices = self.buffer_id(indices)?;
        let vs = self.resources[vertices].desc.units();
        let ix = u64::from(index_count) * 4;
        if vs < 28
            || !vs.is_multiple_of(28)
            || index_count == 0
            || !index_count.is_multiple_of(3)
            || ix > self.resources[indices].desc.units()
        {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "raster requires seven-f32 vertices and nonzero triangle u32 indices within the index buffer",
                if vs < 28 || !vs.is_multiple_of(28) { vertices } else { indices },
            ));
        }
        Ok((vertices, indices))
    }
}

fn read(resource: usize, range: Range<u64>, purpose: AccessPurpose) -> ResourceAccess {
    ResourceAccess {
        resource,
        range,
        read: true,
        write: false,
        initializes: false,
        discard_after: false,
        purpose,
    }
}
fn write(resource: usize, range: Range<u64>, purpose: AccessPurpose) -> ResourceAccess {
    ResourceAccess {
        resource,
        range,
        read: false,
        write: true,
        initializes: true,
        discard_after: false,
        purpose,
    }
}
fn attachment(resource: usize, ops: AttachmentOps, purpose: AccessPurpose) -> ResourceAccess {
    ResourceAccess {
        resource,
        range: 0..1,
        read: ops.load == LoadAction::Load,
        write: true,
        initializes: ops.load != LoadAction::Discard,
        discard_after: ops.store == StoreOp::Discard,
        purpose,
    }
}
fn validate_purpose(
    kind: PassKind,
    desc: ResourceDesc,
    access: AccessKind,
    purpose: AccessPurpose,
) -> Result<(), GraphError> {
    use AccessPurpose::*;
    let mode = match purpose {
        CopySource | StorageRead | Sampled | Vertex | Index => access == AccessKind::Read,
        CopyDestination | StorageWrite => access == AccessKind::Write,
        StorageReadWrite => access == AccessKind::ReadWrite,
        ColorAttachment | DepthAttachment => access != AccessKind::Read,
    };
    let domain = match purpose {
        CopySource | CopyDestination => kind == PassKind::Copy,
        StorageRead | StorageWrite | StorageReadWrite => kind == PassKind::Compute,
        Sampled => {
            kind == PassKind::Raster
                && matches!(
                    desc,
                    ResourceDesc::Image(ImageDesc {
                        format: ImageFormat::Rgba8Unorm,
                        ..
                    })
                )
        }
        Vertex | Index => kind == PassKind::Raster && matches!(desc, ResourceDesc::Buffer(_)),
        ColorAttachment => {
            kind == PassKind::Raster
                && matches!(
                    desc,
                    ResourceDesc::Image(ImageDesc {
                        format: ImageFormat::Rgba8Unorm,
                        ..
                    })
                )
        }
        DepthAttachment => {
            kind == PassKind::Raster
                && matches!(
                    desc,
                    ResourceDesc::Image(ImageDesc {
                        format: ImageFormat::Depth32Float,
                        ..
                    })
                )
        }
    };
    if !mode || !domain {
        return Err(GraphError::new(
            GraphErrorKind::InvalidInput,
            "access purpose, kind, resource type and pass kind are incompatible",
        ));
    }
    Ok(())
}
