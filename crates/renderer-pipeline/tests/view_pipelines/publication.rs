//! Publication is tested with real submissions and an injected recoverable consumer error.
use renderer::{
    capacity::Preparation,
    graph::{Graph, GraphCapacity, GraphError, GraphPool, ImageDesc},
    runtime::{FrameSubmission, Runtime, RuntimeConfig, RuntimeLimits},
};
use renderer_pipeline::{
    material::Materials,
    mesh::{MeshCapacity, Meshes},
    pipeline::*,
    unlit_forward::IDENTITY,
};
use std::{cell::Cell, num::NonZeroUsize, rc::Rc, time::Duration};

struct HistoryProbe {
    publications: Rc<[Cell<u32>; 2]>,
    interrupt: bool,
}
impl ViewPipeline for HistoryProbe {
    type State = usize;
    type Update = u64;
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
    fn create_view(&self, c: &CameraView) -> PipelineResult<usize> {
        Ok(c.id.0 as usize)
    }
    fn record(
        &mut self,
        g: &mut Graph,
        _: &mut usize,
        _: &CameraView,
        _: PreparedScene<'_, Self>,
    ) -> Result<(ViewImages, u64), GraphError> {
        let color = g.image("color", ImageDesc::rgba16_float(1, 1))?;
        g.upload_image("pixel", color, &[0; 8])?;
        Ok((ViewImages { color, depth: None }, g.graph_id()))
    }
    fn accept(
        &mut self,
        state: &mut usize,
        update: &mut u64,
        receipt: &FrameSubmission,
    ) -> PipelineResult<()> {
        assert_eq!(*update, receipt.graph_id());
        if *state == 0 && std::mem::take(&mut self.interrupt) {
            return Err("injected publication failure".into());
        }
        let counter = &self.publications[*state];
        counter.set(counter.get() + 1);
        Ok(())
    }
}
pub fn check(api: RuntimeConfig) -> PipelineResult<()> {
    let calls = Rc::new([Cell::new(0), Cell::new(0)]);
    let mut prep = Preparation::new(NonZeroUsize::MIN);
    let mut driver = ViewRenderer::prepare(
        HistoryProbe {
            publications: calls.clone(),
            interrupt: true,
        },
        [1, 1],
        NonZeroUsize::new(2).unwrap(),
        None,
        &mut prep,
    )?;
    let cap = prep.graph_capacity(GraphCapacity::default())?;
    let mut rt = Runtime::new(
        api,
        RuntimeLimits {
            graph: cap,
            ..Default::default()
        },
        prep.working_set(),
    )?;
    driver.initialize(&mut rt)?;
    let mut pool = GraphPool::new(cap, 2)?;
    let a = CameraView {
        id: ViewId(0),
        extent: [1, 1],
        camera: ViewInput::new(
            IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?,
        bounds: Bounds::new([-1.; 3], [1.; 3])?,
        sun: Sun::new([0., 1., 0.], [1.; 3])?,
    };
    let b = CameraView { id: ViewId(1), ..a };
    driver.register_view(&a)?;
    driver.register_view(&b)?;
    let meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let materials = Materials::default();
    let scene = SceneInput::new(&meshes, &materials, &[]);
    let mut cancelled = driver.begin_frame(&mut pool)?;
    cancelled.draw_view(scene, &a)?;
    drop(cancelled);
    let mut frame = driver.begin_frame(&mut pool)?;
    frame.draw_view(scene, &a)?;
    let color = frame.draw_view(scene, &b)?.color;
    frame.graph().export_image(color)?;
    let Err(ViewSubmitError::Accepted { frame: id, .. }) = frame.submit(&mut rt) else {
        panic!("publication should require recovery")
    };
    assert_eq!((calls[0].get(), calls[1].get()), (0, 1));
    assert!(driver.begin_frame(&mut pool).is_err());
    let receipt = driver.finish_submission()?;
    assert_eq!(receipt.id(), id);
    assert_eq!((calls[0].get(), calls[1].get()), (1, 1));
    assert!(driver.finish_submission().is_err());
    let stale = receipt.export_image(color)?;
    drop(
        rt.wait_for_frame(id, Duration::from_secs(10))?
            .ok_or("frame timed out")?,
    );
    rt.delete(stale.key())?;
    let mut rejected = driver.begin_frame(&mut pool)?;
    rejected.draw_view(scene, &a)?;
    let handle = rejected.graph().import_image("old color", &stale)?;
    rejected.graph().readback_image("stale read", handle)?;
    assert!(matches!(
        rejected.submit(&mut rt),
        Err(ViewSubmitError::Rejected(_))
    ));
    assert_eq!((calls[0].get(), calls[1].get()), (1, 1));
    driver.remove_view(a.id, &mut rt)?;
    driver.clear(&mut rt)?;
    super::validation::check_validation(rt)
}
