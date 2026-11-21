//! Two squad members share geometry and materials. The monitor uses a cheaper
//! silhouette while the equipment view shows the individual armor slot.
#![cfg(all(
    feature = "graphics",
    any(feature = "vulkan", all(windows, feature = "d3d12"))
))]
use renderer::{
    canvas::{self, Layer, Paint},
    core::{
        ImageFormat,
        graph::{GraphPool, ImageDesc, LoadOp, RasterRect, SampleCount},
        runtime::RuntimeConfig,
    },
    material::{Materials, PbrParams},
    mesh::{JointWeights, MeshCapacity, MeshData, Meshes},
    model::{Model, ModelInstance, ModelLod, ModelPart},
    pbr::{Bounds, PbrVertex, Projection, ShadowSettings, Sun, Transform, ViewInput},
    pipeline::{
        CameraGroup, CameraView, MeshIndexRange, PipelineResult, RenderObject, RendererSetup,
        SceneAssets, SceneInput, StandardConfig, Technique, ViewId,
    },
    unlit_forward::IDENTITY,
};
use std::time::Duration;

#[path = "../../renderer/examples/support/validation.rs"]
mod validation;

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: renderer::core::measurement::CountingAllocator<std::alloc::System> =
    renderer::core::measurement::CountingAllocator::new(std::alloc::System);

fn emission(rgb: [f32; 3]) -> PbrParams {
    let mut params = PbrParams::color([0., 0., 0., 1.]);
    params.factors.double_sided = true;
    params.factors.emissive = rgb;
    params
}

fn asset_draws(actors: &[ModelInstance]) -> Vec<RenderObject> {
    actors.iter().flat_map(ModelInstance::asset_draws).collect()
}

fn check(config: RuntimeConfig) -> PipelineResult<()> {
    let projection = ViewInput::new(
        IDENTITY,
        [0., 0., -1.],
        [0., 0., 1.],
        [1., 2.],
        Projection::Orthographic,
    )?;
    let cameras = [[48, 24], [12, 6]].map(|extent| CameraView {
        id: ViewId(extent[0].into()),
        extent,
        camera: projection,
    });
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let vertices: Vec<_> = [-0.75, 0., 0.75]
        .into_iter()
        .flat_map(|y| {
            [-0.35, 0.35].map(|x| {
                PbrVertex::new([x, y, 0.5], [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap()
            })
        })
        .collect();
    let weights = vertices
        .iter()
        .map(|v| {
            let w = (v.position()[1] + 0.75) / 1.5;
            JointWeights {
                joints: [0, 1, 0, 0],
                weights: [1. - w, w, 0., 0.],
            }
        })
        .collect::<Vec<_>>();
    let mesh = meshes.create(MeshData::skinned_pbr(
        &vertices,
        &[0, 1, 2, 1, 3, 2, 2, 3, 4, 3, 5, 4],
        2,
        &weights,
    )?)?;
    let mut materials = Materials::default();
    let body = materials.create_pbr(emission([1., 0., 0.]))?;
    let armor = materials.create_pbr(emission([0., 0., 1.]))?;
    let glow = materials.create_pbr(emission([0., 1., 0.]))?;
    let body_part = ModelPart {
        mesh,
        indices: Some(MeshIndexRange::new(0..6)?),
        material_slot: 0,
    };
    let armor_part = ModelPart {
        mesh,
        indices: Some(MeshIndexRange::new(6..12)?),
        material_slot: 1,
    };
    let model = Model::new(
        Bounds::new([-0.35, -0.75, 0.5], [0.35, 0.75, 0.5])?,
        vec![body, armor],
        vec![
            ModelLod {
                min_pixels: 10.,
                parts: vec![body_part, armor_part],
            },
            ModelLod {
                min_pixels: 0.,
                parts: vec![body_part],
            },
        ],
    )?;
    let mut actors = [-0.5, 0.5].map(|x| {
        let mut actor = model.instantiate();
        actor.transform =
            Transform::new([[1., 0., 0., x], [0., 1., 0., 0.], [0., 0., 1., 0.]]).unwrap();
        actor
    });
    let poses = [meshes.create_pose(mesh)?, meshes.create_pose(mesh)?];
    for (actor, pose) in actors.iter_mut().zip(poses) {
        actor.set_pose(mesh, Some(pose))?;
    }
    let mut assets = asset_draws(&actors);
    // Load the optional emissive material along with the equipment.
    assets.push(RenderObject::new(mesh, glow));
    let world_bounds = Bounds::new([-1., -1., 0.], [1., 1., 1.])?;
    let sun = Sun::new([0., 0., -1.], [0.01; 3])?;
    let mut detailed = StandardConfig::new(cameras[0].extent);
    detailed.shadows = Some(ShadowSettings {
        resolution: 32,
        distance: 2.,
        ..Default::default()
    });
    let mut setup = RendererSetup::new(CameraGroup::standard(
        cameras[0],
        Technique::Forward,
        detailed,
    ));
    setup.add_group(CameraGroup::standard(
        cameras[1],
        Technique::Forward,
        StandardConfig::new(cameras[1].extent),
    ));
    let mut plan = setup.prepare(SceneAssets::new(&mut meshes, &mut materials, &assets))?;
    for [w, h] in cameras.map(|c| c.extent) {
        plan.reserve_readback("viewport", ImageDesc::rgba8(w, h))?;
    }
    for _ in 0..renderer::pbr::CASCADE_COUNT {
        plan.reserve_readback("caster depth", ImageDesc::depth32(32, 32))?;
    }
    plan.preparation_mut()
        .reserve_raster_pass("canvas", SampleCount::X1, |pass| {
            pass.attachment()?;
            pass.draws(&canvas::programs(ImageFormat::Rgba8Unorm)?[1], 1)
        })?;
    let mut drawing = plan.build(config)?;
    let mut host_pool = GraphPool::new(drawing.runtime().limits().graph, 1)?;
    let mut objects = Vec::with_capacity(4);
    let mut rest_shadow = vec![false; 32 * 32 * renderer::pbr::CASCADE_COUNT];
    let mut depth = Vec::with_capacity(rest_shadow.len());
    let mut rest_mask = [vec![false; 24 * 24], vec![false; 24 * 24]];
    let mut mask = Vec::with_capacity(rest_mask[0].len());
    let mut shadow_reads = Vec::with_capacity(renderer::pbr::CASCADE_COUNT);
    let mut storage = vec![0; 48 * 24 * 4];
    for step in 0..7 {
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        let before = renderer::core::measurement::Snapshot::now();
        match step {
            1 => actors[0].set_material(1, Some(glow))?,
            2 => {
                materials.update_pbr(body, emission([0., 1., 1.]))?;
                materials.collect_gpu(drawing.runtime())?;
                drawing.prepare_assets(SceneAssets::new(&mut meshes, &mut materials, &assets))?;
            }
            3 => actors[0].set_material(1, None)?,
            4..=6 => {
                for (side, pose) in poses.iter().enumerate() {
                    let dy = match (step, side) {
                        (4, 0) => -0.4,
                        (5, 1) => 0.2,
                        _ => 0.,
                    };
                    let joint =
                        Transform::new([[1., 0., 0., 0.], [0., 1., 0., dy], [0., 0., 1., 0.]])?;
                    meshes.set_joint_transforms(*pose, &[Transform::IDENTITY, joint])?;
                }
                assert!(meshes.set_joint_transforms(poses[0], &[]).is_err());
            }
            _ => {}
        }
        let mut graph = host_pool.begin()?;
        let target = graph.image("host viewport", ImageDesc::rgba8(48, 24))?;
        let (mut frame, runtime) = drawing.begin_host_frame(graph)?;
        let mut reads = [None; 2];
        for (camera, read) in cameras.iter().zip(&mut reads) {
            objects.clear();
            objects.extend(actors.iter().flat_map(|actor| actor.draws(camera)));
            let input = SceneInput::new(&meshes, &materials, &objects)
                .with_bounds(world_bounds)
                .with_sun(sun);
            let hdr = frame.draw_scene(camera, input)?.color;
            let main = camera.id == cameras[0].id;
            let image = frame.write_output(camera.id, hdr, 1., main.then_some(target))?;
            if !main {
                *read = Some(
                    frame
                        .graph()
                        .readback_image("viewport", image.raw_image())?,
                );
            }
        }
        let shadows = frame
            .standard_output(cameras[0].id)
            .unwrap()
            .scene
            .shadows
            .unwrap();
        shadow_reads.clear();
        for shadow in shadows {
            shadow_reads.push(frame.graph().readback_image("caster depth", shadow)?);
        }
        let (mut graph, mut views) = frame.finish_recording()?;
        canvas::record(
            &mut graph,
            target,
            LoadOp::Load,
            &[Layer {
                paint: Paint::Color([1.; 4]),
                rect: RasterRect::new(0, 0, 1, 1),
                clip: None,
            }],
        )?;
        reads[0] = Some(graph.readback_image("viewport", target)?);
        let ticket = runtime.submit(&graph.compile()?)?;
        views.publish(&ticket)?;
        drop(views);
        let mut result = runtime
            .wait_for_frame(ticket.id(), Duration::from_secs(15))?
            .ok_or("view timeout")?;
        depth.clear();
        for &read in &shadow_reads {
            let mut layer = [0; 32 * 32 * 4];
            result.readback_into(read, &mut layer)?;
            depth.extend(
                layer
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&v| f32::from_le_bytes(v) < 1.),
            );
        }
        if step == 0 {
            assert!(depth.iter().any(|&occupied| occupied), "empty cascades");
            rest_shadow.copy_from_slice(&depth);
        } else {
            assert_eq!(
                depth != rest_shadow,
                (4..=5).contains(&step),
                "caster coverage at step {step}"
            );
        }
        for (camera, read) in cameras.iter().zip(reads) {
            let [w, h] = camera.extent.map(|n| n as usize);
            let pixels = &mut storage[..w * h * 4];
            result.readback_into(read.unwrap(), pixels)?;
            if camera.id == cameras[0].id {
                assert_eq!(pixels[..4], [255; 4]);
                pixels[..4].copy_from_slice(&[0, 0, 0, 255]);
            }
            for (side, baseline) in rest_mask.iter_mut().enumerate() {
                // Bins are black, red body, blue armor, green armor and cyan body.
                let mut counts = [0usize; 5];
                mask.clear();
                for row in pixels.chunks_exact(w * 4) {
                    for p in row[side * w * 2..(side + 1) * w * 2].as_chunks::<4>().0 {
                        let bin = match [p[0] > 100, p[1] > 100, p[2] > 100] {
                            [false, false, false] => 0,
                            [true, false, false] => 1,
                            [false, false, true] => 2,
                            [false, true, false] => 3,
                            [false, true, true] => 4,
                            rgb => panic!("unexpected color {rgb:?}"),
                        };
                        counts[bin] += 1;
                        mask.push(bin != 0);
                    }
                }
                let detailed = camera.id == cameras[0].id;
                if detailed {
                    if step == 0 {
                        baseline.copy_from_slice(&mask);
                    } else {
                        assert_eq!(
                            mask != *baseline,
                            (step, side) == (4, 0) || (step, side) == (5, 1),
                            "posed silhouette at step {step} side {side}"
                        );
                    }
                }
                let glowing = side == 0 && (1..=2).contains(&step);
                assert_eq!(
                    counts.map(|n| n > 0),
                    [
                        true,
                        step < 2,
                        detailed && !glowing,
                        detailed && glowing,
                        step >= 2
                    ],
                    "step {step} side {side} size {w}x{h}: {counts:?}"
                );
            }
        }
        drop((result, ticket));
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        if step >= 4 {
            let delta = renderer::core::measurement::Snapshot::now().since(before);
            assert!(
                delta.is_allocation_free(),
                "animated frame {step}: {delta:?}"
            );
        }
    }
    let mut runtime = drawing.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    validation::check_validation(runtime)
}

#[test]
#[ignore = "requires real devices for each enabled backend"]
fn posed_models_in_two_views() -> PipelineResult<()> {
    #[cfg(feature = "vulkan")]
    check(RuntimeConfig::Vulkan(Default::default()))?;
    #[cfg(all(windows, feature = "d3d12"))]
    check(RuntimeConfig::D3D12(Default::default()))?;
    Ok(())
}
