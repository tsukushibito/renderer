//! One GPU frame renders two techniques at different extents from shared assets.
use renderer::{
    capacity::Preparation,
    graph::{GraphCapacity, GraphPool},
    runtime::{Runtime, RuntimeConfig, RuntimeLimits},
};
use renderer_pipeline::{
    material::{Materials, UnlitParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    passes::output::SdrOutput,
    pipeline::*,
    unlit_forward::{IDENTITY, Vertex},
};
use std::{num::NonZeroUsize, time::Duration};

pub fn check(api: RuntimeConfig) -> PipelineResult<()> {
    let n = NonZeroUsize::MIN;
    let limits = SceneLimits {
        frames_in_flight: n,
        max_draws: 1,
        max_materials: 1,
        max_transforms: 1,
        max_point_lights: 0,
        max_casters_per_cascade: 0,
    };
    let size = [8, 8];
    let mut prep = Preparation::new(n);
    let output = Some(SdrOutput::display(false));
    let mut driver = ViewRenderer::new();
    let standard = driver.prepare_pipeline(
        StandardPipeline::forward(size, limits)?,
        size,
        n,
        output,
        &mut prep,
    )?;
    let minimap_size = [4, 4];
    let toon = driver.prepare_pipeline(
        super::toon_pipeline::prepare(minimap_size, limits, &[])?,
        minimap_size,
        n,
        output,
        &mut prep,
    )?;
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let vertices = [[-1., -1., 0.5], [0., 1., 0.5], [1., -1., 0.5]].map(|position| Vertex {
        position,
        color: [1.; 4],
    });
    let mesh = meshes.create(MeshData::unlit(&vertices, &[0, 1, 2])?)?;
    meshes.reserve(&mut prep)?;
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
    let cap = prep.graph_capacity(GraphCapacity::default())?;
    let mut rt = Runtime::new(
        api,
        RuntimeLimits {
            graph: cap,
            ..Default::default()
        },
        prep.working_set(),
    )?;
    let mut pool = GraphPool::new(cap, 1)?;
    for id in [
        driver.initialize_pipeline(standard, &mut rt)?,
        driver.initialize_pipeline(toon, &mut rt)?,
        meshes.prepare_gpu(&mut rt, &mut GraphPool::new(cap, 1)?)?,
    ]
    .into_iter()
    .flatten()
    {
        drop(
            rt.wait_for_frame(id, Duration::from_secs(10))?
                .ok_or("setup timed out")?,
        );
    }
    let minimap = CameraView {
        id: ViewId(1),
        extent: minimap_size,
        ..camera
    };
    driver.register_view(standard, &camera)?;
    driver.register_view(toon, &minimap)?;
    let scene = SceneInput::new(&meshes, &materials, &objects);
    driver.prepare_scene(standard, scene)?;
    driver.prepare_scene(toon, scene)?;
    let mut frame = driver.begin_frame(&mut pool)?;
    let mut draw = |pipeline, camera: &CameraView| -> PipelineResult<_> {
        let bound = frame.bind_scene(pipeline, scene)?;
        let hdr = frame.draw_view(bound, camera)?.color;
        let sdr = frame.write_output(camera.id, hdr, 1., None)?;
        Ok(frame.graph().readback_image("result", sdr.raw_image())?)
    };
    let main_read = draw(standard, &camera)?;
    let map_read = draw(toon, &minimap)?;
    let compiled = frame.compile()?;
    assert_eq!(
        compiled
            .ordered_pass_names()
            .filter(|name| *name == "result")
            .count(),
        2
    );
    let receipt = compiled.submit(&mut rt)?;
    let mut result = rt
        .wait_for_frame(receipt.id(), Duration::from_secs(10))?
        .ok_or("timed out")?;
    let mut a = [0; 8 * 8 * 4];
    let mut b = [0; 4 * 4 * 4];
    result.readback_into(main_read, &mut a)?;
    result.readback_into(map_read, &mut b)?;
    let (a, b) = (&a[144..148], &b[40..44]);
    assert!(a[0] > a[1] && b[0] > b[1]);
    assert_ne!(a, b);
    drop((result, receipt));
    driver.clear(&mut rt)?;
    meshes.release_gpu(&mut rt)?;
    materials.clear(&mut rt)?;
    super::validation::check_validation(rt)
}
