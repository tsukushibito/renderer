//! Publication is tested with real submissions and an injected recoverable consumer error.
use renderer::{
    material::Materials,
    mesh::{MeshCapacity, Meshes},
    pipeline::*,
    unlit_forward::IDENTITY,
};
use renderer_core::{
    capacity::Preparation,
    graph::{Graph, GraphCapacity, GraphError, GraphPool, ImageDesc},
    runtime::{FrameSubmission, Runtime, RuntimeConfig, RuntimeLimits},
};
use std::{cell::Cell, num::NonZeroUsize, rc::Rc, time::Duration};

struct HistoryProbe {
    publications: Rc<[Cell<u32>; 2]>,
    interrupt: Rc<Cell<bool>>,
    fail_create: Rc<Cell<bool>>,
    fail_release: Rc<Cell<bool>>,
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
        if self.fail_create.replace(false) {
            return Err("injected state creation failure".into());
        }
        Ok(c.id.0 as usize)
    }
    fn release_view(&mut self, _: &mut usize, _: &mut Runtime) -> PipelineResult<()> {
        if self.fail_release.replace(false) {
            return Err("injected state release failure".into());
        }
        Ok(())
    }
    fn record(
        &mut self,
        g: &mut Graph,
        _: &mut usize,
        camera: &CameraView,
        _: PreparedScene<'_, Self>,
    ) -> Result<(ViewImages, u64), GraphError> {
        let color = g.image(
            "color",
            ImageDesc::rgba16_float(camera.extent[0], camera.extent[1]),
        )?;
        g.upload_image("pixel", color, &[0; 16][..8 * camera.extent[0] as usize])?;
        Ok((ViewImages { color, depth: None }, g.graph_id()))
    }
    fn accept(
        &mut self,
        state: &mut usize,
        update: &mut u64,
        receipt: &FrameSubmission,
    ) -> PipelineResult<()> {
        assert_eq!(*update, receipt.graph_id());
        if *state == 0 && self.interrupt.replace(false) {
            return Err("injected publication failure".into());
        }
        let counter = &self.publications[*state];
        counter.set(counter.get() + 1);
        Ok(())
    }
}
pub fn check(api: RuntimeConfig) -> PipelineResult<()> {
    let fail_create = Rc::new(Cell::new(false));
    let fail_release = Rc::new(Cell::new(false));
    let calls = Rc::new([Cell::new(0), Cell::new(0)]);
    let interrupt = Rc::new(Cell::new(true));
    let mut prep = Preparation::new(NonZeroUsize::MIN);
    let mut driver = ViewRenderer::new();
    let group = driver.prepare_pipeline(
        HistoryProbe {
            publications: calls.clone(),
            fail_create: fail_create.clone(),
            fail_release: fail_release.clone(),
            interrupt: interrupt.clone(),
        },
        [1, 1],
        NonZeroUsize::MIN,
        None,
        &mut prep,
    )?;
    let second = driver.prepare_pipeline(
        HistoryProbe {
            publications: calls.clone(),
            fail_create: fail_create.clone(),
            fail_release: fail_release.clone(),
            interrupt: Rc::new(Cell::new(false)),
        },
        [2, 1],
        NonZeroUsize::MIN,
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
    driver.initialize_pipeline(group, &mut rt)?;
    driver.initialize_pipeline(second, &mut rt)?;
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
    };
    let b = CameraView {
        id: ViewId(1),
        extent: [2, 1],
        ..a
    };
    driver.register_view(group, &a)?;
    driver.register_view(second, &b)?;
    let meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let materials = Materials::default();
    let scene = SceneInput::new(&meshes, &materials, &[]);
    let mut cancelled = driver.begin_frame(&mut pool)?;
    cancelled.draw_view(cancelled.bind_scene(group, scene)?, &a)?;
    drop(cancelled);
    let mut frame = driver.begin_frame(&mut pool)?;
    let bound = frame.bind_scene(group, scene)?;
    frame.draw_view(bound, &a)?;
    let color = frame.draw_view(frame.bind_scene(second, scene)?, &b)?.color;
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
    rejected.draw_view(rejected.bind_scene(group, scene)?, &a)?;
    let handle = rejected.graph().import_image("old color", &stale)?;
    rejected.graph().readback_image("stale read", handle)?;
    assert!(matches!(
        rejected.submit(&mut rt),
        Err(ViewSubmitError::Rejected(_))
    ));
    assert_eq!((calls[0].get(), calls[1].get()), (1, 1));
    let mut abandoned = driver.begin_host_frame(pool.begin()?)?;
    abandoned.draw_scene(&a, scene)?;
    drop(abandoned.finish_recording()?);
    assert_eq!((calls[0].get(), calls[1].get()), (1, 1));

    let mut host = driver.begin_host_frame(pool.begin()?)?;
    host.draw_scene(&a, scene)?;
    let output = host.draw_scene(&b, scene)?.color;
    let (mut graph, mut histories) = host.finish_recording()?;
    assert!(histories.publish(&receipt).is_err());
    drop(receipt);
    let read = graph.readback_image("host read", output)?;
    let host_receipt = rt.submit(&graph.compile()?)?;
    interrupt.set(true);
    assert!(histories.publish(&host_receipt).is_err());
    drop(histories);
    assert_eq!((calls[0].get(), calls[1].get()), (1, 2));
    assert!(driver.begin_frame(&mut pool).is_err());
    driver.finish_host_submission(&host_receipt)?;
    assert_eq!((calls[0].get(), calls[1].get()), (2, 2));
    let mut result = rt
        .wait_for_frame(host_receipt.id(), Duration::from_secs(10))?
        .ok_or("host timeout")?;
    let mut pixels = [255; 16];
    result.readback_into(read, &mut pixels)?;
    assert_eq!(pixels, [0; 16]);
    drop((result, host_receipt));
    driver.remove_view(b.id, &mut rt)?;
    let moved = CameraView {
        extent: b.extent,
        ..a
    };
    fail_create.set(true);
    assert!(driver.reconfigure_view(second, &moved, &mut rt).is_err());
    // Creation failed before the original registration/history was touched.
    let mut frame = driver.begin_frame(&mut pool)?;
    frame.draw_view(frame.bind_scene(group, scene)?, &a)?;
    drop(frame);
    fail_release.set(true);
    assert!(driver.reconfigure_view(second, &moved, &mut rt).is_err());
    assert!(driver.begin_frame(&mut pool).is_err());
    driver.reconfigure_view(second, &moved, &mut rt)?;
    let mut frame = driver.begin_frame(&mut pool)?;
    frame.draw_view(frame.bind_scene(second, scene)?, &moved)?;
    drop(frame);
    driver.remove_view(a.id, &mut rt)?;
    driver.clear(&mut rt)?;
    super::validation::check_validation(rt)
}
