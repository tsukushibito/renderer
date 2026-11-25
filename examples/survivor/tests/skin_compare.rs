//! Same indexed Survivor model, palette, target and draw count on both GPU paths.
#![cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
use sashiko::animation::CharacterCatalog;
use sashiko_renderer::core as sashiko_renderer_core;
use sashiko_renderer_core::{
    ImageFormat,
    capacity::{GraphRequirements, Preparation},
    graph::*,
    profiling::GpuTiming,
    program::*,
    runtime::{Runtime, RuntimeConfig, RuntimeLimits},
};
use std::{
    error::Error,
    num::{NonZeroU32, NonZeroUsize},
    path::Path,
    time::{Duration, Instant},
};
mod validation {
    use super::sashiko_renderer_core;
    include!("../../../crates/sashiko-renderer/examples/support/validation.rs");
}
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const PALETTE: usize = 128 * 96;
const SIDE: u32 = 128;

const fn attribute(location: u32, offset: u32, format: VertexFormat) -> VertexAttribute {
    VertexAttribute {
        stream: 0,
        location,
        offset,
        format,
    }
}
const ATTRS: [VertexAttribute; 6] = [
    attribute(0, 0, VertexFormat::Float32x3),
    attribute(1, 12, VertexFormat::Float32x3),
    attribute(2, 24, VertexFormat::Float32x4),
    attribute(3, 40, VertexFormat::Float32x2),
    attribute(4, 48, VertexFormat::Float32x4),
    attribute(5, 64, VertexFormat::Float32x4),
];
macro_rules! shader {
    ($file:literal) => {
        include_bytes!(concat!("../../../crates/sashiko-renderer/shaders/", $file))
    };
}
macro_rules! stages {
    ($vert:literal) => {
        sashiko_renderer_core::program::embedded_shader_binaries!(
            RasterShaderStages {
                vertex: include_bytes!(concat!(
                    "../../../crates/sashiko-renderer/shaders/",
                    $vert,
                    ".spv"
                )),
                fragment: Some(shader!("pick.frag.spv"))
            },
            D3D12RasterShaders {
                stages: RasterShaderStages {
                    vertex: include_bytes!(concat!(
                        "../../../crates/sashiko-renderer/shaders/dxil/",
                        $vert,
                        ".dxil"
                    )),
                    fragment: Some(shader!("dxil/pick.frag.dxil"))
                },
                inline_constants: None
            }
        )
    };
}

#[allow(unsafe_code)]
fn program(vertex: bool) -> ManagedRasterProgram {
    static POSED: RasterProgramDescriptor = RasterProgramDescriptor {
        entry: "main",
        vertex_stride: 48,
        secondary_vertex_stride: 0,
        vertex_attributes: ATTRS.split_at(4).0,
        parameters: RasterParameterLayout {
            size: 0,
            stages: ShaderStages::EMPTY,
        },
        color_outputs: &[ImageFormat::Rgba8Unorm],
        bindings: &[],
        binding_bytes: &[],
        sampling_pairs: &[],
        blend: BlendState::Replace,
        depth: DepthState::Disabled,
        shaders: stages!("skin_compare.vert"),
    };
    static VERTEX: RasterProgramDescriptor = RasterProgramDescriptor {
        vertex_stride: 80,
        vertex_attributes: &ATTRS,
        bindings: &[BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::Uniform,
            min_size: PALETTE as u64,
            stages: ShaderStages::VERTEX,
        }],
        binding_bytes: &[PALETTE as u64],
        shaders: stages!("skin_compare_vertex.vert"),
        ..POSED
    };
    // SAFETY: reviewed binary inputs match ATTRS. All skin palette indices
    // are masked to 0..128, and the fragment shader only writes its input color.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(if vertex { &VERTEX } else { &POSED }) }
}

#[derive(Clone, Copy, Debug)]
struct Crowd {
    objects: usize,
    poses: usize,
    visible: usize,
}

fn compare(mut api: RuntimeConfig, vertex: bool, views: usize, crowd: Crowd) -> Result<Vec<u8>> {
    let catalog = CharacterCatalog::open(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/survivor/characters/catalog.json"),
    )?;
    let model = catalog.load(catalog.id("primary").unwrap())?;
    let clip = model.clip("walk").unwrap();
    let skin = model.model.mesh_data()?.prepare_compute_skin()?;
    // Current raster formats accept float attributes. Convert the normalized
    // compute input once at load so both paths see exactly the same weights.
    let mut vs = Vec::new();
    if vertex {
        for record in skin
            .bind_bytes()
            .as_chunks::<72>()
            .0
            .iter()
            .take(model.model.vertices().count())
        {
            vs.extend_from_slice(&record[..48]);
            for idx in record[48..56].as_chunks::<2>().0 {
                vs.extend_from_slice(&f32::from(u16::from_le_bytes(*idx)).to_le_bytes());
            }
            vs.extend_from_slice(&record[56..]);
        }
    }
    let input = if vertex { &vs[..] } else { skin.bind_bytes() };
    let indices: Vec<_> = model
        .model
        .indices()
        .iter()
        .flat_map(|i| i.to_le_bytes())
        .collect();
    let raster = program(vertex);
    let size = |len: usize| BufferDesc { size: len as u64 };
    let image = ImageDesc::rgba8(SIDE, SIDE);
    let mut prep = Preparation::new(NonZeroUsize::MIN);
    prep.reserve_buffer(size(input.len()), 1)?;
    prep.reserve_buffer(size(indices.len()), 1)?;
    for _ in 0..crowd.poses {
        if vertex {
            prep.reserve_buffer(size(PALETTE), 1)?;
        } else {
            skin.reserve_pose(&mut prep)?;
        }
    }
    prep.reserve_transient_image("target", image)?;
    for _ in 0..views {
        prep.reserve_raster_pass("view", SampleCount::X1, |pass| {
            pass.attachment()?;
            pass.draws(&raster, crowd.visible)
        })?;
    }
    let setup_bytes = input.len() + indices.len();
    prep.add_graph_requirements(GraphRequirements {
        // The input and index imports remain live alongside every visible palette.
        resources: 2 + if vertex { crowd.poses } else { 0 },
        passes: 2 + if vertex { crowd.poses } else { 0 },
        accesses: 2 + if vertex { crowd.poses } else { 0 },
        upload_bytes: setup_bytes + if vertex { PALETTE * crowd.poses } else { 0 },
        ..Default::default()
    })?;
    let capacity = prep.graph_capacity(GraphCapacity::default())?;
    api.fit_preparation(
        &prep,
        setup_bytes.max(PALETTE * crowd.poses) as u64,
        u64::from(SIDE * SIDE * 4),
    )?;
    let mut runtime = Runtime::new(
        api,
        RuntimeLimits {
            graph: capacity,
            ..Default::default()
        },
        prep.working_set(),
    )?;
    let mut pool = GraphPool::new(capacity, 2)?;
    let timeout = Duration::from_secs(30);
    let (mut load, mut resident) = (pool.begin()?, Vec::new());
    for bytes in [input, &indices] {
        let key = runtime.register_buffer("resident", size(bytes.len()))?;
        let handle = load.import_buffer("resident", &runtime.refresh_buffer(key)?)?;
        load.upload_buffer("initialize", handle, 0, bytes)?;
        load.export_buffer(handle)?;
        resident.push((key, handle));
    }
    let submitted = runtime.submit(&load.compile()?)?;
    let bind = submitted.export_buffer(resident[0].1)?;
    let ib = submitted.export_buffer(resident[1].1)?;
    runtime
        .wait_for_frame(submitted.id(), timeout)?
        .ok_or("startup timeout")?;
    runtime.set_profiling(true)?;
    let mut times = Vec::new();
    let mut pixels = vec![0; (SIDE * SIDE * 4) as usize];
    let mut frames = Vec::with_capacity(32);
    let mut streams = vec![None; crowd.poses];
    let mut draws = Vec::with_capacity(crowd.visible);
    for tick in 0..41 {
        let start = Instant::now();
        streams.fill(None);
        draws.clear();
        let mut graph = pool.begin()?;
        let rest = graph.import_readonly_buffer("bind", &bind)?;
        let indices = graph.import_readonly_buffer("indices", &ib)?;
        // Rotate a known visible set; materialize each distinct palette once.
        for object in 0..crowd.visible {
            let pose = ((object + tick as usize) % crowd.objects) % crowd.poses;
            if let Some(draw) = streams[pose] {
                draws.push(draw);
                continue;
            }
            let palette = clip.sample(tick + pose as u64 * 7, NonZeroU32::new(60).unwrap());
            let (vertices, bindings) = if vertex {
                let mut bytes = [0; PALETTE];
                for (dest, matrix) in bytes.as_chunks_mut::<96>().0.iter_mut().zip(palette) {
                    *dest = matrix.encode();
                }
                let buffer = graph.buffer("palette", size(PALETTE))?;
                graph.upload_buffer("palette", buffer, 0, &bytes)?;
                (
                    rest,
                    Some(graph.raster_bindings(
                        &raster,
                        &[Binding {
                            group: 0,
                            binding: 0,
                            resource: BindingResource::Buffer {
                                buffer,
                                offset: 0,
                                size: PALETTE as u64,
                            },
                        }],
                    )?),
                )
            } else {
                (skin.add_pose(&mut graph, rest, palette)?, None)
            };
            let draw = RasterDraw {
                vertices,
                indices,
                bindings,
                first_index: 0,
                index_count: model.model.indices().len() as u32,
                instance_count: 1,
                secondary_vertices: None,
                parameters: Default::default(),
            };
            streams[pose] = Some(draw);
            draws.push(draw);
        }
        let target = graph.image("target", image)?;
        for view in 0..views {
            graph.raster(
                "view",
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(target, 0, 0)?,
                        load: if view == 0 {
                            LoadOp::Clear([0.; 4])
                        } else {
                            LoadOp::Load
                        },
                        store: StoreOp::Store,
                    }],
                    color_resolves: &[],
                    depth: None,
                },
                |pass| pass.draw(&raster, draws.iter().copied()),
            )?;
        }
        graph.export_frame_image(target)?;
        let read = (tick == 40)
            .then(|| graph.readback_image("check", target))
            .transpose()?;
        let plan = graph.compile()?;
        assert_eq!(
            plan.ordered_pass_names().filter(|&s| s == "view").count(),
            views
        );
        let frame = runtime.submit(&plan)?;
        let mut done = runtime
            .wait_for_frame(frame.id(), timeout)?
            .ok_or("draw timed out")?;
        if let Some(read) = read {
            done.readback_into(read, &mut pixels)?;
        } else if tick >= 8 {
            let GpuTiming::Available { elapsed, .. } = done.metrics().ok_or("missing metrics")?.gpu
            else {
                return Err("timestamp unavailable".into());
            };
            times.push(elapsed);
            frames.push(start.elapsed());
        }
    }
    times.sort_unstable();
    frames.sort_unstable();
    println!(
        "vertex={vertex} passes={views} {crowd:?}, median GPU {:?}, build-to-completion {:?}, resident input {} B, used posed cache {} B",
        times[times.len() / 2],
        frames[frames.len() / 2],
        input.len(),
        if vertex {
            0
        } else {
            skin.bind_bytes().len() / 72 * 48 * crowd.visible.min(crowd.poses)
        }
    );
    for (key, _) in resident {
        runtime.delete(key)?;
    }
    validation::check_validation(runtime)?;
    Ok(pixels)
}

fn check(api: RuntimeConfig) -> Result<()> {
    for (objects, poses, visible) in [(1, 1, 1), (16, 1, 16), (16, 16, 4), (16, 16, 16)] {
        let crowd = Crowd {
            objects,
            poses,
            visible,
        };
        for views in [1, 4] {
            let cs = compare(api, false, views, crowd)?;
            let vs = compare(api, true, views, crowd)?;
            assert!(cs.as_chunks::<4>().0.iter().filter(|p| p[3] != 0).count() > 100);
            assert!(cs.iter().zip(vs).all(|(a, b)| a.abs_diff(b) <= 1));
        }
    }
    Ok(())
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires Vulkan timestamps"]
fn vulkan_skin_comparison() -> Result<()> {
    check(RuntimeConfig::Vulkan(Default::default()))
}
#[cfg(feature = "d3d12")]
#[test]
#[ignore = "requires D3D12 timestamps"]
fn d3d12_skin_comparison() -> Result<()> {
    check(RuntimeConfig::D3D12(Default::default()))
}
