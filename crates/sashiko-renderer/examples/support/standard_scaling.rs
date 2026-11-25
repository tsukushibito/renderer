#[path = "scaling_geometry.rs"]
mod geometry;
#[path = "validation.rs"]
mod validation;

use geometry::{INDICES, SEED, TRIANGLE, camera_x, placement};
use sashiko_renderer::{
    material::{AlphaMode, Materials, PbrParams, UnlitParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{Bounds, PbrVertex, Projection, ShadowSettings, Sun, Technique, Transform, ViewInput},
    pipeline::{
        CameraGroup, CameraView, PipelineResult, RenderObject, RendererSetup, SceneAssets,
        SceneInput, SceneRenderer, StandardConfig, StandardPipeline, ViewId,
    },
    unlit_forward::{IDENTITY, Vertex},
};
use sashiko_renderer_core::{
    graph::{ImageDesc, PassHandle},
    jobs::{JobSystem, RendererJobs},
    profiling::GpuTiming,
    runtime::{FrameId, Runtime, RuntimeConfig},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    path::PathBuf,
    time::{Duration, Instant},
};

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: sashiko_renderer_core::measurement::CountingAllocator<std::alloc::System> =
    sashiko_renderer_core::measurement::CountingAllocator::new(std::alloc::System);
const WAIT: Duration = Duration::from_secs(60);
const MAP: u32 = 128;

#[derive(Clone, Copy, PartialEq)]
enum Workload {
    Opaque,
    Textured,
    Blend,
    ShadowStatic,
    ShadowDynamic,
}
impl Workload {
    fn name(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::Textured => "textured",
            Self::Blend => "blend",
            Self::ShadowStatic => "shadow-static",
            Self::ShadowDynamic => "shadow-dynamic",
        }
    }
    fn shadow(self) -> bool {
        matches!(self, Self::ShadowStatic | Self::ShadowDynamic)
    }
}

struct Options {
    api: String,
    objects: usize,
    visible: u32,
    workload: Workload,
    team: NonZeroUsize,
    scene: bool,
    graph: bool,
    native: bool,
    warmup: usize,
    frames: usize,
    extent: [u32; 2],
    profile: bool,
    capture: bool,
    report: PathBuf,
}
impl Options {
    fn parse() -> PipelineResult<Self> {
        let mut args = std::env::args().skip(1);
        let mut out = Self {
            api: String::new(),
            objects: 50000,
            visible: 50,
            workload: Workload::Opaque,
            team: NonZeroUsize::new(4).unwrap(),
            scene: false,
            graph: false,
            native: false,
            warmup: 120,
            frames: 600,
            extent: [512; 2],
            profile: false,
            capture: false,
            report: PathBuf::new(),
        };
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| format!("missing value for {flag}"))
            };
            match flag.as_str() {
                "--api" => out.api = value()?,
                "--objects" => out.objects = value()?.parse()?,
                "--visible" => out.visible = value()?.parse()?,
                "--workload" => {
                    out.workload = match value()?.as_str() {
                        "opaque" => Workload::Opaque,
                        "textured" => Workload::Textured,
                        "blend" => Workload::Blend,
                        "shadow-static" => Workload::ShadowStatic,
                        "shadow-dynamic" => Workload::ShadowDynamic,
                        _ => return Err("invalid workload".into()),
                    }
                }
                "--participants" => out.team = value()?.parse()?,
                "--scene-jobs" => out.scene = true,
                "--graph-jobs" => out.graph = true,
                "--native-jobs" => out.native = true,
                "--warmup" => out.warmup = value()?.parse()?,
                "--frames" => out.frames = value()?.parse()?,
                "--extent" => {
                    let dimensions = value()?;
                    let (x, y) = dimensions.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
                    out.extent = [x.parse()?, y.parse()?];
                }
                "--profile" => out.profile = true,
                "--capture" => out.capture = true,
                "--report" => out.report = value()?.into(),
                _ => return Err(format!("unknown option {flag}").into()),
            }
        }
        if !matches!(out.objects, 1024 | 10000 | 50000)
            || out.visible > 100
            || out.frames < 1
            || out.extent.contains(&0)
            || out.extent.iter().any(|n| *n > 4096)
            || out.report.as_os_str().is_empty()
        {
            return Err("expected 1024/10000/50000 objects, visibility 0..=100, nonempty extent <=4096, --frames >=1 and --report path".into());
        }
        Ok(out)
    }
}

struct Scene {
    meshes: Meshes,
    materials: Materials,
    objects: Vec<RenderObject>,
    paint: Vec<sashiko_renderer::material::MaterialHandle>,
    sun: Sun,
    bounds: Bounds,
}
fn transform([x, y, z, s]: [f32; 4]) -> PipelineResult<Transform> {
    Ok(Transform::new([
        [s, 0., 0., x],
        [0., s, 0., y],
        [0., 0., s, z],
    ])?)
}
fn camera(step: usize, options: &Options) -> PipelineResult<CameraView> {
    let x = if options.workload.shadow() {
        0.
    } else {
        camera_x(step)
    };
    let mut matrix = IDENTITY;
    matrix[3][0] = -x;
    Ok(CameraView {
        id: ViewId(0),
        extent: options.extent,
        camera: ViewInput::new(
            matrix,
            [x, 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?,
    })
}

impl Scene {
    fn new(options: &Options) -> PipelineResult<Self> {
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 1,
            retired_mesh_versions: 1,
        })?;
        let mut materials = Materials::default();
        let mut paint = Vec::new();
        let mesh = if options.workload == Workload::Opaque {
            paint.push(materials.create_unlit(UnlitParams { color: [1.; 4] })?);
            meshes.create(MeshData::unlit(
                &TRIANGLE.map(|position| Vertex {
                    position,
                    color: [1.; 4],
                }),
                &INDICES,
            )?)?
        } else {
            for i in 0..if options.workload == Workload::Textured {
                4
            } else if options.workload == Workload::Blend {
                2
            } else {
                1
            } {
                paint.push(materials.create_pbr(paint_params(i, options.workload))?);
            }
            let vertices = TRIANGLE.map(|p| {
                PbrVertex::new(
                    p,
                    [0., 0., -1.],
                    [1., 0., 0., 1.],
                    [(p[0] + 1.) * 0.5, (p[1] + 1.) * 0.5],
                )
                .unwrap()
            });
            meshes.create(MeshData::pbr(&vertices, &INDICES)?)?
        };
        let mut objects = Vec::with_capacity(options.objects);
        for i in 0..options.objects {
            let material = if options.workload == Workload::Blend {
                usize::from(i % 5 == 0)
            } else {
                i % paint.len()
            };
            let mut object = RenderObject::new(mesh, paint[material]).with_transform(transform(
                placement(i, options.objects, options.visible, SEED),
            )?);
            object.casts_shadow = options.workload.shadow();
            objects.push(object);
        }
        Ok(Self {
            meshes,
            materials,
            objects,
            paint,
            sun: Sun::from_lux([0.3, 0.1, -1.], [1.; 3], 2.)?,
            bounds: Bounds::new([-1.25, -1.25, 0.], [4., 1.25, 1.])?,
        })
    }
    fn input(&self) -> SceneInput<'_> {
        SceneInput::new(&self.meshes, &self.materials, &self.objects)
            .with_sun(self.sun)
            .with_bounds(self.bounds)
    }
    fn update(&mut self, frame: usize, options: &Options) -> PipelineResult<()> {
        if options.workload == Workload::ShadowDynamic {
            // Moving a stable 1/64 subset exercises real history invalidation.
            for i in (0..self.objects.len()).step_by(64) {
                let mut position = placement(i, options.objects, options.visible, SEED);
                position[1] += camera_x(frame) * 0.5;
                self.objects[i].transform = transform(position)?;
            }
        }
        Ok(())
    }
}
fn paint_params(i: usize, workload: Workload) -> PbrParams {
    let mut p = PbrParams::color(
        [
            [0.85, 0.65, 0.4, 1.],
            [0.3, 0.8, 0.7, 1.],
            [0.5, 0.4, 0.9, 1.],
            [0.9, 0.3, 0.4, 1.],
        ][i],
    );
    p.factors.double_sided = true;
    if workload == Workload::Blend && i == 1 {
        p.factors.alpha_mode = AlphaMode::Blend;
        p.factors.base_color[3] = 0.5;
    }
    p
}
fn backend(api: &str) -> PipelineResult<RuntimeConfig> {
    let strict = cfg!(feature = "development-validation");
    match api {
        #[cfg(feature = "vulkan")]
        "vulkan" => Ok(RuntimeConfig::Vulkan(
            sashiko_renderer_core::runtime::VulkanConfig {
                validation: if strict {
                    sashiko_renderer_core::vulkan::ValidationMode::Required
                } else {
                    sashiko_renderer_core::vulkan::ValidationMode::Disabled
                },
                ..Default::default()
            },
        )),
        #[cfg(all(windows, feature = "d3d12"))]
        "d3d12" => Ok(RuntimeConfig::D3D12(
            sashiko_renderer_core::runtime::D3D12Config {
                validation: if strict {
                    sashiko_renderer_core::d3d12::D3D12ValidationMode::Required
                } else {
                    sashiko_renderer_core::d3d12::D3D12ValidationMode::Disabled
                },
                ..Default::default()
            },
        )),
        _ => Err("pass an enabled physical GPU backend: vulkan or d3d12".into()),
    }
}
fn identity(runtime: &Runtime) -> PipelineResult<Value> {
    #[cfg(feature = "vulkan")]
    if let Some(v) = runtime.as_vulkan() {
        let gpu = v.device_info();
        if gpu.device_type == "CPU" {
            return Err("benchmark requires a physical GPU".into());
        }
        return Ok(
            json!({"api":"vulkan","device":gpu.name,"luid":gpu.device_luid,"driver":gpu.driver_version_raw}),
        );
    }
    #[cfg(all(windows, feature = "d3d12"))]
    if let Some(v) = runtime.as_d3d12() {
        let gpu = v.device_info();
        return Ok(
            json!({"api":"d3d12","device":gpu.name,"luid":gpu.luid,"driver":gpu.driver_version}),
        );
    }
    Err("unsupported backend".into())
}
type Capture = [Option<PassHandle>; 7]; // SDR, HDR, depth, four shadow maps
#[derive(Default)]
struct Sample {
    interval: Duration,
    update: Duration,
    build: Duration,
    compile: Duration,
    passes: usize,
    shadow_reused: Option<[bool; 4]>,
    times: Option<[Duration; 5]>,
}
impl Sample {
    fn report(&self) -> Value {
        let ns = |d: Duration| d.as_nanos() as u64;
        json!({"interval_ns":ns(self.interval),"input_update_ns":ns(self.update),
            "build_ns":ns(self.build),"compile_ns":ns(self.compile),"passes":self.passes,
            "shadow_reused":self.shadow_reused,"prepare_ns":self.times.map(|v|ns(v[0])),
            "record_ns":self.times.map(|v|ns(v[1])),"submit_ns":self.times.map(|v|ns(v[2])),
            "retire_ns":self.times.map(|v|ns(v[3])),"gpu_ns":self.times.map(|v|ns(v[4]))})
    }
}
fn draw(
    renderer: &mut SceneRenderer,
    scene: &mut Scene,
    opts: &Options,
    step: usize,
    capture: bool,
) -> PipelineResult<(FrameId, Sample, Capture)> {
    let mut sample = Sample::default();
    let start = Instant::now();
    scene.update(step, opts)?;
    let camera = camera(step, opts)?;
    sample.update = start.elapsed();
    let start = Instant::now();
    let mut frame = renderer.begin_frame()?;
    let input = frame.bind_for_view(camera.id, scene.input())?;
    let view = frame.draw_view(input, &camera)?;
    let standard = frame
        .standard_output(camera.id)
        .ok_or("missing standard output")?;
    sample.shadow_reused = standard.reused_shadows();
    let maps = standard.scene.shadows;
    let output = frame
        .write_output(camera.id, view.color, 1., None)?
        .raw_image();
    let mut reads = [None; 7];
    if capture {
        for (slot, image) in reads.iter_mut().zip(
            [Some(output), Some(view.color), view.depth]
                .into_iter()
                .chain(maps.into_iter().flatten().map(Some)),
        ) {
            if let Some(image) = image {
                *slot = Some(frame.graph().readback_image("capture", image)?);
            }
        }
    } else {
        frame.graph().export_frame_image(output)?;
    }
    sample.build = start.elapsed();
    let start = Instant::now();
    let frame = frame.compile()?;
    sample.compile = start.elapsed();
    sample.passes = frame.pass_count();
    Ok((frame.submit()?.id(), sample, reads))
}

fn retire(
    renderer: &mut SceneRenderer,
    id: FrameId,
    sample: &mut Sample,
    profile: bool,
) -> PipelineResult<()> {
    let completion = renderer
        .runtime()
        .wait_for_frame(id, WAIT)?
        .ok_or("GPU frame timed out")?;
    if profile {
        let m = completion.metrics().ok_or("missing GPU profile")?;
        let GpuTiming::Available { elapsed, .. } = m.gpu else {
            return Err("missing GPU timestamps".into());
        };
        sample.times = Some([
            m.cpu_prepare,
            m.cpu_record,
            m.cpu_submit,
            m.cpu_retire,
            elapsed,
        ]);
    }
    Ok(())
}
fn measure(
    renderer: &mut SceneRenderer,
    scene: &mut Scene,
    opts: &Options,
    count: usize,
) -> PipelineResult<(Duration, Vec<Sample>, Value)> {
    let mut samples = Vec::with_capacity(count);
    let mut pending = VecDeque::with_capacity(2);
    let mut previous = None;
    #[cfg(feature = "memory-profiling")]
    let before = sashiko_renderer_core::measurement::Snapshot::now();
    let start = Instant::now();
    for step in 0..count {
        if pending.len() == 2 {
            let (i, id) = pending.pop_front().unwrap();
            retire(renderer, id, &mut samples[i], opts.profile)?;
        }
        let now = Instant::now();
        let (id, mut sample, _) = draw(renderer, scene, opts, step, false)?;
        sample.interval = previous.map_or(Duration::ZERO, |last| now - last);
        previous = Some(now);
        samples.push(sample);
        pending.push_back((step, id));
    }
    for (i, id) in pending.drain(..) {
        retire(renderer, id, &mut samples[i], opts.profile)?;
    }
    let wall = start.elapsed();
    #[cfg(feature = "memory-profiling")]
    let memory = {
        let delta = sashiko_renderer_core::measurement::Snapshot::now().since(before);
        json!({"allocation_free":delta.is_allocation_free(),"rust":delta.rust.map(|v|v.calls),
            "vma_host":delta.vma_host.map(|v|v.calls),"native":delta.native,"overflowed":delta.overflowed})
    };
    #[cfg(not(feature = "memory-profiling"))]
    let memory = Value::Null;
    Ok((wall, samples, memory))
}
fn capture(
    renderer: &mut SceneRenderer,
    scene: &mut Scene,
    opts: &Options,
) -> PipelineResult<Value> {
    // Read two distinct positions to detect a stale-frame/camera replay.
    let mut results = Vec::new();
    for step in [0, 17] {
        let (id, _, reads) = draw(renderer, scene, opts, step, true)?;
        let mut done = renderer
            .runtime()
            .wait_for_frame(id, WAIT)?
            .ok_or("readback timed out")?;
        let mut planes = Vec::new();
        for read in reads.into_iter().flatten() {
            let mut data = vec![0; done.readback_len(read)?];
            done.readback_into(read, &mut data)?;
            if planes.is_empty()
                && opts.visible > 0
                && !data.chunks_exact(4).any(|p| p[..3] != [0; 3])
            {
                return Err("all SDR pixels are black with visible geometry".into());
            }
            planes.push(format!("{:x}", Sha256::digest(&data)));
        }
        results.push(json!({"frame":step,"planes":planes}));
    }
    Ok(results.into())
}
pub fn run() -> PipelineResult<()> {
    let o = Options::parse()?;
    let mut scene = Scene::new(&o)?;
    let shared = if o.scene || o.graph || o.native {
        Some(RendererJobs::new(JobSystem::new(o.team)?))
    } else {
        None
    };
    let mut config = StandardConfig::new(o.extent);
    if o.workload.shadow() {
        config.shadows = Some(ShadowSettings {
            resolution: MAP,
            distance: 2.,
            ..Default::default()
        });
    }
    let enable_scene = o.scene;
    let scene_jobs = shared.clone();
    let group = CameraGroup::custom(camera(0, &o)?, move |_, limits, defs| {
        let pipeline = StandardPipeline::new(Technique::Forward, config, limits, defs)?;
        if enable_scene {
            let _ = &scene_jobs;
        }
        Ok(pipeline)
    });
    let mut setup = RendererSetup::new(group);
    setup.frames_in_flight = NonZeroUsize::new(2).unwrap();
    let mut plan = setup.prepare(SceneAssets::new(
        &mut scene.meshes,
        &mut scene.materials,
        &scene.objects,
    ))?;
    let desc = ImageDesc::rgba8_srgb(2, 2);
    if o.workload == Workload::Textured {
        plan.reserve_texture("checker", desc)?;
    }
    if o.capture {
        let [w, h] = o.extent;
        for desc in [
            ImageDesc::rgba8(w, h),
            ImageDesc::rgba16_float(w, h),
            ImageDesc::depth32(w, h),
        ] {
            plan.reserve_readback("capture", desc)?;
        }
        if o.workload.shadow() {
            for _ in 0..4 {
                plan.reserve_readback("capture", ImageDesc::depth32(MAP, MAP))?;
            }
        }
    }
    let mut renderer = plan.build(backend(&o.api)?)?;
    let device = identity(renderer.runtime())?;
    let mut texture = None;
    if o.workload == Workload::Textured {
        let (id, tex) = renderer.upload_texture(
            "checker",
            desc,
            &[
                255, 255, 255, 255, 80, 80, 80, 255, 80, 80, 80, 255, 255, 255, 255, 255,
            ],
        )?;
        renderer
            .runtime()
            .wait_for_frame(id, WAIT)?
            .ok_or("texture upload timed out")?;
        for (i, &material) in scene.paint.iter().enumerate() {
            let mut p = paint_params(i, o.workload);
            p.textures.base_color = Some(tex.snapshot().clone().into());
            scene.materials.update_pbr(material, p)?;
        }
        scene.materials.collect_gpu(renderer.runtime())?;
        renderer.prepare_assets(SceneAssets::new(
            &mut scene.meshes,
            &mut scene.materials,
            &scene.objects,
        ))?;
        texture = Some(tex);
    }
    if o.native {
        renderer.runtime().prepare_raster_recording(
            shared.as_ref().unwrap().clone(),
            NonZeroUsize::new(16).unwrap(),
        )?;
    }
    if o.profile {
        renderer.runtime().set_profiling(true)?;
    }
    measure(&mut renderer, &mut scene, &o, o.warmup)?;
    let (wall, samples, memory) = measure(&mut renderer, &mut scene, &o, o.frames)?;
    let captures = if o.capture {
        capture(&mut renderer, &mut scene, &o)?
    } else {
        Value::Null
    };
    let mut runtime = renderer.into_runtime()?;
    scene.meshes.release_gpu(&mut runtime)?;
    scene.materials.clear(&mut runtime)?;
    drop(texture);
    validation::check_validation(runtime)?;
    let report = json!({"schema":1,"api":o.api,"device":device,"validation":{"strict":cfg!(feature="development-validation"),"clean":true},
        "objects":o.objects,"visible":o.visible,"seed":SEED,"workload":o.workload.name(),"extent":o.extent,"in_flight":2,
        "participants":o.team.get(),"scene_jobs":o.scene,"graph_jobs":o.graph,"native_jobs":o.native,
        "warmup":o.warmup,"frames":o.frames,"profile":o.profile,"wall_ns":wall.as_nanos() as u64,
        "samples":samples.iter().map(Sample::report).collect::<Vec<_>>(),"memory":memory,"capture":captures});
    serde_json::to_writer(
        std::io::BufWriter::new(std::fs::File::create(o.report)?),
        &report,
    )?;
    Ok(())
}
