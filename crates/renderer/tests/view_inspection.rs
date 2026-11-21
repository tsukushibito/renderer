//! Inspect a heterogeneous frame without creating a GPU Runtime.
use renderer::{
    material::Materials,
    mesh::{MeshCapacity, Meshes},
    pipeline::*,
    unlit_forward::IDENTITY,
};
use renderer_core::{
    capacity::Preparation,
    graph::{Graph, GraphCapacity, GraphError, GraphPool, ImageDesc},
};
use std::num::NonZeroUsize;

struct ClearView;
impl ViewPipeline for ClearView {
    type State = ();
    type Update = ();
    fn reserve(
        &self,
        p: &mut Preparation,
        [w, h]: [u32; 2],
        count: NonZeroUsize,
    ) -> Result<(), GraphError> {
        p.reserve_image(ImageDesc::rgba16_float(w, h), count.get())
    }
    fn check_object(&self, _: SceneInput<'_>, _: &RenderObject) -> Result<(), GraphError> {
        Ok(())
    }
    fn create_view(&self, _: &CameraView) -> PipelineResult<()> {
        Ok(())
    }
    fn record(
        &mut self,
        g: &mut Graph,
        _: &mut (),
        c: &CameraView,
        _: PreparedScene<'_, Self>,
    ) -> Result<(ViewImages, ()), GraphError> {
        let [w, h] = c.extent;
        let color = g.image("color", ImageDesc::rgba16_float(w, h))?;
        g.upload_image("clear", color, &[0; 32])?;
        Ok((ViewImages { color, depth: None }, ()))
    }
}

#[test]
fn inspect_then_cancel_a_frame_without_gpu_initialization() -> PipelineResult<()> {
    let n = NonZeroUsize::MIN;
    let mut preparation = Preparation::new(n);
    let mut views = ViewRenderer::new();
    let group = views.prepare_pipeline(ClearView, [2, 2], n, None, &mut preparation)?;
    let camera = CameraView {
        id: ViewId(7),
        extent: [2, 2],
        camera: ViewInput::new(
            IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?,
    };
    views.register_view(group, &camera)?;
    let meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let materials = Materials::default();
    let scene = SceneInput::new(&meshes, &materials, &[]);
    let mut pool = GraphPool::new(preparation.graph_capacity(GraphCapacity::default())?, 1)?;
    for _ in 0..2 {
        let mut frame = views.begin_frame(&mut pool)?;
        let bound = frame.bind_scene(group, scene)?;
        let image = frame.draw_view(bound, &camera)?.color;
        frame.graph().export_image(image)?;
        let compiled = frame.compile()?;
        assert_eq!(compiled.ordered_pass_names().collect::<Vec<_>>(), ["clear"]);
    }
    Ok(())
}
