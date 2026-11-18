//! Transactional construction of copy, fixed shader and declaration-only passes.
//!
//! Each builder validates raw inputs and appends only after all operands succeed.

use crate::ImageFormat;
use std::{ops::Range, sync::Arc};

use super::accesses::conflicting_accesses_in;
use super::{
    Access, AccessKind, AccessPurpose, AccessRange, BufferHandle, Graph, GraphError,
    GraphErrorKind, ImageDesc, ImageHandle, MeshUpload, Operation, PassHandle, PassKind,
    ResourceAccess, ResourceDesc, ResourceHandle, SampleCount, UploadSource, overlaps,
};

/// Roll back unpublished builder storage on a returned error or during an
/// unwind from a caller-provided iterator or recording callback.
struct PassTransaction<'a> {
    graph: &'a mut Graph,
    accesses: usize,
    draws: usize,
    raster_batches: usize,
    raster_constant_words: usize,
    uploads: usize,
    names: usize,
    bindings: usize,
    attachments: usize,
    committed: bool,
}

impl<'a> PassTransaction<'a> {
    fn new(graph: &'a mut Graph) -> Self {
        Self {
            accesses: graph.accesses.len(),
            draws: graph.draws.len(),
            raster_batches: graph.raster_batches.len(),
            raster_constant_words: graph.raster_constant_words.len(),
            uploads: graph.uploads.len(),
            names: graph.names.len(),
            bindings: graph.bindings.len(),
            attachments: graph.attachments.len(),
            graph,
            committed: false,
        }
    }
}

impl Drop for PassTransaction<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let data = self.graph.data_mut();
        data.accesses.truncate(self.accesses);
        data.draws.truncate(self.draws);
        data.raster_batches.truncate(self.raster_batches);
        data.raster_constant_words
            .truncate(self.raster_constant_words);
        data.uploads.truncate(self.uploads);
        data.names.truncate(self.names);
        data.bindings.truncate(self.bindings);
        data.attachments.truncate(self.attachments);
    }
}

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

    /// Immutable geometry bridges supply an owned stream and its private proof;
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
                "immutable geometry upload must preserve stream size and aligned byte positions",
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
            let desc = graph.image_desc_at(dst)?;
            if desc.array_layers != 1 || desc.sample_count != SampleCount::X1 {
                return Err(graph.resource_error(
                    GraphErrorKind::Unsupported,
                    "image upload requires one array layer and a single sample",
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
                    (AccessRange::Buffer(range), ResourceDesc::Buffer(_)) => {
                        let size = range.end.checked_sub(range.start).ok_or_else(|| {
                            graph
                                .resource_error(
                                    GraphErrorKind::InvalidInput,
                                    "reversed byte range",
                                    id,
                                )
                                .with_range(access.range.clone())
                        })?;
                        graph.buffer_range(id, range.start, size, false)?
                    }
                    (
                        AccessRange::Image {
                            mip,
                            layer,
                            aspect,
                        },
                        ResourceDesc::Image(desc),
                    ) if *mip < desc.mip_levels
                        && *layer < desc.array_layers
                        && *aspect == desc.format.aspect() =>
                    {
                        let unit = u64::from(*layer) * u64::from(desc.mip_levels)
                            + u64::from(*mip);
                        unit..unit + 1
                    }
                    _ => {
                        return Err(graph
                            .resource_error(
                                GraphErrorKind::Unsupported,
                                "image subresource must fit its mip/layer counts and match its format aspect",
                                id,
                            )
                            .with_range(access.range.clone()));
                    }
                };
                validate_purpose(kind, graph.resources[id].desc, access.kind, access.purpose)
                    .map_err(|error| {
                        graph
                            .resource_context(error, id)
                            .with_range(access.range.clone())
                    })?;
                let stages = match kind {
                    PassKind::Compute => crate::program::ShaderStages::COMPUTE,
                    PassKind::Raster => crate::program::ShaderStages::VERTEX
                        .union(crate::program::ShaderStages::FRAGMENT),
                    PassKind::Copy => crate::program::ShaderStages::EMPTY,
                };
                graph.append_access(ResourceAccess {
                    stages,
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
            let conflict = {
                let data = graph.data_mut();
                conflicting_accesses_in(
                    &data.accesses[encoded.clone()],
                    |access| (access.resource, access.range.clone(), access.write),
                    &mut data.overlap,
                )
                .map_err(|_| {
                    super::storage::capacity_error(
                        super::GraphCapacityKind::Accesses,
                        data.capacity.accesses,
                        Some(encoded.len()),
                    )
                })?
            };
            if let Some((left, _)) = conflict {
                let access = &graph.accesses[encoded.start + left];
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "overlapping pass accesses must use one read_write declaration",
                        access.resource,
                    )
                    .with_range(graph.access_range(access.resource, access.range.clone())));
            }
            Ok((Operation::Declared(kind), encoded, side_effect))
        })
    }
    pub(super) fn build_pass(
        &mut self,
        name: &str,
        build: impl FnOnce(&mut Self) -> Result<(Operation, Range<usize>, bool), GraphError>,
    ) -> Result<PassHandle, GraphError> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(tracy_client::span_location!("renderer.graph.build_pass"), 0)
        });
        let mut transaction = PassTransaction::new(self);
        let result = build(transaction.graph).and_then(|(operation, accesses, side_effect)| {
            #[cfg(feature = "tracy")]
            let _span = tracy_client::Client::running().map(|client| {
                client.span(
                    tracy_client::span_location!("renderer.graph.compact_reads"),
                    0,
                )
            });
            // All access-sensitive validation has completed. Compact exact
            // readers only now, preserving binding-slot error attribution and
            // every write, purpose, stage and original first occurrence.
            let data = transaction.graph.data_mut();
            data.read_accesses.clear();
            let mut end = accesses.start;
            for index in accesses.clone() {
                let access = data.accesses[index].clone();
                if access.read
                    && !access.write
                    && !data.read_accesses.insert(&access, end, &data.accesses)
                {
                    continue;
                }
                data.accesses[end] = access;
                end += 1;
            }
            data.accesses.truncate(end);
            transaction
                .graph
                .push(name, operation, accesses.start..end, side_effect)
        });
        transaction.committed = result.is_ok();
        result.map_err(|error| error.with_pass_name(name))
    }
}

pub(super) fn read(resource: usize, range: Range<u64>, purpose: AccessPurpose) -> ResourceAccess {
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
        CopySource | Uniform | StorageRead | Sampled | Vertex | Index | Indirect => {
            access == AccessKind::Read
        }
        CopyDestination | StorageWrite | ColorResolve => access == AccessKind::Write,
        StorageReadWrite => access == AccessKind::ReadWrite,
        ColorAttachment | DepthAttachment => access != AccessKind::Read,
    };
    let domain = match purpose {
        ColorResolve | Indirect => false,
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
    let multisampled = matches!(
        desc,
        ResourceDesc::Image(ImageDesc {
            sample_count: SampleCount::X4,
            ..
        })
    );
    if !mode || !domain || (multisampled && !matches!(purpose, ColorAttachment | DepthAttachment)) {
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
        RasterAttachments, RasterDraw, RasterParameters, StoreOp,
    };
    use crate::program::ManagedRasterProgram;

    fn uploaded_draw(graph: &mut Graph, name: &str) -> RasterDraw {
        let vertices = graph.buffer(name, BufferDesc { size: 84 }).unwrap();
        let indices = graph.buffer(name, BufferDesc { size: 12 }).unwrap();
        graph.upload_buffer(name, vertices, 0, &[0; 84]).unwrap();
        graph.upload_buffer(name, indices, 0, &[0; 12]).unwrap();
        RasterDraw {
            bindings: None,
            vertices,
            indices,
            first_index: 0,
            index_count: 3,
            instance_count: 1,
            parameters: RasterParameters::default().into(),
        }
    }

    #[test]
    fn indexed_instances_capture_exact_ranges_and_preserve_unread_gaps() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let default = uploaded_draw(&mut graph, "original geometry");
        let indices = graph
            .buffer("ranged indices", BufferDesc { size: 60 })
            .unwrap();
        graph
            .upload_buffer("first triangle", indices, 0, &[0; 12])
            .unwrap();
        graph
            .upload_buffer("later triangles", indices, 24, &[0; 20])
            .unwrap();
        let target = graph.image("target", ImageDesc::rgba8(2, 2)).unwrap();
        let colors = [Attachment {
            view: graph.image_view(target, 0, 0).unwrap(),
            load: LoadOp::Clear([0.0; 4]),
            store: StoreOp::Store,
        }];
        let attachments = RasterAttachments {
            colors: &colors,
            depth: None,
            color_resolves: &[],
        };
        let program = ManagedRasterProgram::mesh();
        let baseline = RasterDraw { indices, ..default };
        let later = RasterDraw {
            first_index: 6,
            instance_count: 2,
            ..baseline
        };
        let overlapping = RasterDraw {
            first_index: 8,
            instance_count: 4,
            ..later
        };
        for invalid in [
            RasterDraw {
                first_index: u32::MAX,
                ..later
            },
            RasterDraw {
                instance_count: 0,
                ..later
            },
        ] {
            assert_eq!(
                graph
                    .raster("invalid range or count", attachments, |raster| {
                        raster.draw(&program, [later, invalid])
                    },)
                    .unwrap_err()
                    .kind(),
                GraphErrorKind::InvalidInput,
            );
        }
        let pass = graph
            .raster("draw the initialized ranges", attachments, |raster| {
                raster.draw(&program, [overlapping, baseline, later])
            })
            .unwrap();
        graph.export_image(target).unwrap();
        let plan = graph.compile().unwrap();
        assert!(
            plan.accesses(pass.index)
                .iter()
                .filter(|access| access.purpose == AccessPurpose::Index)
                .map(|access| access.range.clone())
                .eq([0..12, 24..44])
        );
        let Operation::Raster { draws, .. } = &plan.passes[pass.index].operation else {
            panic!("expected Raster operation");
        };
        assert!(
            plan.raster_draws(draws)
                .iter()
                .map(|draw| (draw.first_index, draw.instance_count))
                .eq([(8, 4), (0, 1), (6, 2)])
        );
    }

    #[test]
    fn ranged_index_draw_cannot_consume_a_hole_between_uploads() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let draw = uploaded_draw(&mut graph, "geometry");
        let indices = graph
            .buffer("partial indices", BufferDesc { size: 24 })
            .unwrap();
        graph
            .upload_buffer("partial indices", indices, 16, &[0; 8])
            .unwrap();
        let target = graph.image("target", ImageDesc::rgba8(2, 2)).unwrap();
        graph
            .raster(
                "reads a missing index",
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(target, 0, 0).unwrap(),
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    depth: None,
                    color_resolves: &[],
                },
                |raster| {
                    raster.draw(
                        &ManagedRasterProgram::mesh(),
                        [RasterDraw {
                            indices,
                            first_index: 3,
                            instance_count: 2,
                            ..draw
                        }],
                    )
                },
            )
            .unwrap();
        graph.export_image(target).unwrap();
        let error = graph.compile().unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
        assert_eq!(error.resource(), Some(indices.into()));
    }

    fn textured_bindings(graph: &mut Graph) -> (super::super::RasterBindings, ImageHandle) {
        use crate::graph::{Binding, BindingResource};
        use crate::program::SamplerDescriptor;
        let uniform = graph.buffer("uniform", BufferDesc { size: 16 }).unwrap();
        graph
            .upload_buffer("uniform contents", uniform, 0, &[0; 16])
            .unwrap();
        let texture = graph.image("texture", ImageDesc::rgba8(2, 2)).unwrap();
        let view = graph.image_view(texture, 0, 0).unwrap();
        let bindings = [
            Binding {
                group: 0,
                binding: 0,
                resource: BindingResource::Buffer {
                    buffer: uniform,
                    offset: 0,
                    size: 16,
                },
            },
            Binding {
                group: 1,
                binding: 0,
                resource: BindingResource::ImageView(view),
            },
            Binding {
                group: 1,
                binding: 1,
                resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
            },
        ];
        (
            graph
                .raster_bindings(&ManagedRasterProgram::textured_mesh(), &bindings)
                .unwrap(),
            texture,
        )
    }

    #[test]
    fn shared_set_preserves_draws_after_failed_pass_without_per_draw_operand_capacity() {
        let mut pool = GraphPool::new(
            GraphCapacity {
                bindings: 3,
                draws: 24,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let mut mesh = uploaded_draw(&mut graph, "geometry");
        let (bindings, texture) = textured_bindings(&mut graph);
        graph
            .upload_image("texture contents", texture, &[0; 16])
            .unwrap();
        mesh.bindings = Some(bindings);
        let output = graph.image("output", ImageDesc::rgba8(2, 2)).unwrap();
        let colors = [Attachment {
            view: graph.image_view(output, 0, 0).unwrap(),
            load: LoadOp::Clear([0.0; 4]),
            store: StoreOp::Store,
        }];
        let invalid = RasterDraw {
            parameters: super::super::RasterConstants::default(),
            ..mesh
        };
        let before = (
            graph.accesses.len(),
            graph.draws.len(),
            graph.attachments.len(),
        );
        assert_eq!(
            graph
                .raster(
                    "failed after first draw",
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &colors,
                        depth: None
                    },
                    |raster| {
                        raster.draw(&ManagedRasterProgram::textured_mesh(), [mesh, invalid])
                    },
                )
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput
        );
        assert_eq!(
            (
                graph.accesses.len(),
                graph.draws.len(),
                graph.attachments.len()
            ),
            before
        );
        let draws = std::array::from_fn::<_, 24, _>(|index| RasterDraw {
            parameters: super::super::RasterConstants::new(&[index as u32; 20]).unwrap(),
            ..mesh
        });
        let pass = graph
            .raster(
                "shared descriptors",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &colors,
                    depth: None,
                },
                |raster| raster.draw(&ManagedRasterProgram::textured_mesh(), draws),
            )
            .unwrap();
        graph.readback_image("read output", output).unwrap();
        let plan = graph.compile().unwrap();
        let Operation::Raster {
            batches,
            draws: owned,
            ..
        } = &plan.passes[pass.index].operation
        else {
            panic!()
        };
        let [batch] = plan.raster_batches(batches) else {
            panic!("one Program batch")
        };
        assert_eq!(plan.bindings.len(), 3);
        for ((actual, words), expected) in plan
            .raster_draws(owned)
            .iter()
            .zip(plan.raster_constants(batch))
            .zip(draws)
        {
            assert_eq!(actual.bindings, 0..3);
            assert_eq!(words, expected.parameters.words());
            assert_eq!(actual.index_count, expected.index_count);
        }
        assert_eq!(
            plan.accesses(pass.index).len(),
            5,
            "one uniform, texture, vertex, index and attachment footprint"
        );
    }

    #[test]
    fn binding_template_capacity_failure_preserves_sets_and_allows_pass_retry() {
        let mut pool = GraphPool::new(
            GraphCapacity {
                bindings: 9,
                accesses: 13,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let mesh = uploaded_draw(&mut graph, "geometry");
        let sets = std::array::from_fn::<_, 3, _>(|_| {
            let (set, texture) = textured_bindings(&mut graph);
            graph
                .upload_image("texture contents", texture, &[0; 16])
                .unwrap();
            set
        });
        let output = graph.image("output", ImageDesc::rgba8(2, 2)).unwrap();
        let colors = [Attachment {
            view: graph.image_view(output, 0, 0).unwrap(),
            load: LoadOp::Clear([0.0; 4]),
            store: StoreOp::Store,
        }];
        let before = (
            graph.accesses.len(),
            graph.draws.len(),
            graph.attachments.len(),
        );
        let error = graph
            .raster(
                "too many binding reads",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &colors,
                    depth: None,
                },
                |raster| {
                    raster.draw(
                        &ManagedRasterProgram::textured_mesh(),
                        sets.map(|set| RasterDraw {
                            bindings: Some(set),
                            ..mesh
                        }),
                    )
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::CapacityExhausted);
        assert_eq!(
            error.capacity().unwrap().kind,
            super::super::GraphCapacityKind::Accesses
        );
        assert_eq!(error.capacity().unwrap().required, Some(14));
        assert_eq!(
            (
                graph.accesses.len(),
                graph.draws.len(),
                graph.attachments.len()
            ),
            before,
        );
        assert_eq!(graph.bindings.len(), 9);
        assert_eq!(graph.binding_accesses.len(), 6);
        let pass = graph
            .raster(
                "retry published set",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &colors,
                    depth: None,
                },
                |raster| {
                    raster.draw(
                        &ManagedRasterProgram::textured_mesh(),
                        [RasterDraw {
                            bindings: Some(sets[0]),
                            ..mesh
                        }; 2],
                    )
                },
            )
            .unwrap();
        graph.export_image(output).unwrap();
        let plan = graph.compile().unwrap();
        assert_eq!(plan.accesses(pass.index).len(), 5);
        assert_eq!(
            plan.pass_count(),
            5,
            "retry retains exactly its four input producers"
        );
    }

    #[test]
    fn reused_set_selects_each_pass_producer_and_does_not_keep_culled_reads() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let mut mesh = uploaded_draw(&mut graph, "geometry");
        // Creation precedes either texture writer: it must not select contents.
        let (bindings, texture) = textured_bindings(&mut graph);
        mesh.bindings = Some(bindings);
        graph
            .upload_image("old texture", texture, &[0; 16])
            .unwrap();
        let discarded = graph
            .image("culled output", ImageDesc::rgba8(2, 2))
            .unwrap();
        let output = graph.image("live output", ImageDesc::rgba8(2, 2)).unwrap();
        for (index, target) in [discarded, output].into_iter().enumerate() {
            if index == 1 {
                graph
                    .upload_image("new texture", texture, &[1; 16])
                    .unwrap();
            }
            graph
                .raster(
                    if index == 0 {
                        "culled draw"
                    } else {
                        "live draw"
                    },
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[Attachment {
                            view: graph.image_view(target, 0, 0).unwrap(),
                            load: LoadOp::Clear([0.0; 4]),
                            store: StoreOp::Store,
                        }],
                        depth: None,
                    },
                    |raster| raster.draw(&ManagedRasterProgram::textured_mesh(), [mesh]),
                )
                .unwrap();
        }
        graph.readback_image("result", output).unwrap();
        let plan = graph.compile().unwrap();
        let live = plan.ordered_pass_names().collect::<Vec<_>>();
        assert!(live.contains(&"new texture"));
        assert!(live.contains(&"live draw"));
        assert!(!live.contains(&"old texture"));
        assert!(!live.contains(&"culled draw"));
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
                            RasterAttachments {
                                color_resolves: &[],
                                colors: &colors,
                                depth: load.map(|load| Attachment {
                                    view: depth_view,
                                    load,
                                    store: StoreOp::Store,
                                }),
                            },
                            |raster| raster.draw(&program, draws),
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
                        color_resolves: &[],
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
                        RasterAttachments {
                            color_resolves: &[],
                            colors: &colors,
                            depth: Some(Attachment {
                                view: depth_view,
                                load: LoadOp::Load,
                                store: StoreOp::Store,
                            }),
                        },
                        |raster| raster.draw(&program, draws),
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
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &colors,
                        depth: Some(Attachment {
                            view: depth,
                            load: LoadOp::Clear(0.75),
                            store: StoreOp::Store,
                        }),
                    },
                    |raster| raster.draw(&ManagedRasterProgram::mesh(), draws),
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
            batches,
            draws,
            colors,
            depth: depth_index,
        } = &plan.passes[pass.index].operation
        else {
            panic!("owned Raster operation")
        };
        let [batch] = plan.raster_batches(batches) else {
            panic!("one Program batch")
        };
        assert!(batch.program.same(&ManagedRasterProgram::mesh()));
        assert_eq!(&batch.draws, draws);
        let owned = plan.raster_draws(draws);
        assert_eq!(owned.len(), 2);
        for ((actual, words), expected) in owned
            .iter()
            .zip(plan.raster_constants(batch))
            .zip([first, second])
        {
            assert_eq!(actual.vertices, expected.vertices.index);
            assert_eq!(actual.indices, expected.indices.index);
            assert_eq!(actual.index_count, expected.index_count);
            assert_eq!(words, expected.parameters.words());
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
    fn one_raster_scope_preserves_program_switches_and_shares_attachments() {
        let mut pool = GraphPool::new(
            GraphCapacity {
                draws: 3,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let mesh = uploaded_draw(&mut graph, "geometry");
        let (bindings, texture) = textured_bindings(&mut graph);
        graph
            .upload_image("texture contents", texture, &[255; 16])
            .unwrap();
        let textured = RasterDraw {
            bindings: Some(bindings),
            ..mesh
        };
        let output = graph.image("output", ImageDesc::rgba8(2, 2)).unwrap();
        let target = graph.image_view(output, 0, 0).unwrap();
        let pass = graph
            .raster(
                "ordered graphics",
                RasterAttachments {
                    colors: &[Attachment {
                        view: target,
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    color_resolves: &[],
                    depth: None,
                },
                |raster| {
                    raster.draw(&ManagedRasterProgram::mesh(), [mesh])?;
                    raster.draw(&ManagedRasterProgram::textured_mesh(), [textured])?;
                    raster.draw(&ManagedRasterProgram::mesh(), [mesh])
                },
            )
            .unwrap();
        graph.readback_image("output pixels", output).unwrap();
        let plan = graph.compile().unwrap();
        let Operation::Raster {
            batches,
            draws,
            colors,
            depth: None,
        } = &plan.passes[pass.index].operation
        else {
            panic!("combined Raster operation");
        };
        let [first, second, third] = plan.raster_batches(batches) else {
            panic!("three ordered batches");
        };
        assert!(first.program.same(&ManagedRasterProgram::mesh()));
        assert!(second.program.same(&ManagedRasterProgram::textured_mesh()));
        assert!(third.program.same(&first.program));
        assert_eq!(first.draws.start, draws.start);
        assert_eq!(first.draws.end, second.draws.start);
        assert_eq!(second.draws.end, third.draws.start);
        assert_eq!(third.draws.end, draws.end);
        assert_eq!(colors.len(), 1);
        let accesses = plan.accesses(pass.index);
        assert_eq!(
            accesses
                .iter()
                .filter(|access| access.purpose == AccessPurpose::ColorAttachment)
                .count(),
            1
        );
        assert!(
            accesses
                .iter()
                .any(|access| access.resource == texture.index
                    && access.purpose == AccessPurpose::Sampled)
        );
    }

    #[test]
    fn callbacks_cannot_publish_failed_draws_and_unwind_restores_capacity() {
        let mut pool = GraphPool::new(
            GraphCapacity {
                draws: 3,
                raster_constant_words: Some(40),
                attachments: 1,
                ..GraphCapacity::default()
            },
            1,
        )
        .unwrap();
        let mut graph = pool.begin().unwrap();
        let mesh = uploaded_draw(&mut graph, "geometry");
        let output = graph.image("output", ImageDesc::rgba8(2, 2)).unwrap();
        let attachment = Attachment {
            view: graph.image_view(output, 0, 0).unwrap(),
            load: LoadOp::Clear([0.0; 4]),
            store: StoreOp::Store,
        };
        let attachments = RasterAttachments {
            colors: &[attachment],
            color_resolves: &[],
            depth: None,
        };
        let program = ManagedRasterProgram::mesh();
        let error = graph
            .raster("constant budget exceeded", attachments, |raster| {
                raster.draw(&program, [mesh])?;
                assert!(raster.draw(&program, [mesh, mesh]).is_err());
                Ok(())
            })
            .unwrap_err();
        assert_eq!(
            error.capacity(),
            Some(crate::graph::GraphCapacityError {
                kind: crate::graph::GraphCapacityKind::RasterConstantWords,
                limit: 40,
                required: Some(60),
            })
        );
        assert!(graph.raster_constant_words.is_empty());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = graph.raster("unwound", attachments, |raster| {
                    raster.draw(&program, [mesh])?;
                    panic!("application callback panicked");
                });
            }))
            .is_err()
        );
        assert!(graph.raster_constant_words.is_empty());
        graph
            .raster("retry", attachments, |raster| {
                raster.draw(&program, [mesh])?;
                raster.draw(&program, [mesh])
            })
            .unwrap();
        graph.readback_image("output pixels", output).unwrap();
        let plan = graph.compile().unwrap();
        assert!(
            plan.ordered_pass_names()
                .eq(["geometry", "geometry", "retry", "output pixels"])
        );
        assert_eq!(plan.raster_constant_words.len(), 40);
        drop(plan);
        let reused = pool.begin().unwrap();
        assert!(reused.raster_constant_words.is_empty());
        assert_eq!(reused.raster_constant_word_limit, 40);
    }

    #[test]
    fn shader_read_conflicts_with_resolve_output_from_the_same_raster_scope() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let mesh = uploaded_draw(&mut graph, "geometry");
        let (bindings, resolve) = textured_bindings(&mut graph);
        let color = graph
            .image(
                "MSAA color",
                ImageDesc::rgba8(2, 2).with_sample_count(SampleCount::X4),
            )
            .unwrap();
        let target = graph.image_view(color, 0, 0).unwrap();
        let resolve_view = graph.image_view(resolve, 0, 0).unwrap();
        let error = graph
            .raster(
                "resolve with feedback",
                RasterAttachments {
                    colors: &[Attachment {
                        view: target,
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Discard,
                    }],
                    color_resolves: &[Some(resolve_view)],
                    depth: None,
                },
                |raster| {
                    raster.draw(&ManagedRasterProgram::mesh(), [mesh])?;
                    raster.draw(
                        &ManagedRasterProgram::textured_mesh(),
                        [RasterDraw {
                            bindings: Some(bindings),
                            ..mesh
                        }],
                    )
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.resource(), Some(resolve.into()));
    }

    #[test]
    fn late_invalid_draw_returns_all_owned_draw_capacity() {
        let mut pool = GraphPool::new(
            GraphCapacity {
                draws: 2,
                raster_constant_words: Some(40),
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
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: None,
                },
                |raster| raster.draw(&ManagedRasterProgram::mesh(), [draw, invalid]),
            )
            .unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.pass_name(), Some("resolution failed"));
        graph
            .raster(
                "retry",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[target],
                    depth: None,
                },
                |raster| raster.draw(&ManagedRasterProgram::mesh(), [draw, draw]),
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
            let first_set = graph
                .raster_bindings(&ManagedRasterProgram::textured_mesh(), &first)
                .unwrap();
            let second_set = graph
                .raster_bindings(&ManagedRasterProgram::textured_mesh(), &second)
                .unwrap();
            let draws = [
                RasterDraw {
                    bindings: Some(first_set),
                    ..mesh
                },
                RasterDraw {
                    bindings: Some(second_set),
                    ..mesh
                },
            ];
            let pass = graph
                .raster(
                    "materials",
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[Attachment {
                            view: output_view,
                            load: LoadOp::Clear([0.0; 4]),
                            store: StoreOp::Store,
                        }],
                        depth: None,
                    },
                    |raster| raster.draw(&ManagedRasterProgram::textured_mesh(), draws),
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
        assert_eq!(
            accesses
                .iter()
                .filter(|access| access.purpose == AccessPurpose::Uniform)
                .count(),
            1,
            "identical camera footprints shared across material sets are emitted once"
        );
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
