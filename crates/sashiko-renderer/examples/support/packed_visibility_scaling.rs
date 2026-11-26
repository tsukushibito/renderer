#[path = "scaling_geometry.rs"]
mod geometry;
#[path = "validation.rs"]
mod validation;

use std::{
    error::Error,
    num::NonZeroUsize,
    time::{Duration, Instant},
};

use sashiko_renderer::{
    passes::visibility::{IndexedGeometry, PreparedVisibility, WorldBounds},
    pbr::{Bounds, Frustum},
};
use sashiko_renderer_core::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    geometry::GeometryData,
    graph::{
        Attachment, BufferDesc, CompiledGraph, GraphPool, ImageDesc, ImportBuffer,
        IndexedIndirectArgs, IndexedIndirectTable, IndexedIndirectTemplate, LoadOp, PassHandle,
        RasterAttachments, RasterDraw, RasterParameters, StoreOp,
    },
    jobs::{JobSystem, SharedJobs},
    program::ManagedRasterProgram,
    runtime::{FrameSubmission, Runtime, RuntimeConfig, RuntimeLimits},
};
use serde_json::json;
use sha2::{Digest, Sha256};

type Failure = Box<dyn Error>;
const FRAMES: usize = 2;
const SIZE: u32 = 512;
const WAIT: Duration = Duration::from_secs(60);
const VERTEX_STRIDE: usize = 28;
const TRIANGLE_BYTES: usize = VERTEX_STRIDE * 3;
const CAMERA: BufferDesc = BufferDesc { size: 64 };

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: sashiko_renderer_core::measurement::CountingAllocator<std::alloc::System> =
    sashiko_renderer_core::measurement::CountingAllocator::new(std::alloc::System);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    SeparateDraws,
    MergedRanges,
    EntireGroups,
    Gpu,
}

impl Route {
    fn name(self) -> &'static str {
        match self {
            Self::SeparateDraws => "cpu",
            Self::MergedRanges => "cpu-ranges",
            Self::EntireGroups => "direct-all",
            Self::Gpu => "gpu",
        }
    }
}

struct Options {
    api: String,
    route: Route,
    objects: usize,
    visible: u32,
    moving: usize,
    groups: usize,
    workers: usize,
    native: bool,
    graph_jobs: bool,
    warmup: usize,
    frames: usize,
    profile: bool,
}

impl Options {
    fn parse() -> Result<Self, Failure> {
        let mut options = Self {
            api: "vulkan".into(),
            route: Route::SeparateDraws,
            objects: 50_000,
            visible: 50,
            moving: 0,
            groups: 1,
            workers: 0,
            native: false,
            graph_jobs: false,
            warmup: 120,
            frames: 600,
            profile: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            let mut next = || args.next().ok_or("missing argument value");
            match flag.as_str() {
                "--api" => options.api = next()?,
                "--route" => {
                    options.route = match next()?.as_str() {
                        "cpu" => Route::SeparateDraws,
                        "cpu-ranges" => Route::MergedRanges,
                        "direct-all" => Route::EntireGroups,
                        "gpu" => Route::Gpu,
                        _ => return Err("expected cpu, cpu-ranges, direct-all, or gpu".into()),
                    }
                }
                "--objects" => options.objects = next()?.parse()?,
                "--visible" => options.visible = next()?.parse()?,
                "--moving" => options.moving = next()?.parse()?,
                "--groups" => options.groups = next()?.parse()?,
                "--workers" => options.workers = next()?.parse()?,
                "--native" => options.native = true,
                "--graph-jobs" => options.graph_jobs = true,
                "--warmup" => options.warmup = next()?.parse()?,
                "--frames" => options.frames = next()?.parse()?,
                "--profile" => options.profile = true,
                _ => return Err(format!("unknown option {flag}").into()),
            }
        }
        if options.objects == 0
            || options.objects > 50_000
            || options.visible == 0
            || options.visible > 100
            || options.moving > options.objects
            || options.groups == 0
            || options.groups > 32
            || options.groups > options.objects
            || options.frames < 2
            || options.workers > std::thread::available_parallelism()?.get()
            || (options.workers == 0 && (options.native || options.graph_jobs))
        {
            return Err("invalid workload or worker configuration".into());
        }
        Ok(options)
    }
}

struct Scene {
    centers: Vec<[f32; 4]>,
    bounds: Vec<Bounds>,
    vertices: Vec<u8>,
    visible: Vec<bool>,
    ordinals: Vec<usize>,
    layout: PreparedVisibility,
    arguments: Vec<Option<IndexedIndirectArgs>>,
    encoded: Vec<u8>,
    group_ends: Vec<usize>,
}

impl Scene {
    fn new(options: &Options) -> Result<Self, Failure> {
        let count = options.objects;
        // Bake in stable spatial order once. The packing is identical for both routes
        // and never changes with the camera or visibility mask.
        let mut order: Vec<_> = (0..count).collect();
        order.sort_unstable_by_key(|&i| (i * 7919 + geometry::SEED as usize) % count);
        let centers = order
            .into_iter()
            .map(|i| geometry::placement(i, count, options.visible, geometry::SEED))
            .collect();
        let mut group_ends = vec![0];
        let mut tables = Vec::new();
        for group in 1..=options.groups {
            let end = group * count / options.groups;
            let commands: Vec<_> = (*group_ends.last().unwrap()..end)
                .map(|i| IndexedIndirectTemplate {
                    first_index: i as u32 * 3,
                    index_count: 3,
                })
                .collect();
            tables.push(IndexedIndirectTable::new(&commands)?);
            group_ends.push(end);
        }
        let layout = PreparedVisibility::new(&tables)?;
        let mut scene = Self {
            centers,
            bounds: vec![Bounds::new([0.; 3], [0.; 3])?; count],
            vertices: vec![0; count * TRIANGLE_BYTES],
            visible: vec![false; count],
            ordinals: Vec::with_capacity(count),
            arguments: vec![None; options.groups],
            encoded: vec![0; layout.bounds_desc().size as usize],
            group_ends,
            layout,
        };
        scene.update(count, 0.)?;
        Ok(scene)
    }

    fn update(&mut self, count: usize, motion: f32) -> Result<(), Failure> {
        for (i, chunk) in self.vertices[..count * TRIANGLE_BYTES]
            .as_chunks_mut::<TRIANGLE_BYTES>()
            .0
            .iter_mut()
            .enumerate()
        {
            let [x, y, z, half] = self.centers[i];
            let y = y + motion * half;
            self.bounds[i] = Bounds::new([x - half, y - half, z], [x + half, y + half, z])?;
            for (v, [dx, dy, _]) in chunk
                .as_chunks_mut::<VERTEX_STRIDE>()
                .0
                .iter_mut()
                .zip(geometry::TRIANGLE)
            {
                v.copy_from_slice(
                    [x + half * dx, y + half * dy, z, 1., 1., 1., 1.]
                        .map(f32::to_le_bytes)
                        .as_flattened(),
                );
            }
        }
        Ok(())
    }

    fn encode(&mut self) -> Result<(), Failure> {
        self.layout
            .encode_bounds(&mut self.encoded, |group, slot| {
                let b = self.bounds[self.group_ends[group] + slot];
                WorldBounds {
                    min: b.min(),
                    max: b.max(),
                    enabled: true,
                }
            })?;
        Ok(())
    }

    fn classify(
        &mut self,
        camera: [[f32; 4]; 4],
        jobs: Option<&SharedJobs>,
    ) -> Result<(), Failure> {
        let frustum = Frustum::new(camera, Bounds::new([-1.25, -1.25, 0.], [4., 1.25, 1.])?);
        let bounds = &self.bounds;
        let classify = |first: usize, mask: &mut [bool]| {
            for (slot, b) in mask.iter_mut().zip(&bounds[first..]) {
                *slot = frustum.overlaps(*b);
            }
        };
        if let Some(jobs) = jobs {
            let chunk = self.visible.len().div_ceil(jobs.parallelism().get() * 4);
            jobs.with(|team| {
                team.scope(|scope| {
                    scope.for_each_chunk_mut(
                        &mut self.visible,
                        NonZeroUsize::new(chunk).unwrap(),
                        classify,
                    )
                })
            });
        } else {
            classify(0, &mut self.visible);
        }
        self.ordinals.clear();
        self.ordinals.extend(
            self.visible
                .iter()
                .enumerate()
                .filter_map(|(i, &v)| v.then_some(i)),
        );
        Ok(())
    }
}

struct Resources {
    vertices: ImportBuffer,
    indices: ImportBuffer,
    bounds: Option<ImportBuffer>,
}

struct Frame {
    plan: CompiledGraph,
    readback: Option<[PassHandle; 2]>,
    phases: [Duration; 4],
}

fn initialize(
    options: &Options,
    scene: &mut Scene,
    pool: &mut GraphPool,
    rt: &mut Runtime,
) -> Result<Resources, Failure> {
    let idx: Vec<_> = (0..options.objects as u32)
        .flat_map(|i| geometry::INDICES.map(|j| 3 * i + j))
        .collect();
    let data = GeometryData::new(VERTEX_STRIDE as u32, &scene.vertices, &idx)?;
    let mut graph = pool.begin()?;
    let v = graph.buffer(
        "packed geometry",
        BufferDesc {
            size: data.vertex_byte_len(),
        },
    )?;
    let i = graph.buffer(
        "packed triangles",
        BufferDesc {
            size: data.index_byte_len(),
        },
    )?;
    data.upload_vertices(&mut graph, "upload vertices", v)?;
    data.upload_indices(&mut graph, "upload indices", i)?;
    graph.export_buffer(v)?;
    graph.export_buffer(i)?;
    let b = if options.route == Route::Gpu {
        scene.encode()?;
        let b = graph.buffer("packed bounds", scene.layout.bounds_desc())?;
        graph.upload_buffer("upload bounds", b, 0, &scene.encoded)?;
        graph.export_buffer(b)?;
        Some(b)
    } else {
        None
    };
    let receipt = rt.submit(&graph.compile()?)?;
    let result = Resources {
        vertices: receipt.export_buffer(v)?,
        indices: receipt.export_buffer(i)?,
        bounds: b.map(|b| receipt.export_buffer(b)).transpose()?,
    };
    rt.wait_for_frame(receipt.id(), WAIT)?
        .ok_or("setup timed out")?;
    Ok(result)
}

fn build(
    options: &Options,
    scene: &mut Scene,
    pool: &mut GraphPool,
    r: &Resources,
    jobs: Option<&SharedJobs>,
    frame: usize,
    capture: bool,
) -> Result<Frame, Failure> {
    let mut mark = options.profile.then(Instant::now);
    let mut lap = || {
        let next = options.profile.then(Instant::now);
        let elapsed = mark
            .zip(next)
            .map_or(Duration::ZERO, |(start, end)| end - start);
        mark = next;
        elapsed
    };
    let mut graph = pool.begin()?;
    let vertices = graph.import_buffer("mesh", &r.vertices)?;
    let indices = graph.import_buffer("indices", &r.indices)?;
    let mut matrix = *RasterParameters::default().transform();
    matrix[3][0] = -geometry::camera_x(frame);
    if options.moving > 0 {
        scene.update(options.moving, geometry::camera_x(frame) * 32.)?;
        graph.upload_buffer(
            "move vertices",
            vertices,
            0,
            &scene.vertices[..options.moving * TRIANGLE_BYTES],
        )?;
    }
    let update = lap();
    let args = if let Some(bounds) = &r.bounds {
        let bounds = graph.import_buffer("bounds", bounds)?;
        if options.moving > 0 {
            for (group, offsets) in scene.group_ends.windows(2).enumerate() {
                let count = offsets[1].min(options.moving).saturating_sub(offsets[0]);
                if count == 0 {
                    break;
                }
                let bytes =
                    scene
                        .layout
                        .encode_group_bounds(&mut scene.encoded, group, 0..count, |i| {
                            let b = scene.bounds[offsets[0] + i];
                            WorldBounds {
                                min: b.min(),
                                max: b.max(),
                                enabled: true,
                            }
                        })?;
                graph.upload_buffer(
                    "update bounds",
                    bounds,
                    bytes.start as u64,
                    &scene.encoded[bytes],
                )?;
            }
        }
        let camera = graph.buffer("camera", CAMERA)?;
        graph.upload_buffer(
            "update camera",
            camera,
            0,
            matrix
                .map(|column| column.map(f32::to_le_bytes))
                .as_flattened()
                .as_flattened(),
        )?;
        let vis =
            scene
                .layout
                .add_passes(&mut graph, camera, bounds, &mut scene.arguments, |_| {
                    Some(IndexedGeometry { vertices, indices })
                })?;
        Some(vis.arguments)
    } else {
        if options.route != Route::EntireGroups {
            scene.classify(matrix, jobs)?;
        }
        None
    };
    let visibility = lap();
    let color = graph.image("HDR", ImageDesc::rgba16_float(SIZE, SIZE))?;
    let depth = graph.image("depth", ImageDesc::depth32(SIZE, SIZE))?;
    let program = ManagedRasterProgram::mesh_hdr();
    let constants = RasterParameters::new(matrix, [1.; 4])?.into();
    let attachments = RasterAttachments {
        colors: &[Attachment {
            view: graph.image_view(color, 0, 0)?,
            load: LoadOp::Clear([0., 0., 0., 1.]),
            store: StoreOp::Store,
        }],
        depth: Some(Attachment {
            view: graph.image_view(depth, 0, 0)?,
            load: LoadOp::Clear(1.),
            store: StoreOp::Store,
        }),
        color_resolves: &[],
    };
    graph.raster("opaque triangles", attachments, |pass| {
        let command = |start: usize, end: usize| RasterDraw {
            vertices,
            indices,
            first_index: (start * 3) as u32,
            index_count: ((end - start) * 3) as u32,
            instance_count: 1,
            secondary_vertices: None,
            parameters: constants,
            bindings: None,
        };
        if let Some(args) = args {
            for &arg in args {
                pass.draw_indirect(&program, arg.expect("all groups enabled"), None, constants)?;
            }
        } else {
            let ordinals = &scene.ordinals;
            let chunk_size = ordinals.len().div_ceil(16usize.max(options.groups)).max(1);
            for group in scene.group_ends.windows(2) {
                if options.route == Route::EntireGroups {
                    pass.draw(&program, [command(group[0], group[1])])?;
                    continue;
                }
                let begin = ordinals.partition_point(|&i| i < group[0]);
                let end = ordinals.partition_point(|&i| i < group[1]);
                let visible = &ordinals[begin..end];
                if options.route == Route::MergedRanges {
                    let ranges = visible.chunk_by(|a, b| *b == *a + 1);
                    pass.draw(&program, ranges.map(|r| command(r[0], r[r.len() - 1] + 1)))?;
                } else {
                    for chunk in visible.chunks(chunk_size) {
                        pass.draw_generated(&program, chunk.len(), |i| {
                            command(chunk[i], chunk[i] + 1)
                        })?;
                    }
                }
            }
        }
        Ok(())
    })?;
    let readback = if capture {
        Some([
            graph.readback_image("capture HDR", color)?,
            graph.readback_image("capture depth", depth)?,
        ])
    } else {
        graph.export_frame_image(color)?;
        None
    };
    let raster = lap();
    let plan = graph.compile()?;
    Ok(Frame {
        plan,
        readback,
        phases: [update, visibility, raster, lap()],
    })
}

fn refresh(options: &Options, rt: &Runtime, r: &mut Resources) -> Result<(), Failure> {
    if options.moving > 0 {
        r.vertices = rt.refresh_buffer(r.vertices.key())?;
        if let Some(b) = &mut r.bounds {
            *b = rt.refresh_buffer(b.key())?;
        }
    }
    Ok(())
}

fn adapter(rt: &Runtime) -> serde_json::Value {
    #[cfg(feature = "vulkan")]
    if let Some(v) = rt.as_vulkan() {
        let info = v.device_info();
        return json!({"name":info.name,"driver":info.driver_version_raw,"luid":info.device_luid});
    }
    #[cfg(all(windows, feature = "d3d12"))]
    if let Some(d) = rt.as_d3d12() {
        let info = d.device_info();
        return json!({"name":info.name,"driver":info.driver_version,"luid":info.luid});
    }
    unreachable!("native backend required")
}

pub fn run() -> Result<(), Failure> {
    let o = Options::parse()?;
    let mut scene = Scene::new(&o)?;
    let team = NonZeroUsize::new(o.workers)
        .map(JobSystem::new)
        .transpose()?
        .map(SharedJobs::new);
    let mut prep = Preparation::new(NonZeroUsize::new(FRAMES).unwrap());
    let vertices = BufferDesc {
        size: scene.vertices.len() as u64,
    };
    let indices = BufferDesc {
        size: (o.objects * 12) as u64,
    };
    let mut classes = vec![
        ResourceClass::Buffer {
            desc: vertices,
            count: 1,
        },
        ResourceClass::Buffer {
            desc: indices,
            count: 1,
        },
    ];
    if o.route == Route::Gpu {
        scene.layout.reserve(&mut prep)?;
        prep.reserve_buffer(CAMERA, 1)?;
        classes.push(ResourceClass::Buffer {
            desc: scene.layout.bounds_desc(),
            count: 1,
        });
    }
    let vertex_update = o.moving * TRIANGLE_BYTES;
    let bound_bytes = if o.route == Route::Gpu {
        scene.encoded.len()
    } else {
        0
    };
    let draw_count = if o.route == Route::Gpu {
        o.groups
    } else {
        o.objects
    };
    prep.reserve_image(ImageDesc::rgba16_float(SIZE, SIZE), 1)?;
    prep.reserve_image(ImageDesc::depth32(SIZE, SIZE), 1)?;
    prep.add_requirements(
        GraphRequirements {
            resources: 10,
            passes: 12 + o.groups,
            accesses: 32 + o.groups,
            draws: draw_count,
            raster_constant_words: draw_count * 20,
            upload_bytes: (vertex_update + bound_bytes + 64).max(bound_bytes),
            ..Default::default()
        },
        WorkingSet {
            resources: &classes,
            raster: &[ManagedRasterProgram::mesh_hdr().into()],
            ..Default::default()
        },
    )?;
    let mut config = match o.api.as_str() {
        #[cfg(feature = "vulkan")]
        "vulkan" => RuntimeConfig::Vulkan(sashiko_renderer_core::runtime::VulkanConfig {
            validation: if cfg!(feature = "development-validation") {
                sashiko_renderer_core::vulkan::ValidationMode::Required
            } else {
                sashiko_renderer_core::vulkan::ValidationMode::Disabled
            },
            ..Default::default()
        }),
        #[cfg(all(windows, feature = "d3d12"))]
        "d3d12" => RuntimeConfig::D3D12(sashiko_renderer_core::runtime::D3D12Config {
            validation: if cfg!(feature = "development-validation") {
                sashiko_renderer_core::d3d12::D3D12ValidationMode::Required
            } else {
                sashiko_renderer_core::d3d12::D3D12ValidationMode::Disabled
            },
            ..Default::default()
        }),
        _ => return Err("requested native API unavailable".into()),
    };
    let upload = (vertices.size + indices.size + bound_bytes as u64)
        .max((vertex_update + bound_bytes + 64) as u64 + scene.layout.template_transfer_bytes());
    config.fit_preparation(&prep, upload, u64::from(SIZE) * u64::from(SIZE) * 12)?;
    let mut limits = prep.runtime_limits(RuntimeLimits {
        frames: FRAMES,
        results: FRAMES,
        ..Default::default()
    })?;
    limits.resource_capacity = limits.resource_capacity.max(prep.resource_count()? + 4);
    let mut rt = Runtime::new(config, limits, prep.working_set())?;
    #[cfg(feature = "vulkan")]
    if rt
        .as_vulkan()
        .is_some_and(|v| v.device_info().device_type == "CPU")
    {
        return Err("physical GPU required".into());
    }
    if o.native {
        rt.prepare_raster_recording(
            team.as_ref().unwrap().clone(),
            NonZeroUsize::new(16 + o.groups).unwrap(),
        )?;
    }
    if o.profile {
        rt.set_profiling(true)?;
    }
    let mut cold_pool = GraphPool::new(limits.graph, 1)?;
    let mut r = initialize(&o, &mut scene, &mut cold_pool, &mut rt)?;
    drop(cold_pool);
    // A partial update of a registered buffer retains its upload provenance
    // across submissions. The next checkout may overlap both in-flight frames.
    let slots = FRAMES + usize::from(o.moving > 0) * (FRAMES + 1);
    let mut pool = GraphPool::new(limits.graph, slots)?;
    if o.graph_jobs {
        pool.set_raster_jobs(team.as_ref().unwrap().clone(), NonZeroUsize::MIN);
    }
    let mut intervals = Vec::with_capacity(o.frames);
    let mut phases: [Vec<f64>; 8] =
        std::array::from_fn(|_| Vec::with_capacity(if o.profile { o.frames } else { 0 }));

    for (frames, measuring) in [(o.warmup, false), (o.frames, true)] {
        let mut previous = None;
        #[cfg(feature = "memory-profiling")]
        let memory = sashiko_renderer_core::measurement::Snapshot::now();
        let started = Instant::now();
        for frame in 0..frames {
            let now = Instant::now();
            if let Some(previous) = previous
                && measuring
            {
                intervals.push(now.duration_since(previous).as_secs_f64() * 1000.);
            }
            previous = Some(now);
            if rt.pending_frames() == FRAMES {
                retire(&mut rt, o.profile && measuring, &mut phases[4..])?;
            }
            let built = build(&o, &mut scene, &mut pool, &r, team.as_ref(), frame, false)?;
            if measuring && o.profile {
                for (series, d) in phases.iter_mut().zip(built.phases) {
                    series.push(d.as_secs_f64() * 1000.);
                }
            }
            drop(rt.submit(&built.plan)?);
            refresh(&o, &rt, &mut r)?;
        }
        while rt.pending_frames() > 0 {
            retire(&mut rt, o.profile && measuring, &mut phases[4..])?;
        }
        let elapsed = started.elapsed();
        if measuring {
            #[cfg(feature = "memory-profiling")]
            let memory = sashiko_renderer_core::measurement::Snapshot::now().since(memory);
            intervals.sort_by(f64::total_cmp);
            for series in &mut phases {
                series.sort_by(f64::total_cmp);
            }
            let percentile = |p: usize| intervals[(intervals.len() * p).div_ceil(100) - 1];
            let mut report = json!({"route":o.route.name(),"api":o.api,"objects":o.objects,
                "visibility_percent":o.visible,"moving_objects":o.moving,"groups":o.groups,"workers":o.workers,"native":o.native,"graph_jobs":o.graph_jobs,
                "warmup":o.warmup,"frames":frames,"fps":frames as f64/elapsed.as_secs_f64(),
                "p95_ms":percentile(95),"p99_ms":percentile(99),"gpu_p50_ms":phases[7].get(phases[7].len()/2),
                "template_upload_bytes":if o.route == Route::Gpu{scene.layout.template_transfer_bytes()}else{0},
                "bounds_upload_bytes":if o.route == Route::Gpu {o.moving*32}else{0}});
            if o.profile {
                let mut profile = serde_json::Map::new();
                let names = [
                    "update_geometry",
                    "visibility",
                    "raster_graph",
                    "compile",
                    "prepare",
                    "record",
                    "submit",
                    "gpu",
                ];
                for (name, series) in names.into_iter().zip(&phases) {
                    profile.insert(name.into(), json!(series[series.len() / 2]));
                }
                report["phase_p50_ms"] = profile.into();
            }
            #[cfg(feature = "memory-profiling")]
            {
                report["memory"] = json!({"rust":memory.rust.map(|e|e.calls),"vma":memory.vma_host.map(|e|e.calls),"native":memory.native,"overflowed":memory.overflowed});
            }
            let hashes = capture_hashes(
                &o,
                &mut scene,
                &mut pool,
                &mut rt,
                &mut r,
                team.as_ref(),
                false,
            )?;
            if o.moving > 0 {
                report["boundary_hdr_depth_sha256"] = json!(capture_hashes(
                    &o,
                    &mut scene,
                    &mut pool,
                    &mut rt,
                    &mut r,
                    team.as_ref(),
                    true
                )?);
            }
            report["adapter"] = adapter(&rt);
            report["strict"] = json!(cfg!(feature = "development-validation"));
            report["hdr_depth_sha256"] = json!(hashes);
            println!("{report}");
        }
    }
    for key in [
        Some(r.indices.key()),
        Some(r.vertices.key()),
        r.bounds.as_ref().map(ImportBuffer::key),
    ]
    .into_iter()
    .flatten()
    {
        rt.delete(key)?;
    }
    validation::check_validation(rt)
}

/// Readbacks run after the final timed GPU drain. The additional moving-object
/// replay crosses the left clip plane and probes a strip outside the static grid.
fn capture_hashes(
    o: &Options,
    scene: &mut Scene,
    pool: &mut GraphPool,
    rt: &mut Runtime,
    resources: &mut Resources,
    jobs: Option<&SharedJobs>,
    boundary: bool,
) -> Result<Vec<String>, Failure> {
    let mut hashes = Vec::with_capacity(6);
    for frame in [0, 31, 63] {
        if boundary {
            // Keep one sentinel large enough to cover samples even at 50k candidates.
            scene.centers[0][1] = 0.;
            scene.centers[0][3] = 0.02;
            for center in &mut scene.centers[..o.moving] {
                center[0] = if frame == 31 { -0.97 } else { -1.18 };
            }
        }
        let built = build(o, scene, pool, resources, jobs, frame, true)?;
        let receipt: FrameSubmission = rt.submit(&built.plan)?;
        refresh(o, rt, resources)?;
        let mut done = rt
            .wait_for_frame(receipt.id(), WAIT)?
            .ok_or("capture timeout")?;
        for (index, h) in built.readback.unwrap().into_iter().enumerate() {
            let mut bytes = vec![0; done.readback_len(h)?];
            done.readback_into(h, &mut bytes)?;
            if bytes.as_chunks::<8>().0.iter().all(|v| v == &bytes[..8]) && !boundary {
                return Err("empty or constant frame".into());
            }
            if boundary && index == 0 && o.objects >= 1024 {
                let width = SIZE as usize;
                let lit = bytes
                    .as_chunks::<8>()
                    .0
                    .chunks_exact(width)
                    .any(|row| row[..width / 32].iter().any(|pixel| pixel[..2] != [0, 0]));
                if lit != (frame == 31) {
                    return Err("moving triangle did not cross the clip plane as expected".into());
                }
            }
            hashes.push(format!("{:x}", Sha256::digest(bytes)));
        }
    }
    Ok(hashes)
}

fn retire(rt: &mut Runtime, profile: bool, phases: &mut [Vec<f64>]) -> Result<(), Failure> {
    let done = rt.wait_frame(WAIT)?.ok_or("frame timed out")?;
    if profile {
        let metrics = done.metrics().ok_or("missing metrics")?;
        for (series, duration) in
            phases
                .iter_mut()
                .zip([metrics.cpu_prepare, metrics.cpu_record, metrics.cpu_submit])
        {
            series.push(duration.as_secs_f64() * 1000.);
        }
        match metrics.gpu {
            sashiko_renderer_core::GpuTiming::Available { elapsed, .. } => {
                phases[3].push(elapsed.as_secs_f64() * 1000.)
            }
            _ => return Err("GPU timing unavailable".into()),
        }
    }
    Ok(())
}
