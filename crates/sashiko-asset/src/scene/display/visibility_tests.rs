use super::*;
use crate::scene::SceneSpatialIndex;
use sashiko_renderer::{
    core::{
        runtime::{RuntimeConfig, VulkanConfig},
        vulkan::ValidationMode,
    },
    picking::PickPass,
    pipeline::{CameraGroup, RendererSetup, Technique},
};

/// Read the actual alpha-tested camera and cascade depth and the solid picking
/// IDs from a freshly prepared view, so previous shadow history cannot mask a miss.
fn read_planes(
    mut scene: DisplayScene,
    camera: CameraView,
    ids: &[u32],
) -> PipelineResult<Vec<Vec<u8>>> {
    let [w, h] = camera.extent;
    let dim = scene.settings.shadow_resolution;
    let mut group = CameraGroup::standard(camera, Technique::Forward, scene.config());
    group.output = None;
    let max = scene.objects.len();
    let mut plan = RendererSetup::new(group).prepare(scene.assets())?;
    let mut pick = PickPass::prepare(&mut plan, camera.extent, max, 0)?;
    const NAMES: [&str; 6] = [
        "placement IDs",
        "camera depth",
        "cascade 0",
        "cascade 1",
        "cascade 2",
        "cascade 3",
    ];
    let sizes = [
        ImageDesc::rgba8(w, h),
        ImageDesc::depth32(w, h),
        ImageDesc::depth32(dim, dim),
        ImageDesc::depth32(dim, dim),
        ImageDesc::depth32(dim, dim),
        ImageDesc::depth32(dim, dim),
    ];
    for (name, desc) in NAMES.into_iter().zip(sizes) {
        plan.reserve_readback(name, desc)?;
    }
    let mut renderer = plan.build(RuntimeConfig::Vulkan(VulkanConfig {
        validation: ValidationMode::Required,
        ..Default::default()
    }))?;
    scene.upload_materials(&mut renderer)?;
    renderer.prepare_assets(scene.assets())?;
    let mut frame = renderer.begin_frame()?;
    frame.draw_scene(&camera, scene.input())?;
    let images = frame.standard_output(camera.id).unwrap().scene;
    let ids = pick
        .record(&mut frame, &camera, scene.input(), ids)?
        .image();
    let [a, b, c, d] = images
        .shadows
        .ok_or("fixture requires directional shadow maps")?;
    let mut reads = Vec::new();
    for (image, name) in [ids, images.depth.unwrap(), a, b, c, d]
        .into_iter()
        .zip(NAMES)
    {
        reads.push(frame.graph().readback_image(name, image)?);
    }
    let submission = frame.submit()?;
    let mut result = renderer
        .runtime()
        .wait_for_frame(submission.id(), Duration::from_secs(120))?
        .ok_or("visibility GPU capture timed out")?;
    let mut planes = Vec::new();
    for read in reads {
        let mut bytes = vec![0; result.readback_len(read)?];
        result.readback_into(read, &mut bytes)?;
        planes.push(bytes);
    }
    drop(result);
    drop(submission);
    let mut runtime = renderer.into_runtime()?;
    scene.release(&mut runtime)?;
    let monitor = runtime.as_vulkan().unwrap().validation_monitor();
    drop(runtime);
    let report = monitor.report();
    assert!(
        report.core_validation == Some(true)
            && report.synchronization_validation == Some(true)
            && report.is_clean(),
        "{report:?}"
    );
    Ok(planes)
}

#[test]
#[ignore = "requires the cooked Landscape overview and a validated Vulkan device"]
fn fixed_selection_preserves_real_depth_ids_and_offscreen_shadows() -> PipelineResult<()> {
    let path = std::path::PathBuf::from(std::env::var("SASHIKO_VISIBILITY_PACKAGE")?);
    let source = ScenePackage::open(&path)?;
    let full = source.view("view-0")?.camera([3840, 2160], [0.02, 1500.])?;
    let camera = FilmProjection::crop(full, [930, 1340, 128, 128])?;
    let index = SceneSpatialIndex::new(&source, &ObjectStore::new(&path), 8.)?;
    let all = DisplayScene::open(&path, "view-0", camera.extent, None)?;
    let view = camera
        .camera
        .culling(all.bounds, all.sun, all.settings.direct_shadows())?;
    let mut keep = vec![false; all.objects.len()];
    let mut offscreen = 0;
    index.visit(&view, |p| {
        keep[p.placement] = true;
        offscreen += usize::from(!p.camera);
    });
    let ids: Vec<_> = (1..=u32::try_from(keep.len())?).collect();
    let selected: Vec<_> = ids
        .iter()
        .zip(&keep)
        .filter_map(|(&id, &keep)| keep.then_some(id))
        .collect();
    assert!(offscreen > 0 && selected.len() < ids.len());
    let baseline = read_planes(all, camera, &ids)?;
    let subset = DisplayScene::open(&path, "view-0", camera.extent, Some(&[camera]))?;
    let actual = read_planes(subset, camera, &selected)?;
    let mut occupied = Vec::new();
    for (i, (a, b)) in baseline.iter().zip(actual).enumerate() {
        assert_eq!(a.len(), b.len());
        assert!(
            a == &b,
            "plane {i} differs at byte {:?}",
            a.iter().zip(&b).position(|(x, y)| x != y)
        );
        let clear = if i == 0 { [0; 4] } else { 1f32.to_le_bytes() };
        occupied.push(a.as_chunks::<4>().0.iter().filter(|&&p| p != clear).count());
    }
    assert!(occupied[0] > 0 && occupied[1] > 0 && occupied[2..].iter().any(|&n| n > 0));
    println!(
        "placements {} -> {}, offscreen casters {offscreen}, occupied texels {occupied:?}",
        ids.len(),
        selected.len()
    );
    Ok(())
}
