//! Both drivers consume the very same registered GPU buffers and material handle.
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

fn pixel<P: ViewPipeline>(
    driver: &mut ViewRenderer<P>,
    rt: &mut Runtime,
    pool: &mut GraphPool,
    scene: SceneInput<'_>,
    camera: &CameraView,
) -> PipelineResult<[u8; 4]> {
    driver.prepare_scene(scene)?;
    let mut frame = driver.begin_frame(pool)?;
    let hdr = frame.draw_view(scene, camera)?.color;
    let sdr = frame.write_output(hdr, 1., None)?;
    let read = frame.graph().readback_image("result", sdr.raw_image())?;
    let submitted = frame.submit(rt)?;
    let mut result = rt
        .wait_for_frame(submitted.id(), Duration::from_secs(10))?
        .ok_or("timed out")?;
    let mut bytes = [0; 8 * 8 * 4];
    result.readback_into(read, &mut bytes)?;
    Ok(bytes[144..148].try_into()?)
}

pub fn check(api: RuntimeConfig) -> PipelineResult<()> {
    let n = NonZeroUsize::MIN;
    let limits = SceneLimits {
        frames_in_flight: n,
        max_draws: 1,
        max_materials: 1,
        max_transforms: 1,
        max_casters_per_cascade: 0,
    };
    let size = [8, 8];
    let mut prep = Preparation::new(n);
    let output = Some(SdrOutput::display(false));
    let mut standard = ViewRenderer::prepare(
        StandardPipeline::forward(size, limits)?,
        size,
        n,
        output,
        &mut prep,
    )?;
    let mut toon = ViewRenderer::prepare(
        super::toon_pipeline::prepare(size, limits, &[])?,
        size,
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
        bounds: Bounds::new([-1.; 3], [1.; 3])?,
        sun: Sun::new([0., 0., -1.], [1.; 3])?,
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
        standard.initialize(&mut rt)?,
        toon.initialize(&mut rt)?,
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
    standard.register_view(&camera)?;
    toon.register_view(&camera)?;
    let scene = SceneInput::new(&meshes, &materials, &objects);
    let a = pixel(&mut standard, &mut rt, &mut pool, scene, &camera)?;
    let b = pixel(&mut toon, &mut rt, &mut pool, scene, &camera)?;
    assert!(a[0] > a[1] && b[0] > b[1]);
    assert_ne!(a, b);
    standard.clear(&mut rt)?;
    toon.clear(&mut rt)?;
    meshes.release_gpu(&mut rt)?;
    materials.clear(&mut rt)?;
    super::validation::check_validation(rt)
}
