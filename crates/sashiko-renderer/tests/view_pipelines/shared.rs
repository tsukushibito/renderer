//! One GPU frame renders two techniques at different extents from shared assets.
use sashiko_renderer::{
    material::{Materials, UnlitParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pipeline::*,
    unlit_forward::{IDENTITY, Vertex},
};
use sashiko_renderer_core::{graph::ImageDesc, runtime::RuntimeConfig};
use std::time::Duration;

pub fn check(api: RuntimeConfig) -> PipelineResult<()> {
    let size = [8, 8];
    let minimap_size = [4, 4];
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let vertices = [[-1., -1., 0.5], [0., 1., 0.5], [1., -1., 0.5]].map(|position| Vertex {
        position,
        color: [1.; 4],
    });
    let mesh = meshes.create(MeshData::unlit(&vertices, &[0, 1, 2])?)?;
    let mut materials = Materials::default();
    let material = materials.create_unlit(UnlitParams {
        color: [1., 0.2, 0.1, 1.],
    })?;
    let objects = [RenderObject::new(mesh, material)];
    let camera = CameraView {
        id: ViewId(0),
        extent: size,
        camera: ViewInput::new(
            IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?,
    };
    let minimap = CameraView {
        id: ViewId(1),
        extent: minimap_size,
        ..camera
    };
    let mut setup = RendererSetup::standard(camera);
    setup.add_group(CameraGroup::custom(minimap, super::toon_pipeline::prepare));
    setup.mesh_upload_bytes = 16;
    let mut plan = setup.prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
    for [w, h] in [size, minimap_size] {
        plan.reserve_readback("result", ImageDesc::rgba8(w, h))?;
    }
    let limits = plan.scene_limits();
    let mut build = plan.begin_build(api)?;
    while build.poll()?.is_pending() {
        std::thread::yield_now();
    }
    let mut renderer = build.finish()?;
    let scene = SceneInput::new(&meshes, &materials, &objects);
    let mut frame = renderer.begin_frame()?;
    let mut draw = |camera: &CameraView| -> PipelineResult<_> {
        let hdr = frame.draw_scene(camera, scene)?.color;
        let sdr = frame.write_output(camera.id, hdr, 1., None)?;
        Ok(frame.graph().readback_image("result", sdr.raw_image())?)
    };
    let main_read = draw(&camera)?;
    let map_read = draw(&minimap)?;
    let compiled = frame.compile()?;
    assert_eq!(
        compiled
            .ordered_pass_names()
            .filter(|name| *name == "result")
            .count(),
        2
    );
    let receipt = compiled.submit()?;
    let mut result = renderer
        .runtime()
        .wait_for_frame(receipt.id(), Duration::from_secs(10))?
        .ok_or("timed out")?;
    let mut a = [0; 8 * 8 * 4];
    let mut b = [0; 4 * 4 * 4];
    result.readback_into(main_read, &mut a)?;
    result.readback_into(map_read, &mut b)?;
    assert!(a[144] > a[145] && b[40] > b[41]);
    assert_ne!(a[144..148], b[40..44]);
    drop((result, receipt));
    let mut rt = renderer.into_runtime()?;
    // Compare every output byte against the independent low-level assembly path.
    let mut driver = ViewRenderer::new();
    let n = limits.frames_in_flight;
    let mut prep = sashiko_renderer_core::capacity::Preparation::new(n);
    let sdr = Some(sashiko_renderer::passes::output::SdrOutput::display(false));
    let standard = driver.prepare_pipeline(
        StandardPipeline::forward(size, limits)?,
        size,
        n,
        sdr,
        &mut prep,
    )?;
    let toon = driver.prepare_pipeline(
        super::toon_pipeline::prepare(minimap_size, limits, &[])?,
        minimap_size,
        n,
        sdr,
        &mut prep,
    )?;
    rt.prepare_working_set(prep.working_set())?;
    for (id, view) in [(standard, &camera), (toon, &minimap)] {
        if let Some(frame) = driver.initialize_pipeline(id, &mut rt)? {
            drop(
                rt.wait_for_frame(frame, Duration::from_secs(10))?
                    .ok_or("reference setup timeout")?,
            );
        }
        driver.register_view(id, view)?;
        driver.prepare_scene(id, scene)?;
    }
    let mut pool = sashiko_renderer_core::graph::GraphPool::new(rt.limits().graph, 1)?;
    let mut frame = driver.begin_frame(&mut pool)?;
    let mut reads = Vec::new();
    for (id, view) in [(standard, &camera), (toon, &minimap)] {
        let scene = frame.bind_scene(id, scene)?;
        let hdr = frame.draw_view(scene, view)?.color;
        let color = frame.write_output(view.id, hdr, 1., None)?;
        reads.push(frame.graph().readback_image("result", color.raw_image())?);
    }
    let reference = frame.submit(&mut rt)?;
    let mut result = rt
        .wait_for_frame(reference.id(), Duration::from_secs(10))?
        .ok_or("reference timeout")?;
    for (read, expected) in reads.into_iter().zip([a.as_slice(), b.as_slice()]) {
        let mut pixels = vec![0; expected.len()];
        result.readback_into(read, &mut pixels)?;
        assert_eq!(pixels, expected);
    }
    drop((reference, result));
    driver.clear(&mut rt)?;
    meshes.release_gpu(&mut rt)?;
    materials.clear(&mut rt)?;
    super::validation::check_validation(rt)
}
