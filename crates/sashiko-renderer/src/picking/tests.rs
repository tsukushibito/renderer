use super::*;
use crate::{
    mesh::{JointWeights, MeshData},
    pbr::{PbrVertex, Transform},
};
use sashiko_renderer_core::{
    capacity::{GraphRequirements, Preparation},
    graph::{BufferDesc, GraphCapacity, GraphPool},
    runtime::{Runtime, RuntimeConfig, RuntimeLimits},
};
use std::{error::Error, num::NonZeroUsize, time::Duration};
#[path = "../../examples/support/validation.rs"]
mod validation;

// Two independent views consume one compute output. Read back both raster
// results and every attribute, including the tail of a second workgroup.
fn compute_skin_views(api: RuntimeConfig) -> Result<(), Box<dyn Error>> {
    let vertices: Vec<_> = (0..65)
        .map(|i| {
            let [x, y] = [[-0.6, -0.6], [0.6, -0.6], [0., 0.6]][i % 3];
            PbrVertex::new(
                [x + 2.0, y, 0.5],
                [0., 0., 1.],
                [1., 1., 0., -1.],
                [i as f32 / 65., 0.25],
            )
        })
        .collect::<Result<_, _>>()?;
    let palette = [
        Transform::IDENTITY,
        Transform::new([[-2., 0., 0., 3.5], [0., 2., 0., 0.], [0., 0., 0.5, 0.]])?,
    ];
    let weights = JointWeights {
        joints: [0, 1, u16::MAX, 0],
        weights: [1., 3., 0., 0.],
    };
    let mesh = MeshData::skinned_pbr(&vertices, &[0, 64, 2], 2, &[weights; 65])?;
    let skin = mesh.prepare_compute_skin()?;
    let bind_desc = BufferDesc {
        size: skin.bind_bytes().len() as u64,
    };
    let index_desc = BufferDesc { size: 12 };
    let mut prep = Preparation::new(NonZeroUsize::MIN);
    skin.reserve_pose(&mut prep)?;
    prep.reserve_buffer(bind_desc, 1)?;
    prep.reserve_buffer(index_desc, 1)?;
    prep.add_graph_requirements(GraphRequirements {
        upload_bytes: skin.bind_bytes().len() + 12,
        ..Default::default()
    })?;
    let color = ImageDesc::rgba8(8, 8);
    let depth = ImageDesc::depth32(8, 8);
    let pick = program(48)?;
    for view in ["first view", "second view"] {
        prep.reserve_raster_pass(view, SampleCount::X1, |pass| {
            pass.transient_attachment(COLOR, color)?;
            pass.transient_attachment(DEPTH, depth)?;
            pass.draws(&pick, 1)
        })?;
    }
    let capacity = prep.graph_capacity(GraphCapacity::default())?;
    let mut runtime = Runtime::new(
        api,
        RuntimeLimits {
            graph: capacity,
            ..Default::default()
        },
        prep.working_set(),
    )?;
    let mut pool = GraphPool::new(capacity, 2)?;
    let key = runtime.register_buffer("bind vertices", bind_desc)?;
    let uploaded = {
        let mut load = pool.begin()?;
        let buffer = load.import_buffer("bind vertices", &runtime.refresh_buffer(key)?)?;
        load.upload_buffer("bind data", buffer, 0, skin.bind_bytes())?;
        load.export_buffer(buffer)?;
        let frame = runtime.submit(&load.compile()?)?;
        (frame.id(), frame.export_buffer(buffer)?)
    };
    let mut graph = pool.begin()?;
    let bind = graph.import_buffer("resident bind vertices", &uploaded.1)?;
    let indices = graph.buffer("indices", index_desc)?;
    graph.upload_buffer(
        "indices",
        indices,
        0,
        [0u32, 64, 2].map(u32::to_le_bytes).as_flattened(),
    )?;
    let posed = skin.add_pose(&mut graph, bind, &palette)?;
    let mut images = Vec::new();
    for (view, id) in [("first view", 3), ("second view", 7)] {
        let image = graph.image(COLOR, color)?;
        let z = graph.image(DEPTH, depth)?;
        let mut words = [0; 29];
        for i in [0, 5, 10, 15, 16, 21, 26] {
            words[i] = 1f32.to_bits();
        }
        words[28] = id;
        graph.raster(
            view,
            RasterAttachments {
                colors: &[Attachment {
                    view: graph.image_view(image, 0, 0)?,
                    load: LoadOp::Clear([0.; 4]),
                    store: StoreOp::Store,
                }],
                color_resolves: &[],
                depth: Some(Attachment {
                    view: graph.image_view(z, 0, 0)?,
                    load: LoadOp::Clear(1.),
                    store: StoreOp::Discard,
                }),
            },
            |pass| {
                pass.draw(
                    &pick,
                    [&RasterDraw {
                        bindings: None,
                        secondary_vertices: None,
                        vertices: posed,
                        indices,
                        first_index: 0,
                        index_count: 3,
                        instance_count: 1,
                        parameters: RasterConstants::new(&words)?,
                    }],
                )
            },
        )?;
        images.push((graph.readback_image("picked pixels", image)?, id));
    }
    let read = graph.readback_buffer("skinned stream", posed, 0, 128 * 48)?;
    let plan = graph.compile()?;
    assert_eq!(
        plan.ordered_pass_names()
            .filter(|&s| s == "compute skin vertices")
            .count(),
        1
    );
    let sent = runtime.submit(&plan)?;
    runtime
        .wait_for_frame(uploaded.0, Duration::from_secs(15))?
        .ok_or("bind upload timed out")?;
    let mut done = runtime
        .wait_for_frame(sent.id(), Duration::from_secs(15))?
        .ok_or("skin timed out")?;
    for (image, id) in images {
        let mut pixels = [0; 8 * 8 * 4];
        done.readback_into(image, &mut pixels)?;
        assert_eq!(pixels.as_chunks::<4>().0[4 * 8 + 4], id.to_le_bytes());
    }
    let mut bytes = [0; 128 * 48];
    done.readback_into(read, &mut bytes)?;
    for (vertex, actual) in vertices.iter().zip(bytes.as_chunks::<48>().0) {
        let expected = vertex
            .skinned(palette.into_iter().zip([0.25, 0.75]))?
            .encode();
        for (a, b) in actual
            .as_chunks::<4>()
            .0
            .iter()
            .zip(expected.as_chunks::<4>().0)
        {
            assert!((f32::from_le_bytes(*a) - f32::from_le_bytes(*b)).abs() < 1e-5);
        }
    }
    assert!(bytes[65 * 48..].iter().all(|&byte| byte == 0));
    drop(done);
    runtime.delete(key)?;
    validation::check_validation(runtime)
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Vulkan GPU"]
fn vulkan_compute_skin_views() -> Result<(), Box<dyn Error>> {
    compute_skin_views(RuntimeConfig::Vulkan(
        sashiko_renderer_core::runtime::VulkanConfig {
            staging_bytes: 65536,
            ..Default::default()
        },
    ))
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a D3D12 GPU"]
fn d3d12_compute_skin_views() -> Result<(), Box<dyn Error>> {
    compute_skin_views(RuntimeConfig::D3D12(
        sashiko_renderer_core::runtime::D3D12Config {
            upload_bytes: 65536,
            readback_bytes: 65536,
            max_buffer_bytes: 65536,
            ..Default::default()
        },
    ))
}
