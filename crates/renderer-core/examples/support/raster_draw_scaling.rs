use std::{
    collections::VecDeque,
    error::Error,
    ffi::OsString,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use renderer_core::{
    GpuTiming, ImageFormat, SubmissionMetrics,
    d3d12::{
        D3D12BufferClass, D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
        D3D12Submission, D3D12SubmissionResult, D3D12ValidationMode,
    },
    geometry::GeometryData,
    graph::{
        Attachment, Binding, BindingResource, BufferDesc, BufferHandle, CompiledGraph, Graph,
        GraphCapacity, GraphPool, ImageDesc, ImageHandle, ImportBuffer, LoadOp, PassHandle,
        RasterAttachments, RasterConstants, RasterDraw, RasterParameters, ResourceKey, StoreOp,
    },
    program::{
        BindingLayout, BindingType, BlendState, D3D12RasterShaders, DepthState,
        ManagedRasterProgram, RasterParameterLayout, RasterProgramDescriptor, RasterShaderStages,
        ShaderBinaries, ShaderStages, VertexAttribute, VertexFormat,
    },
    vulkan::{
        FrameCapacity, ResourceClass, RuntimeConfig, Submission, SubmissionResult, ValidationMode,
        VulkanRuntime,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Failure = Box<dyn Error>;
const EXTENT: u32 = 512;
const IN_FLIGHT: usize = 2;
const WAIT: Duration = Duration::from_secs(30);
const STAGING_BYTES: u64 = 8 * 1024 * 1024;
const STRICT: bool = cfg!(feature = "development-validation");
const UNIFORM: BufferDesc = BufferDesc { size: 16 };
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const TRIANGLE: [[f32; 2]; 3] = [[-0.75, -0.65], [0.75, -0.65], [0.0, 0.95]];

static UNIFORM_PROGRAM: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 12,
    secondary_vertex_stride: 0,
    vertex_attributes: &[VertexAttribute {
        stream: 0,
        location: 0,
        offset: 0,
        format: VertexFormat::Float32x3,
    }],
    parameters: RasterParameterLayout {
        size: 0,
        stages: ShaderStages::EMPTY,
    },
    color_outputs: &[ImageFormat::Rgba8Unorm, ImageFormat::Rgba8Unorm],
    bindings: &[BindingLayout {
        group: 0,
        binding: 2,
        kind: BindingType::Uniform,
        min_size: UNIFORM.size,
        stages: ShaderStages::FRAGMENT,
    }],
    binding_bytes: &[UNIFORM.size],
    sampling_pairs: &[],
    blend: BlendState::Replace,
    depth: DepthState::Disabled,
    shaders: ShaderBinaries::both(
        RasterShaderStages {
            vertex: include_bytes!("../../tests/support/shaders/raster_batches.vert.spv"),
            fragment: Some(include_bytes!(
                "../../tests/support/shaders/raster_batches_uniform.frag.spv"
            )),
        },
        D3D12RasterShaders {
            stages: RasterShaderStages {
                vertex: include_bytes!("../../tests/support/shaders/dxil/raster_batches.vert.dxil"),
                fragment: Some(include_bytes!(
                    "../../tests/support/shaders/dxil/raster_batches_uniform.frag.dxil"
                )),
            },
            inline_constants: None,
        },
    ),
};

#[derive(Clone, Copy)]
enum Api {
    Vulkan,
    D3d12,
}

struct Options {
    api: Api,
    draws: usize,
    words: usize,
    report: PathBuf,
    warmup: usize,
    frames: usize,
    profile: bool,
    capture: Option<PathBuf>,
}

impl Options {
    fn parse() -> Result<Self, Failure> {
        let mut args = std::env::args().skip(1);
        let (mut api, mut draws, mut words, mut report) = (None, None, None, None);
        let (mut warmup, mut frames, mut profile, mut capture) = (120, 600, false, None);
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| format!("missing value for {flag}"))
            };
            match flag.as_str() {
                "--api" => {
                    api = Some(match value()?.as_str() {
                        "vulkan" => Api::Vulkan,
                        "d3d12" => Api::D3d12,
                        _ => return Err("API must be vulkan or d3d12".into()),
                    });
                }
                "--draws" => draws = Some(value()?.parse::<usize>()?),
                "--words" => words = Some(value()?.parse::<usize>()?),
                "--report" => report = Some(PathBuf::from(value()?)),
                "--warmup" => warmup = value()?.parse()?,
                "--frames" => frames = value()?.parse()?,
                "--profile" => profile = true,
                "--capture" => capture = Some(PathBuf::from(value()?)),
                _ => return Err(format!("unknown option {flag}").into()),
            }
        }
        let draws = draws.ok_or("missing --draws")?;
        let words = words.ok_or("missing --words")?;
        if !matches!(draws, 1_024 | 10_000 | 50_000) || !matches!(words, 0 | 20) {
            return Err(
                "supported draw counts are 1024, 10000 and 50000; words must be 0 or 20".into(),
            );
        }
        if frames == 0 && (capture.is_none() || warmup != 0) {
            return Err("zero measured frames require --capture and --warmup 0".into());
        }
        Ok(Self {
            api: api.ok_or("missing --api")?,
            draws,
            words,
            report: report.ok_or("missing --report")?,
            warmup,
            frames,
            profile,
            capture,
        })
    }
}

enum Runtime {
    Vulkan(Box<VulkanRuntime>),
    D3d12(Box<D3D12Runtime>),
}

#[allow(
    clippy::large_enum_variant,
    reason = "keep submission tokens inline to avoid per-frame allocation"
)]
enum Pending {
    Vulkan(Submission),
    D3d12(D3D12Submission),
}

#[allow(
    clippy::large_enum_variant,
    reason = "keep result tokens inline to avoid per-frame allocation"
)]
enum Completed {
    Vulkan(SubmissionResult),
    D3d12(D3D12SubmissionResult),
}

impl Runtime {
    fn new(
        options: &Options,
        capacity: GraphCapacity,
        program: ManagedRasterProgram,
        buffers: &[BufferDesc],
    ) -> Result<Self, Failure> {
        let colors = if options.words == 0 { 2 } else { 1 };
        let resources = buffers.len() + colors * IN_FLIGHT;
        let images = D3D12ImageClass {
            desc: color_desc(),
            count: colors * IN_FLIGHT,
        };
        match options.api {
            Api::Vulkan => {
                let mut runtime = VulkanRuntime::new_with_validation(
                    RuntimeConfig {
                        execution_slots: IN_FLIGHT,
                        resource_capacity: resources,
                        staging_bytes: STAGING_BYTES,
                        max_shader_bindings: 1,
                        ..Default::default()
                    },
                    if STRICT {
                        ValidationMode::Required
                    } else {
                        ValidationMode::Disabled
                    },
                )?;
                if runtime.device_info().device_type == "CPU" {
                    return Err("the benchmark requires a physical GPU".into());
                }
                let classes = buffers
                    .iter()
                    .map(|&desc| ResourceClass::Buffer { desc, count: 1 })
                    .chain([ResourceClass::Image {
                        desc: images.desc,
                        count: images.count,
                    }])
                    .collect::<Vec<_>>();
                runtime.prepare_working_set(
                    FrameCapacity {
                        graph: capacity,
                        work_slots: IN_FLIGHT,
                        result_slots: IN_FLIGHT,
                        metadata_slots: IN_FLIGHT,
                        index_prefixes_per_resource: 0,
                        ..Default::default()
                    },
                    &classes,
                )?;
                runtime.prepare_raster_programs(&[program])?;
                if options.profile {
                    runtime.set_profiling(true)?;
                }
                Ok(Self::Vulkan(Box::new(runtime)))
            }
            Api::D3d12 => {
                let classes = buffers
                    .iter()
                    .map(|desc| D3D12BufferClass {
                        size: desc.size,
                        count: 1,
                    })
                    .collect::<Vec<_>>();
                let mut runtime = D3D12Runtime::new_with_working_set(
                    D3D12RuntimeConfig {
                        validation: if STRICT {
                            D3D12ValidationMode::Required
                        } else {
                            D3D12ValidationMode::Disabled
                        },
                        capacity: D3D12FrameCapacity {
                            executions: IN_FLIGHT,
                            results: IN_FLIGHT,
                            resources: capacity.resources,
                            resource_capacity: resources,
                            operations: capacity.draws + capacity.passes,
                            buffers: buffers.len(),
                            buffer_bytes: buffers.iter().map(|desc| desc.size).max().unwrap(),
                            upload_bytes: STAGING_BYTES,
                            readback_bytes: STAGING_BYTES,
                            index_prefixes_per_resource: 0,
                            ..Default::default()
                        },
                    },
                    &classes,
                    &[images],
                    &[program.into()],
                    &[],
                )?;
                if options.profile {
                    runtime.set_profiling(true)?;
                }
                Ok(Self::D3d12(Box::new(runtime)))
            }
        }
    }

    fn submit(&mut self, plan: &CompiledGraph) -> Result<Pending, Failure> {
        Ok(match self {
            Self::Vulkan(runtime) => Pending::Vulkan(runtime.submit(plan)?),
            Self::D3d12(runtime) => Pending::D3d12(runtime.submit(plan)?),
        })
    }

    fn delete(&mut self, key: ResourceKey) -> Result<(), Failure> {
        match self {
            Self::Vulkan(runtime) => runtime.delete(key)?,
            Self::D3d12(runtime) => runtime.delete(key)?,
        }
        Ok(())
    }

    fn identity(&self) -> Value {
        match self {
            Self::Vulkan(runtime) => {
                let gpu = runtime.device_info();
                json!({"api": "vulkan", "device": gpu.name, "luid": gpu.device_luid,
                    "driver": gpu.driver_version_raw})
            }
            Self::D3d12(runtime) => {
                let gpu = runtime.device_info();
                json!({"api": "d3d12", "device": gpu.name, "luid": gpu.luid,
                    "driver": gpu.driver_version})
            }
        }
    }

    fn close(self) -> Result<Value, Failure> {
        match self {
            Self::Vulkan(runtime) => {
                let monitor = runtime.validation_monitor();
                drop(runtime);
                let report = monitor.report();
                if !report.is_clean()
                    || (STRICT
                        && (report.core_validation != Some(true)
                            || report.synchronization_validation != Some(true)))
                {
                    return Err(format!("Vulkan validation: {report:?}").into());
                }
                Ok(json!({"clean": true, "strict": STRICT,
                    "core": report.core_validation, "sync": report.synchronization_validation,
                    "warnings": report.warnings, "errors": report.errors}))
            }
            Self::D3d12(runtime) => {
                let report = runtime.shutdown()?;
                if !report.is_clean()
                    || report.debug_layer_enabled != STRICT
                    || report.gpu_based_validation_enabled != STRICT
                    || (STRICT && !report.teardown_checked)
                {
                    return Err(format!("D3D12 validation: {report:?}").into());
                }
                Ok(json!({"clean": true, "strict": STRICT,
                    "debug_layer": report.debug_layer_enabled, "gbv": report.gpu_based_validation_enabled,
                    "warnings": report.warnings, "errors": report.errors}))
            }
        }
    }
}

impl Pending {
    fn wait(&mut self) -> Result<Completed, Failure> {
        match self {
            Self::Vulkan(pending) => Ok(Completed::Vulkan(
                pending.wait(WAIT)?.ok_or("Vulkan frame timed out")?,
            )),
            Self::D3d12(pending) => Ok(Completed::D3d12(
                pending.wait(WAIT)?.ok_or("D3D12 frame timed out")?,
            )),
        }
    }

    fn export_buffer(&self, handle: BufferHandle) -> Result<ImportBuffer, Failure> {
        Ok(match self {
            Self::Vulkan(pending) => pending.export_buffer(handle)?,
            Self::D3d12(pending) => pending.export_buffer(handle)?,
        })
    }

    fn output_key(&self, handle: ImageHandle) -> Result<ResourceKey, Failure> {
        Ok(match self {
            Self::Vulkan(pending) => pending.export_image(handle)?.key(),
            Self::D3d12(pending) => pending.export_image(handle)?.key(),
        })
    }
}

impl Completed {
    fn metrics(&self) -> Option<SubmissionMetrics> {
        match self {
            Self::Vulkan(result) => result.metrics().copied(),
            Self::D3d12(result) => result.metrics().copied(),
        }
    }

    fn readback(&mut self, pass: PassHandle, bytes: &mut [u8]) -> Result<(), Failure> {
        match self {
            Self::Vulkan(result) => result.readback_into(pass, bytes)?,
            Self::D3d12(result) => result.readback_into(pass, bytes)?,
        }
        Ok(())
    }
}

fn position(draw: usize) -> [f32; 2] {
    const GRID: usize = EXTENT as usize / 2;
    let clip = |cell| (2.0 * cell as f32 + 0.5) * (2.0 / EXTENT as f32) - 1.0;
    [clip(draw % GRID), clip(draw / GRID)]
}

fn color_desc() -> ImageDesc {
    ImageDesc::rgba8(EXTENT, EXTENT)
}

struct Scene {
    geometry: GeometryData,
    program: ManagedRasterProgram,
    parameters: Option<[Vec<RasterConstants>; 2]>,
    draws: usize,
}

impl Scene {
    fn new(options: &Options) -> Result<Self, Failure> {
        let scale = 2.0 / EXTENT as f32;
        let (geometry, program, parameters) = if options.words == 0 {
            let vertices = (0..options.draws)
                .flat_map(|draw| {
                    let [x, y] = position(draw);
                    TRIANGLE.map(|[dx, dy]| [x + dx * scale, y + dy * scale, 0.5])
                })
                .flatten()
                .flat_map(f32::to_le_bytes)
                .collect::<Vec<_>>();
            let indices = (0..3 * options.draws as u32).collect::<Vec<_>>();
            // SAFETY: These existing test shaders read the declared vec3 vertex and
            // one uniform vec4, and write exactly two color outputs without side effects.
            let program =
                unsafe { ManagedRasterProgram::from_trusted_descriptor(&UNIFORM_PROGRAM) };
            (GeometryData::new(12, &vertices, &indices)?, program, None)
        } else {
            let vertices =
                TRIANGLE.map(|[x, y]| [x, y, 0.5, 1.0, 1.0, 1.0, 1.0].map(f32::to_le_bytes));
            let constants = |variant: usize| -> Result<Vec<RasterConstants>, Failure> {
                (0..options.draws)
                    .map(|draw| {
                        let [x, y] = position(draw);
                        let tint = [
                            0.25 + (draw % 31) as f32 / 64.0,
                            0.25 + (draw % 29) as f32 / 64.0,
                            if variant == 0 { 0.375 } else { 0.75 },
                            1.0,
                        ];
                        let transform = [
                            [scale, 0.0, 0.0, 0.0],
                            [0.0, scale, 0.0, 0.0],
                            [0.0, 0.0, 1.0, 0.0],
                            [x + variant as f32 * scale / 8.0, y, 0.0, 1.0],
                        ];
                        Ok(RasterParameters::new(transform, tint)?.into())
                    })
                    .collect()
            };
            (
                GeometryData::new(28, vertices.as_flattened().as_flattened(), &[0, 1, 2])?,
                ManagedRasterProgram::mesh(),
                Some([constants(0)?, constants(1)?]),
            )
        };
        Ok(Self {
            geometry,
            program,
            parameters,
            draws: options.draws,
        })
    }

    fn buffers(&self) -> Vec<BufferDesc> {
        let geometry = [
            BufferDesc {
                size: self.geometry.vertex_byte_len(),
            },
            BufferDesc {
                size: self.geometry.index_byte_len(),
            },
        ];
        geometry
            .into_iter()
            .chain(self.parameters.is_none().then_some(UNIFORM))
            .collect()
    }

    fn upload(
        &self,
        runtime: &mut Runtime,
        buffers: &[BufferDesc],
    ) -> Result<ResidentScene, Failure> {
        let mut pool = GraphPool::new(
            GraphCapacity {
                draws: 0,
                ..GraphCapacity::default()
            },
            1,
        )?;
        let mut graph = pool.begin()?;
        let vertices = graph.buffer("resident vertices", buffers[0])?;
        let indices = graph.buffer("resident indices", buffers[1])?;
        self.geometry
            .upload_vertices(&mut graph, "upload vertices", vertices)?;
        self.geometry
            .upload_indices(&mut graph, "upload indices", indices)?;
        graph.export_buffer(vertices)?;
        graph.export_buffer(indices)?;
        let uniform = if self.parameters.is_none() {
            let color = graph.buffer("resident color uniform", UNIFORM)?;
            graph.upload_buffer(
                "upload uniform",
                color,
                0,
                [0.25_f32, 0.75, 0.5, 1.0]
                    .map(f32::to_le_bytes)
                    .as_flattened(),
            )?;
            graph.export_buffer(color)?;
            Some(color)
        } else {
            None
        };
        let plan = graph.compile()?;
        let mut pending = runtime.submit(&plan)?;
        let scene = ResidentScene {
            vertices: pending.export_buffer(vertices)?,
            indices: pending.export_buffer(indices)?,
            uniform: uniform
                .map(|handle| pending.export_buffer(handle))
                .transpose()?,
        };
        drop(pending.wait()?);
        Ok(scene)
    }

    fn build(
        &self,
        pool: &mut GraphPool,
        resident: &ResidentScene,
        variant: usize,
        capture: bool,
    ) -> Result<(Graph, ImageHandle, [Option<PassHandle>; 2]), Failure> {
        let mut graph = pool.begin()?;
        let vertices = graph.import_buffer("vertices", &resident.vertices)?;
        let indices = graph.import_buffer("indices", &resident.indices)?;
        let color = graph.image("scene color", color_desc())?;
        let mut colors = [Attachment {
            view: graph.image_view(color, 0, 0)?,
            load: LoadOp::Clear(CLEAR),
            store: StoreOp::Store,
        }; 2];
        let second = if let Some(uniform) = &resident.uniform {
            let secondary = graph.image("secondary color", color_desc())?;
            colors[1].view = graph.image_view(secondary, 0, 0)?;
            let uniform = graph.import_buffer("shared color uniform", uniform)?;
            let bindings = graph.raster_bindings(
                &self.program,
                &[Binding {
                    group: 0,
                    binding: 2,
                    resource: BindingResource::Buffer {
                        buffer: uniform,
                        offset: 0,
                        size: UNIFORM.size,
                    },
                }],
            )?;
            graph.raster(
                "objects",
                RasterAttachments {
                    colors: &colors,
                    depth: None,
                    color_resolves: &[],
                },
                |pass| {
                    pass.draw(
                        &self.program,
                        (0..self.draws as u32).map(|draw| RasterDraw {
                            secondary_vertices: None,
                            vertices,
                            indices,
                            first_index: draw * 3,
                            index_count: 3,
                            instance_count: 1,
                            bindings: Some(bindings),
                            parameters: RasterConstants::default(),
                        }),
                    )
                },
            )?;
            Some(secondary)
        } else {
            let parameters = &self.parameters.as_ref().unwrap()[variant];
            graph.raster(
                "objects",
                RasterAttachments {
                    colors: &colors[..1],
                    depth: None,
                    color_resolves: &[],
                },
                |pass| {
                    pass.draw(
                        &self.program,
                        parameters.iter().map(|&parameters| RasterDraw {
                            secondary_vertices: None,
                            vertices,
                            indices,
                            first_index: 0,
                            index_count: 3,
                            instance_count: 1,
                            bindings: None,
                            parameters,
                        }),
                    )
                },
            )?;
            None
        };
        let mut passes = [None, None];
        if capture {
            passes[0] = Some(graph.readback_image("read scene color", color)?);
            if let Some(second) = second {
                passes[1] = Some(graph.readback_image("read secondary color", second)?);
            }
        } else {
            graph.export_image(color)?;
        }
        Ok((graph, color, passes))
    }
}

struct ResidentScene {
    vertices: ImportBuffer,
    indices: ImportBuffer,
    uniform: Option<ImportBuffer>,
}

struct Sample {
    active_ns: u64,
    build_ns: u64,
    compile_ns: u64,
    interval_ns: u64,
    metrics: Option<SubmissionMetrics>,
}

fn ns(duration: Duration) -> u64 {
    duration.as_nanos() as u64
}

impl Sample {
    fn json(&self) -> Value {
        let mut result = json!({
            "active_ns": self.active_ns, "build_ns": self.build_ns,
            "compile_ns": self.compile_ns, "interval_ns": self.interval_ns,
        });
        if let Some(metrics) = self.metrics {
            result["prepare_ns"] = ns(metrics.cpu_prepare).into();
            result["record_ns"] = ns(metrics.cpu_record).into();
            result["submit_ns"] = ns(metrics.cpu_submit).into();
            result["retire_ns"] = ns(metrics.cpu_retire).into();
            result["gpu_ns"] = match metrics.gpu {
                GpuTiming::Available { elapsed, .. } => ns(elapsed).into(),
                _ => Value::Null,
            };
        }
        result
    }
}

fn measure(
    runtime: &mut Runtime,
    pool: &mut GraphPool,
    scene: &Scene,
    resident: &ResidentScene,
    frames: usize,
    collect: bool,
) -> Result<(Vec<Sample>, u64), Failure> {
    let mut samples: Vec<Sample> = Vec::with_capacity(if collect { frames } else { 0 });
    let mut queue = VecDeque::<(usize, Pending)>::with_capacity(IN_FLIGHT);
    let mut last_start = Instant::now();
    let mut first_start = last_start;
    for frame in 0..frames {
        if queue.len() == IN_FLIGHT {
            let (previous, mut pending) = queue.pop_front().unwrap();
            let completed = pending.wait()?;
            if collect {
                samples[previous].metrics = completed.metrics();
            }
        }
        let started = Instant::now();
        if frame == 0 {
            first_start = started;
        }
        let (graph, color, _) = scene.build(pool, resident, frame % 2, false)?;
        let built = Instant::now();
        let plan = graph.compile()?;
        let compiled = Instant::now();
        let pending = runtime.submit(&plan)?;
        let submitted = Instant::now();
        if collect {
            samples.push(Sample {
                active_ns: ns(submitted.duration_since(started)),
                build_ns: ns(built.duration_since(started)),
                compile_ns: ns(compiled.duration_since(built)),
                interval_ns: ns(started.duration_since(last_start)),
                metrics: None,
            });
        }
        last_start = started;
        runtime.delete(pending.output_key(color)?)?;
        queue.push_back((frame, pending));
    }
    while let Some((frame, mut pending)) = queue.pop_front() {
        let completed = pending.wait()?;
        if collect {
            samples[frame].metrics = completed.metrics();
        }
    }
    if let Some(first) = samples.first_mut() {
        // Use completed frame throughput for the entire measurement, including drain.
        // The first sample has no previous start in this cohort.
        first.interval_ns = 0;
    }
    Ok((samples, ns(first_start.elapsed())))
}

fn capture(
    runtime: &mut Runtime,
    pool: &mut GraphPool,
    scene: &Scene,
    resident: &ResidentScene,
    stem: &Path,
) -> Result<Vec<Value>, Failure> {
    let mut output = vec![0; (EXTENT * EXTENT * 4) as usize];
    let mut captures = Vec::new();
    for variant in 0..2 {
        let (graph, _, passes) = scene.build(pool, resident, variant, true)?;
        let mut completed = runtime.submit(&graph.compile()?)?.wait()?;
        for (target, pass) in passes.into_iter().flatten().enumerate() {
            completed.readback(pass, &mut output)?;
            let colored = output
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|&&pixel| pixel != [0, 0, 0, 255])
                .count();
            if colored < scene.draws {
                return Err(format!(
                    "rendered only {colored} colored pixels for {} objects",
                    scene.draws
                )
                .into());
            }
            let mut path: OsString = stem.as_os_str().into();
            path.push(format!(".{variant}-{target}.rgba"));
            std::fs::write(&path, &output)?;
            captures.push(json!({
                "variant": variant, "target": target, "colored_pixels": colored,
                "path": PathBuf::from(path), "sha256": format!("{:x}", Sha256::digest(&output)),
            }));
        }
    }
    Ok(captures)
}

pub fn run() -> Result<(), Failure> {
    let options = Options::parse()?;
    let capacity = GraphCapacity {
        draws: options.draws,
        ..GraphCapacity::default()
    };
    let scene = Scene::new(&options)?;
    let buffers = scene.buffers();
    let mut runtime = Runtime::new(&options, capacity, scene.program.clone(), &buffers)?;
    let identity = runtime.identity();
    let resident = scene.upload(&mut runtime, &buffers)?;
    let mut pool = GraphPool::new(capacity, IN_FLIGHT)?;
    measure(
        &mut runtime,
        &mut pool,
        &scene,
        &resident,
        options.warmup,
        false,
    )?;
    let (samples, wall_ns) = measure(
        &mut runtime,
        &mut pool,
        &scene,
        &resident,
        options.frames,
        true,
    )?;
    let captures = options
        .capture
        .as_ref()
        .map(|stem| capture(&mut runtime, &mut pool, &scene, &resident, stem))
        .transpose()?
        .unwrap_or_default();
    assert!(pool.is_idle());
    let validation = runtime.close()?;
    let mut report = File::create(options.report)?;
    serde_json::to_writer_pretty(
        &mut report,
        &json!({
            "device": identity, "draws": options.draws, "words": options.words,
            "extent": EXTENT, "in_flight": IN_FLIGHT, "warmup": options.warmup,
            "frames": options.frames, "profile": options.profile, "wall_ns": wall_ns,
            "samples": samples.iter().map(Sample::json).collect::<Vec<_>>(),
            "captures": captures, "validation": validation,
        }),
    )?;
    report.write_all(b"\n")?;
    Ok(())
}
