#[path = "raster_draw_jobs.rs"]
mod jobs;

use jobs::DrawPreparation;
use std::{
    collections::VecDeque,
    error::Error,
    ffi::OsString,
    fs::File,
    io::Write,
    num::NonZeroUsize,
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use sashiko_renderer_core::{
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

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: sashiko_renderer_core::measurement::CountingAllocator<std::alloc::System> =
    sashiko_renderer_core::measurement::CountingAllocator::new(std::alloc::System);

type Failure = Box<dyn Error>;
const EXTENT: u32 = 512;
const IN_FLIGHT: usize = 2;
// At the two-target workload's 4 MiB per pass across both in-flight slots,
// 32 passes reserve 128 MiB of RGBA8 image payload, plus native overhead.
const MAX_PASSES: usize = 32;
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

#[derive(Clone, Copy, PartialEq)]
enum Topology {
    Independent,
    Chain,
}

impl Topology {
    fn name(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::Chain => "chain",
        }
    }
}

struct Options {
    api: Api,
    draws: usize,
    words: usize,
    passes: usize,
    batches_per_pass: usize,
    topology: Topology,
    workers: usize,
    native_workers: usize,
    graph_workers: usize,
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
        let (mut passes, mut batches_per_pass, mut workers) = (1usize, 1usize, 0usize);
        let mut native_workers = 0usize;
        let mut graph_workers = 0usize;
        let mut topology = Topology::Independent;
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| format!("missing value for {flag}"))
            };
            match flag.as_str() {
                "--help" | "-h" => {
                    println!(
                        "raster_draw_scaling --api <vulkan|d3d12> --draws <1..=50000> --words <0|20> --report <path>\n\
                         [--passes <1..={MAX_PASSES}>] [--batches-per-pass <positive count>] [--topology <independent|chain>]\n\
                         [--workers <0 for direct, or 1 through available logical CPU count>]\n\
                         [--graph-workers <0 for direct, or scoped capture team size>] [--native-workers <0 for direct, or scoped recording team size>] [--warmup <count>] [--frames <count>] [--profile] [--capture <stem>]"
                    );
                    std::process::exit(0);
                }
                "--api" => {
                    api = Some(match value()?.as_str() {
                        "vulkan" => Api::Vulkan,
                        "d3d12" => Api::D3d12,
                        _ => return Err("API must be vulkan or d3d12".into()),
                    });
                }
                "--draws" => draws = Some(value()?.parse::<usize>()?),
                "--words" => words = Some(value()?.parse::<usize>()?),
                "--passes" => passes = value()?.parse()?,
                "--batches-per-pass" => batches_per_pass = value()?.parse()?,
                "--graph-workers" => graph_workers = value()?.parse()?,
                "--native-workers" => native_workers = value()?.parse()?,
                "--workers" => workers = value()?.parse()?,
                "--topology" => {
                    topology = match value()?.as_str() {
                        "independent" => Topology::Independent,
                        "chain" => Topology::Chain,
                        _ => return Err("topology must be independent or chain".into()),
                    }
                }
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
        if !(1..=50_000).contains(&draws) || !matches!(words, 0 | 20) {
            return Err("draws must be in 1..=50000 and words must be 0 or 20".into());
        }
        if frames == 0 && (capture.is_none() || warmup != 0) {
            return Err("zero measured frames require --capture and --warmup 0".into());
        }
        if !(1..=MAX_PASSES).contains(&passes) {
            return Err(format!(
                "--passes must be 1..={MAX_PASSES} (128 MiB image payload at two targets and two in-flight frames)"
            ).into());
        }
        if batches_per_pass == 0 || passes > draws / batches_per_pass {
            return Err("each requested pass and batch must contain at least one draw".into());
        }
        if workers.max(native_workers).max(graph_workers) > 1 {
            let available = std::thread::available_parallelism()?.get();
            if workers.max(native_workers).max(graph_workers) > available {
                return Err(format!("worker counts must each be 0 or 1..={available} on this host (includes the calling thread)").into());
            }
        }
        Ok(Self {
            api: api.ok_or("missing --api")?,
            draws,
            words,
            passes,
            batches_per_pass,
            topology,
            workers,
            native_workers,
            graph_workers,
            report: report.ok_or("missing --report")?,
            warmup,
            frames,
            profile,
            capture,
        })
    }

    fn color_count(&self) -> usize {
        if self.words == 0 { 2 } else { 1 }
    }

    fn capacity(&self) -> GraphCapacity {
        let base = GraphCapacity::default();
        let outputs = self.passes * self.color_count();
        let passes = self.passes + outputs;
        GraphCapacity {
            draws: self.draws,
            resources: base.resources.max(outputs + 3),
            passes: base.passes.max(passes),
            attachments: base.attachments.max(outputs),
            dependencies: base.dependencies.max(self.passes - 1),
            accesses: base.accesses.max(self.passes * 8),
            segments: base.segments.max(outputs + 3),
            readers: base.readers.max(self.passes * 5),
            edges: base.edges.max(self.passes * 4),
            name_bytes: base.name_bytes.max(passes * 64 + 64),
            ..base
        }
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
        native_jobs: Option<sashiko_renderer_core::jobs::RendererJobs>,
    ) -> Result<Self, Failure> {
        let colors = options.passes * options.color_count();
        let resources = buffers.len() + colors * IN_FLIGHT;
        let readback_bytes = if options.capture.is_some() {
            STAGING_BYTES.max(colors as u64 * u64::from(EXTENT).pow(2) * 4)
        } else {
            STAGING_BYTES
        };
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
                        staging_bytes: readback_bytes,
                        staging_ranges: RuntimeConfig::default().staging_ranges.max(colors),
                        max_shader_bindings: 1,
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
                if let Some(jobs) = native_jobs {
                    runtime.prepare_raster_recording(
                        jobs,
                        NonZeroUsize::new(options.passes * options.batches_per_pass).unwrap(),
                    )?;
                }
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
                            readback_bytes,
                            index_prefixes_per_resource: 0,
                            ..Default::default()
                        },
                    },
                    &classes,
                    &[images],
                    &[program.into()],
                    &[],
                )?;
                if let Some(jobs) = native_jobs {
                    runtime.prepare_raster_recording(
                        jobs,
                        NonZeroUsize::new(options.passes * options.batches_per_pass).unwrap(),
                    )?;
                }
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

struct Layout {
    pass_names: Vec<String>,
    batches: Vec<Range<usize>>,
    batches_per_pass: usize,
    topology: Topology,
}

impl Layout {
    fn new(options: &Options) -> Self {
        let mut batches = Vec::with_capacity(options.passes * options.batches_per_pass);
        for pass in 0..options.passes {
            let start = pass * options.draws / options.passes;
            let end = (pass + 1) * options.draws / options.passes;
            let boundary = |batch| start + batch * (end - start) / options.batches_per_pass;
            for batch in 0..options.batches_per_pass {
                batches.push(boundary(batch)..boundary(batch + 1));
            }
        }
        Self {
            pass_names: (0..options.passes)
                .map(|i| format!("objects {i}"))
                .collect(),
            batches,
            batches_per_pass: options.batches_per_pass,
            topology: options.topology,
        }
    }

    fn pass_draws(&self, pass: usize) -> Range<usize> {
        let first = pass * self.batches_per_pass;
        self.batches[first].start..self.batches[first + self.batches_per_pass - 1].end
    }
}

struct Scene {
    geometry: GeometryData,
    program: ManagedRasterProgram,
    parameters: Option<[Vec<RasterConstants>; 2]>,
    layout: Layout,
    preparation: Option<DrawPreparation>,
    // Reused after submission has acquired its own native leases and exports.
    outputs: Vec<ImageHandle>,
    readbacks: Vec<PassHandle>,
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
            layout: Layout::new(options),
            preparation: NonZeroUsize::new(options.workers)
                .map(|workers| DrawPreparation::new(workers, options.draws))
                .transpose()?,
            outputs: Vec::with_capacity(options.passes),
            readbacks: Vec::with_capacity(options.passes * options.color_count()),
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
        &mut self,
        pool: &mut GraphPool,
        resident: &ResidentScene,
        variant: usize,
        capture: bool,
        verify: bool,
    ) -> Result<(Graph, Option<Duration>), Failure> {
        self.outputs.clear();
        self.readbacks.clear();
        let mut graph = pool.begin()?;
        let vertices = graph.import_buffer("vertices", &resident.vertices)?;
        let indices = graph.import_buffer("indices", &resident.indices)?;
        let bindings = if let Some(uniform) = &resident.uniform {
            let uniform = graph.import_buffer("shared color uniform", uniform)?;
            Some(graph.raster_bindings(
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
            )?)
        } else {
            None
        };
        let template = RasterDraw {
            vertices,
            indices,
            secondary_vertices: None,
            bindings,
            first_index: 0,
            index_count: 3,
            instance_count: 1,
            parameters: RasterConstants::default(),
        };
        let parameters = self
            .parameters
            .as_ref()
            .map(|sets| sets[variant].as_slice());
        let packet_time = self.preparation.as_mut().map(|prep| {
            let started = Instant::now();
            prep.prepare(template, parameters, &self.layout.batches);
            started.elapsed()
        });
        let direct = |ordinal: usize| match parameters {
            None => RasterDraw {
                first_index: ordinal as u32 * 3,
                ..template
            },
            Some(values) => RasterDraw {
                parameters: values[ordinal],
                ..template
            },
        };
        if verify && let Some(prep) = &self.preparation {
            for range in &self.layout.batches {
                if !prep
                    .draws(range.clone())
                    .iter()
                    .copied()
                    .eq(range.clone().map(direct))
                {
                    return Err(format!(
                        "prepared draws differ from the ordered input at {range:?}"
                    )
                    .into());
                }
            }
        }
        let mut previous = None;
        for (pass_index, batches) in self
            .layout
            .batches
            .chunks_exact(self.layout.batches_per_pass)
            .enumerate()
        {
            let color = graph.image("scene color", color_desc())?;
            let mut colors = [Attachment {
                view: graph.image_view(color, 0, 0)?,
                load: LoadOp::Clear(CLEAR),
                store: StoreOp::Store,
            }; 2];
            let secondary = if bindings.is_some() {
                let image = graph.image("secondary color", color_desc())?;
                colors[1].view = graph.image_view(image, 0, 0)?;
                Some(image)
            } else {
                None
            };
            let pass = graph.raster(
                &self.layout.pass_names[pass_index],
                RasterAttachments {
                    colors: &colors[..1 + usize::from(secondary.is_some())],
                    depth: None,
                    color_resolves: &[],
                },
                |pass| {
                    for range in batches {
                        if let Some(prep) = &self.preparation {
                            let draws = prep.draws(range.clone());
                            pass.draw_generated(&self.program, draws.len(), |i| &draws[i])?;
                        } else {
                            pass.draw_generated(&self.program, range.len(), |i| {
                                direct(range.start + i)
                            })?;
                        }
                    }
                    Ok(())
                },
            )?;
            if self.layout.topology == Topology::Chain
                && let Some(prior) = previous
            {
                graph.depends_on(pass, prior)?;
            }
            previous = Some(pass);
            if capture {
                self.readbacks
                    .push(graph.readback_image("read scene color", color)?);
                if let Some(image) = secondary {
                    self.readbacks
                        .push(graph.readback_image("read secondary color", image)?);
                }
            } else {
                // Each Raster pass has its own externally visible output, including
                // every independent pass. The shared imported buffers are read-only.
                graph.export_image(color)?;
                self.outputs.push(color);
            }
        }
        Ok((graph, packet_time))
    }

    fn verify_plan(&self, plan: &CompiledGraph) -> Result<(), Failure> {
        if plan.pass_count() != self.layout.pass_names.len() + self.readbacks.len()
            || !plan
                .ordered_pass_names()
                .filter(|name| name.starts_with("objects "))
                .eq(self.layout.pass_names.iter().map(String::as_str))
        {
            return Err("compiled graph did not retain every raster pass in expected order".into());
        }
        Ok(())
    }
}

struct ResidentScene {
    vertices: ImportBuffer,
    indices: ImportBuffer,
    uniform: Option<ImportBuffer>,
}

struct Sample {
    raster: Option<sashiko_renderer_core::graph::RasterBuildMetrics>,
    active_ns: u64,
    build_ns: u64,
    packet_prepare_ns: Option<u64>,
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
            "packet_prepare_ns": self.packet_prepare_ns,
            "compile_ns": self.compile_ns, "interval_ns": self.interval_ns,
        });
        if let Some(raster) = self.raster {
            result["streaming_capture_ns"] = ns(raster.streaming_capture).into();
            result["generated_capture_ns"] = ns(raster.generated_capture).into();
            result["generated_merge_ns"] = ns(raster.generated_merge).into();
            result["finalize_reads_ns"] = ns(raster.finalize_reads).into();
            result["generated_runs"] = raster.generated_runs.into();
            result["generated_draws"] = raster.generated_draws.into();
        }
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
    scene: &mut Scene,
    resident: &ResidentScene,
    frames: usize,
    mut samples: Option<&mut Vec<Sample>>,
) -> Result<(u64, Option<Value>), Failure> {
    let mut queue = VecDeque::<(usize, Pending)>::with_capacity(IN_FLIGHT);
    #[cfg(feature = "memory-profiling")]
    let before = sashiko_renderer_core::measurement::Snapshot::now();
    let mut last_start = Instant::now();
    let mut first_start = last_start;
    for frame in 0..frames {
        if queue.len() == IN_FLIGHT {
            let (previous, mut pending) = queue.pop_front().unwrap();
            let completed = pending.wait()?;
            if let Some(samples) = &mut samples {
                samples[previous].metrics = completed.metrics();
            }
        }
        let started = Instant::now();
        if frame == 0 {
            first_start = started;
        }
        let (graph, packet_time) = scene.build(pool, resident, frame % 2, false, false)?;
        let built = Instant::now();
        let plan = graph.compile()?;
        let compiled = Instant::now();
        let pending = runtime.submit(&plan)?;
        let submitted = Instant::now();
        if let Some(samples) = &mut samples {
            samples.push(Sample {
                raster: plan.raster_build_metrics(),
                active_ns: ns(submitted.duration_since(started)),
                build_ns: ns(built.duration_since(started)),
                packet_prepare_ns: packet_time.map(ns),
                compile_ns: ns(compiled.duration_since(built)),
                interval_ns: ns(started.duration_since(last_start)),
                metrics: None,
            });
        }
        last_start = started;
        for &output in &scene.outputs {
            runtime.delete(pending.output_key(output)?)?;
        }
        queue.push_back((frame, pending));
    }
    while let Some((frame, mut pending)) = queue.pop_front() {
        let completed = pending.wait()?;
        if let Some(samples) = &mut samples {
            samples[frame].metrics = completed.metrics();
        }
    }
    if let Some(first) = samples.and_then(|samples| samples.first_mut()) {
        // Use completed frame throughput for the entire measurement, including drain.
        // The first sample has no previous start in this cohort.
        first.interval_ns = 0;
    }
    let wall = ns(first_start.elapsed());
    #[cfg(feature = "memory-profiling")]
    let memory = {
        let delta = sashiko_renderer_core::measurement::Snapshot::now().since(before);
        Some(
            json!({"rust_calls": delta.rust.map(|e| e.calls), "vma_host_calls": delta.vma_host.map(|e| e.calls),
            "native_events": delta.native, "overflowed": delta.overflowed}),
        )
    };
    #[cfg(not(feature = "memory-profiling"))]
    let memory = None;
    Ok((wall, memory))
}

/// The grid places the single covered pixel for each ordinal at the even x/y
/// coordinate of its 2x2 cell. Check every cell and every clear pixel, on both
/// MRT planes, independently of CPU packet preparation.
fn verify_pixels(
    rgba: &[u8],
    draws: Range<usize>,
    words: usize,
    target: usize,
    variant: usize,
) -> Result<(), Failure> {
    let width = EXTENT as usize;
    let unorm = |v: f32| (255.0 * v).round() as u8;
    for (offset, pixel) in rgba.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (offset % width, offset / width);
        let ordinal = y / 2 * (width / 2) + x / 2;
        if x % 2 != 0 || y % 2 != 0 || !draws.contains(&ordinal) {
            if pixel != &[0, 0, 0, 255] {
                return Err(format!("unexpected covered pixel at ({x}, {y})").into());
            }
            continue;
        }
        let expected = if words == 0 {
            if target == 0 {
                [64, 191, 128, 255]
            } else {
                [128, 64, 191, 255]
            }
        } else {
            [
                unorm(0.25 + (ordinal % 31) as f32 / 64.0),
                unorm(0.25 + (ordinal % 29) as f32 / 64.0),
                unorm(if variant == 0 { 0.375 } else { 0.75 }),
                255,
            ]
        };
        if !pixel
            .iter()
            .zip(expected)
            .all(|(&value, want)| value.abs_diff(want) <= 1)
        {
            return Err(format!(
                "draw {ordinal} pixel ({x}, {y}) is {pixel:?}, expected {expected:?}"
            )
            .into());
        }
    }
    Ok(())
}

fn capture(
    runtime: &mut Runtime,
    pool: &mut GraphPool,
    scene: &mut Scene,
    resident: &ResidentScene,
    options: &Options,
    stem: &Path,
) -> Result<Vec<Value>, Failure> {
    let mut output = vec![0; (EXTENT * EXTENT * 4) as usize];
    let mut captures = Vec::new();
    for variant in 0..2 {
        let (graph, _) = scene.build(pool, resident, variant, true, true)?;
        let plan = graph.compile()?;
        scene.verify_plan(&plan)?;
        let mut completed = runtime.submit(&plan)?.wait()?;
        for (index, &readback) in scene.readbacks.iter().enumerate() {
            let pass = index / options.color_count();
            let target = index % options.color_count();
            let draws = scene.layout.pass_draws(pass);
            completed.readback(readback, &mut output)?;
            verify_pixels(&output, draws.clone(), options.words, target, variant)?;
            let mut path: OsString = stem.as_os_str().into();
            if options.passes == 1 {
                path.push(format!(".{variant}-{target}.rgba"));
            } else {
                path.push(format!(".{variant}-pass{pass}-{target}.rgba"));
            }
            std::fs::write(&path, &output)?;
            captures.push(json!({
                "variant": variant, "pass": pass, "target": target, "colored_pixels": draws.len(),
                "first_draw": draws.start, "path": PathBuf::from(path),
                "sha256": format!("{:x}", Sha256::digest(&output)),
            }));
        }
    }
    Ok(captures)
}

pub fn run() -> Result<(), Failure> {
    let options = Options::parse()?;
    let mut samples = Vec::new();
    samples.try_reserve_exact(options.frames)?;
    let capacity = options.capacity();
    let mut scene = Scene::new(&options)?;
    let buffers = scene.buffers();
    let make_jobs = |count| {
        NonZeroUsize::new(count)
            .map(|n| {
                sashiko_jobs::JobSystem::new(n).map(sashiko_renderer_core::jobs::RendererJobs::new)
            })
            .transpose()
    };
    let graph_jobs = make_jobs(options.graph_workers)?;
    let native_jobs = if options.native_workers == options.graph_workers {
        graph_jobs.clone()
    } else {
        make_jobs(options.native_workers)?
    };
    let mut runtime = Runtime::new(
        &options,
        capacity,
        scene.program.clone(),
        &buffers,
        native_jobs,
    )?;
    let identity = runtime.identity();
    let resident = scene.upload(&mut runtime, &buffers)?;
    let mut pool = GraphPool::new(capacity, IN_FLIGHT)?;
    pool.set_raster_profiling(options.profile);
    if let Some(jobs) = graph_jobs {
        pool.set_raster_jobs(jobs, NonZeroUsize::MIN);
    }
    // Audit the exported, timed topology and inputs outside every measurement
    // window. Capture has extra readback consumers and is verified separately.
    let (graph, _) = scene.build(&mut pool, &resident, 0, false, true)?;
    scene.verify_plan(&graph.compile()?)?;
    measure(
        &mut runtime,
        &mut pool,
        &mut scene,
        &resident,
        options.warmup,
        None,
    )?;
    let (wall_ns, memory) = measure(
        &mut runtime,
        &mut pool,
        &mut scene,
        &resident,
        options.frames,
        Some(&mut samples),
    )?;
    let captures = options
        .capture
        .as_ref()
        .map(|stem| {
            capture(
                &mut runtime,
                &mut pool,
                &mut scene,
                &resident,
                &options,
                stem,
            )
        })
        .transpose()?
        .unwrap_or_default();
    assert!(pool.is_idle());
    let validation = runtime.close()?;
    let mut report = File::create(options.report)?;
    serde_json::to_writer_pretty(
        &mut report,
        &json!({
            "device": identity, "draws": options.draws, "words": options.words,
            "passes": options.passes, "batches_per_pass": options.batches_per_pass,
            "topology": options.topology.name(), "workers": options.workers, "native_workers": options.native_workers, "graph_workers": options.graph_workers,
            "packet_capacity_bytes": scene.preparation.as_ref().map_or(0, DrawPreparation::packet_capacity_bytes),
            "draws_per_pass": (0..options.passes).map(|p| scene.layout.pass_draws(p).len()).collect::<Vec<_>>(),
            "extent": EXTENT, "in_flight": IN_FLIGHT, "warmup": options.warmup,
            "frames": options.frames, "profile": options.profile, "wall_ns": wall_ns, "memory": memory,
            "samples": samples.iter().map(Sample::json).collect::<Vec<_>>(),
            "captures": captures, "validation": validation,
        }),
    )?;
    report.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uneven_passes_and_batches_cover_every_ordinal_in_declaration_order() {
        let options = Options {
            api: Api::Vulkan,
            draws: 1_024,
            words: 20,
            passes: 3,
            batches_per_pass: 5,
            topology: Topology::Chain,
            workers: 4,
            native_workers: 4,
            graph_workers: 4,
            report: PathBuf::new(),
            warmup: 0,
            frames: 0,
            profile: false,
            capture: None,
        };
        let layout = Layout::new(&options);
        assert_eq!(layout.batches.len(), 15);
        assert!(layout.batches.iter().all(|range| !range.is_empty()));
        assert!(layout.batches.iter().cloned().flatten().eq(0..1_024));
        assert_eq!(layout.pass_draws(0), 0..341);
        assert_eq!(layout.pass_draws(2), 682..1_024);
    }

    #[test]
    fn capture_rejects_a_missing_or_misplaced_draw_and_a_swapped_mrt_plane() {
        let mut image = [0, 0, 0, 255].repeat((EXTENT * EXTENT) as usize);
        image[8..12].copy_from_slice(&[128, 64, 191, 255]);
        assert!(verify_pixels(&image, 1..2, 0, 1, 0).is_ok());
        assert!(verify_pixels(&image, 1..2, 0, 0, 0).is_err());
        assert!(verify_pixels(&image, 0..2, 0, 1, 0).is_err());
        assert!(verify_pixels(&image, 2..3, 0, 1, 0).is_err());
    }
}
