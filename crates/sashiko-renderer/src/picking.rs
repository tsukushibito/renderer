//! Depth-tested, two-sided selection geometry. The host supplies an opaque ID
//! per part and retains its mapping to live objects until it consumes the result.
use crate::{
    pipeline::{CameraView, PipelineResult, PreparedSetup, SceneInput, ViewFrame},
    shader_binaries::packaged_shaders,
};
use sashiko_renderer_core::{
    ImageFormat,
    graph::{
        Attachment, Graph, GraphCapacityKind, GraphError, ImageDesc, ImageHandle, ImageRegion,
        LoadOp, PassHandle, RasterAttachments, RasterConstants, RasterDraw, SampleCount, StoreOp,
    },
    program::{
        BlendState, CompareOp, DepthState, ManagedRasterProgram, RasterParameterLayout,
        RasterProgramDescriptor, ShaderStages, VertexAttribute, VertexFormat,
    },
    runtime::{ExecutionError, FrameCompletion},
};
use std::num::NonZeroU32;

const COLOR: &str = "pick IDs";
const DEPTH: &str = "pick depth";
const PASS: &str = "pick geometry";
const PIXEL: &str = "pick texel";
const COPY: &str = "copy picked texel";
const READ: &str = "read picked texel";
const SINGLE: ImageDesc = ImageDesc::rgba8(1, 1);

#[allow(unsafe_code)]
fn program(stride: u32) -> Result<ManagedRasterProgram, GraphError> {
    static PBR: RasterProgramDescriptor = RasterProgramDescriptor {
        entry: "main",
        vertex_stride: crate::passes::pbr::VERTEX_BYTES as u32,
        secondary_vertex_stride: 0,
        vertex_attributes: &[VertexAttribute {
            stream: 0,
            location: 0,
            offset: 0,
            format: VertexFormat::Float32x3,
        }],
        parameters: RasterParameterLayout {
            size: 116,
            stages: ShaderStages::VERTEX,
        },
        color_outputs: &[ImageFormat::Rgba8Unorm],
        bindings: &[],
        binding_bytes: &[],
        sampling_pairs: &[],
        blend: BlendState::Replace,
        depth: DepthState::Required {
            compare: CompareOp::Less,
            write: true,
            require_load: false,
        },
        shaders: packaged_shaders!(
            raster: ("pick.vert", "pick.frag"),
            inline_constants: Some(sashiko_renderer_core::program::D3D12InlineConstantBinding { register: 7, space: 0 }),
            nonfiltering: &[],
        ),
    };
    static UNLIT: RasterProgramDescriptor = RasterProgramDescriptor {
        vertex_stride: 28,
        ..PBR
    };
    let descriptor = if stride == PBR.vertex_stride {
        &PBR
    } else if stride == UNLIT.vertex_stride {
        &UNLIT
    } else {
        return Err(GraphError::invalid_input(
            "picking supports PBR and vertex-color geometry streams",
        ));
    };
    // SAFETY: the packaged vertex shader reads one float3 and 116 bytes of vertex
    // constants at b7. The fragment shader replaces one RGBA8 output with the
    // four normalized ID bytes. Neither stage accesses descriptors or storage.
    Ok(unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) })
}

/// Reusable storage for drawing solid selection proxies, including skinned parts.
/// Supply the geometry that should participate for this view, including occluders.
pub struct PickPass {
    extent: [u32; 2],
    draws: Vec<(u32, RasterDraw)>,
}

impl PickPass {
    pub fn prepare(
        plan: &mut PreparedSetup<'_>,
        extent: [u32; 2],
        max_parts: usize,
        pixel_queries: usize,
    ) -> PipelineResult<Self> {
        let [w, h] = extent;
        if extent.contains(&0) {
            return Err("picking requires a nonempty viewport".into());
        }
        let prep = plan.preparation_mut();
        crate::mesh::Meshes::reserve_imports(prep, max_parts)?;
        prep.reserve_raster_pass(PASS, SampleCount::X1, |pass| {
            pass.transient_attachment(COLOR, ImageDesc::rgba8(w, h))?;
            pass.transient_attachment(DEPTH, ImageDesc::depth32(w, h))?;
            // Either standard vertex layout can occupy every draw slot.
            for stride in [crate::passes::pbr::VERTEX_BYTES as u32, 28] {
                pass.draws(&program(stride)?, max_parts)?;
            }
            Ok(())
        })?;
        for _ in 0..pixel_queries {
            plan.preparation_mut()
                .reserve_transient_image(PIXEL, SINGLE)?;
            plan.preparation_mut().reserve_copy(COPY)?;
            plan.reserve_readback(READ, SINGLE)?;
        }
        let mut draws = Vec::new();
        draws.try_reserve_exact(max_parts)?;
        Ok(Self { extent, draws })
    }

    /// Zero is reserved for the background and unselectable solid occluders.
    pub fn record(
        &mut self,
        frame: &mut ViewFrame<'_>,
        camera: &CameraView,
        scene: SceneInput<'_>,
        ids: &[u32],
    ) -> Result<PickImage, GraphError> {
        if camera.extent != self.extent || ids.len() != scene.objects.len() {
            return Err(GraphError::invalid_input(
                "picking extent or part ID count differs from the input",
            ));
        }
        if ids.len() > self.draws.capacity() {
            return Err(GraphError::capacity_error(
                GraphCapacityKind::Draws,
                self.draws.capacity(),
                Some(ids.len()),
            ));
        }
        frame.with_geometry(scene, |graph, scene| {
            let mut words = [0; 29];
            for (dst, v) in words
                .iter_mut()
                .zip(camera.camera.clip_from_world().into_iter().flatten())
            {
                *dst = v.to_bits();
            }
            self.draws.clear();
            for (object, &id) in scene
                .objects
                .iter()
                .zip(ids)
                .filter(|(object, _)| object.camera_visible)
            {
                let stride = scene.meshes.position_stride(object.mesh)?;
                let mesh = scene.import_mesh(graph, object)?;
                let model = object.transform.columns();
                for row in 0..3 {
                    for col in 0..4 {
                        words[16 + 4 * row + col] = model[col][row].to_bits();
                    }
                }
                words[28] = id;
                self.draws.push((
                    stride,
                    RasterDraw {
                        bindings: None,
                        secondary_vertices: None,
                        vertices: mesh.vertices,
                        indices: mesh.indices,
                        first_index: mesh.first_index,
                        index_count: mesh.index_count,
                        instance_count: 1,
                        parameters: RasterConstants::new(&words)?,
                    },
                ));
            }
            let [w, h] = self.extent;
            let color = graph.image(COLOR, ImageDesc::rgba8(w, h))?;
            let depth = graph.image(DEPTH, ImageDesc::depth32(w, h))?;
            graph.raster(
                PASS,
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(color, 0, 0)?,
                        load: LoadOp::Clear([0.; 4]),
                        store: StoreOp::Store,
                    }],
                    color_resolves: &[],
                    depth: Some(Attachment {
                        view: graph.image_view(depth, 0, 0)?,
                        load: LoadOp::Clear(1.),
                        store: StoreOp::Discard,
                    }),
                },
                |pass| {
                    for (stride, draw) in &self.draws {
                        pass.draw(&program(*stride)?, [draw])?;
                    }
                    Ok(())
                },
            )?;
            Ok(PickImage(color))
        })
    }
}

#[derive(Clone, Copy)]
pub struct PickImage(ImageHandle);
impl PickImage {
    /// The unfiltered ID attachment can also be read by a host selection-outline pass.
    pub fn image(self) -> ImageHandle {
        self.0
    }

    /// Schedule a four-byte readback for a top-left-origin viewport coordinate.
    pub fn read_pixel(
        self,
        graph: &mut Graph,
        point: [u32; 2],
    ) -> Result<PickReadback, GraphError> {
        let pixel = graph.image(PIXEL, SINGLE)?;
        graph.copy_image_region(
            COPY,
            self.0,
            ImageRegion {
                mip_level: 0,
                origin: point,
                extent: [1, 1],
            },
            pixel,
            0,
            [0, 0],
        )?;
        Ok(PickReadback(graph.readback_image(READ, pixel)?))
    }
}

#[derive(Clone, Copy)]
pub struct PickReadback(PassHandle);
impl PickReadback {
    pub fn read(
        self,
        completed: &mut FrameCompletion,
    ) -> Result<Option<NonZeroU32>, ExecutionError> {
        let mut bytes = [0; 4];
        completed.readback_into(self.0, &mut bytes)?;
        Ok(NonZeroU32::new(u32::from_le_bytes(bytes)))
    }
}

#[cfg(all(test, any(feature = "vulkan", all(windows, feature = "d3d12"))))]
mod tests;
