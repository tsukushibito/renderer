use renderer::{
    capacity::Preparation,
    geometry::GeometryData,
    graph::{BufferDesc, Graph, GraphCapacity, GraphPool, SampleCount},
    program::{
        BlendState, CompareOp, D3D12InlineConstantBinding, D3D12RasterShaders, DepthState,
        ManagedRasterProgram, RasterShaderStages, ShaderBinaries,
    },
    runtime::{Runtime, RuntimeConfig, RuntimeLimits},
};
use renderer_pipeline::{
    material::{
        AlphaMode, MaterialDefinition, MaterialFactors, Materials, Parameter, PbrParams,
        UnlitParams, Value,
    },
    pbr,
    scene::{Draw, Frame, Mesh, Scene},
    unlit_forward::{self, IDENTITY},
};
use std::{error::Error, num::NonZeroUsize, time::Duration};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn toon(blend: bool) -> Result<MaterialDefinition> {
    let base = ManagedRasterProgram::mesh_hdr();
    let mut desc = base.descriptor();
    let stages = RasterShaderStages {
        vertex: &include_bytes!("../../../renderer/shaders/mesh.vert.spv")[..],
        fragment: Some(&include_bytes!("../../shaders/toon.frag.spv")[..]),
    };
    let dxil = D3D12RasterShaders {
        stages: RasterShaderStages {
            vertex: &include_bytes!("../../../renderer/shaders/dxil/mesh.vert.dxil")[..],
            fragment: Some(&include_bytes!("../../shaders/dxil/toon.frag.dxil")[..]),
        },
        inline_constants: Some(D3D12InlineConstantBinding {
            register: 0,
            space: 0,
        }),
    };
    desc.shaders = ShaderBinaries::both(stages, dxil);
    if blend {
        desc.blend = BlendState::Alpha;
        desc.depth = DepthState::Required {
            compare: CompareOp::Less,
            write: false,
            require_load: true,
        };
    }
    // SAFETY: the unchanged mesh vertex stage consumes its original position/color
    // layout and 80-byte transform/tint. toon.frag reads only TEXCOORD0 and
    // writes a single color, with no descriptors, side effects or indexing.
    let program = unsafe { ManagedRasterProgram::from_trusted_owned_descriptor(desc)? };
    Ok(MaterialDefinition::new(
        program,
        (!blend).then(ManagedRasterProgram::depth_only),
        if blend {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        },
        vec![Parameter::Constant {
            name: "color".into(),
            word: 16,
            default: Value::Color([1.; 4]),
        }],
    )?)
}
fn quad(pbr: bool) -> Result<GeometryData> {
    let points = [[-1., -1., 0.], [1., -1., 0.], [1., 1., 0.], [-1., 1., 0.]];
    let indices = [0, 1, 2, 2, 3, 0];
    if pbr {
        let mut bytes = Vec::new();
        for p in points {
            bytes
                .extend(pbr::PbrVertex::new(p, [0., 0., -1.], [1., 0., 0., 1.], [0., 0.])?.encode())
        }
        Ok(GeometryData::new(48, &bytes, &indices)?)
    } else {
        Ok(unlit_forward::mesh_data(
            &points.map(|position| unlit_forward::Vertex {
                position,
                color: [1.; 4],
            }),
            &indices,
        )?)
    }
}
fn upload(graph: &mut Graph, data: &GeometryData) -> Result<Mesh> {
    let vertices = graph.buffer(
        "vertices",
        BufferDesc {
            size: data.vertex_byte_len(),
        },
    )?;
    let indices = graph.buffer(
        "indices",
        BufferDesc {
            size: data.index_byte_len(),
        },
    )?;
    data.upload_vertices(graph, "vertices", vertices)?;
    data.upload_indices(graph, "indices", indices)?;
    Ok(Mesh {
        vertex_stride: data.vertex_stride(),
        vertices,
        indices,
        first_index: 0,
        index_count: data.index_count(),
    })
}
fn check(config: RuntimeConfig, technique: pbr::Technique) -> Result<()> {
    let opaque = toon(false)?;
    let blend = toon(true)?;
    let slot = |name: &str, group, binding, default| Parameter::Binding {
        name: name.into(),
        group,
        binding,
        default,
    };
    let textured = MaterialDefinition::new(
        ManagedRasterProgram::textured_mesh_hdr(),
        None,
        AlphaMode::Opaque,
        vec![
            Parameter::Constant {
                name: "color".into(),
                word: 16,
                default: Value::Color([1.; 4]),
            },
            slot("offset", 0, 0, None),
            slot("albedo", 1, 0, None),
            slot(
                "filter",
                1,
                1,
                Some(Value::Sampler(
                    renderer::program::SamplerDescriptor::NearestClamp,
                )),
            ),
        ],
    )?;
    let mut materials = Materials::default();
    let brick = |alpha_mode, alpha| PbrParams {
        factors: MaterialFactors {
            base_color: [0., 0., 0., alpha],
            emissive: [1., 0., 0.],
            double_sided: true,
            alpha_mode,
            ..Default::default()
        },
        ..Default::default()
    };
    let floor = materials.create_pbr(brick(AlphaMode::Opaque, 1.))?;
    let glass = materials.create_pbr(brick(AlphaMode::Blend, 0.5))?;
    let unlit = materials.create_unlit(UnlitParams {
        color: [0., 0., 1., 1.],
    })?;
    let green = materials.create_custom(&opaque, &[("color", Value::Color([0., 0.6, 0., 1.]))])?;
    let blue = materials.create_custom(&blend, &[("color", Value::Color([0., 0., 1., 0.5]))])?;
    let mut scene = Scene::new(
        technique,
        pbr::Config {
            extent: [32, 16],
            clear_color: [0.; 4],
            shadows: (technique == pbr::Technique::Deferred).then_some(pbr::ShadowSettings {
                resolution: 32,
                ..Default::default()
            }),
            output: None,
            samples: SampleCount::X1,
            camera_storage: pbr::CameraStorage::Transient,
        },
        pbr::SceneLimits {
            frames_in_flight: NonZeroUsize::MIN,
            max_draws: 7,
            max_materials: 6,
            max_casters_per_cascade: 1,
            max_transforms: 2,
        },
        &[opaque, blend, textured.clone()],
    )?;
    let geometry = [quad(true)?, quad(false)?];
    let mut prep = Preparation::new(NonZeroUsize::MIN);
    prep.add_alternatives(&[scene.preparation()])?;
    for mesh in &geometry {
        for size in [mesh.vertex_byte_len(), mesh.index_byte_len()] {
            prep.reserve_buffer(BufferDesc { size }, 1)?
        }
    }
    let offset_desc = BufferDesc { size: 16 };
    let map_desc = renderer::graph::ImageDesc::rgba8(1, 1);
    prep.reserve_buffer(offset_desc, 1)?;
    prep.reserve_image(map_desc, 2)?;
    let capacity = prep.graph_capacity(GraphCapacity::default())?;
    let mut pool = GraphPool::new(capacity, 1)?;
    let mut runtime = Runtime::new(
        config,
        RuntimeLimits {
            graph: capacity,
            ..Default::default()
        },
        prep.working_set(),
    )?;
    let (mut resources, id) = scene.defaults().initialize(&mut runtime)?;
    drop(
        runtime
            .wait_for_frame(id, Duration::from_secs(10))?
            .ok_or("default upload timed out")?,
    );
    // The resident uniform retains the startup upload's provenance. Keep its
    // graph storage separate from the slot recycled by normal frames.
    let mut startup = GraphPool::new(capacity, 1)?;
    let mut graph = startup.begin()?;
    let offset = graph.buffer("vertex offset", offset_desc)?;
    graph.upload_buffer("zero offset", offset, 0, &[0; 16])?;
    graph.export_buffer(offset)?;
    let mut map = |pixel: &[u8]| -> Result<_> {
        let image = graph.image("swatch", map_desc)?;
        graph.upload_image("color", image, pixel)?;
        graph.export_image(image)?;
        Ok(image)
    };
    let cyan = map(&[0, 255, 255, 255])?;
    let yellow = map(&[255, 255, 0, 255])?;
    let plan = graph.compile()?;
    let pending = runtime.submit(&plan)?;
    let offset = pending.export_buffer(offset)?;
    let cyan = pending.export_image(cyan)?;
    let yellow = pending.export_image(yellow)?;
    drop(
        runtime
            .wait_for_frame(pending.id(), Duration::from_secs(10))?
            .ok_or("texture upload timed out")?,
    );
    drop((pending, plan));
    let swatch = materials.create_custom(
        &textured,
        &[
            ("offset", Value::Uniform(offset.clone())),
            ("albedo", Value::Texture(cyan.clone())),
        ],
    )?;
    let mut bytes = [0u8; 32 * 16 * 8];
    let input = pbr::ViewInput::new(
        IDENTITY,
        [0., 0., -1.],
        [0., 0., 1.],
        [1., 2.],
        pbr::Projection::Orthographic,
    )?;
    for index in 0..4 {
        #[cfg(feature = "memory-profiling")]
        let before = renderer::measurement::Snapshot::now();
        let mut graph = pool.begin()?;
        let defaults = resources.import(&mut graph)?;
        let view = scene.prepare_view(
            input,
            pbr::Sun::new([0., 0., -1.], [0.; 3])?,
            pbr::Bounds::new([-1., -1., 0.], [1., 1., 1.])?,
            defaults.environment()?,
        )?;
        let pbr_mesh = upload(&mut graph, &geometry[0])?;
        let mesh = upload(&mut graph, &geometry[1])?;
        let draw = |mesh, mat, width, x, z| -> Result<Draw> {
            let transform =
                pbr::Transform::new([[width, 0., 0., x], [0., 1., 0., 0.], [0., 0., 1., z]])?;
            Ok(Draw {
                transform: Some(transform),
                casts_shadow: mat == green,
                ..Draw::new(mesh, mat)
            })
        };
        let draws = [
            draw(mesh, blue, 0.25, 0., 0.2)?,
            draw(mesh, unlit, 1., 0., 0.8)?,
            draw(mesh, green, 0.55, 0., 0.4)?,
            draw(pbr_mesh, glass, 0.25, 0., 0.3)?,
            draw(pbr_mesh, floor, 1., 0., 0.5)?,
            draw(mesh, unlit, 0.1, -0.7, 0.2)?,
            draw(mesh, swatch, 0.1, 0.8, 0.2)?,
        ];
        let images =
            scene.add_scene(&mut graph, &materials, Frame::new(&view, defaults, &draws))?;
        let read = graph.readback_image("mixed hdr", images.hdr)?;
        let shadows = images
            .shadows
            .map(|maps| graph.readback_image("mixed caster depth", maps[3]))
            .transpose()?;
        let compiled = graph.compile()?;
        let pending = runtime.submit(&compiled)?;
        if index == 0 {
            materials.update_custom(swatch, &[("albedo", Value::Texture(yellow.clone()))])?;
            materials.update_custom(green, &[("color", Value::Color([0., 0.3, 0., 1.]))])?;
        }
        let mut frame = runtime
            .wait_for_frame(pending.id(), Duration::from_secs(10))?
            .ok_or("mixed frame timed out")?;
        frame.readback_into(read, &mut bytes)?;
        if let Some(read) = shadows {
            let mut depth = [0; 32 * 32 * 4];
            frame.readback_into(read, &mut depth)?;
            assert!(
                depth
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| f32::from_le_bytes(*pixel) < 1.0),
                "custom caster wrote no depth"
            );
        }
        drop((frame, pending, compiled));
        #[cfg(feature = "memory-profiling")]
        if index >= 2 {
            let delta = renderer::measurement::Snapshot::now().since(before);
            assert!(delta.is_allocation_free(), "{delta:?}");
        }
        let pixel = |x: usize| -> [f32; 3] {
            let start = (32 * 8 + x) * 8;
            std::array::from_fn(|i| {
                let j = start + i * 2;
                let bits = u16::from_le_bytes([bytes[j], bytes[j + 1]]);
                if bits == 0 {
                    0.
                } else {
                    (1. + f32::from(bits & 1023) / 1024.) * 2f32.powi(i32::from(bits >> 10) - 15)
                }
            })
        };
        let green = if index == 0 { 2. / 3. } else { 1. / 3. };
        for (x, expected) in [
            (1, [1., 0., 0.]),
            (
                28,
                if index == 0 {
                    [0., 1., 1.]
                } else {
                    [1., 1., 0.]
                },
            ),
            (4, [0., 0., 1.]),
            (9, [0., green, 0.]),
            (16, [0.25, green * 0.25, 0.5]),
        ] {
            let actual = pixel(x);
            assert!(
                actual
                    .iter()
                    .zip(expected)
                    .all(|(a, e)| (a - e).abs() < 0.012),
                "{technique:?} frame {index} x={x}: {actual:?} expected {expected:?}"
            );
        }
    }
    drop(materials);
    for key in [offset.key(), cyan.key(), yellow.key()] {
        runtime.delete(key)?;
    }
    resources.clear(&mut runtime)?;
    super::runtime::check_validation(runtime)
}
#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires real Vulkan GPU"]
fn vulkan_mixed_materials() -> Result<()> {
    for t in [pbr::Technique::Forward, pbr::Technique::Deferred] {
        check(RuntimeConfig::Vulkan(Default::default()), t)?
    }
    Ok(())
}
#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires real D3D12 GPU"]
fn d3d12_mixed_materials() -> Result<()> {
    for t in [pbr::Technique::Forward, pbr::Technique::Deferred] {
        check(RuntimeConfig::D3D12(Default::default()), t)?
    }
    Ok(())
}
