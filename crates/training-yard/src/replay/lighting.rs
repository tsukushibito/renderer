//! The main camera and an equipment camera illuminate the same asset independently.
use crate::camera;
use renderer::{
    core::{graph::ImageDesc, runtime::Runtime},
    material::{Materials, PbrParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{Bounds, PbrVertex, ShadowSettings, Sun, Transform},
    pipeline::{
        CameraGroup, PipelineResult, RenderObject, RendererSetup, SceneAssets, SceneInput,
        StandardConfig, Technique, ViewId,
    },
};
use std::time::Duration;

pub(super) fn check(mut runtime: Runtime) -> PipelineResult<Runtime> {
    let main = camera(1, [32, 32])?;
    let preview = camera(2, [16, 16])?;
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let vertices = [[-1., -1.], [1., -1.], [-1., 1.], [1., 1.]].map(|[x, y]| {
        PbrVertex::new([x, y, 0.], [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap()
    });
    let mesh = meshes.create(MeshData::pbr(&vertices, &[0, 2, 1, 1, 2, 3])?)?;
    let mut materials = Materials::default();
    let mut surface = PbrParams::color([1.; 4]);
    surface.factors.double_sided = true;
    let quad = RenderObject::new(mesh, materials.create_pbr(surface)?);
    let pose =
        |size, depth| Transform::new([[size, 0., 0., 0.], [0., size, 0., 0.], [0., 0., 1., depth]]);
    let mut receiver = quad.with_transform(pose(1., 0.5)?);
    receiver.casts_shadow = false;
    // The main camera clips z < 0. The caster between the sun and the
    // receiver can still darken the middle of the visible surface.
    let caster = quad.with_transform(pose(0.4, -0.25)?);
    let mut objects = [receiver, caster];
    let world = Bounds::new([-1., -1., -0.5], [1., 1., 1.])?;
    let mut shadows = StandardConfig::new(main.extent);
    shadows.shadows = Some(ShadowSettings {
        resolution: 64,
        distance: 2.,
        ..Default::default()
    });
    let mut setup = RendererSetup::new(CameraGroup::standard(main, Technique::Forward, shadows));
    setup.add_group(CameraGroup::standard(
        preview,
        Technique::Forward,
        StandardConfig::new(preview.extent),
    ));
    let mut plan = setup.prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
    plan.reserve_readback("main", ImageDesc::rgba8(32, 32))?;
    plan.reserve_readback("surface depth", ImageDesc::depth32(32, 32))?;
    plan.reserve_readback("equipment", ImageDesc::rgba8(16, 16))?;
    plan.prepare_runtime(&mut runtime)?;
    let mut drawing = plan.begin_with_runtime(runtime)?.wait()?;
    let red = Sun::new([0., 0., -1.], [2., 0., 0.])?;
    let green = Sun::new([0., 0., -1.], [0., 2., 0.])?;
    let mut main_pixels = [0; 32 * 32 * 4];
    let mut depths = [0; 32 * 32 * 4];
    let mut equipment = [0; 16 * 16 * 4];
    let mut baseline = main_pixels;
    let mut preview_baseline = equipment;
    for step in 0..4 {
        if step == 1 {
            let (driver, _, _) = drawing.parts_mut();
            let id = driver.pipeline_for_view(preview.id)?;
            let extra = renderer::pipeline::CameraView {
                id: ViewId(3),
                ..preview
            };
            assert!(driver.register_view(id, &extra).is_err());
            assert!(driver.pipeline_for_view(extra.id).is_err());
        }
        let mut frame = drawing.begin_frame()?;
        if step == 2 {
            objects[1].transform =
                Transform::new([[0.4, 0., 0., 0.75], [0., 0.4, 0., 0.], [0., 0., 1., -0.25]])?;
        }
        let main_objects = if matches!(step, 1 | 2) {
            &objects[..]
        } else {
            &objects[..1]
        };
        let main_view = frame.draw_scene(
            &main,
            SceneInput::new(&meshes, &materials, main_objects)
                .with_sun(red)
                .with_bounds(world),
        )?;
        let depth = frame.graph().readback_image(
            "surface depth",
            main_view.depth.ok_or("main depth missing")?,
        )?;
        let color = frame.write_output(main.id, main_view.color, 1., None)?;
        let main_read = frame.graph().readback_image("main", color.raw_image())?;
        let preview_view = frame.draw_scene(
            &preview,
            SceneInput::new(&meshes, &materials, &objects[..1])
                .with_sun(green)
                .with_bounds(world),
        )?;
        let color = frame.write_output(preview.id, preview_view.color, 1., None)?;
        let preview_read = frame
            .graph()
            .readback_image("equipment", color.raw_image())?;
        let id = frame.submit()?.id();
        let mut result = drawing
            .runtime()
            .wait_for_frame(id, Duration::from_secs(15))?
            .ok_or("equipment lighting timeout")?;
        result.readback_into(main_read, &mut main_pixels)?;
        result.readback_into(depth, &mut depths)?;
        result.readback_into(preview_read, &mut equipment)?;
        assert!(
            depths
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| f32::from_le_bytes(*p) == 0.5)
        );
        let center = (16 * 32 + 16) * 4;
        if step == 0 {
            assert!(
                main_pixels[center] > 60 && main_pixels[center + 1] < 10,
                "red main light {:?}",
                &main_pixels[center..center + 4]
            );
            assert!(
                equipment[0] < 10 && equipment[1] > 60,
                "green preview light {:?}",
                &equipment[..4]
            );
            baseline = main_pixels;
            preview_baseline = equipment;
        } else {
            assert_eq!(equipment.as_slice(), preview_baseline.as_slice());
            if step == 1 {
                assert!(
                    main_pixels[center] + 20 < baseline[center],
                    "offscreen caster did not darken the receiver: {} -> {}",
                    baseline[center],
                    main_pixels[center]
                );
                assert_eq!(&main_pixels[..4], &baseline[..4]);
            } else if step == 2 {
                let right = (16 * 32 + 27) * 4;
                assert_eq!(main_pixels[center], baseline[center]);
                assert!(
                    i16::from(main_pixels[right]) + 20 < i16::from(baseline[right]),
                    "moving caster did not move its shadow"
                );
            } else {
                assert_eq!(main_pixels.as_slice(), baseline.as_slice());
            }
        }
    }
    let mut runtime = drawing.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    Ok(runtime)
}
