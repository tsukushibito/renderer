//! Transactional construction of copy, fixed shader and declaration-only passes.
//!
//! Each builder validates raw inputs and appends only after all operands succeed.

use crate::ImageFormat;
use std::{borrow::Borrow, ops::Range, sync::Arc};

use super::accesses::conflicting_accesses_in;
use super::{
    Access, AccessKind, AccessPurpose, AccessRange, BufferHandle, ComputeTransform, Graph,
    GraphError, GraphErrorKind, ImageDesc, ImageHandle, LoadOp, MeshUpload, Operation, PassHandle,
    PassKind, RasterAttachments, RasterDraw, RasterDrawOperands, ResourceAccess, ResourceDesc,
    ResourceHandle, UploadSource, overlaps,
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
    /// Upload every texel of a single-layer color image, including every mip.
    /// Bytes use tightly packed rows in the format's component order, with
    /// consecutive mip levels from zero to the smallest selected level.
    pub fn upload_image(
        &mut self,
        name: &str,
        dst: ImageHandle,
        bytes: &[u8],
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let dst = graph.image_id(dst)?;
            if graph.image_desc_at(dst)?.array_layers != 1 {
                return Err(graph.resource_error(
                    GraphErrorKind::Unsupported,
                    "image upload requires one array layer",
                    dst,
                ));
            }
            if graph.image_desc_at(dst)?.format.aspect() != super::ImageAspect::Color {
                return Err(graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "image upload requires a color format",
                    dst,
                ));
            }
            if graph.image_desc_at(dst)?.byte_len() != Some(bytes.len() as u64) {
                return Err(graph.resource_error(
                    GraphErrorKind::InvalidInput,
                    "image upload must cover its exact extent",
                    dst,
                ));
            }
            let data = graph.append_upload(bytes)?;
            let mip_levels = u64::from(graph.image_desc_at(dst)?.mip_levels);
            Ok((
                Operation::UploadImage { dst, data },
                graph.append_accesses([write(
                    dst,
                    0..mip_levels,
                    AccessPurpose::CopyDestination,
                )])?,
                false,
            ))
        })
    }

    /// Copies a complete RGBA8 or RGBA16F image into a distinct image with the
    /// same format and extent, without changing its native component bits.
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
            graph.compatible_image_copy_pair(src, dst)?;
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
    /// Read every texel of a single-mip, single-layer image, including D32.
    /// Results use tightly packed rows in the image's native component format.
    pub fn readback_image(
        &mut self,
        name: &str,
        src: ImageHandle,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            graph.single_image(src)?;
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
    /// Own a draw sequence using an explicit trusted Raster Program. Inputs are
    /// u32 triangles in the Program's single per-vertex stream layout.
    /// Accepts borrowed slices or iterators of owned or borrowed draws. Constants
    /// and draws are captured into prepared storage in caller order. Iterator
    /// size hints are not trusted for capacity or empty-pass decisions.
    pub fn raster<'a, I, D>(
        &mut self,
        name: &str,
        program: &crate::program::ManagedRasterProgram,
        draws: I,
        attachments: RasterAttachments<'_>,
    ) -> Result<PassHandle, GraphError>
    where
        I: IntoIterator<Item = D>,
        D: Borrow<RasterDraw<'a>>,
    {
        if program.requires_depth_load()
            && !attachments
                .depth
                .is_some_and(|depth| matches!(depth.load, LoadOp::Load))
        {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "Raster Program requires a depth attachment with Load",
            )
            .with_pass_name(name));
        }
        (|| {
            if program.requires_depth() && attachments.depth.is_none() {
                return Err(GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "Raster Program requires a depth attachment",
                ));
            }
            if attachments.colors.len() != program.color_outputs().len() {
                return Err(GraphError::new(
                    GraphErrorKind::Unsupported,
                    "Raster attachment count differs from the Program output contract",
                ));
            }
            for (attachment, format) in attachments.colors.iter().zip(program.color_outputs()) {
                let resource = self.image_id(attachment.view.image)?;
                if self.image_desc_at(resource)?.format != *format {
                    return Err(self.resource_error(
                        GraphErrorKind::InvalidInput,
                        "Raster attachment format differs from the Program output contract",
                        resource,
                    ));
                }
            }
            Ok(())
        })()
        .map_err(|error: GraphError| error.with_pass_name(name))?;
        let mut draws = draws.into_iter().peekable();
        if draws.peek().is_none() {
            return self.attachment_pass(name, attachments);
        }
        self.build_pass(name, |graph| {
            let first_draw = graph.draws.len();
            let first_access = graph.accesses.len();
            graph.data_mut().clear_raster_ranges();
            for draw in draws {
                let draw = draw.borrow();
                let (vertices, indices, index_count, parameters, bindings) = (
                    draw.vertices,
                    draw.indices,
                    draw.index_count,
                    draw.parameters,
                    draw.bindings,
                );
                if graph.draws.len() == graph.capacity.draws {
                    return Err(super::storage::capacity_error(
                        super::GraphCapacityKind::Draws,
                        graph.capacity.draws,
                        graph.draws.len().checked_add(1),
                    ));
                }
                if parameters.words().len() * 4 != program.parameter_size() as usize {
                    return Err(GraphError::new(
                        GraphErrorKind::InvalidInput,
                        "Raster inline constants do not match the Program ABI",
                    ));
                }
                let bindings =
                    graph.append_bindings(program.bindings(), program.binding_bytes(), bindings)?;
                let (vertices, indices) = graph.raster_buffers(
                    vertices,
                    indices,
                    index_count,
                    u64::from(program.vertex_stride()),
                )?;
                let vs = graph.resources[vertices].desc.units();
                let ix = u64::from(index_count) * 4;
                let data = graph.data_mut();
                for (resource, purpose, size) in [(vertices, 0, vs), (indices, 1, ix)] {
                    if data.raster_ranges[resource] == [0; 2] {
                        // A validated resource is registered once, so this list
                        // fits the same prepared resource capacity as the ranges.
                        data.raster_resources.push(resource);
                    }
                    let range = &mut data.raster_ranges[resource][purpose];
                    *range = (*range).max(size);
                }
                data.draws.push(RasterDrawOperands {
                    bindings,
                    vertices,
                    indices,
                    index_count,
                    parameters,
                });
            }
            // Preserve resource-order accesses without scanning unused resources.
            graph.data_mut().raster_resources.sort_unstable();
            for index in 0..graph.raster_resources.len() {
                let resource = graph.raster_resources[index];
                let [vertices, indices] = graph.raster_ranges[resource];
                if vertices != 0 {
                    graph.append_access(read(resource, 0..vertices, AccessPurpose::Vertex))?;
                }
                if indices != 0 {
                    graph.append_access(read(resource, 0..indices, AccessPurpose::Index))?;
                }
            }
            let plan = graph.append_attachments(attachments)?;
            if !program.bindings().is_empty()
                && let Some(left) =
                    graph.conflicting_pass_access(first_access..graph.accesses.len())?
            {
                let access = &graph.accesses[first_access + left];
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "overlapping Raster shader and attachment footprints are unsupported",
                        access.resource,
                    )
                    .with_range(graph.access_range(access.resource, access.range.clone())));
            }
            Ok((
                Operation::Raster {
                    program: *program,
                    draws: first_draw..graph.draws.len(),
                    colors: plan.colors,
                    depth: plan.depth,
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
                    ) if *mip < d.mip_levels && *layer < d.array_layers && *aspect == d.format.aspect() =>
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
            stages: match kind {
                        PassKind::Compute => crate::program::ShaderStages::COMPUTE,
                        PassKind::Raster => crate::program::ShaderStages::VERTEX.union(crate::program::ShaderStages::FRAGMENT),
                        PassKind::Copy => crate::program::ShaderStages::EMPTY,
                    },
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
                "raster requires complete vertex strides and nonzero triangle u32 indices within the index buffer",
                if vs < stride || !vs.is_multiple_of(stride) { vertices } else { indices },
            ));
        }
        Ok((vertices, indices))
    }
}

fn read(resource: usize, range: Range<u64>, purpose: AccessPurpose) -> ResourceAccess {
    ResourceAccess {
        stages: purpose.default_stages(),
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
        stages: purpose.default_stages(),
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
                        format: ImageFormat::Rgba8Unorm | ImageFormat::Rgba16Float,
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
                        format: ImageFormat::Rgba8Unorm | ImageFormat::Rgba16Float,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{
        Attachment, BufferDesc, GraphCapacity, GraphPool, ImageAspect, LoadAction, LoadOp,
        RasterParameters, StoreOp,
    };
    use crate::program::ManagedRasterProgram;

    fn uploaded_draw(graph: &mut Graph, name: &str) -> RasterDraw<'static> {
        let vertices = graph.buffer(name, BufferDesc { size: 84 }).unwrap();
        let indices = graph.buffer(name, BufferDesc { size: 12 }).unwrap();
        graph.upload_buffer(name, vertices, 0, &[0; 84]).unwrap();
        graph.upload_buffer(name, indices, 0, &[0; 12]).unwrap();
        RasterDraw {
            bindings: &[],
            vertices,
            indices,
            index_count: 3,
            parameters: RasterParameters::default().into(),
        }
    }

    #[test]
    fn prepassed_raster_requires_loaded_depth_even_without_draws() {
        for program in [
            ManagedRasterProgram::mesh_prepassed(),
            ManagedRasterProgram::mesh_hdr_prepassed(),
        ] {
            let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
            let mut graph = pool.begin().unwrap();
            let draw = uploaded_draw(&mut graph, "mesh");
            let color = graph
                .image(
                    "color",
                    ImageDesc {
                        format: if program.color_outputs() == [crate::ImageFormat::Rgba16Float] {
                            ImageFormat::Rgba16Float
                        } else {
                            ImageFormat::Rgba8Unorm
                        },
                        ..ImageDesc::rgba8(4, 4)
                    },
                )
                .unwrap();
            let depth = graph.image("depth", ImageDesc::depth32(4, 4)).unwrap();
            let colors = [Attachment {
                view: graph.image_view(color, 0, 0).unwrap(),
                load: LoadOp::Clear([0.0; 4]),
                store: StoreOp::Store,
            }];
            let depth_view = graph.image_view(depth, 0, 0).unwrap();
            for draws in [&[][..], &[draw][..]] {
                for load in [None, Some(LoadOp::Clear(1.0)), Some(LoadOp::Discard)] {
                    let before = graph.passes.len();
                    let error = graph
                        .raster(
                            "invalid prepass continuation",
                            &program,
                            draws,
                            RasterAttachments {
                                colors: &colors,
                                depth: load.map(|load| Attachment {
                                    view: depth_view,
                                    load,
                                    store: StoreOp::Store,
                                }),
                            },
                        )
                        .unwrap_err();
                    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
                    assert_eq!(error.pass_name(), Some("invalid prepass continuation"));
                    assert_eq!(graph.passes.len(), before, "rejection appends no pass");
                }
            }
            graph
                .attachment_pass(
                    "initial depth",
                    RasterAttachments {
                        colors: &[],
                        depth: Some(Attachment {
                            view: depth_view,
                            load: LoadOp::Clear(1.0),
                            store: StoreOp::Store,
                        }),
                    },
                )
                .unwrap();
            for draws in [&[][..], &[draw][..]] {
                graph
                    .raster(
                        "valid prepass continuation",
                        &program,
                        draws,
                        RasterAttachments {
                            colors: &colors,
                            depth: Some(Attachment {
                                view: depth_view,
                                load: LoadOp::Load,
                                store: StoreOp::Store,
                            }),
                        },
                    )
                    .unwrap();
            }
            graph.readback_image("color result", color).unwrap();
            graph.compile().unwrap();
        }
    }

    #[test]
    fn frozen_raster_retains_program_draw_order_constants_and_attachment_views() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let mut first = uploaded_draw(&mut graph, "first");
        first.indices = first.vertices;
        first.parameters = RasterParameters::new(
            *RasterParameters::default().transform(),
            [0.25, 0.5, 0.75, 1.0],
        )
        .unwrap()
        .into();
        let mut second = uploaded_draw(&mut graph, "second");
        let mut transform = *RasterParameters::default().transform();
        transform[3][0] = 0.5;
        second.parameters = RasterParameters::new(transform, [1.0; 4]).unwrap().into();
        let image = graph
            .image(
                "array color",
                ImageDesc {
                    mip_levels: 2,
                    array_layers: 2,
                    ..ImageDesc::rgba8(4, 4)
                },
            )
            .unwrap();
        let color = graph.image_view(image, 1, 1).unwrap();
        let depth_image = graph.image("depth", ImageDesc::depth32(2, 2)).unwrap();
        let depth = graph.image_view(depth_image, 0, 0).unwrap();
        let pass = {
            let mut draws = [first, second];
            let mut colors = [Attachment {
                view: color,
                load: LoadOp::Clear([0.1, 0.2, 0.3, 1.0]),
                store: StoreOp::Store,
            }];
            let pass = graph
                .raster(
                    "owned",
                    &ManagedRasterProgram::mesh(),
                    draws,
                    RasterAttachments {
                        colors: &colors,
                        depth: Some(Attachment {
                            view: depth,
                            load: LoadOp::Clear(0.75),
                            store: StoreOp::Store,
                        }),
                    },
                )
                .unwrap();
            draws.swap(0, 1);
            draws[0].parameters =
                RasterParameters::new(*RasterParameters::default().transform(), [0.0; 4])
                    .unwrap()
                    .into();
            colors[0].load = LoadOp::Discard;
            std::hint::black_box((draws, colors));
            pass
        };
        graph
            .declare_pass(
                "observe",
                PassKind::Copy,
                &[Access {
                    resource: image.into(),
                    range: AccessRange::Image {
                        mip: 1,
                        layer: 1,
                        aspect: ImageAspect::Color,
                    },
                    kind: AccessKind::Read,
                    purpose: AccessPurpose::CopySource,
                }],
                true,
            )
            .unwrap();
        let plan = graph.compile().unwrap();
        let Operation::Raster {
            program,
            draws,
            colors,
            depth: depth_index,
        } = &plan.passes[pass.index].operation
        else {
            panic!("owned Raster operation")
        };
        assert!(program.same(&ManagedRasterProgram::mesh()));
        let owned = plan.raster_draws(draws);
        assert_eq!(owned.len(), 2);
        for (actual, expected) in owned.iter().zip([first, second]) {
            assert_eq!(actual.vertices, expected.vertices.index);
            assert_eq!(actual.indices, expected.indices.index);
            assert_eq!(actual.index_count, expected.index_count);
            assert_eq!(actual.parameters, expected.parameters);
        }
        assert_eq!(colors.len(), 1);
        let attachment = plan.attachments[colors.start];
        assert_eq!(attachment.view, color);
        assert_eq!(attachment.clear, [0.1, 0.2, 0.3, 1.0]);
        assert_eq!(attachment.ops.load, LoadAction::Clear);
        let owned_depth = plan.attachments[depth_index.unwrap()];
        assert_eq!(owned_depth.view, depth);
        assert_eq!(owned_depth.clear[0], 0.75);
        let accesses = plan.accesses(pass.index);
        assert!(
            accesses
                .iter()
                .any(|access| access.resource == first.vertices.index
                    && access.purpose == AccessPurpose::Vertex
                    && access.range == (0..84))
        );
        assert!(
            accesses
                .iter()
                .any(|access| access.resource == first.vertices.index
                    && access.purpose == AccessPurpose::Index
                    && access.range == (0..12))
        );
    }

    #[test]
    fn late_invalid_draw_returns_all_owned_draw_capacity() {
        let mut pool = GraphPool::new(
            GraphCapacity {
                draws: 2,
                attachments: 1,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let draw = uploaded_draw(&mut graph, "mesh");
        let image = graph.image("color", ImageDesc::rgba8(2, 2)).unwrap();
        let target = Attachment {
            view: graph.image_view(image, 0, 0).unwrap(),
            load: LoadOp::Clear([0.0; 4]),
            store: StoreOp::Store,
        };
        let invalid = RasterDraw {
            index_count: 0,
            ..draw
        };
        let error = graph
            .raster(
                "resolution failed",
                &ManagedRasterProgram::mesh(),
                [draw, invalid],
                RasterAttachments {
                    colors: &[target],
                    depth: None,
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.pass_name(), Some("resolution failed"));
        graph
            .raster(
                "retry",
                &ManagedRasterProgram::mesh(),
                [draw, draw],
                RasterAttachments {
                    colors: &[target],
                    depth: None,
                },
            )
            .unwrap();
        graph.readback_image("pixels", image).unwrap();
        assert_eq!(graph.compile().unwrap().pass_count(), 4);
    }
    #[test]
    fn owned_draw_descriptors_preserve_materials_views_samplers_and_shader_scopes() {
        use crate::graph::{Binding, BindingOperands, BindingResource};
        use crate::program::{ManagedProgram, SamplerDescriptor, ShaderStages};
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let mesh = uploaded_draw(&mut graph, "mesh");
        let uniform = graph.buffer("uniform", BufferDesc { size: 64 }).unwrap();
        graph
            .upload_buffer("uniform bytes", uniform, 16, &[0; 16])
            .unwrap();
        let a = graph.image("a", ImageDesc::rgba8(2, 2)).unwrap();
        let b = graph.image("b", ImageDesc::rgba8(2, 2)).unwrap();
        graph.upload_image("a pixels", a, &[0; 16]).unwrap();
        graph.upload_image("b pixels", b, &[0; 16]).unwrap();
        let a_view = graph.image_view(a, 0, 0).unwrap();
        let b_view = graph.image_view(b, 0, 0).unwrap();
        let output = graph.image("color", ImageDesc::rgba8(2, 2)).unwrap();
        let output_view = graph.image_view(output, 0, 0).unwrap();
        let pass = {
            let material = |view| {
                [
                    Binding {
                        group: 1,
                        binding: 1,
                        resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                    },
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: uniform,
                            offset: 16,
                            size: 48,
                        },
                    },
                    Binding {
                        group: 1,
                        binding: 0,
                        resource: BindingResource::ImageView(view),
                    },
                ]
            };
            let mut first = material(a_view);
            let second = material(b_view);
            let draws = [
                RasterDraw {
                    bindings: &first,
                    ..mesh
                },
                RasterDraw {
                    bindings: &second,
                    ..mesh
                },
            ];
            let pass = graph
                .raster(
                    "materials",
                    &ManagedRasterProgram::textured_mesh(),
                    draws,
                    RasterAttachments {
                        colors: &[Attachment {
                            view: output_view,
                            load: LoadOp::Clear([0.0; 4]),
                            store: StoreOp::Store,
                        }],
                        depth: None,
                    },
                )
                .unwrap();
            first.fill(second[0]);
            std::hint::black_box(first);
            pass
        };
        let result = graph.buffer("result", BufferDesc { size: 80 }).unwrap();
        let dispatch = graph
            .dispatch(
                "sample output",
                &ManagedProgram::sampled_float_readback(),
                &[
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::ImageView(output_view),
                    },
                    Binding {
                        group: 0,
                        binding: 1,
                        resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                    },
                    Binding {
                        group: 1,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: result,
                            offset: 16,
                            size: 64,
                        },
                    },
                ],
                [1; 3],
            )
            .unwrap();
        graph.readback_buffer("bytes", result, 16, 64).unwrap();
        let plan = graph.compile().unwrap();
        let Operation::Raster { draws, .. } = &plan.passes[pass.index].operation else {
            panic!()
        };
        let owned = &plan.draws[draws.clone()];
        assert_eq!(owned.len(), 2);
        assert_eq!(owned[0].bindings.len(), 3);
        assert_eq!(owned[0].bindings.end, owned[1].bindings.start);
        for (draw, view) in owned.iter().zip([a_view, b_view]) {
            let bindings = &plan.bindings[draw.bindings.clone()];
            assert!(
                matches!(bindings[0], BindingOperands::Buffer { resource, range: crate::program::BufferRange { offset: 16, size: 48 } } if resource == uniform.index)
            );
            assert!(matches!(bindings[1], BindingOperands::ImageView(actual) if actual == view));
            assert!(matches!(
                bindings[2],
                BindingOperands::Sampler(SamplerDescriptor::NearestClamp)
            ));
        }
        let accesses = plan.accesses(pass.index);
        for access in accesses
            .iter()
            .filter(|access| access.purpose == AccessPurpose::Uniform)
        {
            assert_eq!(access.stages, ShaderStages::VERTEX);
            assert_eq!(
                access.range,
                16..32,
                "descriptor tail is not a shader footprint"
            );
        }
        for access in accesses
            .iter()
            .filter(|access| access.purpose == AccessPurpose::Sampled)
        {
            assert_eq!(access.stages, ShaderStages::FRAGMENT);
        }
        assert!(
            plan.accesses(dispatch.index)
                .iter()
                .all(|access| access.stages == ShaderStages::COMPUTE)
        );
        assert_eq!(plan.accesses(dispatch.index)[1].range, 16..80);
    }
}
