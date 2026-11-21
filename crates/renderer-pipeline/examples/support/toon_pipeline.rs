//! Application implementation using the public asset and Graph APIs.
use renderer::{
    capacity::{GraphRequirements, Preparation, WorkingSet},
    graph::{
        Attachment, Graph, GraphError, ImageDesc, LoadOp, RasterAttachments, RasterDraw,
        RasterParameters, StoreOp,
    },
    program::ManagedRasterProgram,
};
use renderer_pipeline::{
    material::{MaterialDefinition, MaterialInstance},
    pbr::SceneLimits,
    pipeline::{
        CameraView, PipelineResult, PreparedScene, RenderObject, SceneInput, ViewImages,
        ViewPipeline,
    },
};
use std::num::NonZeroUsize;

pub struct ToonPipeline {
    program: ManagedRasterProgram,
    draws: Vec<RasterDraw>,
    limit: usize,
}
pub fn prepare(
    _: [u32; 2],
    limits: SceneLimits,
    _: &[MaterialDefinition],
) -> PipelineResult<ToonPipeline> {
    Ok(ToonPipeline {
        program: super::views::toon::toon(false)?.forward().clone(),
        draws: Vec::with_capacity(limits.max_draws),
        limit: limits.max_draws,
    })
}

impl ViewPipeline for ToonPipeline {
    type State = ();
    type Update = ();

    fn reserve(
        &self,
        p: &mut Preparation,
        [w, h]: [u32; 2],
        views: NonZeroUsize,
    ) -> Result<(), GraphError> {
        p.reserve_image(ImageDesc::rgba16_float(w, h), views.get())?;
        p.reserve_image(ImageDesc::depth32(w, h), views.get())?;
        for _ in 0..views.get() {
            let draws = self.limit;
            p.add_requirements(
                GraphRequirements {
                    resources: 2 + 2 * draws,
                    passes: 1,
                    draws,
                    accesses: 2 + 2 * draws,
                    attachments: 2,
                    raster_constant_words: 20 * draws,
                    name_bytes: 64 + 32 * draws,
                    ..Default::default()
                },
                WorkingSet {
                    raster: &[self.program.clone().into()],
                    ..Default::default()
                },
            )?;
        }
        Ok(())
    }
    fn create_view(&self, _: &CameraView) -> PipelineResult<()> {
        Ok(())
    }
    fn check_object(&self, scene: SceneInput<'_>, object: &RenderObject) -> Result<(), GraphError> {
        if !matches!(
            scene.materials.get(object.material)?,
            MaterialInstance::Unlit(_)
        ) {
            return Err(GraphError::invalid_input(
                "ToonPipeline requires a vertex-color material",
            ));
        }
        scene.meshes.check_layout(object.mesh, &self.program)
    }
    fn record(
        &mut self,
        g: &mut Graph,
        _: &mut (),
        camera: &CameraView,
        scene: PreparedScene<'_, Self>,
    ) -> Result<(ViewImages, ()), GraphError> {
        let scene = scene.input();
        if scene.objects.len() > self.limit {
            return Err(GraphError::invalid_input("toon draw capacity exceeded"));
        }
        self.draws.clear();
        for obj in scene.objects {
            let mesh = scene.meshes.import(g, obj.mesh)?;
            let MaterialInstance::Unlit(mat) = scene.materials.get(obj.material)? else {
                unreachable!("validated material revision")
            };
            let model = obj.transform.columns();
            let vp = camera.camera.clip_from_world();
            let clip = std::array::from_fn(|c| {
                std::array::from_fn(|r| (0..4).map(|k| vp[k][r] * model[c][k]).sum())
            });
            let tint = std::array::from_fn(|i| obj.tint[i] * mat.color[i]);
            self.draws.push(RasterDraw {
                bindings: None,
                vertices: mesh.vertices,
                indices: mesh.indices,
                first_index: mesh.first_index,
                index_count: mesh.index_count,
                instance_count: 1,
                parameters: RasterParameters::new(clip, tint)?.into(),
            });
        }
        let [w, h] = camera.extent;
        let color = g.image("toon color", ImageDesc::rgba16_float(w, h))?;
        let depth = g.image("toon depth", ImageDesc::depth32(w, h))?;
        g.raster(
            "toon",
            RasterAttachments {
                colors: &[Attachment {
                    view: g.image_view(color, 0, 0)?,
                    load: LoadOp::Clear([0.; 4]),
                    store: StoreOp::Store,
                }],
                color_resolves: &[],
                depth: Some(Attachment {
                    view: g.image_view(depth, 0, 0)?,
                    load: LoadOp::Clear(1.),
                    store: StoreOp::Store,
                }),
            },
            |pass| {
                for &draw in &self.draws {
                    pass.draw(&self.program, [draw])?;
                }
                Ok(())
            },
        )?;
        Ok((
            ViewImages {
                color,
                depth: Some(depth),
            },
            (),
        ))
    }
}
