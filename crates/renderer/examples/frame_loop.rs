//! Headless engine integration and opt-in baseline capture.
//!
//! `--verify report.json --churn` checks updates/deletion with Validation.
//! `--benchmark report.json` explicitly disables requested Validation and adds
//! GPU timestamps. Both modes use bounded frame polling and read back every
//! frame; their timings describe this small offscreen workload, not display FPS.

use std::{
    collections::HashMap,
    error::Error,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use renderer::{
    FrameId, GpuTiming, MemoryStats, RuntimeConfig, RuntimeStats, SubmissionResult, ValidationMode,
    ValidationReport, VulkanEngine, VulkanRuntime,
    engine::{Material, MaterialHandle, MeshHandle},
    graph::{ComputeTransform, ImageDesc, ImportImage, PassHandle},
};

#[path = "support/frame_report.rs"]
mod frame_report;
#[path = "support/scene.rs"]
mod scene;

const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;
const IN_FLIGHT: usize = 3;
const RESOURCE_CAPACITY: usize = 64;
const STAGING_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SHADER_BINDINGS: u32 = 64;
const COMPLETION_BUDGET: Duration = Duration::from_secs(10);
const HISTORY_SEED: [u8; 4] = [17, 34, 51, 255];

struct Options {
    benchmark: bool,
    output: PathBuf,
    frames: usize,
    warmup: usize,
    runs: usize,
    history: bool,
    custom: bool,
    churn: bool,
    ppm: Option<PathBuf>,
}

impl Options {
    fn parse() -> Result<Option<Self>, Box<dyn Error>> {
        let mut args = std::env::args().skip(1);
        let Some(mode) = args.next() else {
            print_usage();
            return Ok(None);
        };
        if mode == "--help" || mode == "-h" {
            print_usage();
            return Ok(None);
        }
        let benchmark = match mode.as_str() {
            "--benchmark" => true,
            "--verify" => false,
            _ => return Err("first argument must be --verify or --benchmark".into()),
        };
        let mut options = Self {
            benchmark,
            output: args.next().ok_or("pass a JSON output path")?.into(),
            frames: if benchmark { 128 } else { 24 },
            warmup: if benchmark { 32 } else { 0 },
            runs: if benchmark { 3 } else { 1 },
            history: true,
            custom: true,
            churn: false,
            ppm: None,
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--frames" => options.frames = args.next().ok_or("missing frame count")?.parse()?,
                "--warmup" => {
                    options.warmup = args.next().ok_or("missing warmup count")?.parse()?
                }
                "--runs" => options.runs = args.next().ok_or("missing run count")?.parse()?,
                "--no-history" => options.history = false,
                "--no-custom" => options.custom = false,
                "--churn" => options.churn = true,
                "--ppm" => options.ppm = Some(args.next().ok_or("missing PPM path")?.into()),
                _ => return Err(format!("unknown argument: {arg}").into()),
            }
        }
        if options.frames == 0
            || options.runs == 0
            || options.frames > 100_000
            || options.warmup > 100_000
            || options.runs > 100
        {
            return Err(
                "frames/runs must be nonzero; limits are 100000 frames and 100 runs".into(),
            );
        }
        if benchmark && options.warmup < IN_FLIGHT * 2 {
            return Err("benchmark warmup must be at least twice the in-flight capacity".into());
        }
        if benchmark && cfg!(debug_assertions) {
            return Err("run benchmark mode with cargo run --release".into());
        }
        Ok(Some(options))
    }
}

fn print_usage() {
    println!(
        "frame_loop --verify|--benchmark report.json \
         [--frames N] [--warmup N] [--runs N] [--no-history] [--no-custom] \
         [--churn] [--ppm last-frame.ppm]\n\
         Defaults: verify=24 frames/0 warmup/1 run; benchmark=128/32/3.\n\
         Fixed workload: 128x96, 3 in-flight frames, one mesh, custom SwapRedBlue,\n\
         optional 1x1 history recurrence, output/history readback on every frame."
    );
}

struct ProfileSample {
    prepare_ns: u128,
    record_ns: u128,
    submit_ns: u128,
    retire_ns: u128,
    gpu_ns: Option<u128>,
    gpu_status: &'static str,
    timestamp_period_ns: Option<f64>,
    timestamp_valid_bits: Option<u32>,
}

struct Sample {
    frame: usize,
    build_ns: u128,
    compile_ns: u128,
    observed_latency_ns: u128,
    completed_poll_ns: u128,
    profile: Option<ProfileSample>,
    output_checksum: u64,
}

struct RunReport {
    device: renderer::DeviceInfo,
    validation: ValidationReport,
    samples: Vec<Sample>,
    warmed_stats: Option<RuntimeStats>,
    final_stats: RuntimeStats,
    warmed_memory: Option<MemoryStats>,
    final_memory: MemoryStats,
    poll_calls: usize,
    pending_polls: usize,
    poll_total_ns: u128,
    peak_pending_frames: usize,
    measured_loop_ns: u128,
    plateau_checked: bool,
}

struct PendingSample {
    frame: usize,
    build_ns: u128,
    compile_ns: u128,
    submitted: Instant,
    color_readback: PassHandle,
    history_readback: Option<PassHandle>,
    expected_center: [u8; 4],
    expected_history: [u8; 4],
}

struct Collector {
    pending: HashMap<FrameId, PendingSample>,
    samples: Vec<Sample>,
    poll_calls: usize,
    pending_polls: usize,
    poll_total_ns: u128,
    peak_pending_frames: usize,
    last_pixels: Option<Vec<u8>>,
    held_readback: Option<CompletedReadback>,
}

struct CompletedReadback {
    pending: PendingSample,
    profile: Option<ProfileSample>,
    observed_latency_ns: u128,
    completed_poll_ns: u128,
    result: SubmissionResult,
}

impl Collector {
    fn poll_one(
        &mut self,
        engine: &mut VulkanEngine,
        options: &Options,
    ) -> Result<bool, Box<dyn Error>> {
        let start = Instant::now();
        let completed = engine.runtime_mut().poll_frame()?;
        let elapsed = start.elapsed().as_nanos();
        self.poll_calls += 1;
        self.poll_total_ns += elapsed;
        let Some(completed) = completed else {
            self.pending_polls += 1;
            return Ok(false);
        };
        let pending = self
            .pending
            .remove(&completed.id())
            .ok_or("completion does not belong to a submitted frame")?;
        let latency = pending.submitted.elapsed().as_nanos();
        let profile = completed.result.metrics().map(|metrics| {
            let (gpu_ns, gpu_status, timestamp_period_ns, timestamp_valid_bits) = match &metrics.gpu
            {
                GpuTiming::Available {
                    elapsed,
                    timestamp_period_ns,
                    valid_bits,
                } => (
                    Some(elapsed.as_nanos()),
                    "available",
                    Some(*timestamp_period_ns),
                    Some(*valid_bits),
                ),
                GpuTiming::NotReady => (None, "not_ready", None, None),
                GpuTiming::AmbiguousCounterWrap => (None, "ambiguous_counter_wrap", None, None),
                _ => (None, "unsupported_timing_state", None, None),
            };
            ProfileSample {
                prepare_ns: metrics.cpu_prepare.as_nanos(),
                record_ns: metrics.cpu_record.as_nanos(),
                submit_ns: metrics.cpu_submit.as_nanos(),
                retire_ns: metrics.cpu_retire.as_nanos(),
                gpu_ns,
                gpu_status,
                timestamp_period_ns,
                timestamp_valid_bits,
            }
        });
        let observed = CompletedReadback {
            pending,
            profile,
            observed_latency_ns: latency,
            completed_poll_ns: elapsed,
            result: completed.result,
        };
        // Retain one completed CPU result while the runtime reuses its execution
        // slot. Its readback ranges remain leased until the next completion.
        if let Some(previous) = self.held_readback.replace(observed) {
            self.consume_readback(previous, options)?;
        }
        Ok(true)
    }

    fn consume_readback(
        &mut self,
        mut completed: CompletedReadback,
        options: &Options,
    ) -> Result<(), Box<dyn Error>> {
        let pending = completed.pending;
        let rgba = completed.result.readback(pending.color_readback)?;
        verify_color(&rgba, pending.expected_center)?;
        if let Some(pass) = pending.history_readback {
            let history = completed.result.readback(pass)?;
            if history != pending.expected_history {
                return Err(
                    format!("history mismatch at frame {}: {history:?}", pending.frame).into(),
                );
            }
        }
        if pending.frame >= options.warmup {
            self.samples.push(Sample {
                frame: pending.frame - options.warmup,
                build_ns: pending.build_ns,
                compile_ns: pending.compile_ns,
                observed_latency_ns: completed.observed_latency_ns,
                completed_poll_ns: completed.completed_poll_ns,
                profile: completed.profile,
                output_checksum: checksum(&rgba),
            });
        }
        if pending.frame == options.warmup + options.frames - 1 && options.ppm.is_some() {
            self.last_pixels = Some(rgba);
        }
        Ok(())
    }

    fn wait_for_one(
        &mut self,
        engine: &mut VulkanEngine,
        options: &Options,
    ) -> Result<(), Box<dyn Error>> {
        let start = Instant::now();
        while !self.poll_one(engine, options)? {
            if start.elapsed() >= COMPLETION_BUDGET {
                return Err(
                    "nonblocking frame polling timed out; pending work remains owned".into(),
                );
            }
            thread::yield_now();
        }
        Ok(())
    }
}

fn run(options: &Options) -> Result<RunReport, Box<dyn Error>> {
    let validation = if options.benchmark {
        ValidationMode::Disabled
    } else {
        ValidationMode::Required
    };
    let config = RuntimeConfig {
        execution_slots: IN_FLIGHT,
        resource_capacity: RESOURCE_CAPACITY,
        staging_bytes: STAGING_BYTES,
        max_shader_bindings: MAX_SHADER_BINDINGS,
    };
    let mut runtime = VulkanRuntime::new_with_validation(config, validation)?;
    runtime.set_profiling(options.benchmark)?;
    let device = runtime.device_info();
    let monitor = runtime.validation_monitor();
    let mut engine = VulkanEngine::new(runtime, 4, 4)?;
    let mut mesh = engine.create_mesh(scene::triangle([1.0, 0.0, 0.0, 1.0])?)?;
    let mut material = engine.create_material(Material::default())?;
    let view = scene::view(WIDTH, HEIGHT);
    let mut collector = Collector {
        pending: HashMap::new(),
        samples: Vec::with_capacity(options.frames),
        poll_calls: 0,
        pending_polls: 0,
        poll_total_ns: 0,
        peak_pending_frames: 0,
        last_pixels: None,
        held_readback: None,
    };
    let mut history: Option<ImportImage> = None;
    let mut warmed_stats = None;
    let mut warmed_memory = None;
    let mut measured_start = None;
    for frame in 0..options.warmup + options.frames {
        // Fill the bounded slots before polling. This also warms every execution
        // and descriptor slot even when the GPU has already finished the work.
        while engine.runtime().pending_frames() >= IN_FLIGHT {
            collector.wait_for_one(&mut engine, options)?;
        }
        if frame == options.warmup {
            warmed_stats = Some(engine.runtime().stats());
            warmed_memory = Some(engine.runtime().memory_stats()?);
            measured_start = Some(Instant::now());
        }
        let phase = if options.churn { (frame / 8) % 3 } else { 0 };
        if options.churn && frame > 0 && frame.is_multiple_of(8) {
            change_assets(&mut engine, &mut mesh, &mut material, phase)?;
        }
        let build_start = Instant::now();
        let mut prepared = engine.build_frame(
            &view,
            &[scene::draw(
                mesh,
                material,
                if phase == 1 { 0.1 } else { 0.0 },
            )],
            true,
            |graph, color| scene::custom_pass(graph, color, WIDTH, HEIGHT, options.custom),
        )?;
        let old_history = history.take();
        let (history_output, history_readback) = if options.history {
            let source = if let Some(previous) = old_history.as_ref() {
                prepared.graph.import_image("previous history", previous)?
            } else {
                let source = prepared
                    .graph
                    .image("history seed", ImageDesc::rgba8(1, 1))?;
                prepared
                    .graph
                    .upload_image("initialize history", source, HISTORY_SEED.to_vec())?;
                source
            };
            let output = prepared
                .graph
                .image("next history", ImageDesc::rgba8(1, 1))?;
            prepared
                .graph
                .compute("history invert", source, output, ComputeTransform::Invert)?;
            prepared.graph.export_image(output)?;
            let readback = prepared.graph.readback_image("history readback", output)?;
            (Some(output), Some(readback))
        } else {
            (None, None)
        };
        let build_ns = build_start.elapsed().as_nanos();
        let compile_start = Instant::now();
        let plan = prepared.graph.compile()?;
        let compile_ns = compile_start.elapsed().as_nanos();
        let rendered = engine.submit_frame(prepared, &plan)?;
        let submitted = Instant::now();
        if let Some(output) = history_output {
            history = Some(rendered.submission.export_image(output)?);
        }
        if let Some(previous) = old_history {
            engine.runtime_mut().delete(previous.key())?;
        }
        // No future frame needs this color output. Deleting its registration
        // does not destroy native objects still referenced by submitted work.
        engine.runtime_mut().delete(rendered.output.key())?;
        let mut expected_center = match phase {
            0 => [255, 0, 0, 255],
            1 => [0, 0, 0, 255],
            _ => [0, 0, 255, 255],
        };
        if options.custom {
            expected_center.swap(0, 2);
        }
        let mut expected_history = HISTORY_SEED;
        if frame.is_multiple_of(2) {
            for value in &mut expected_history[..3] {
                *value = 255 - *value;
            }
        }
        collector.pending.insert(
            rendered.submission.id(),
            PendingSample {
                frame,
                build_ns,
                compile_ns,
                submitted,
                color_readback: rendered
                    .readback
                    .ok_or("requested color readback is missing")?,
                history_readback,
                expected_center,
                expected_history,
            },
        );
        collector.peak_pending_frames = collector
            .peak_pending_frames
            .max(engine.runtime().pending_frames());
        // FrameSubmission is metadata: dropping it never waits or cancels work.
    }
    while !collector.pending.is_empty() {
        collector.wait_for_one(&mut engine, options)?;
    }
    if let Some(last) = collector.held_readback.take() {
        collector.consume_readback(last, options)?;
    }
    let measured_loop_ns = measured_start
        .ok_or("missing measurement start")?
        .elapsed()
        .as_nanos();
    if !engine.runtime_mut().drain_frames(COMPLETION_BUDGET)? {
        return Err("explicit shutdown drain timed out".into());
    }
    let final_stats = engine.runtime().stats();
    let final_memory = engine.runtime().memory_stats()?;
    let plateau_checked = options.benchmark && !options.churn;
    if plateau_checked
        && creations(warmed_stats.as_ref().ok_or("missing warmup stats")?)
            != creations(&final_stats)
    {
        return Err(
            "native resource creation continued after warmup for the fixed workload".into(),
        );
    }
    if let Some(previous) = history {
        engine.runtime_mut().delete(previous.key())?;
    }
    engine.delete_mesh(mesh)?;
    engine.delete_material(material)?;
    if let (Some(path), Some(rgba)) = (&options.ppm, &collector.last_pixels) {
        frame_report::write_ppm(path, WIDTH, HEIGHT, rgba)?;
    }
    drop(engine);
    let validation = monitor.report();
    if !validation.is_clean() {
        return Err(format!("Validation was not clean through shutdown: {validation:?}").into());
    }
    if !options.benchmark
        && (!validation.startup_status_recognized
            || validation.core_validation != Some(true)
            || validation.synchronization_validation != Some(true))
    {
        return Err(
            format!("Core + SyncVal effectiveness was not confirmed: {validation:?}").into(),
        );
    }
    if options.benchmark
        && (validation.core_validation == Some(true)
            || validation.synchronization_validation == Some(true))
    {
        return Err(
            "performance run had externally enabled Validation; use a clean environment".into(),
        );
    }
    collector.samples.sort_by_key(|sample| sample.frame);
    Ok(RunReport {
        device,
        validation,
        samples: collector.samples,
        warmed_stats,
        final_stats,
        warmed_memory,
        final_memory,
        poll_calls: collector.poll_calls,
        pending_polls: collector.pending_polls,
        poll_total_ns: collector.poll_total_ns,
        peak_pending_frames: collector.peak_pending_frames,
        measured_loop_ns,
        plateau_checked,
    })
}

fn creations(stats: &RuntimeStats) -> [usize; 3] {
    [
        stats.resources_created,
        stats.execution_slots_created,
        stats.descriptor_arenas_created,
    ]
}

fn change_assets(
    engine: &mut VulkanEngine,
    mesh: &mut MeshHandle,
    material: &mut MaterialHandle,
    phase: usize,
) -> Result<(), Box<dyn Error>> {
    let color = match phase {
        0 => [1.0, 0.0, 0.0, 1.0],
        1 => [0.0, 1.0, 0.0, 1.0],
        _ => [0.0, 0.0, 1.0, 1.0],
    };
    let value = Material {
        tint: if phase == 1 {
            [1.0, 0.0, 1.0, 1.0]
        } else {
            [1.0; 4]
        },
    };
    if phase == 2 {
        engine.delete_mesh(*mesh)?;
        engine.delete_material(*material)?;
        *mesh = engine.create_mesh(scene::triangle(color)?)?;
        *material = engine.create_material(value)?;
    } else {
        engine.update_mesh(*mesh, scene::triangle(color)?)?;
        engine.update_material(*material, value)?;
    }
    Ok(())
}

fn verify_color(rgba: &[u8], expected_center: [u8; 4]) -> Result<(), Box<dyn Error>> {
    if rgba.len() != (WIDTH * HEIGHT * 4) as usize {
        return Err("unexpected output byte length".into());
    }
    let center = ((HEIGHT / 2 * WIDTH + WIDTH / 2) * 4) as usize;
    if rgba[..4] != [0, 0, 0, 255] || rgba[center..center + 4] != expected_center {
        return Err(format!(
            "unexpected output: corner={:?}, center={:?}, expected={expected_center:?}",
            &rgba[..4],
            &rgba[center..center + 4]
        )
        .into());
    }
    Ok(())
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let Some(options) = Options::parse()? else {
        return Ok(());
    };
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let host = frame_report::host_metadata();
    let mut reports = Vec::with_capacity(options.runs);
    for index in 0..options.runs {
        let report = run(&options)?;
        println!(
            "run {}: verified {} measured frames, peak {} pending, native creates {:?}",
            index + 1,
            report.samples.len(),
            report.peak_pending_frames,
            creations(&report.final_stats)
        );
        reports.push(report);
    }
    frame_report::write_report(&options, &reports, &host)?;
    println!("Saved {}", options.output.display());
    Ok(())
}
