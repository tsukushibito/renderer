//! Shared executable acceptance scenarios. GPU creation is always explicit.

use std::{
    collections::HashMap,
    error::Error,
    fmt::Write as _,
    process::Command,
    time::{Duration, Instant},
};

use renderer::{
    FrameId, GpuTiming, MeshCacheConfig, MeshUploadState, RenderedFrame, RuntimeConfig,
    ValidationMode, ValidationReport, VulkanEngine, VulkanRuntime,
    engine::{Draw, Engine, FrameSnapshot, IDENTITY, Material, MeshHandle, View},
    graph::{ComputeTransform, PassHandle},
};

use super::engine_geometry::{grid, rectangle};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const BUDGET: Duration = Duration::from_secs(30);
const STAGING_BYTES: u64 = 256 * 1024;
const IN_FLIGHT: usize = 3;
const CACHE_MESHES: usize = 4;
const CACHE_BYTES: u64 = 32 * 1024 * 1024;
const BLACK: [u8; 4] = [0, 0, 0, 255];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Verify,
    Benchmark,
}

impl Mode {
    fn validation(self) -> ValidationMode {
        match self {
            Self::Verify => ValidationMode::Required,
            Self::Benchmark => ValidationMode::Disabled,
        }
    }
}

#[derive(Default)]
struct ColdUpload {
    submitted_bytes: u64,
    submissions: usize,
    accepted_us: f64,
    completed_us: f64,
}

#[derive(Default)]
struct Sample {
    build_us: f64,
    compile_us: f64,
    submit_us: f64,
    active_us: f64,
    observed_latency_us: f64,
    prepare_us: Option<f64>,
    record_us: Option<f64>,
    native_submit_us: Option<f64>,
    retire_us: Option<f64>,
    gpu_us: Option<f64>,
}

struct Expected {
    pass: PassHandle,
    bytes: Vec<u8>,
    sample: Option<Sample>,
    submitted: Instant,
}

struct Driver {
    renderer: VulkanEngine,
    pending: HashMap<FrameId, Expected>,
    samples: Vec<Sample>,
    images_checked: usize,
    max_pending: usize,
}

impl Driver {
    fn new(mode: Mode) -> Result<Self> {
        let mut runtime = VulkanRuntime::new_with_validation(
            RuntimeConfig {
                execution_slots: 4,
                resource_capacity: 64,
                staging_bytes: STAGING_BYTES,
                max_shader_bindings: 64,
            },
            mode.validation(),
        )?;
        if runtime.device_info().device_type == "CPU" {
            return Err("real GPU acceptance does not accept a CPU Vulkan device".into());
        }
        if mode == Mode::Benchmark {
            runtime.set_profiling(true)?;
        }
        let renderer = VulkanEngine::new(
            runtime,
            MeshCacheConfig {
                max_meshes: CACHE_MESHES,
                max_bytes: CACHE_BYTES,
                max_upload_submissions: 3,
            },
        )?;
        Ok(Self {
            renderer,
            pending: HashMap::new(),
            samples: Vec::new(),
            images_checked: 0,
            max_pending: 0,
        })
    }

    fn ready(&mut self, snapshot: &FrameSnapshot) -> Result<ColdUpload> {
        let started = Instant::now();
        let mut report = ColdUpload::default();
        let mut progress = self.renderer.prepare_snapshot(snapshot)?;
        loop {
            if progress.submitted_bytes != 0 {
                report.submitted_bytes += progress.submitted_bytes;
                report.submissions += 1;
            }
            if progress.state == MeshUploadState::Ready {
                report.accepted_us = micros(started.elapsed());
                break;
            }
            if started.elapsed() >= BUDGET {
                return Err("mesh upload acceptance timed out".into());
            }
            progress = self.renderer.wait_uploads(BUDGET - started.elapsed())?;
        }
        while progress.pending_submissions != 0 {
            if started.elapsed() >= BUDGET {
                return Err("mesh upload completion timed out".into());
            }
            progress = self.renderer.wait_uploads(BUDGET - started.elapsed())?;
        }
        report.completed_us = micros(started.elapsed());
        Ok(report)
    }

    fn enqueue(
        &mut self,
        snapshot: &FrameSnapshot,
        expected: Vec<u8>,
        compute: bool,
        measured: bool,
    ) -> Result<()> {
        while self.renderer.runtime().pending_frames() >= IN_FLIGHT {
            self.wait_one()?;
        }
        let started = Instant::now();
        let prepared = self
            .renderer
            .build_snapshot(snapshot, true, |graph, color| {
                if compute {
                    let desc = graph.image_desc(color)?;
                    let output = graph.image("workload Compute output", desc)?;
                    graph.compute("workload Compute", color, output, ComputeTransform::Invert)?;
                    Ok(output)
                } else {
                    Ok(color)
                }
            })?;
        let built = Instant::now();
        let plan = prepared.graph.compile()?;
        let compiled = Instant::now();
        let frame = self.renderer.submit_frame(prepared, &plan)?;
        let submitted = Instant::now();
        self.accept_frame(
            frame,
            expected,
            measured.then(|| Sample {
                build_us: micros(built.duration_since(started)),
                compile_us: micros(compiled.duration_since(built)),
                submit_us: micros(submitted.duration_since(compiled)),
                active_us: micros(submitted.duration_since(started)),
                ..Sample::default()
            }),
        )
    }

    fn accept_frame(
        &mut self,
        mut frame: RenderedFrame,
        bytes: Vec<u8>,
        sample: Option<Sample>,
    ) -> Result<()> {
        let pass = frame.readback.ok_or("workload frame is missing readback")?;
        self.pending.insert(
            frame.submission.id(),
            Expected {
                pass,
                bytes,
                sample,
                submitted: Instant::now(),
            },
        );
        self.renderer.release_output(&mut frame)?;
        self.max_pending = self.max_pending.max(self.pending.len());
        Ok(())
    }

    fn wait_one(&mut self) -> Result<()> {
        let mut completed = self
            .renderer
            .runtime_mut()
            .wait_frame(BUDGET)?
            .ok_or("frame completion timed out")?;
        let mut expected = self
            .pending
            .remove(&completed.id())
            .ok_or("unexpected frame completion")?;
        if let Some(sample) = &mut expected.sample {
            sample.observed_latency_us = micros(expected.submitted.elapsed());
            if let Some(metrics) = completed.result.metrics() {
                sample.prepare_us = Some(micros(metrics.cpu_prepare));
                sample.record_us = Some(micros(metrics.cpu_record));
                sample.native_submit_us = Some(micros(metrics.cpu_submit));
                sample.retire_us = Some(micros(metrics.cpu_retire));
                if let GpuTiming::Available { elapsed, .. } = metrics.gpu {
                    sample.gpu_us = Some(micros(elapsed));
                }
            }
        }
        let bytes = completed.result.readback(expected.pass)?;
        if bytes != expected.bytes {
            let mismatch = bytes.iter().zip(&expected.bytes).position(|(a, b)| a != b);
            return Err(format!(
                "whole RGBA mismatch at byte {mismatch:?}, lengths {} vs {}",
                bytes.len(),
                expected.bytes.len()
            )
            .into());
        }
        self.images_checked += 1;
        if let Some(sample) = expected.sample {
            self.samples.push(sample);
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        while !self.pending.is_empty() {
            self.wait_one()?;
        }
        Ok(())
    }
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000.0
}

fn view(width: u32, height: u32) -> View {
    View {
        width,
        height,
        clip_from_world: IDENTITY,
        clear_color: [0.0, 0.0, 0.0, 1.0],
    }
}

fn snapshot(
    assets: &Engine,
    mesh: MeshHandle,
    material: renderer::engine::MaterialHandle,
    view: &View,
) -> Result<FrameSnapshot> {
    Ok(assets.snapshot(
        view,
        &[Draw {
            mesh,
            material,
            world_from_model: IDENTITY,
        }],
    )?)
}

fn assert_validation(mode: Mode, report: &ValidationReport) -> Result<()> {
    if !report.is_clean() {
        return Err(format!("validation through native destruction: {report:#?}").into());
    }
    if mode == Mode::Verify
        && (report.core_validation != Some(true) || report.synchronization_validation != Some(true))
    {
        return Err(format!("required Core/SyncVal were not confirmed: {report:#?}").into());
    }
    Ok(())
}

fn json_string(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(output, "\\u{:04x}", c as u32);
            }
            c => output.push(c),
        }
    }
    output.push('"');
    output
}

fn percentile(values: impl Iterator<Item = f64>, fraction: f64) -> f64 {
    let mut values = values.collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return 0.0;
    }
    values[((values.len() as f64 * fraction).ceil() as usize).saturating_sub(1)]
}

pub(super) fn run(mode: Mode) -> Result<String> {
    let mut driver = Driver::new(mode)?;
    let monitor = driver.renderer.runtime().validation_monitor();
    let device = driver.renderer.runtime().device_info();
    let mut assets = Engine::new(64, 8)?;
    let red = assets.create_material(Material {
        tint: [1.0, 0.0, 0.0, 1.0],
    })?;
    let blue = assets.create_material(Material {
        tint: [0.0, 0.0, 1.0, 1.0],
    })?;
    let large = grid(400);
    let large_vertices = large.vertex_count;
    let large_indices = large.index_count;
    let large_bytes = large.vertices.len() + large.indices.len();
    let mesh = assets.create_mesh(large.mesh)?;
    let secondary_grid = grid(256);
    let cold_bytes = large_bytes + secondary_grid.vertices.len() + secondary_grid.indices.len();
    let secondary = assets.create_mesh(secondary_grid.mesh)?;
    let both = assets.snapshot(
        &view(64, 64),
        &[
            Draw {
                mesh,
                material: red,
                world_from_model: IDENTITY,
            },
            Draw {
                mesh: secondary,
                material: red,
                world_from_model: IDENTITY,
            },
        ],
    )?;
    let cold = driver.ready(&both)?;
    if cold.submissions < 2
        || large_bytes as u64 <= STAGING_BYTES
        || cold.submitted_bytes != cold_bytes as u64
    {
        return Err("large workload did not exercise split upload".into());
    }
    let captured = snapshot(&assets, mesh, red, &view(64, 64))?;
    let runs = if mode == Mode::Benchmark { 3 } else { 1 };
    let warmup = if mode == Mode::Benchmark { 32 } else { 3 };
    let frames = if mode == Mode::Benchmark { 128 } else { 3 };
    let expected = rectangle(64, 64, [8, 8, 56, 56], [255, 0, 0, 255], BLACK);
    for _ in 0..runs {
        for _ in 0..warmup {
            driver.enqueue(&captured, expected.clone(), false, false)?;
        }
        driver.drain()?;
        for _ in 0..frames {
            driver.enqueue(&captured, expected.clone(), false, true)?;
        }
        driver.drain()?;
    }

    // Both Views are built before either is accepted. They share the same
    // immutable resident geometry, but have different transforms and consumers.
    let a = snapshot(&assets, mesh, red, &view(64, 64))?;
    let mut transform = IDENTITY;
    transform[0][0] = 0.5;
    transform[3][0] = 0.125;
    let b = assets.snapshot(
        &view(48, 32),
        &[Draw {
            mesh,
            material: blue,
            world_from_model: transform,
        }],
    )?;
    let pa = driver
        .renderer
        .build_snapshot(&a, true, |_, image| Ok(image))?;
    let pb = driver.renderer.build_snapshot(&b, true, |graph, image| {
        let output = graph.image("second View Compute output", graph.image_desc(image)?)?;
        graph.compute(
            "second View invert",
            image,
            output,
            ComputeTransform::Invert,
        )?;
        Ok(output)
    })?;
    let plan_a = pa.graph.compile()?;
    let plan_b = pb.graph.compile()?;
    let fa = driver.renderer.submit_frame(pa, &plan_a)?;
    driver.accept_frame(fa, expected.clone(), None)?;
    let fb = driver.renderer.submit_frame(pb, &plan_b)?;
    driver.accept_frame(
        fb,
        rectangle(48, 32, [18, 4, 36, 28], [255, 255, 0, 255], [255; 4]),
        None,
    )?;
    driver.drain()?;

    // Keep every CPU mesh alive while revisiting a working set much larger than
    // GPU residency. The first three outputs remain pending across CPU mutation.
    let mut stream = Vec::new();
    for _ in 0..32 {
        stream.push(assets.create_mesh(grid(8).mesh)?);
    }
    for handle in stream.iter().take(3) {
        let frame = snapshot(&assets, *handle, red, &view(32, 32))?;
        driver.ready(&frame)?;
        driver.enqueue(
            &frame,
            rectangle(32, 32, [4, 4, 28, 28], [255, 0, 0, 255], BLACK),
            false,
            false,
        )?;
    }
    if driver.pending.len() != IN_FLIGHT {
        return Err("three in-flight frame ownership was not exercised".into());
    }
    assets.update_mesh(stream[0], grid(12).mesh)?;
    assets.delete_mesh(stream[1])?;
    stream[1] = assets.create_mesh(grid(10).mesh)?;
    for &handle in &stream {
        let frame = snapshot(&assets, handle, red, &view(32, 32))?;
        driver.ready(&frame)?;
        driver.enqueue(
            &frame,
            rectangle(32, 32, [4, 4, 28, 28], [255, 0, 0, 255], BLACK),
            false,
            false,
        )?;
        if driver.renderer.runtime().stats().registered_resources > CACHE_MESHES * 2 {
            return Err("mesh registrations exceeded configured bounded residency".into());
        }
    }
    driver.drain()?;
    // Explicit unload must preserve CPU assets and permit later re-upload.
    driver.renderer.unload_mesh(stream[0])?;
    let replay = snapshot(&assets, stream[0], blue, &view(32, 32))?;
    driver.ready(&replay)?;
    driver.enqueue(
        &replay,
        rectangle(32, 32, [4, 4, 28, 28], [0, 0, 255, 255], BLACK),
        false,
        false,
    )?;
    driver.drain()?;

    // The same owned snapshot survives CPU deletion and remains an input to a
    // second independent backend residency cache.
    assets.delete_mesh(stream[0])?;
    assets.delete_material(blue)?;
    let mut second = Driver::new(mode)?;
    let second_monitor = second.renderer.runtime().validation_monitor();
    second.ready(&replay)?;
    second.enqueue(
        &replay,
        rectangle(32, 32, [4, 4, 28, 28], [0, 0, 255, 255], BLACK),
        false,
        false,
    )?;
    second.drain()?;
    drop(second);
    let second_report = second_monitor.report();
    assert_validation(mode, &second_report)?;

    let stats = driver.renderer.runtime().stats();
    let memory = driver.renderer.runtime().memory_stats()?;
    let cache = driver.renderer.mesh_cache_stats();
    if cache.resident_meshes > CACHE_MESHES || cache.resident_logical_bytes > CACHE_BYTES {
        return Err("mesh cache exceeded count or byte budget".into());
    }
    let samples = std::mem::take(&mut driver.samples);
    let checked = driver.images_checked + 1;
    let max_pending = driver.max_pending;
    drop(driver);
    let validation = monitor.report();
    assert_validation(mode, &validation)?;
    let mut output = String::new();
    let command = |program: &str, arguments: &[&str]| {
        Command::new(program)
            .args(arguments)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_else(|| "unavailable".into())
    };
    write!(
        output,
        "{{\n\"schema\":1,\"mode\":{},\"device\":{},\"driver\":{},\"api\":{},\n",
        json_string(&format!("{mode:?}")),
        json_string(&device.name),
        json_string(&format!("{:?}", device.driver_info)),
        json_string(&device.api_version)
    )?;
    write!(
        output,
        "\"host\":{{\"source_commit\":{},\"rustc\":{},\"os\":{},\"arch\":{},\"debug_assertions\":{}}},\n",
        json_string(&command("git", &["rev-parse", "HEAD"])),
        json_string(&command("rustc", &["--version"])),
        json_string(std::env::consts::OS),
        json_string(std::env::consts::ARCH),
        cfg!(debug_assertions)
    )?;
    write!(
        output,
        "\"workload\":{{\"vertices\":{large_vertices},\"indices\":{large_indices},\"mesh_bytes\":{large_bytes},\"stream_cpu_assets\":32,\"views_built_ahead\":2,\"snapshot_renderers\":2,\"staging_bytes\":{STAGING_BYTES},\"cache_meshes\":{CACHE_MESHES},\"cache_bytes\":{CACHE_BYTES},\"in_flight\":{IN_FLIGHT},\"runs\":{runs},\"warmup_per_run\":{warmup},\"measured_frames_per_run\":{frames}}},\n"
    )?;
    write!(
        output,
        "\"cold_upload\":{{\"accepted_bytes\":{},\"submissions\":{},\"accepted_us\":{},\"completed_us\":{}}},\n",
        cold.submitted_bytes, cold.submissions, cold.accepted_us, cold.completed_us
    )?;
    write!(
        output,
        "\"checks\":{{\"whole_rgba_frames\":{checked},\"max_pending_frames\":{max_pending},\"validation_warnings\":{},\"validation_errors\":{},\"callback_failures\":{},\"core_validation\":{},\"sync_validation\":{}}},\n",
        validation.warnings,
        validation.errors,
        validation.callback_failures,
        option_bool(validation.core_validation),
        option_bool(validation.synchronization_validation)
    )?;
    let cache_json = format!(
        "{{\"resident_meshes\":{},\"resident_logical_bytes\":{},\"ready_meshes\":{},\"queued_meshes\":{},\"prepared_retained_meshes\":{},\"pending_upload_submissions\":{},\"evictions\":{}}}",
        cache.resident_meshes,
        cache.resident_logical_bytes,
        cache.ready_meshes,
        cache.queued_meshes,
        cache.prepared_retained_meshes,
        cache.pending_upload_submissions,
        cache.evictions
    );
    write!(
        output,
        "\"memory\":{{\"registered_logical_bytes\":{},\"retained_logical_bytes\":{},\"cached_logical_bytes\":{},\"vma_allocation_bytes\":{},\"vma_block_bytes\":{},\"upload_peak_reserved_bytes\":{},\"upload_capacity_bytes\":{},\"resource_creates\":{},\"resource_reuses\":{},\"mesh_cache\":{}}},\n",
        memory.registered_logical_bytes,
        memory.retained_logical_bytes,
        memory.cached_logical_bytes,
        memory.vma_allocation_bytes,
        memory.vma_block_bytes,
        memory.upload.peak_reserved_bytes,
        memory.upload.capacity_bytes,
        stats.resources_created,
        stats.resources_reused,
        cache_json
    )?;
    write!(
        output,
        "\"summary_us\":{{\"active_median\":{},\"active_p95\":{},\"gpu_median\":{},\"gpu_p95\":{},\"observed_latency_p95\":{}}},\n",
        percentile(samples.iter().map(|s| s.active_us), 0.5),
        percentile(samples.iter().map(|s| s.active_us), 0.95),
        optional_percentile(samples.iter().filter_map(|s| s.gpu_us), 0.5),
        optional_percentile(samples.iter().filter_map(|s| s.gpu_us), 0.95),
        percentile(samples.iter().map(|s| s.observed_latency_us), 0.95)
    )?;
    output.push_str("\"run_summaries_us\":[");
    for (index, run) in samples.chunks(frames).enumerate() {
        if index != 0 {
            output.push(',');
        }
        write!(
            output,
            "{{\"run\":{index},\"samples\":{},\"active_median\":{},\"active_p95\":{},\"gpu_median\":{},\"gpu_p95\":{}}}",
            run.len(),
            percentile(run.iter().map(|s| s.active_us), 0.5),
            percentile(run.iter().map(|s| s.active_us), 0.95),
            optional_percentile(run.iter().filter_map(|s| s.gpu_us), 0.5),
            optional_percentile(run.iter().filter_map(|s| s.gpu_us), 0.95)
        )?;
    }
    output.push_str("],\n");
    output.push_str("\"sample_columns\":[\"build_us\",\"compile_us\",\"submit_us\",\"active_us\",\"observed_latency_us\",\"prepare_us\",\"record_us\",\"native_submit_us\",\"retire_us\",\"gpu_us\"],\n\"samples\":[\n");
    for (index, sample) in samples.iter().enumerate() {
        if index != 0 {
            output.push_str(",\n");
        }
        write!(
            output,
            "[{},{},{},{},{},{},{},{},{},{}]",
            sample.build_us,
            sample.compile_us,
            sample.submit_us,
            sample.active_us,
            sample.observed_latency_us,
            optional_number(sample.prepare_us),
            optional_number(sample.record_us),
            optional_number(sample.native_submit_us),
            optional_number(sample.retire_us),
            sample
                .gpu_us
                .map_or_else(|| "null".into(), |v| v.to_string())
        )?;
    }
    output.push_str("\n],\"measurement_limits\":\"CPU active excludes capacity waits; GPU includes readback, excludes presentation. Observed latency starts after submit return and ends at completion observation, not scanout. No clock/affinity/power control or performance baseline.\"\n}\n");
    Ok(output)
}

fn optional_number(value: Option<f64>) -> String {
    value.map_or_else(|| "null".into(), |value| value.to_string())
}

fn optional_percentile(values: impl Iterator<Item = f64>, fraction: f64) -> String {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        "null".into()
    } else {
        percentile(values.into_iter(), fraction).to_string()
    }
}

fn option_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_statistics_and_escaping_are_stable() {
        assert_eq!(percentile([3.0, 1.0, 2.0].into_iter(), 0.5), 2.0);
        assert_eq!(percentile([3.0, 1.0, 2.0].into_iter(), 0.95), 3.0);
        assert_eq!(json_string("a\n\"b\\"), "\"a\\n\\\"b\\\\\"");
    }
}
