//! The host keeps ownership of its scheduler and asset registries throughout cancellable setup.
use renderer::{
    material::{Materials, PbrParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{PbrVertex, Projection, ViewInput},
    pipeline::{CameraView, PipelineResult, RenderObject, RendererSetup, SceneAssets, ViewId},
    unlit_forward::IDENTITY,
};
use renderer_core::{
    graph::{GraphPool, ImageDesc},
    runtime::{Runtime, RuntimeConfig, RuntimeLimits},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub fn check(mut backend: RuntimeConfig) -> PipelineResult<()> {
    let camera = CameraView {
        id: ViewId(1),
        extent: [8, 8],
        camera: ViewInput::new(
            IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?,
    };
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 2,
    })?;
    let vertices = [[-1., -1., 0.5], [0., 1., 0.5], [1., -1., 0.5]].map(|position| {
        PbrVertex::new(position, [0., 0., -1.], [1., 0., 0., 1.], [0., 0.]).unwrap()
    });
    let mesh = meshes.create(MeshData::pbr(&vertices, &[0, 1, 2])?)?;
    let mut materials = Materials::default();
    let material = materials.create_pbr(PbrParams::color([1.; 4]))?;
    let unrelated = materials.duplicate(material)?;
    let original = materials.snapshot(material)?;
    let objects = [RenderObject::new(mesh, material)];
    let setup = || {
        let mut setup = RendererSetup::standard(camera);
        setup.mesh_upload_bytes = 16;
        setup
    };
    let mut plan = setup().prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
    let host_desc = ImageDesc::rgba8(2, 2);
    plan.reserve_texture("host", host_desc)?;
    plan.fit_backend(&mut backend)?;
    let limits = plan.runtime_limits(RuntimeLimits {
        frames: 2,
        ..Default::default()
    })?;
    let mut runtime = Runtime::new(backend, limits, plan.preparation().working_set())?;
    let mut pool = GraphPool::new(limits.graph, 2)?;
    let (foreign, image) = runtime.upload_texture(&mut pool, "host", host_desc, &[93; 16])?;
    let mut graph = pool.begin()?;
    let host_image = graph.import_image("host", image.snapshot())?;
    let read = graph.readback_image("host result", host_image)?;
    let host_receipt = runtime.submit(&graph.compile()?)?;
    let mut build = plan.begin_with_runtime(&mut runtime)?;
    assert!(build.poll().is_err(), "host owns both frame slots");
    assert!(build.pending_frame().is_none());
    let mut result = build
        .runtime()
        .wait_for_frame(host_receipt.id(), Duration::from_secs(10))?
        .ok_or("host read timeout")?;
    let mut pixels = [0; 16];
    result.readback_into(read, &mut pixels)?;
    assert_eq!(pixels, [93; 16]);
    drop((result, host_receipt));
    // Cancel with a partial mesh upload on the queue. Repeated attempts reuse its capacity.
    let mut last = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "no mesh batch was accepted");
        assert!(build.poll()?.is_pending());
        if let Some(frame) = build.pending_frame() {
            if last.is_some_and(|previous| previous != frame) {
                break;
            }
            last = Some(frame);
        }
    }
    let pending = build.cancel()?.expect("active mesh batch");
    drop(build);
    drop(
        runtime
            .wait_for_frame(pending, Duration::from_secs(10))?
            .ok_or("mesh timeout")?,
    );
    assert!(meshes.import(&mut pool.begin()?, mesh).is_err());
    // The named wait in setup must leave this earlier host completion available.
    drop(
        runtime
            .wait_for_frame(foreign, Duration::from_secs(10))?
            .ok_or("host timeout")?,
    );
    assert_eq!(
        runtime.refresh_image(image.snapshot().key())?,
        *image.snapshot()
    );
    // Cancel after publishing GPU versions provisionally, including the material.
    let mut build = setup()
        .prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?
        .begin_with_runtime(&mut runtime)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while build.poll()?.is_pending() {
        assert!(Instant::now() < deadline);
    }
    drop(build);
    assert!(Arc::ptr_eq(&original, &materials.snapshot(material)?));
    assert!(meshes.import(&mut pool.begin()?, mesh).is_err());
    let mut renderer = setup()
        .prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?
        .begin_with_runtime(&mut runtime)?
        .wait()?;
    let incompatible = RendererSetup::new(renderer::pipeline::CameraGroup::custom(
        camera,
        super::toon_pipeline::prepare,
    ))
    .prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
    let rejected = renderer.replace_working_set(incompatible).unwrap_err();
    assert!(
        rejected.to_string().contains("ToonPipeline requires"),
        "{rejected}"
    );
    let mut frame = renderer.begin_frame()?;
    let view = frame.draw_scene(
        &camera,
        renderer::pipeline::SceneInput::new(&meshes, &materials, &objects),
    )?;
    let read = frame
        .graph()
        .readback_image("surviving view", view.depth.unwrap())?;
    let ticket = frame.submit()?;
    let mut result = renderer
        .runtime()
        .wait_for_frame(ticket.id(), Duration::from_secs(15))?
        .ok_or("old view timeout")?;
    let mut depths = [0; 8 * 8 * 4];
    result.readback_into(read, &mut depths)?;
    assert!(
        depths
            .as_chunks::<4>()
            .0
            .iter()
            .any(|&d| f32::from_le_bytes(d) == 0.5)
    );
    drop((result, ticket));
    drop(renderer);
    assert!(Arc::ptr_eq(&original, &materials.snapshot(unrelated)?));
    assert!(!Arc::ptr_eq(&original, &materials.snapshot(material)?));
    let old_count = meshes.import(&mut pool.begin()?, mesh)?.index_count;
    meshes.update(mesh, MeshData::pbr(&vertices, &[0, 1, 2, 2, 1, 0])?)?;
    let plan = setup().prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
    runtime.prepare_working_set(plan.preparation().working_set())?;
    let mut build = plan.begin_with_runtime(&mut runtime)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while build.poll()?.is_pending() {
        assert!(Instant::now() < deadline);
    }
    drop(build);
    assert_eq!(
        meshes.import(&mut pool.begin()?, mesh)?.index_count,
        old_count
    );
    drop(image);
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    super::validation::check_validation(runtime)
}
