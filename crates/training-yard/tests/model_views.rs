//! Two squad members share geometry and materials. The monitor uses a cheaper
//! silhouette while the equipment view shows the individual armor slot.
#![cfg(all(
    feature = "graphics",
    any(feature = "vulkan", all(windows, feature = "d3d12"))
))]
use renderer::{
    core::{graph::ImageDesc, runtime::RuntimeConfig},
    material::{Materials, PbrParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    model::{Model, ModelInstance, ModelLod, ModelPart},
    pbr::{Bounds, PbrVertex, Projection, Transform, ViewInput},
    pipeline::{
        CameraGroup, CameraView, MeshIndexRange, PipelineResult, RenderObject, RendererSetup,
        SceneAssets, SceneInput, StandardConfig, Technique, ViewId,
    },
    unlit_forward::IDENTITY,
};
use std::time::Duration;

#[path = "../../renderer/examples/support/validation.rs"]
mod validation;

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
    let mesh = meshes.create(MeshData::pbr(
        &vertices,
        &[0, 1, 2, 1, 3, 2, 2, 3, 4, 3, 5, 4],
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
    let mut assets = asset_draws(&actors);
    // Load the optional emissive material along with the equipment.
    assets.push(RenderObject::new(mesh, glow));
    let mut setup = RendererSetup::standard(cameras[0]);
    setup.add_group(CameraGroup::standard(
        cameras[1],
        Technique::Forward,
        StandardConfig::new(cameras[1].extent),
    ));
    let mut plan = setup.prepare(SceneAssets::new(&mut meshes, &mut materials, &assets))?;
    for [w, h] in cameras.map(|c| c.extent) {
        plan.reserve_readback("viewport", ImageDesc::rgba8(w, h))?;
    }
    let mut drawing = plan.build(config)?;
    let mut objects = Vec::with_capacity(4);
    for step in 0..4 {
        match step {
            1 => actors[0].set_material(1, Some(glow))?,
            2 => {
                materials.update_pbr(body, emission([0., 1., 1.]))?;
                materials.collect_gpu(drawing.runtime())?;
                drawing.prepare_assets(SceneAssets::new(&mut meshes, &mut materials, &assets))?;
            }
            3 => actors[0].set_material(1, None)?,
            _ => {}
        }
        let mut frame = drawing.begin_frame()?;
        let mut reads = [None; 2];
        for (camera, read) in cameras.iter().zip(&mut reads) {
            objects.clear();
            objects.extend(actors.iter().flat_map(|actor| actor.draws(camera)));
            let input = SceneInput::new(&meshes, &materials, &objects);
            let hdr = frame.draw_scene(camera, input)?.color;
            let image = frame.write_output(camera.id, hdr, 1., None)?;
            *read = Some(
                frame
                    .graph()
                    .readback_image("viewport", image.raw_image())?,
            );
        }
        let ticket = frame.submit()?;
        let mut result = drawing
            .runtime()
            .wait_for_frame(ticket.id(), Duration::from_secs(15))?
            .ok_or("view timeout")?;
        for (camera, read) in cameras.iter().zip(reads) {
            let [w, h] = camera.extent.map(|n| n as usize);
            let mut pixels = vec![0; w * h * 4];
            result.readback_into(read.unwrap(), &mut pixels)?;
            for side in 0..2 {
                // Bins are black, red body, blue armor, green armor and cyan body.
                let mut counts = [0usize; 5];
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
                    }
                }
                let detailed = camera.id == cameras[0].id;
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
    }
    let mut runtime = drawing.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    validation::check_validation(runtime)
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Vulkan device"]
fn vulkan_model_views() -> PipelineResult<()> {
    check(RuntimeConfig::Vulkan(Default::default()))
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a D3D12 device"]
fn d3d12_model_views() -> PipelineResult<()> {
    check(RuntimeConfig::D3D12(Default::default()))
}
