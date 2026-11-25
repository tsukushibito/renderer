//! Standard camera and shadow consumers select resident mesh LODs independently.
use sashiko_renderer::{
    material::{Materials, PbrParams},
    mesh::{JointWeights, MeshCapacity, MeshData, Meshes},
    pbr::{PbrVertex, ShadowSettings, Sun, Transform},
    pipeline::*,
    unlit_forward::IDENTITY,
};
use sashiko_renderer_core::runtime::RuntimeConfig;
use std::time::Duration;

fn check(api: RuntimeConfig) -> PipelineResult<()> {
    let extent = [32, 32];
    let camera = CameraView {
        id: ViewId(0),
        extent,
        camera: ViewInput::new(
            IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?,
    };
    let second = CameraView {
        id: ViewId(1),
        ..camera
    };
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 2,
        retired_mesh_versions: 0,
    })?;
    let mut materials = Materials::default();
    let mat = materials.create_pbr(PbrParams::default())?;
    let make_lod = |size: f32| -> PipelineResult<_> {
        let points = [[-size, -size, 0.5], [0., size, 0.5], [size, -size, 0.5]];
        let vertices =
            points.map(|p| PbrVertex::new(p, [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap());
        let weights = [JointWeights {
            joints: [0; 4],
            weights: [1., 0., 0., 0.],
        }; 3];
        Ok(MeshData::skinned_pbr(&vertices, &[0, 1, 2], 1, &weights)?.with_compute_skin()?)
    };
    let detailed = meshes.create(make_lod(0.5)?)?;
    let coarse = meshes.create(make_lod(0.3)?)?;
    let mut object = |mesh| -> PipelineResult<_> {
        let mut object = RenderObject::new(mesh, mat);
        object.pose = Some(meshes.create_pose(mesh)?);
        Ok(object)
    };
    let shared = object(detailed)?;
    let mut hidden = object(detailed)?;
    hidden.casts_shadow = false;
    // Its bind-pose bounds are on screen. Its current palette puts it outside.
    let mut shadow = object(coarse)?;
    shadow.camera_visible = false;
    meshes.set_joint_transforms(
        hidden.pose.unwrap(),
        &[Transform::new([
            [1., 0., 0., 4.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
        ])?],
    )?;
    let objects = [hidden, shared, shadow];
    let mut cfg = StandardConfig::new(extent);
    cfg.shadows = Some(ShadowSettings {
        resolution: 32,
        ..Default::default()
    });
    let mut group = CameraGroup::standard(camera, Technique::Forward, cfg);
    group.add_view(second)?;
    let mut setup = RendererSetup::new(group).prepare(SceneAssets::new(
        &mut meshes,
        &mut materials,
        &objects,
    ))?;
    setup.reserve_readback(
        "camera image",
        sashiko_renderer_core::graph::ImageDesc::rgba8(extent[0], extent[1]),
    )?;
    let mut build = setup.begin_build(api)?;
    while build.poll()?.is_pending() {
        std::thread::yield_now();
    }
    let mut renderer = build.finish()?;
    let bounds = Bounds::new([-8.; 3], [8.; 3])?;
    let sun = Sun::from_lux([0.4, 1., 0.3], [1.; 3], 1.)?;
    // Cold offscreen frame, then both LODs, then both cached shadow histories.
    for (first, other, uploads) in [
        (&objects[..1], &objects[..1], [0, 0]),
        (&objects[..2], &objects[..], [1, 1]),
        (&objects[..2], &objects[..], [1, 0]),
    ] {
        let mut frame = renderer.begin_frame()?;
        for (view, requested, expected) in
            [(&camera, first, uploads[0]), (&second, other, uploads[1])]
        {
            let color = frame
                .draw_scene(
                    view,
                    SceneInput::new(&meshes, &materials, requested)
                        .with_bounds(bounds)
                        .with_sun(sun),
                )?
                .color;
            let output = frame.write_output(view.id, color, 1., None)?.raw_image();
            if view.id == camera.id {
                frame.graph().readback_image("camera image", output)?;
            } else {
                frame.graph().export_frame_image(output)?;
            }
            assert_eq!(meshes.take_recorded_pose_bytes(), expected * 12_288);
        }
        let plan = frame.compile()?;
        assert_eq!(
            plan.ordered_pass_names()
                .filter(|&s| s == "compute skin vertices")
                .count(),
            uploads.iter().sum::<u64>() as usize
        );
        let submitted = plan.submit()?;
        renderer
            .runtime()
            .wait_for_frame(submitted.id(), Duration::from_secs(10))?
            .ok_or("skin views timeout")?;
    }
    let mut runtime = renderer.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    super::validation::check_validation(runtime)
}
#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires Vulkan hardware"]
fn vulkan_skin_visibility() -> PipelineResult<()> {
    check(RuntimeConfig::Vulkan(Default::default()))
}
#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires D3D12 hardware"]
fn d3d12_skin_visibility() -> PipelineResult<()> {
    check(RuntimeConfig::D3D12(Default::default()))
}
