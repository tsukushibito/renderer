//! Measurement boundaries include changing Graph construction and completion.

use super::{
    Error, Options,
    asset::Scene,
    backend::Backend,
    camera::{Camera, Conditions},
    pipeline::{self, Budget, Resident, Workspace},
};
use renderer::{
    SubmissionMetrics,
    graph::{CompiledGraph, GraphPool},
};
use serde_json::{Value, json};
use std::{collections::VecDeque, path::Path, time::Instant};

struct Sample {
    phases: [u64; 5],
    latency: u64,
    native: Option<SubmissionMetrics>,
    poll_cpu_ns: u64,
    yield_ns: u64,
    iteration_ns: u64,
}
struct Pending<B: Backend> {
    work: B::Pending,
    plan: CompiledGraph,
    sample: Sample,
    started: Instant,
    record: bool,
}

fn camera(conditions: &Conditions, options: &Options, frame: usize) -> Result<Camera, Error> {
    if options.fixture {
        Ok(Camera::fixture())
    } else {
        conditions.camera(&options.view, frame)
    }
}
fn uniform(
    conditions: &Conditions,
    options: &Options,
    frame: usize,
) -> Result<(Camera, [u8; 128]), Error> {
    let camera = camera(conditions, options, frame)?;
    Ok((
        camera,
        camera.uniform(
            options.width as f32 / options.height as f32,
            conditions.near,
            conditions.far,
            conditions.light_direction,
            conditions.light_color,
        )?,
    ))
}

#[allow(
    clippy::too_many_arguments,
    reason = "one explicit bounded frame context; avoids another owner abstraction"
)]
fn frames<B: Backend>(
    backend: &mut B,
    resident: &mut Resident,
    scene: &Scene,
    options: &Options,
    conditions: &Conditions,
    pool: &mut GraphPool,
    workspace: &mut Workspace,
    queue: &mut VecDeque<Pending<B>>,
    samples: &mut Vec<Sample>,
    count: usize,
    record: bool,
    drain: bool,
) -> Result<(), Error> {
    fn retire<B: Backend>(mut entry: Pending<B>, samples: &mut Vec<Sample>) -> Result<(), Error> {
        #[cfg(feature = "tracy")]
        let _retire = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("bistro.retire"), 0));
        let started = Instant::now();
        let completed = {
            #[cfg(feature = "tracy")]
            let _poll = tracy_client::Client::running().map(|client| {
                client.span(tracy_client::span_location!("bistro.poll_and_retire"), 0)
            });
            let timeout = Instant::now();
            loop {
                let poll = Instant::now();
                let completed = B::poll(&mut entry.work)?;
                entry.sample.poll_cpu_ns += poll.elapsed().as_nanos() as u64;
                if let Some(completed) = completed {
                    break completed;
                }
                if timeout.elapsed() > super::WAIT {
                    return Err("Bistro completion timed out".into());
                }
                let yielded = Instant::now();
                std::thread::yield_now();
                entry.sample.yield_ns += yielded.elapsed().as_nanos() as u64;
            }
        };
        entry.sample.phases[3] = started.elapsed().as_nanos() as u64;
        entry.sample.native = B::metrics(&completed);
        entry.sample.latency = entry.started.elapsed().as_nanos() as u64;
        let started = Instant::now();
        {
            #[cfg(feature = "tracy")]
            let _return = tracy_client::Client::running()
                .map(|client| client.span(tracy_client::span_location!("bistro.result_return"), 0));
            drop((completed, entry.work, entry.plan));
        }
        entry.sample.phases[4] = started.elapsed().as_nanos() as u64;
        if entry.record {
            samples.push(entry.sample);
        }
        Ok(())
    }
    for frame in 0..count {
        let iteration_started = Instant::now();
        // This zone is a contiguous CPU iteration. Pending.started continues
        // to measure build-to-observed-completion latency across iterations.
        #[cfg(feature = "tracy")]
        let frame_zone = if record {
            tracy_client::Client::running()
                .map(|client| client.span(super::trace::frame_location(), 0))
        } else {
            None
        };
        if queue.len() == options.inflight {
            retire(queue.pop_front().unwrap(), samples)?;
        }
        let started = Instant::now();
        let stage = Instant::now();
        let (camera, bytes) = {
            #[cfg(feature = "tracy")]
            let _uniform = tracy_client::Client::running()
                .map(|client| client.span(tracy_client::span_location!("bistro.uniform"), 0));
            uniform(conditions, options, frame)?
        };
        let built = {
            #[cfg(feature = "tracy")]
            let _build = tracy_client::Client::running()
                .map(|client| client.span(tracy_client::span_location!("bistro.build"), 0));
            pipeline::build(
                pool,
                workspace,
                resident,
                scene,
                options,
                camera,
                &bytes,
                conditions.background,
                false,
            )?
        };
        let build = stage.elapsed().as_nanos() as u64;
        let stage = Instant::now();
        let plan = {
            #[cfg(feature = "tracy")]
            let _compile = tracy_client::Client::running()
                .map(|client| client.span(tracy_client::span_location!("bistro.compile"), 0));
            built.graph.compile()?
        };
        let compile = stage.elapsed().as_nanos() as u64;
        let stage = Instant::now();
        let work = {
            #[cfg(feature = "tracy")]
            let _submit = tracy_client::Client::running()
                .map(|client| client.span(tracy_client::span_location!("bistro.submit"), 0));
            backend.submit(&plan)?
        };
        {
            #[cfg(feature = "tracy")]
            let _export = tracy_client::Client::running().map(|client| {
                client.span(tracy_client::span_location!("bistro.export_and_delete"), 0)
            });
            resident.camera = B::export_buffer(&work, built.camera)?;
            workspace.accept_shadow_updates::<B>(&work, built.shadow_updates)?;
            let output = B::export_image(&work, built.output)?;
            // Keep the Graph live without a mandatory readback. Deleting a registry
            // name is not early GPU release: pending work owns its prepared lease.
            backend.delete(output.key())?;
        }
        let submit = stage.elapsed().as_nanos() as u64;
        queue.push_back(Pending {
            work,
            plan,
            sample: Sample {
                phases: [build, compile, submit, 0, 0],
                latency: 0,
                native: None,
                poll_cpu_ns: 0,
                yield_ns: 0,
                iteration_ns: iteration_started.elapsed().as_nanos() as u64,
            },
            started,
            record,
        });
        #[cfg(feature = "tracy")]
        {
            drop(frame_zone);
            if record && let Some(client) = tracy_client::Client::running() {
                client.frame_mark();
            }
        }
    }
    #[cfg(feature = "tracy")]
    let _drain = if record && drain {
        tracy_client::Client::running().map(|client| client.span(super::trace::drain_location(), 0))
    } else {
        None
    };
    if drain {
        while let Some(entry) = queue.pop_front() {
            retire(entry, samples)?;
        }
    }
    Ok(())
}

fn identity(
    options: &Options,
    cook_hash: &str,
    conditions_hash: &str,
    environment: &Value,
) -> Value {
    let shadow = super::shadow::Parameters::default();
    json!({"cook_manifest_sha256":cook_hash,"conditions_sha256":conditions_hash,
        "shaders_sha256":super::digest(include_bytes!("shaders/manifest.json")),
        "view":if options.fixture {"fixture"}else{options.view.as_str()},"width":options.width,"height":options.height,
        "inflight":options.inflight,"alpha_only":options.alpha_only,"alpha_override":options.force_opaque,
        "environment":environment,"shadows":options.shadows,"aa":options.aa,
        "shadow_profile":{"cascades":4,"resolution":shadow.resolution,"distance_m":shadow.distance,
            "split_lambda":shadow.split_lambda,"blend_fraction":shadow.blend_fraction,
            "depth_bias_texels":shadow.depth_bias_texels,"slope_bias_texels":shadow.slope_bias_texels,
            "normal_offset_texels":shadow.normal_offset_texels,"depth_padding_m":shadow.depth_padding,
            "filter":"manual nearest 3x3 PCF"},
        "aa_profile":{"edge_threshold_min":0.0312,"edge_threshold_max":0.125,"iterations":12,
            "subpixel_quality":0.75,"position":"after Neutral, before sRGB"},
        "lighting":"B3 GGX sun + static split-sum IBL; four stable CSM cascades with manual 3x3 PCF",
        "output":"linear RGBA16F -> EV+1 -> Khronos PBR Neutral -> optional FXAA -> single sRGB OETF -> RGBA8 UNORM; alpha 1",
        "completion":"nonblocking backend poll with thread::yield_now on not-ready; no VSync/presentation"})
}

fn loaded_cook_hash(scene: &Scene, fixture: bool) -> Result<String, Error> {
    if fixture {
        return Ok(super::digest(b"builtin-pbr-material-fixture-v1"));
    }
    scene.source["cooked_manifest_sha256"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "loaded scene has no cooked manifest identity".into())
}

#[allow(
    clippy::too_many_arguments,
    reason = "captures use the same prepared frame context outside measurement"
)]
fn capture<B: Backend>(
    backend: &mut B,
    resident: &mut Resident,
    scene: &Scene,
    options: &Options,
    conditions: &Conditions,
    pool: &mut GraphPool,
    workspace: &mut Workspace,
) -> Result<Value, Error> {
    std::fs::create_dir_all(&options.output)?;
    let (camera, bytes) = uniform(conditions, options, 0)?;
    let built = pipeline::build(
        pool,
        workspace,
        resident,
        scene,
        options,
        camera,
        &bytes,
        conditions.background,
        true,
    )?;
    let plan = built.graph.compile()?;
    let mut pending = backend.submit(&plan)?;
    resident.camera = B::export_buffer(&pending, built.camera)?;
    workspace.accept_shadow_updates::<B>(&pending, built.shadow_updates)?;
    let output = B::export_image(&pending, built.output)?;
    backend.delete(output.key())?;
    let mut completed = B::wait(&mut pending, super::WAIT)?;
    let passes = built.captures.unwrap();
    let pixels = (options.width as usize) * (options.height as usize);
    let mut captures = json!({});
    let mut depth_coverage = 0usize;
    for (slot, name, format, stride) in [
        (0, "hdr", "rgba16f-le", 8),
        (1, "sdr", "rgba8-unorm", 4),
        (2, "depth", "r32f-le", 4),
    ] {
        let mut bytes = vec![0u8; pixels * stride];
        B::readback(&mut completed, passes[slot], &mut bytes)?;
        if slot == 2 {
            for value in bytes.as_chunks::<4>().0 {
                let value = f32::from_le_bytes(*value);
                if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                    return Err("capture has invalid depth".into());
                }
                depth_coverage += usize::from(value < 1.0);
            }
        }
        let path = options.output.join(format!("{name}.bin"));
        std::fs::write(&path, &bytes)?;
        captures[name] = json!({"path":std::fs::canonicalize(&path)?,"format":format,
            "width":options.width,"height":options.height,"sha256":super::digest(&bytes),"byte_length":bytes.len()});
    }
    let mut shadow_captures = json!({});
    if let Some(passes) = built.shadow_captures {
        let resolution = resident.shadow_parameters.resolution;
        for (cascade, pass) in passes.into_iter().enumerate() {
            let mut bytes = vec![0; resolution as usize * resolution as usize * 4];
            B::readback(&mut completed, pass, &mut bytes)?;
            if bytes.as_chunks::<4>().0.iter().any(|v| {
                let d = f32::from_le_bytes(*v);
                !d.is_finite() || !(0.0..=1.0).contains(&d)
            }) {
                return Err("capture has invalid shadow depth".into());
            }
            let name = format!("shadow{cascade}");
            let path = options.output.join(format!("{name}.bin"));
            std::fs::write(&path, &bytes)?;
            shadow_captures[name] = json!({"path":std::fs::canonicalize(&path)?,
                "format":"r32f-le","width":resolution,"height":resolution,
                "sha256":super::digest(&bytes),"byte_length":bytes.len()});
        }
    }
    let mut report = json!({"captures":captures,"shadow_captures":shadow_captures,"draw_count":built.draws,"depth_coverage_pixels":depth_coverage,
        "readback_outside_measurement":true});
    if let Some(updates) = built.shadow_updates {
        report["shadow_reuse_hits"] = json!(updates.map(|update| update.is_none()));
    }
    Ok(report)
}

fn measurement(
    samples: &[Sample],
    options: &Options,
    wall: u64,
    cpu: Value,
) -> Result<Value, Error> {
    if samples.len() != options.frames {
        return Err("measured frame count mismatch".into());
    }
    if !options.gpu_timing && samples.iter().any(|sample| sample.native.is_some()) {
        return Err("unrequested native profiling samples in normal measurement".into());
    }
    let names = [
        "build",
        "compile",
        "submit",
        "poll_and_retire",
        "result_return",
    ];
    let mut phases = json!({});
    for (index, name) in names.into_iter().enumerate() {
        phases[name] = json!(
            samples
                .iter()
                .map(|sample| sample.phases[index])
                .sum::<u64>()
        );
    }
    let mut gpu_total = 0u64;
    let mut gpu_min = u64::MAX;
    let mut gpu_max = 0u64;
    let mut gpu_samples = 0usize;
    let mut native = [0u64; 4];
    for sample in samples {
        if let Some(metrics) = sample.native {
            for (total, value) in native.iter_mut().zip([
                metrics.cpu_prepare,
                metrics.cpu_record,
                metrics.cpu_submit,
                metrics.cpu_retire,
            ]) {
                *total += value.as_nanos() as u64;
            }
            match metrics.gpu {
                renderer::GpuTiming::Available { elapsed, .. } => {
                    let ns = elapsed.as_nanos() as u64;
                    gpu_samples += 1;
                    gpu_total += ns;
                    gpu_min = gpu_min.min(ns);
                    gpu_max = gpu_max.max(ns);
                }
                other => return Err(format!("requested GPU timing unavailable: {other:?}").into()),
            }
        }
    }
    if options.gpu_timing && gpu_samples != samples.len() {
        return Err("missing requested GPU samples".into());
    }
    Ok(
        json!({"frames":samples.len(),"warmup_frames":options.warmup,"inflight":options.inflight,"wall_ns":wall,"cpu":cpu,
        "phase_totals_ns":phases,"native_profiling_enabled":options.gpu_timing,
        "native_phase_totals_ns":options.gpu_timing.then(||json!({"prepare":native[0],"record":native[1],"submit":native[2],"retire":native[3]})),
        "phase_scope":"caller elapsed phases; enabled native CPU timers are nested and must not be added; GPU-timing switch controls native timers and timestamp queries; disabled native totals are null",
        "gpu":{"enabled":options.gpu_timing,"samples":gpu_samples,"total_ns":gpu_total,
            "min_ns":if gpu_samples==0{None}else{Some(gpu_min)},"max_ns":if gpu_samples==0{None}else{Some(gpu_max)}},
        "frame_wall_ns":samples.iter().map(|sample|sample.latency).collect::<Vec<_>>(),
        "frame_samples":samples.iter().enumerate().map(|(frame,s)|json!({"frame":frame,
            "phase_ns":s.phases,"poll_calls_elapsed_ns":s.poll_cpu_ns,"yield_elapsed_ns":s.yield_ns,
            "active_host_elapsed_ns":s.phases[0]+s.phases[1]+s.phases[2]+s.poll_cpu_ns+s.phases[4],
            "iteration_wall_ns":s.iteration_ns,
            "gpu_ns":s.native.and_then(|n|if let renderer::GpuTiming::Available{elapsed,..}=n.gpu {Some(elapsed.as_nanos() as u64)}else{None})})).collect::<Vec<_>>(),
        "active_host_scope":"nonoverlapping caller build/compile/submit/poll-call/result-return elapsed phase sum; excludes explicit yield waits and bookkeeping outside these timers (poll-loop timeout checks, metrics/sample recording); includes poll-call cost and possible OS descheduling; complete windows retain the omitted bookkeeping; not charged CPU or Godot-identical scope",
        "frame_wall_scope":"host build start to observed GPU completion; includes queue residence and intervening host work; excludes result return; not display latency",
        "readback_outside_measurement":true,"presentation":false}),
    )
}

fn write_report(directory: &Path, report: &Value) -> Result<(), Error> {
    std::fs::create_dir_all(directory)?;
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(report)?,
    )?;
    Ok(())
}

#[cfg(feature = "memory-profiling")]
fn memory_report(snapshot: renderer::measurement::Snapshot) -> Value {
    json!({"rust":snapshot.rust.map(|event|json!({"calls":event.calls,"bytes":event.bytes})),
        "vma_host":snapshot.vma_host.map(|event|json!({"calls":event.calls,"bytes":event.bytes})),
        "native":snapshot.native,"overflowed":snapshot.overflowed,"allocation_free":snapshot.is_allocation_free(),
        "event_order":"Rust Allocate/AllocateZeroed/Reallocate/Deallocate; VMA Allocate/Reallocate/Deallocate; native ObjectCreate/ObjectDestroy/ResourceCreate/ResourceDestroy/MemoryAllocate/MemoryFree/DescriptorAllocate/DescriptorFree/Reset"})
}

pub(super) fn execute<B: Backend>(options: &Options) -> Result<(), Error> {
    if options.output.join("report.json").exists() {
        return Err("output report already exists; choose a new output directory".into());
    }
    #[cfg(feature = "tracy")]
    super::trace::prepare_locations();
    #[cfg(feature = "tracy")]
    let tracy_capture_id = super::trace::capture_id()?;
    #[cfg(feature = "memory-profiling")]
    let setup_memory = renderer::measurement::Snapshot::now();
    let setup = Instant::now();
    let stage = Instant::now();
    let condition_bytes = include_bytes!("../../../../../tools/bistro/conditions.json");
    let conditions_hash = super::digest(condition_bytes);
    let conditions = Conditions::parse(&serde_json::from_slice(condition_bytes)?)?;
    eprintln!("Bistro asset load boundary");
    let mut scene = if let Some(path) = &options.manifest {
        Scene::load(path, options.force_opaque)?
    } else {
        Scene::fixture(options.force_opaque)
    };
    if !options.fixture && !conditions.ambient_occlusion {
        scene.disable_environment_occlusion();
    }
    let mut environment =
        if options.fixture {
            super::ibl::Environment::fixture()
        } else {
            super::ibl::Environment::load(options.ibl_manifest.as_deref().ok_or(
                "B3 full-scene rendering requires --ibl-manifest from tools/bistro/ibl.py",
            )?)?
        };
    let environment_identity = environment.identity.clone();
    // Hash the same manifest bytes that Scene::load parsed. A separate read
    // could otherwise identify a different revision from the loaded contents.
    let cook_hash = loaded_cook_hash(&scene, options.fixture)?;
    let asset_load_ns = stage.elapsed().as_nanos() as u64;
    let source = scene.source.clone();
    let counts = json!({"meshes":scene.mesh_count,"primitives":scene.primitives.len(),"materials":scene.materials.len(),
        "source":source,"resident_images_with_defaults":scene.images.len(),
        "alpha_zero_materials":scene.materials.iter().filter(|material|material.alpha_zero).count()});
    let stage = Instant::now();
    let budget = Budget::new(&scene, &environment, options)?;
    let budget_ns = stage.elapsed().as_nanos() as u64;
    let (first_camera, first_uniform) = uniform(&conditions, options, 0)?;
    let _ = first_camera;
    eprintln!(
        "Bistro explicit GPU preparation: {} resources",
        budget.physical_resources
    );
    let stage = Instant::now();
    let mut backend = B::create(options, &budget)?;
    let native_prepare_ns = stage.elapsed().as_nanos() as u64;
    let mut resource_stages = json!({});
    if options.resource_checks {
        resource_stages["prepared"] = backend.memory_snapshot()?;
    }
    let mut runtime = backend.info();
    runtime["process_id"] = json!(std::process::id());
    #[cfg(windows)]
    {
        runtime["main_thread_id"] = json!(super::clock::thread_id());
    }
    let stage = Instant::now();
    let mut resident =
        Resident::upload(&mut backend, &scene, &environment, &budget, &first_uniform)?;
    environment.release_pixels();
    if options.resource_checks {
        resource_stages["uploaded"] = backend.memory_snapshot()?;
    }
    let prepared = json!({"physical_resources":budget.physical_resources,"logical_buffers_per_graph":budget.buffer_count,
        "max_buffer_bytes":budget.max_buffer_bytes,"upload_capacity_bytes":budget.upload_bytes,"readback_capacity_bytes":budget.readback_bytes,
        "buffer_logical_bytes":budget.buffers.iter().map(|(bytes,count)|*bytes*(*count as u64)).sum::<u64>(),
        "image_logical_bytes":budget.images.iter().map(|(desc,count)|desc.byte_len().unwrap()*(*count as u64)).sum::<u64>(),
        "retained_uniform_upload_pools":resident.upload_owners.len(),"geometry_validation_cpu_bytes_retained":0,
        "scope":"packed logical resource bytes and prepared copy budgets; not actual native allocation size or VRAM residency"});
    // Typed geometry publication transfers scalar index guarantees to native
    // provenance. Both original loader bytes and upload owners are gone pre-ROI.
    scene.vertices = Vec::new();
    scene.indices = Vec::new();
    let upload_ns = stage.elapsed().as_nanos() as u64;
    let stage = Instant::now();
    backend.set_profiling(options.gpu_timing)?;
    // One additional Graph backing covers the latest mutable camera provenance
    // when all execution slots have otherwise completed (including inflight=1).
    let mut pool = GraphPool::new(budget.graph, options.inflight + 1)?;
    let mut workspace = Workspace::new(&scene);
    if options.reuse_shadows && options.shadows {
        workspace.enable_shadow_reuse();
    }
    let mut queue = VecDeque::with_capacity(options.inflight);
    let mut samples = Vec::with_capacity(options.frames);
    let frame_storage_and_profiling_prepare_ns = stage.elapsed().as_nanos() as u64;
    eprintln!("Bistro warmup: {} frames", options.warmup);
    let stage = Instant::now();
    frames(
        &mut backend,
        &mut resident,
        &scene,
        options,
        &conditions,
        &mut pool,
        &mut workspace,
        &mut queue,
        &mut samples,
        options.warmup,
        false,
        !options.steady_state_cpu,
    )?;
    let warmup_ns = stage.elapsed().as_nanos() as u64;
    if options.resource_checks {
        resource_stages["warmed"] = backend.memory_snapshot()?;
    }
    let setup_ns = setup.elapsed().as_nanos() as u64;
    #[cfg(feature = "memory-profiling")]
    let setup_memory = renderer::measurement::Snapshot::now().since(setup_memory);
    eprintln!(
        "Bistro measured frames: {} (capture is outside ROI)",
        options.frames
    );
    #[cfg(feature = "tracy")]
    if options.capture_handshake {
        super::trace::ready(
            options,
            &identity(options, &cook_hash, &conditions_hash, &environment_identity),
            tracy_capture_id.as_deref(),
        )?;
    }
    #[cfg(feature = "tracy")]
    let tracy_connected_start = tracy_client::Client::is_connected();
    #[cfg(windows)]
    let frequency = super::clock::frequency()?;
    workspace.reset_shadow_reuse_counts();
    let steady_pending_start = queue.len();
    #[cfg(windows)]
    let cpu_start = super::clock::Boundary::capture()?;
    #[cfg(feature = "memory-profiling")]
    let memory_start = renderer::measurement::Snapshot::now();
    let started = Instant::now();
    {
        #[cfg(feature = "tracy")]
        let _measurement = tracy_client::Client::running()
            .map(|client| client.span(super::trace::measurement_location(), 0));
        frames(
            &mut backend,
            &mut resident,
            &scene,
            options,
            &conditions,
            &mut pool,
            &mut workspace,
            &mut queue,
            &mut samples,
            options.frames,
            true,
            !options.steady_state_cpu,
        )?;
    }
    // A stationary window retains the same bounded pipeline occupancy at
    // both boundaries, like Godot's post-draw callback interval. The drain
    // remains inside the separately reported complete ROI, never discarded.
    let steady_wall = started.elapsed().as_nanos() as u64;
    let steady_pending_end = queue.len();
    #[cfg(windows)]
    let steady_boundary = if options.steady_state_cpu {
        Some(super::clock::Boundary::capture()?)
    } else {
        None
    };
    if options.steady_state_cpu {
        frames(
            &mut backend,
            &mut resident,
            &scene,
            options,
            &conditions,
            &mut pool,
            &mut workspace,
            &mut queue,
            &mut samples,
            0,
            true,
            true,
        )?;
    }
    let wall = started.elapsed().as_nanos() as u64;
    #[cfg(feature = "memory-profiling")]
    let memory = renderer::measurement::Snapshot::now().since(memory_start);
    #[cfg(windows)]
    let cpu = super::clock::Boundary::capture()?.difference(cpu_start, frequency)?;
    #[cfg(windows)]
    let steady_cpu = steady_boundary
        .map(|boundary| boundary.difference(cpu_start, frequency))
        .transpose()?
        .unwrap_or(Value::Null);
    #[cfg(not(windows))]
    let cpu = Value::Null;
    #[cfg(not(windows))]
    let steady_cpu = Value::Null;
    #[cfg(feature = "tracy")]
    let tracy_connected_end = tracy_client::Client::is_connected();
    let mut measured = measurement(&samples, options, wall, cpu)?;
    if let Some((hits, misses)) = workspace.shadow_reuse_counts() {
        measured["shadow_reuse"] = json!({"hits":hits,"misses":misses});
    }
    measured["steady_state_window"] = if options.steady_state_cpu {
        json!({"frames":options.frames,"wall_ns":steady_wall,"cpu":steady_cpu,
            "host_pending_at_start":steady_pending_start,"host_pending_at_end":steady_pending_end,
            "scope":"all-process CPU and caller wall over N changing-route iterations with equal host-owned unretired submission counts; not actual GPU occupancy; includes prior-frame poll/retire/result return, exports/deletion, instrumentation and scheduling; no explicit final drain in this stationary interval; complete ROI cpu/wall and tail remain reported separately"})
    } else {
        Value::Null
    };
    measured["drain_tail_wall_ns"] = json!(wall - steady_wall);
    measured["drain_tail_scope"] = json!(
        "caller wall after stationary endpoint through final pending drain; includes boundary-clock acquisition/bookkeeping, not a pure GPU wait"
    );
    if options.resource_checks {
        resource_stages["after_frames"] = backend.memory_snapshot()?;
    }
    #[cfg(feature = "tracy")]
    let tracy = json!({
        "enabled": true, "capture_handshake": options.capture_handshake,
        "capture_id": tracy_capture_id,
        "connected_at_measurement_start": tracy_connected_start,
        "connected_at_measurement_end": tracy_connected_end,
    });
    #[cfg(feature = "tracy")]
    if options.capture_handshake {
        super::trace::done(options, &measured, &tracy)?;
    }
    let captured = capture(
        &mut backend,
        &mut resident,
        &scene,
        options,
        &conditions,
        &mut pool,
        &mut workspace,
    )?;
    if let Some(hits) = captured.get("shadow_reuse_hits") {
        measured["shadow_reuse"]["output_capture_hits"] = hits.clone();
    }
    let aa_off = if options.aa {
        let mut diagnostic = options.clone();
        diagnostic.aa = false;
        diagnostic.capture_shadows = false;
        diagnostic.output = options.output.join("aa-off");
        Some(capture(
            &mut backend,
            &mut resident,
            &scene,
            &diagnostic,
            &conditions,
            &mut pool,
            &mut workspace,
        )?)
    } else {
        None
    };
    let mut view_reports = Vec::new();
    let mut captured_views = Vec::new();
    if options.capture_all_views && !options.fixture {
        for (name, _) in &conditions.views {
            let mut view = options.clone();
            view.view = name.clone();
            view.output = options.output.join("views").join(name);
            let captured_view = capture(
                &mut backend,
                &mut resident,
                &scene,
                &view,
                &conditions,
                &mut pool,
                &mut workspace,
            )?;
            let aa_off = if view.aa {
                let mut diagnostic = view.clone();
                diagnostic.aa = false;
                diagnostic.capture_shadows = false;
                diagnostic.output = view.output.join("aa-off");
                Some(capture(
                    &mut backend,
                    &mut resident,
                    &scene,
                    &diagnostic,
                    &conditions,
                    &mut pool,
                    &mut workspace,
                )?)
            } else {
                None
            };
            let report = json!({"schema_version":1,"identity":identity(&view,&cook_hash,&conditions_hash,&environment_identity),"runtime":runtime,
                "scene":counts,"capture_only":true,"measurement":Value::Null,"captures":captured_view["captures"],
                "shadow_captures":captured_view["shadow_captures"],"aa_off":aa_off,
                "depth_coverage_pixels":captured_view["depth_coverage_pixels"],"draw_count":captured_view["draw_count"],"readback_outside_measurement":true});
            captured_views.push((name.clone(), view.output, report));
        }
    }
    // No cache registrations survive into the material replacement or
    // overlapping View probes.
    workspace.clear_shadow_reuse(&mut backend)?;
    let resource_checks = if options.resource_checks {
        eprintln!("Bistro resource lifetime, capacity and memory checks (outside ROI)");
        super::lifecycle::execute(
            &mut backend,
            &mut resident,
            &scene,
            options,
            &conditions,
            &budget,
            resource_stages,
        )?
    } else {
        Value::Null
    };
    #[cfg(feature = "memory-profiling")]
    let shutdown_memory = renderer::measurement::Snapshot::now();
    drop((resident, queue, samples, workspace));
    let validation = backend.close()?;
    drop(pool);
    for (name, directory, mut report) in captured_views {
        report["validation"] = validation.clone();
        write_report(&directory, &report)?;
        view_reports.push(
            json!({"view":name,"report":std::fs::canonicalize(directory.join("report.json"))?}),
        );
    }
    #[cfg(feature = "memory-profiling")]
    let memory_json = json!({"normal":memory_report(memory),"normal_allocation_free":memory.is_allocation_free(),
        "setup":memory_report(setup_memory),"shutdown":memory_report(renderer::measurement::Snapshot::now().since(shutdown_memory)),
        "scope":"Graph build/compile/submit/poll/retire/result return; no capture/file I/O; setup/shutdown separate positive controls"});
    #[cfg(not(feature = "memory-profiling"))]
    let memory_json = Value::Null;
    let report = json!({"schema_version":1,"identity":identity(options,&cook_hash,&conditions_hash,&environment_identity),"runtime":runtime,
        "scene":counts,"prepared":prepared,"setup_ns":setup_ns,
        "setup_phases_ns":{"asset_load":asset_load_ns,"budget":budget_ns,"native_prepare":native_prepare_ns,"upload":upload_ns,
            "frame_storage_and_profiling_prepare":frame_storage_and_profiling_prepare_ns,"warmup":warmup_ns},
        "setup_scope":"combined asset/conditions load, boundary validation, native resource/pipeline preparation, upload/CPU input release, frame storage and warmup; checkpoints exclude some reporting/bookkeeping; outside measured frame ROI",
        "measurement":measured,"memory":memory_json,"validation":validation,"resource_checks":resource_checks,
        "captures":captured["captures"],"depth_coverage_pixels":captured["depth_coverage_pixels"],"draw_count":captured["draw_count"],
        "shadow_captures":captured["shadow_captures"],"aa_off":aa_off,
        "views":view_reports,"readback_outside_measurement":true,
        "limits":"B3 Forward PBR; offscreen, no frustum/occlusion culling or Bloom/display FPS claim; whole primitives sorted for transparency; Godot AgX, native IBL/filter and presentation differ from this Neutral pipeline"});
    #[cfg(feature = "tracy")]
    let report = {
        let mut report = report;
        report["tracy"] = tracy;
        report
    };
    write_report(&options.output, &report)?;
    println!("REPORT {}", options.output.join("report.json").display());
    #[cfg(feature = "memory-profiling")]
    if !memory.is_allocation_free() {
        return Err("steady-state allocation/native event gate failed; inspect report.json".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_options() -> Options {
        Options::parse(
            ["--fixture", "--frames", "1", "--warmup", "0"]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap()
    }
    #[test]
    fn missing_requested_gpu_samples_are_not_reported_as_zero_time() {
        let mut options = fixture_options();
        let sample = Sample {
            phases: [1, 2, 3, 4, 5],
            latency: 30,
            native: None,
            poll_cpu_ns: 0,
            yield_ns: 0,
            iteration_ns: 1,
        };
        let measured =
            measurement(std::slice::from_ref(&sample), &options, 100, Value::Null).unwrap();
        assert_eq!(measured["gpu"]["samples"], 0);
        assert_eq!(measured["native_profiling_enabled"], false);
        assert!(measured["native_phase_totals_ns"].is_null());
        options.gpu_timing = true;
        assert!(measurement(std::slice::from_ref(&sample), &options, 100, Value::Null).is_err());
        assert!(measurement(&[], &options, 100, Value::Null).is_err());
    }
    #[test]
    fn report_identity_uses_the_manifest_bytes_actually_loaded() {
        let mut scene = Scene::fixture(false);
        let loaded = "ab".repeat(32);
        scene.source["cooked_manifest_sha256"] = json!(loaded);
        let hash = loaded_cook_hash(&scene, false).unwrap();
        assert_eq!(
            identity(&fixture_options(), &hash, "conditions", &Value::Null)["cook_manifest_sha256"],
            loaded
        );
        scene
            .source
            .as_object_mut()
            .unwrap()
            .remove("cooked_manifest_sha256");
        assert!(loaded_cook_hash(&scene, false).is_err());
        assert!(loaded_cook_hash(&scene, true).is_ok());
    }
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    fn native_fixture<B: Backend>() {
        super::super::asset::material_tests::native_acceptance::<B>();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "renderer-bistro-fixture-{}-{stamp}",
            std::process::id()
        ));
        for (label, alpha_only, force_opaque) in [
            ("materials", false, false),
            ("alpha-zero", true, false),
            ("opaque-override", true, true),
        ] {
            let mut options = fixture_options();
            options.frames = 8;
            options.warmup = 8;
            options.alpha_only = alpha_only;
            options.force_opaque = force_opaque;
            options.output = root.join(label);
            execute::<B>(&options).unwrap();
            let report: Value =
                serde_json::from_slice(&std::fs::read(options.output.join("report.json")).unwrap())
                    .unwrap();
            let coverage = report["depth_coverage_pixels"].as_u64().unwrap();
            assert_eq!(
                coverage == 0,
                alpha_only && !force_opaque,
                "source alpha coverage and opaque diagnostic sensitivity"
            );
            assert_eq!(report["validation"]["clean"], true);
            if alpha_only && !force_opaque {
                super::super::asset::material_tests::alpha_background::<B>(&report);
            }
        }
    }
    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires real Vulkan GPU; execute explicitly in shipping and strict configurations"]
    fn vulkan_materials_and_alpha_coverage() {
        native_fixture::<super::super::backend::Vulkan>();
    }
    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires real D3D12 GPU; execute explicitly in shipping and strict configurations"]
    fn d3d12_materials_and_alpha_coverage() {
        native_fixture::<super::super::backend::D3d12>();
    }
}
