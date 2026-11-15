//! Transactional construction of copy, fixed shader and declaration-only passes.
//!
//! Each builder validates raw inputs and appends only after all operands succeed.

use std::{ops::Range, sync::Arc};

use super::accesses::conflicting_accesses_in;
use super::{
    Access, AccessKind, AccessPurpose, AccessRange, BufferHandle, ComputeTransform, Graph,
    GraphError, GraphErrorKind, ImageAspect, ImageDesc, ImageFormat, ImageHandle, MeshUpload,
    Operation, PassHandle, PassKind, RasterAttachments, RasterDraw, RasterDrawOperands,
    ResourceAccess, ResourceDesc, ResourceHandle, UploadSource, ValidatedRasterParameters,
    overlaps,
};

impl Graph {
    pub fn upload_buffer(
        &mut self,
        name: &str,
        dst: BufferHandle,
        offset: u64,
        bytes: &[u8],
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let dst = graph.buffer_id(dst)?;
            let range = graph.buffer_range(dst, offset, bytes.len() as u64, true)?;
            let data = graph.append_upload(bytes)?;
            Ok((
                Operation::UploadBuffer {
                    dst,
                    offset,
                    data,
                    mesh: None,
                },
                graph.append_accesses([write(dst, range, AccessPurpose::CopyDestination)])?,
                false,
            ))
        })
    }

    /// Retain a slice of shared immutable input without allocating a chunk copy.
    /// The CPU source offset is independent of the aligned GPU destination offset.
    #[cfg(test)]
    pub(crate) fn upload_buffer_slice(
        &mut self,
        name: &str,
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
        name: &str,
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
        name: &str,
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
                Operation::UploadBuffer { dst, offset, data: UploadSource::Shared { bytes, source }, mesh },
                graph.append_accesses([write(dst, range, AccessPurpose::CopyDestination)])?,
                false,
            ))
        })
    }
    pub fn copy_buffer(
        &mut self,
        name: &str,
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
                graph.append_accesses([
                    read(src, source, AccessPurpose::CopySource),
                    write(dst, target, AccessPurpose::CopyDestination),
                ])?,
                false,
            ))
        })
    }
    pub fn readback_buffer(
        &mut self,
        name: &str,
        src: BufferHandle,
        offset: u64,
        size: u64,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.buffer_id(src)?;
            let range = graph.buffer_range(src, offset, size, true)?;
            Ok((
                Operation::ReadbackBuffer { src, offset, size },
                graph.append_accesses([read(src, range, AccessPurpose::CopySource)])?,
                true,
            ))
        })
    }
    pub fn upload_image(
        &mut self,
        name: &str,
        dst: ImageHandle,
        bytes: &[u8],
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let dst = graph.image_id(dst)?;
            graph.color(dst)?;
            if graph.image_desc_at(dst)?.byte_len() != Some(bytes.len() as u64) {
                return Err(graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "image upload must cover its exact extent",
                    dst,
                ));
            }
            let data = graph.append_upload(bytes)?;
            Ok((
                Operation::UploadImage { dst, data },
                graph.append_accesses([write(dst, 0..1, AccessPurpose::CopyDestination)])?,
                false,
            ))
        })
    }

    /// Copies a complete RGBA8 image into a distinct image of the same extent.
    /// The source contents are selected by declaration order and must be initialized.
    pub fn copy_image(
        &mut self,
        name: &str,
        src: ImageHandle,
        dst: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            let dst = graph.image_id(dst)?;
            graph.compatible_color_pair(src, dst)?;
            Ok((
                Operation::CopyImage { src, dst },
                graph.append_accesses([
                    read(src, 0..1, AccessPurpose::CopySource),
                    write(dst, 0..1, AccessPurpose::CopyDestination),
                ])?,
                false,
            ))
        })
    }
    pub fn readback_image(
        &mut self,
        name: &str,
        src: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            graph.color(src)?;
            Ok((
                Operation::ReadbackImage { src },
                graph.append_accesses([read(src, 0..1, AccessPurpose::CopySource)])?,
                true,
            ))
        })
    }
    pub fn compute(
        &mut self,
        name: &str,
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
                graph.append_accesses([
                    read(src, 0..1, AccessPurpose::StorageRead),
                    write(dst, 0..1, AccessPurpose::StorageWrite),
                ])?,
                false,
            ))
        })
    }
    /// Own a draw sequence using an explicit reviewed Raster Program. Current
    /// mesh inputs are u32 triangles and tightly packed xyz/rgba vertices.
    /// Constants and draws are copied into prepared storage in caller order.
    pub fn raster(
        &mut self,
        name: &str,
        program: &crate::program::ManagedRasterProgram,
        draws: &[RasterDraw],
        attachments: RasterAttachments<'_>,
    ) -> Result<PassHandle, GraphError> {
        self.raster_validated(
            name,
            program,
            draws.iter().map(|draw| {
                ValidatedRasterParameters::new(draw.parameters)
                    .map(|parameters| (draw.vertices, draw.indices, draw.index_count, parameters))
            }),
            attachments,
        )
    }

    /// The Engine already guarantees finite immutable parameters at snapshot
    /// creation. This adapter consumes that proof without scanning values again.
    pub(crate) fn raster_validated(
        &mut self,
        name: &str,
        program: &crate::program::ManagedRasterProgram,
        draws: impl ExactSizeIterator<
            Item = Result<(BufferHandle, BufferHandle, u32, ValidatedRasterParameters), GraphError>,
        >,
        attachments: RasterAttachments<'_>,
    ) -> Result<PassHandle, GraphError> {
        if draws.len() == 0 {
            return self.attachment_pass(name, attachments);
        }
        self.build_pass(name, |graph| {
            if attachments.colors.len() != program.color_outputs().len() {
                return Err(GraphError::new(
                    GraphErrorKind::Unsupported,
                    "Raster attachment count differs from the Program output contract",
                ));
            }
            let first_draw = graph.draws.len();
            if draws.len() > graph.capacity.draws.saturating_sub(first_draw) {
                return Err(super::storage::capacity_error(
                    super::GraphCapacityKind::Draws,
                    graph.capacity.draws,
                    first_draw.checked_add(draws.len()),
                ));
            }
            let first_access = graph.accesses.len();
            let resources = graph.resources.len();
            graph.data_mut().raster_ranges[..resources].fill([0; 2]);
            for draw in draws {
                let (vertices, indices, index_count, parameters) = draw?;
                let (vertices, indices) = graph.raster_buffers(
                    vertices,
                    indices,
                    index_count,
                    u64::from(program.vertex_stride()),
                )?;
                let vs = graph.resources[vertices].desc.units();
                let ix = u64::from(index_count) * 4;
                let data = graph.data_mut();
                data.raster_ranges[vertices][0] = vs;
                data.raster_ranges[indices][1] = data.raster_ranges[indices][1].max(ix);
                data.draws.push(RasterDrawOperands {
                    vertices,
                    indices,
                    index_count,
                    parameters: parameters.get(),
                });
            }
            for resource in 0..resources {
                let [vertices, indices] = graph.raster_ranges[resource];
                if vertices != 0 {
                    graph.append_access(read(resource, 0..vertices, AccessPurpose::Vertex))?;
                }
                if indices != 0 {
                    graph.append_access(read(resource, 0..indices, AccessPurpose::Index))?;
                }
            }
            let plan = graph.append_attachments(attachments)?;
            let targets = plan.targets.filter(|_| program.is_mesh());
            Ok((
                Operation::Raster {
                    program: *program,
                    draws: first_draw..graph.draws.len(),
                    colors: plan.colors,
                    depth: plan.depth,
                    targets,
                },
                first_access..graph.accesses.len(),
                false,
            ))
        })
    }

    pub fn composite(
        &mut self,
        name: &str,
        src: ImageHandle,
        dst: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            let dst = graph.image_id(dst)?;
            graph.compatible_color_pair(src, dst)?;
            Ok((
                Operation::Composite { src, dst },
                graph.append_accesses([
                    read(src, 0..1, AccessPurpose::Sampled),
                    write(dst, 0..1, AccessPurpose::ColorAttachment),
                ])?,
                false,
            ))
        })
    }
    /// Represent analysis-only work. A Vulkan runtime must explicitly reject
    /// live declaration-only operations rather than report successful execution.
    pub fn declare_pass(
        &mut self,
        name: &str,
        kind: PassKind,
        accesses: &[Access],
        side_effect: bool,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let first_access = graph.accesses.len();
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
                            mip,
                            layer,
                            aspect,
                        },
                        ResourceDesc::Image(d),
                    ) if *mip < d.mip_levels && *layer < d.array_layers && *aspect
                        == if d.format == ImageFormat::Rgba8Unorm {
                            ImageAspect::Color
                        } else {
                            ImageAspect::Depth
                        } =>
                    {
                        let unit = u64::from(*layer) * u64::from(d.mip_levels) + u64::from(*mip);
                        unit..unit + 1
                    }
                    _ => {
                        return Err(graph.resource_error(GraphErrorKind::Unsupported,
                            "image subresource must fit its mip/layer counts and match its format aspect",
                            id,
                        ).with_range(access.range.clone()));
                    }
                };
                validate_purpose(kind, graph.resources[id].desc, access.kind, access.purpose)
                    .map_err(|error| graph.resource_context(error, id).with_range(access.range.clone()))?;
                graph.append_access(ResourceAccess {
                    resource: id,
                    range,
                    read: access.kind != AccessKind::Write,
                    write: access.kind != AccessKind::Read,
                    initializes: access.kind != AccessKind::Read,
                    discard_after: false,
                    purpose: access.purpose,
                })?;
            }
            // This public input boundary establishes pass-local disjointness.
            // Rechecking the immutable encoded accesses at submit is unnecessary.
            let encoded = first_access..graph.accesses.len();
            let conflict = { let data=graph.data_mut(); conflicting_accesses_in(&data.accesses[encoded.clone()], |access| {
                (access.resource, access.range.clone(), access.write)
            }, &mut data.overlap).map_err(|_| super::storage::capacity_error(super::GraphCapacityKind::Accesses, data.capacity.accesses, Some(encoded.len())))? };
            if let Some((left, _)) = conflict {
                let access = &graph.accesses[encoded.start + left];
                return Err(graph.resource_error(GraphErrorKind::InvalidInput,
                    "overlapping pass accesses must use one read_write declaration",
                    access.resource,
                ).with_range(graph.access_range(access.resource, access.range.clone())));
            }
            Ok((Operation::Declared(kind), encoded, side_effect))
        })
    }
    pub(super) fn build_pass(
        &mut self,
        name: &str,
        build: impl FnOnce(&mut Self) -> Result<(Operation, Range<usize>, bool), GraphError>,
    ) -> Result<PassHandle, GraphError> {
        let checkpoint = (
            self.accesses.len(),
            self.draws.len(),
            self.uploads.len(),
            self.names.len(),
            self.bindings.len(),
            self.attachments.len(),
        );
        let result = build(self).and_then(|(operation, accesses, side_effect)| {
            self.push(name, operation, accesses, side_effect)
        });
        if result.is_err() {
            let data = self.data_mut();
            data.accesses.truncate(checkpoint.0);
            data.draws.truncate(checkpoint.1);
            data.uploads.truncate(checkpoint.2);
            data.names.truncate(checkpoint.3);
            data.bindings.truncate(checkpoint.4);
            data.attachments.truncate(checkpoint.5);
        }
        result.map_err(|error| error.with_pass_name(name))
    }
    fn raster_buffers(
        &self,
        vertices: BufferHandle,
        indices: BufferHandle,
        index_count: u32,
        stride: u64,
    ) -> Result<(usize, usize), GraphError> {
        let vertices = self.buffer_id(vertices)?;
        let indices = self.buffer_id(indices)?;
        let vs = self.resources[vertices].desc.units();
        let ix = u64::from(index_count) * 4;
        if vs < stride
            || !vs.is_multiple_of(stride)
            || index_count == 0
            || !index_count.is_multiple_of(3)
            || ix > self.resources[indices].desc.units()
        {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "raster requires seven-f32 vertices and nonzero triangle u32 indices within the index buffer",
                if vs < stride || !vs.is_multiple_of(stride) { vertices } else { indices },
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
fn validate_purpose(
    kind: PassKind,
    desc: ResourceDesc,
    access: AccessKind,
    purpose: AccessPurpose,
) -> Result<(), GraphError> {
    use AccessPurpose::*;
    let mode = match purpose {
        CopySource | Uniform | StorageRead | Sampled | Vertex | Index => access == AccessKind::Read,
        CopyDestination | StorageWrite => access == AccessKind::Write,
        StorageReadWrite => access == AccessKind::ReadWrite,
        ColorAttachment | DepthAttachment => access != AccessKind::Read,
    };
    let domain = match purpose {
        CopySource | CopyDestination => kind == PassKind::Copy,
        Uniform => kind == PassKind::Compute && matches!(desc, ResourceDesc::Buffer(_)),
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
