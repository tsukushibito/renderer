//! Host-owned ECS extraction and retained objects drive the same populated target range.
use crate::{camera, validation};
use renderer::{
    core::{
        capacity::{CapacityRecovery, RuntimeCapacityKind},
        graph::{GraphCapacityKind, GraphError, GraphPool, ImageDesc, PassHandle},
        runtime::{FrameCompletion, FrameId, Runtime, RuntimeConfig, RuntimeLimits},
    },
    material::{Materials, PbrParams, UnlitParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{PbrVertex, Transform},
    pipeline::{
        CameraView, PipelineResult, PreparedSetup, RenderObject, RendererSetup, SceneAssets,
        SceneInput, SceneRenderer,
    },
    scene::Scene,
    unlit_forward::Vertex,
};
use std::time::Duration;

const COUNT: usize = 1024;
const EXTENT: [u32; 2] = [128, 64];
const BYTES: usize = 128 * 64 * 4;

fn placement(i: usize, offset: usize) -> PipelineResult<Transform> {
    let x = 2. * ((i % 32 * 4 + offset) as f32 + 0.5) / 128. - 1.;
    let y = 2. * ((i / 32 * 2) as f32 + 0.5) / 64. - 1.;
    Ok(Transform::new([
        [1., 0., 0., x],
        [0., 1., 0., y],
        [0., 0., 1., 0.],
    ])?)
}

fn setup(camera: CameraView, assets: SceneAssets<'_>) -> PipelineResult<PreparedSetup<'_>> {
    let mut plan = RendererSetup::standard(camera).prepare(assets)?;
    plan.reserve_readback("targets", ImageDesc::rgba8(EXTENT[0], EXTENT[1]))?;
    Ok(plan)
}

fn submit(
    drawing: &mut SceneRenderer,
    camera: &CameraView,
    input: SceneInput<'_>,
) -> PipelineResult<(FrameId, PassHandle)> {
    let mut frame = drawing.begin_frame()?;
    let hdr = frame.draw_scene(camera, input)?.color;
    let image = frame.write_output(camera.id, hdr, 1., None)?;
    let pixels = frame.graph().readback_image("targets", image.raw_image())?;
    Ok((frame.submit()?.id(), pixels))
}

fn complete(drawing: &mut SceneRenderer, frame: FrameId) -> PipelineResult<FrameCompletion> {
    drawing
        .runtime()
        .wait_for_frame(frame, Duration::from_secs(30))?
        .ok_or_else(|| "target range timed out".into())
}

fn verify(pixels: &[u8; BYTES], step: usize) {
    for (i, p) in pixels.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (i % 128, i / 128);
        let target = y / 2 * 32 + x / 4;
        let offset = if step == 0 {
            0
        } else if step >= 2 && target < 16 {
            2
        } else {
            1
        };
        let covered = y.is_multiple_of(2) && x % 4 == offset;
        let red_target = target.is_multiple_of(2);
        assert_eq!(
            [p[0] > 100, p[1] > 100, p[2] > 100],
            [covered && red_target, covered && !red_target, false],
            "range step {step} at {x},{y}"
        );
    }
}

fn expect_pressure(
    drawing: &mut SceneRenderer,
    host_pool: &mut GraphPool,
    camera: &CameraView,
    input: SceneInput<'_>,
    expected: (RuntimeCapacityKind, CapacityRecovery),
) -> PipelineResult<()> {
    let (mut frame, runtime) = drawing.begin_host_frame(host_pool.begin()?)?;
    let hdr = frame.draw_scene(camera, input)?.color;
    let image = frame.write_output(camera.id, hdr, 1., None)?;
    frame.graph().readback_image("targets", image.raw_image())?;
    let (graph, _histories) = frame.finish_recording()?;
    let error = runtime
        .submit(&graph.compile()?)
        .err()
        .ok_or("occupied runtime unexpectedly accepted work")?;
    let cap = error.capacity().ok_or(error)?;
    assert_eq!((cap.resource, cap.recovery), expected);
    Ok(())
}

pub fn run(mut config: RuntimeConfig) -> PipelineResult<()> {
    let camera = camera(1, EXTENT)?;
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 2,
        retired_mesh_versions: 2,
    })?;
    let positions = [[-1., -1.], [1., -1.], [-1., 1.], [1., 1.]]
        .map(|[x, y]| [x * 0.95 / 128., y * 0.95 / 64., 0.5]);
    let indices = [0, 1, 2, 1, 3, 2];
    let pbr = meshes.create(MeshData::pbr(
        &positions.map(|p| PbrVertex::new(p, [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap()),
        &indices,
    )?)?;
    let unlit = meshes.create(MeshData::unlit(
        &positions.map(|position| Vertex {
            position,
            color: [0., 1., 0., 1.],
        }),
        &indices,
    )?)?;
    let mut materials = Materials::default();
    let mut red = PbrParams::color([0., 0., 0., 1.]);
    red.factors.double_sided = true;
    red.factors.emissive_nits = [1., 0., 0.];
    let props = [
        RenderObject::new(pbr, materials.create_pbr(red)?),
        RenderObject::new(
            unlit,
            materials.create_unlit(UnlitParams { color: [1.; 4] })?,
        ),
    ];
    let mut host = Vec::with_capacity(COUNT);
    let mut scene = Scene::default();
    let mut ids = Vec::with_capacity(COUNT);
    for i in 0..COUNT {
        let mut obj = props[i % 2].with_transform(placement(i, 0)?);
        obj.casts_shadow = false;
        ids.push(scene.spawn(obj));
        host.push(obj);
    }
    let runtime = {
        let full = setup(camera, SceneAssets::new(&mut meshes, &mut materials, &host))?;
        full.fit_backend(&mut config)?;
        Runtime::new(
            config,
            full.runtime_limits(RuntimeLimits {
                frames: 1,
                results: 1,
                ..Default::default()
            })?,
            full.preparation().working_set(),
        )?
    };
    let narrow = setup(
        camera,
        SceneAssets::new(&mut meshes, &mut materials, &host[..2]),
    )?;
    let mut drawing = narrow.begin_with_runtime(runtime)?.wait()?;
    let err = submit(
        &mut drawing,
        &camera,
        SceneInput::new(&meshes, &materials, &host),
    )
    .unwrap_err();
    let cap = err
        .downcast_ref::<GraphError>()
        .and_then(GraphError::capacity)
        .ok_or(err)?;
    assert_eq!(
        (cap.kind, cap.limit, cap.required),
        (GraphCapacityKind::Draws, 2, Some(COUNT))
    );
    // Skip the oversized frame. At the next load boundary expand to the real active set.
    drawing.replace_working_set(setup(
        camera,
        SceneAssets::new(&mut meshes, &mut materials, &host),
    )?)?;
    let mut host_pool = GraphPool::new(drawing.runtime().limits().graph, 1)?;
    let mut changes = Vec::with_capacity(COUNT);
    let mut expected = [0; BYTES];
    let mut actual = [0; BYTES];
    for step in 0..4 {
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        let before = renderer::core::measurement::Snapshot::now();
        changes.clear();
        let changed = match step {
            1 => COUNT,
            2 => 16,
            _ => 0,
        };
        for i in 0..changed {
            let pose = placement(i, step)?;
            changes.push((ids[i], pose));
            host[i].transform = pose;
        }
        scene
            .update_transforms(&changes)
            .map_err(|_| "stale range target")?;
        let (frame, pixels) = submit(
            &mut drawing,
            &camera,
            SceneInput::new(&meshes, &materials, scene.objects()),
        )?;
        if step == 0 {
            expect_pressure(
                &mut drawing,
                &mut host_pool,
                &camera,
                SceneInput::new(&meshes, &materials, &host),
                (RuntimeCapacityKind::ExecutionSlots, CapacityRecovery::Poll),
            )?;
        }
        let mut first = complete(&mut drawing, frame)?;
        if step == 0 {
            expect_pressure(
                &mut drawing,
                &mut host_pool,
                &camera,
                SceneInput::new(&meshes, &materials, &host),
                (
                    RuntimeCapacityKind::ResultSlots,
                    CapacityRecovery::ReleaseResults,
                ),
            )?;
        }
        first.readback_into(pixels, &mut expected)?;
        drop(first);
        let (frame, pixels) = submit(
            &mut drawing,
            &camera,
            SceneInput::new(&meshes, &materials, &host),
        )?;
        complete(&mut drawing, frame)?.readback_into(pixels, &mut actual)?;
        assert_eq!(actual.as_slice(), expected.as_slice());
        verify(&actual, step);
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        if step != 0 {
            let delta = renderer::core::measurement::Snapshot::now().since(before);
            assert!(delta.is_allocation_free(), "range step {step}: {delta:?}");
        }
    }
    let mut runtime = drawing.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    validation::check_validation(runtime)
}
