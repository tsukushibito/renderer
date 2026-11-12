//! std-only capture output; raw samples accompany the summaries.

use std::{
    collections::BTreeMap, error::Error, fmt::Write as _, io::Write as _, path::Path,
    process::Command,
};

use renderer::{MemoryStats, RuntimeStats};

use super::{IN_FLIGHT, MAX_SHADER_BINDINGS, Options, RunReport, Sample, frame_scaling};

pub(super) fn host_metadata() -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    values.insert("os".into(), std::env::consts::OS.into());
    values.insert("arch".into(), std::env::consts::ARCH.into());
    values.insert(
        "debug_assertions".into(),
        cfg!(debug_assertions).to_string(),
    );
    values.insert(
        "rustc".into(),
        command("rustc", &["--version", "--verbose"]),
    );
    values.insert("git_commit".into(), command("git", &["rev-parse", "HEAD"]));
    values.insert(
        "git_worktree_status".into(),
        command("git", &["status", "--porcelain"]),
    );
    values.insert(
        "nvidia_smi_optional".into(),
        command(
            "nvidia-smi",
            &[
                "--query-gpu=name,driver_version,memory.total",
                "--format=csv,noheader",
            ],
        ),
    );
    values.insert(
        "clock_affinity_power".into(),
        "not fixed by this example".into(),
    );
    values.insert(
        "capture_profiler".into(),
        "not configured by this example; external capture is not detected".into(),
    );
    if cfg!(windows) {
        values.insert(
            "os_version_optional".into(),
            command("cmd.exe", &["/C", "ver"]),
        );
        values.insert(
            "cpu_name_optional".into(),
            command(
                "reg.exe",
                &[
                    "query",
                    "HKLM\\HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0",
                    "/v",
                    "ProcessorNameString",
                ],
            ),
        );
    } else {
        values.insert("os_version_optional".into(), command("uname", &["-a"]));
        values.insert("cpu_name_optional".into(), command("lscpu", &[]));
    }
    for key in [
        "PROCESSOR_IDENTIFIER",
        "VULKAN_SDK",
        "RUST_LOG",
        "VK_INSTANCE_LAYERS",
        "VK_LAYER_PATH",
        "VK_LAYER_SETTINGS_PATH",
        "VK_LOADER_LAYERS_ENABLE",
        "VK_LOADER_LAYERS_DISABLE",
    ] {
        values.insert(
            key.into(),
            std::env::var(key).unwrap_or_else(|_| "unset".into()),
        );
    }
    values
}

fn command(program: &str, args: &[&str]) -> String {
    match Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().into()
        }
        Ok(output) => format!(
            "unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => format!("unavailable: {error}"),
    }
}

pub(super) fn write_report(
    options: &Options,
    reports: &[RunReport],
    host: &BTreeMap<String, String>,
) -> Result<(), Box<dyn Error>> {
    let width = options.width;
    let height = options.height;
    let draw_count = options.draws;
    let layout = options.layout.as_str();
    let mesh_layout = options.mesh_layout.as_str();
    let unique_mesh_count = options.unique_mesh_count();
    let mesh_asset_capacity = options.mesh_capacity();
    let resource_capacity = options.resource_capacity();
    let staging_bytes = options.staging_bytes();
    let static_output_checked = !options.churn;
    let mut json = String::from("{\n\"schema\":3,\"mode\":");
    quote(
        &mut json,
        if options.benchmark {
            "benchmark"
        } else {
            "verify"
        },
    );
    let geometry = match options.mesh_layout {
        frame_scaling::MeshLayout::Shared => {
            "one shared persistent 3-vertex/3-index triangle mesh; xyz+linear rgba stride 28; no instancing"
        }
        frame_scaling::MeshLayout::Distinct => {
            "independent persistent mesh assets with identical 3-vertex/3-index triangle geometry; one vertex/index buffer pair per draw; xyz+linear rgba stride 28; no instancing"
        }
    };
    write!(
        json,
        ",\"workload\":{{\"width\":{width},\"height\":{height},\"draw_count\":{draw_count},\"draw_layout\":\"{layout}\",\"mesh_layout\":\"{mesh_layout}\",\"unique_mesh_count\":{unique_mesh_count},\"mesh_asset_capacity\":{mesh_asset_capacity},\"in_flight_capacity\":{IN_FLIGHT},\"resource_capacity\":{resource_capacity},\"upload_and_readback_capacity_bytes_each\":{staging_bytes},\"max_shader_bindings\":{MAX_SHADER_BINDINGS},\"frames_per_run\":{},\"warmup_frames_per_run\":{},\"runs\":{},\"history\":{},\"custom_swap_red_blue\":{},\"asset_churn\":{},\"seed\":0,\"geometry\":\"{geometry}\",\"static_rgba_compared_every_frame\":{static_output_checked},\"readback\":\"every frame color plus optional 1x1 history; one completed result held until next completion\",\"presentation\":\"headless; no VSync\",\"timestamp_scope\":\"whole Graph including readback copies\",\"cpu_active_scope\":\"build_snapshot start through submit return; excludes asset update, CPU snapshot, explicit mesh preparation/waits and draw generation\",\"cpu_caller_active_scope\":\"cpu_active plus asset-update, snapshot and non-wait mesh-prepare calls; excludes draw generation and explicit waits\",\"mesh_upload_wait_scope\":\"explicit private wait_uploads and frame-capacity wait calls while preparing residency; includes bounded retirement/progress API work; frame-capacity waits also appear in completion_wait\",\"overlap_depth\":\"identical depths; later overlapping triangles fail LESS\",\"measured_loop_scope\":\"first measured frame preparation through all observed completions; includes in-flight warmup tail, asset/snapshot/upload preparation, explicit upload/frame waits and CPU readback checks; startup uploads excluded\"}},\"host\":{{",
        options.frames,
        options.warmup,
        options.runs,
        options.history,
        options.custom,
        options.churn
    )?;
    for (index, (key, value)) in host.iter().enumerate() {
        if index != 0 {
            json.push(',');
        }
        quote(&mut json, key);
        json.push(':');
        quote(&mut json, value);
    }
    json.push_str("},\"runs\":[\n");
    for (index, report) in reports.iter().enumerate() {
        if index != 0 {
            json.push_str(",\n");
        }
        write!(json, "{{\"run\":{},\"device\":", index + 1)?;
        let device = &report.device;
        json.push('{');
        for (index, (key, value)) in [
            ("name", device.name.as_str()),
            ("api_version", &device.api_version),
            ("instance_api_version", &device.instance_api_version),
            ("device_type", &device.device_type),
        ]
        .into_iter()
        .enumerate()
        {
            if index != 0 {
                json.push(',');
            }
            quote(&mut json, key);
            json.push(':');
            quote(&mut json, value);
        }
        write!(
            json,
            ",\"vendor_id\":{},\"device_id\":{},\"driver_version_raw\":{},\"driver_id_raw\":{},\"queue_family\":{},\"driver_name\":",
            device.vendor_id,
            device.device_id,
            device.driver_version_raw,
            device.driver_id_raw,
            device.queue_family
        )?;
        optional_string(&mut json, device.driver_name.as_deref());
        json.push_str(",\"driver_info\":");
        optional_string(&mut json, device.driver_info.as_deref());
        json.push_str("},\"validation\":{");
        json.push_str("\"requested\":");
        quote(&mut json, &format!("{:?}", report.validation.requested));
        write!(
            json,
            ",\"startup_status_recognized\":{},\"core\":{},\"sync\":{},\"warnings\":{},\"errors\":{},\"callback_failures\":{},\"dropped_problems\":{},\"dropped_layer_statuses\":{},\"layer_status_messages\":[",
            report.validation.startup_status_recognized,
            optional_bool(report.validation.core_validation),
            optional_bool(report.validation.synchronization_validation),
            report.validation.warnings,
            report.validation.errors,
            report.validation.callback_failures,
            report.validation.dropped_problems,
            report.validation.dropped_layer_statuses
        )?;
        for (index, value) in report.validation.layer_status_messages.iter().enumerate() {
            if index != 0 {
                json.push(',');
            }
            quote(&mut json, value);
        }
        json.push_str("],\"problems\":[");
        for (index, value) in report.validation.problems.iter().enumerate() {
            if index != 0 {
                json.push(',');
            }
            quote(&mut json, value);
        }
        write!(
            json,
            "]}},\"completion_wait\":{{\"calls\":{},\"timeouts\":{},\"total_wall_ns\":{},\"scope\":\"explicit wait_frame calls, including GPU wait and completion retirement; not CPU polling cost\",\"peak_pending_frames\":{},\"measured_loop_ns\":{}}},\"creation_plateau_checked\":{},\"warmed_stats\":",
            report.wait_calls,
            report.timed_out_waits,
            report.wait_total_ns,
            report.peak_pending_frames,
            report.measured_loop_ns,
            report.plateau_checked
        )?;
        if let Some(value) = &report.warmed_stats {
            stats(&mut json, value)?;
        } else {
            json.push_str("null");
        }
        json.push_str(",\"final_stats\":");
        stats(&mut json, &report.final_stats)?;
        json.push_str(",\"warmed_memory\":");
        if let Some(value) = &report.warmed_memory {
            memory(&mut json, value)?;
        } else {
            json.push_str("null");
        }
        json.push_str(",\"final_memory\":");
        memory(&mut json, &report.final_memory)?;
        write!(
            json,
            ",\"startup_mesh_upload\":{{\"mesh_bytes\":{},\"cpu_snapshot_ns\":{},\"prepare_call_ns\":{},\"wait_call_wall_ns\":{},\"scope\":\"completed before the measured loop; prepare and explicit wait API calls; uploaded data is excluded from steady frame active CPU/GPU samples\"}}",
            report.startup_mesh_upload_bytes,
            report.startup_snapshot_ns,
            report.startup_mesh_upload_prepare_ns,
            report.startup_mesh_upload_wait_ns,
        )?;
        json.push_str(",\"summary_ns\":{");
        for (index, field) in [
            "cpu_active",
            "cpu_caller_active",
            "cpu_asset_update",
            "cpu_snapshot",
            "mesh_upload_prepare",
            "mesh_upload_wait",
            "cpu_build",
            "cpu_compile",
            "cpu_prepare",
            "cpu_record",
            "cpu_submit",
            "cpu_retire",
            "completion_wait",
            "submit_return_to_observed",
            "gpu_graph",
        ]
        .into_iter()
        .enumerate()
        {
            if index != 0 {
                json.push(',');
            }
            quote(&mut json, field);
            json.push(':');
            summary(
                &mut json,
                report
                    .samples
                    .iter()
                    .filter_map(|s| value(s, field))
                    .collect(),
            )?;
        }
        json.push_str("},\"raw_samples\":[");
        for (index, sample) in report.samples.iter().enumerate() {
            if index != 0 {
                json.push(',');
            }
            write!(
                json,
                "{{\"frame\":{},\"cpu_active_ns\":{},\"cpu_caller_active_ns\":{},\"cpu_asset_update_ns\":{},\"cpu_snapshot_ns\":{},\"mesh_upload_prepare_ns\":{},\"mesh_upload_wait_ns\":{},\"cpu_build_ns\":{},\"cpu_compile_ns\":{},\"completion_wait_ns\":{},\"submit_return_to_observed_ns\":{},\"output_checksum_fnv1a64\":\"{:016x}\",\"profile\":",
                sample.frame,
                sample.active_cpu_ns,
                sample.active_cpu_ns
                    + sample.asset_update_ns
                    + sample.snapshot_ns
                    + sample.mesh_upload_prepare_ns,
                sample.asset_update_ns,
                sample.snapshot_ns,
                sample.mesh_upload_prepare_ns,
                sample.mesh_upload_wait_ns,
                sample.build_ns,
                sample.compile_ns,
                sample.completion_wait_ns,
                sample.observed_latency_ns,
                sample.output_checksum
            )?;
            if let Some(p) = &sample.profile {
                write!(
                    json,
                    "{{\"cpu_prepare_ns\":{},\"cpu_record_ns\":{},\"cpu_submit_ns\":{},\"cpu_retire_ns\":{},\"gpu_graph_ns\":{},\"timestamp_period_ns\":{},\"timestamp_valid_bits\":{},\"gpu_status\":",
                    p.prepare_ns,
                    p.record_ns,
                    p.submit_ns,
                    p.retire_ns,
                    p.gpu_ns.map_or_else(|| "null".into(), |n| n.to_string()),
                    p.timestamp_period_ns
                        .map_or_else(|| "null".into(), |n| n.to_string()),
                    p.timestamp_valid_bits
                        .map_or_else(|| "null".into(), |n| n.to_string())
                )?;
                quote(&mut json, p.gpu_status);
                json.push('}');
            } else {
                json.push_str("null");
            }
            json.push('}');
        }
        json.push_str("]}");
    }
    json.push_str("\n]}\n");
    ensure_parent(&options.output)?;
    std::fs::write(&options.output, json)?;
    Ok(())
}

fn optional_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    }
}

fn optional_string(output: &mut String, value: Option<&str>) {
    if let Some(value) = value {
        quote(output, value);
    } else {
        output.push_str("null");
    }
}

fn quote(output: &mut String, value: &str) {
    output.push('"');
    for value in value.chars() {
        match value {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c <= '\u{1f}' => {
                write!(output, "\\u{:04x}", u32::from(c)).expect("writing to String");
            }
            c => output.push(c),
        }
    }
    output.push('"');
}

fn stats(output: &mut String, s: &RuntimeStats) -> std::fmt::Result {
    write!(
        output,
        "{{\"resources_created\":{},\"resources_reused\":{},\"registered_resources\":{},\"execution_slots_created\":{},\"execution_slots_reused\":{},\"descriptor_arenas_created\":{},\"descriptor_arenas_reused\":{},\"upload_live_ranges\":{},\"readback_live_ranges\":{}}}",
        s.resources_created,
        s.resources_reused,
        s.registered_resources,
        s.execution_slots_created,
        s.execution_slots_reused,
        s.descriptor_arenas_created,
        s.descriptor_arenas_reused,
        s.upload_live_ranges,
        s.readback_live_ranges
    )
}

fn memory(output: &mut String, s: &MemoryStats) -> std::fmt::Result {
    write!(
        output,
        "{{\"registered_logical_bytes\":{},\"retained_logical_bytes\":{},\"cached_logical_bytes\":{},\"vma_allocation_count\":{},\"vma_allocation_bytes\":{},\"vma_block_count\":{},\"vma_block_bytes\":{},\"process_budget_bytes\":{},\"upload\":",
        s.registered_logical_bytes,
        s.retained_logical_bytes,
        s.cached_logical_bytes,
        s.vma_allocation_count,
        s.vma_allocation_bytes,
        s.vma_block_count,
        s.vma_block_bytes,
        s.process_budget_bytes
            .map_or_else(|| "null".into(), |n| n.to_string())
    )?;
    staging(output, &s.upload)?;
    output.push_str(",\"readback\":");
    staging(output, &s.readback)?;
    output.push('}');
    Ok(())
}

fn staging(output: &mut String, s: &renderer::StagingMemoryStats) -> std::fmt::Result {
    write!(
        output,
        "{{\"capacity_bytes\":{},\"used_bytes\":{},\"reserved_bytes\":{},\"peak_reserved_bytes\":{},\"live_ranges\":{}}}",
        s.capacity_bytes, s.used_bytes, s.reserved_bytes, s.peak_reserved_bytes, s.live_ranges
    )
}

fn value(s: &Sample, field: &str) -> Option<u128> {
    match field {
        "cpu_active" => Some(s.active_cpu_ns),
        "cpu_caller_active" => {
            Some(s.active_cpu_ns + s.asset_update_ns + s.snapshot_ns + s.mesh_upload_prepare_ns)
        }
        "cpu_asset_update" => Some(s.asset_update_ns),
        "cpu_snapshot" => Some(s.snapshot_ns),
        "mesh_upload_prepare" => Some(s.mesh_upload_prepare_ns),
        "mesh_upload_wait" => Some(s.mesh_upload_wait_ns),
        "cpu_build" => Some(s.build_ns),
        "cpu_compile" => Some(s.compile_ns),
        "completion_wait" => Some(s.completion_wait_ns),
        "submit_return_to_observed" => Some(s.observed_latency_ns),
        "cpu_prepare" => s.profile.as_ref().map(|p| p.prepare_ns),
        "cpu_record" => s.profile.as_ref().map(|p| p.record_ns),
        "cpu_submit" => s.profile.as_ref().map(|p| p.submit_ns),
        "cpu_retire" => s.profile.as_ref().map(|p| p.retire_ns),
        "gpu_graph" => s.profile.as_ref().and_then(|p| p.gpu_ns),
        _ => None,
    }
}

fn summary(output: &mut String, mut samples: Vec<u128>) -> std::fmt::Result {
    if samples.is_empty() {
        output.push_str("null");
        return Ok(());
    }
    samples.sort_unstable();
    let median = if samples.len().is_multiple_of(2) {
        samples[samples.len() / 2 - 1] as f64 / 2.0 + samples[samples.len() / 2] as f64 / 2.0
    } else {
        samples[samples.len() / 2] as f64
    };
    let p95 = samples[(samples.len() * 95).div_ceil(100) - 1];
    write!(
        output,
        "{{\"samples\":{},\"median\":{median},\"p95_nearest_rank\":{p95},\"min\":{},\"max\":{}}}",
        samples.len(),
        samples[0],
        samples[samples.len() - 1]
    )
}

fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Headerless packed RGBA8; dimensions and workload are stored in the JSON.
pub(super) fn write_rgba(path: &Path, rgba: &[u8]) -> std::io::Result<()> {
    ensure_parent(path)?;
    std::fs::write(path, rgba)
}

pub(super) fn write_ppm(path: &Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    ensure_parent(path)?;
    let mut output = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(output, "P6\n{width} {height}\n255\n")?;
    for pixel in rgba.as_chunks::<4>().0 {
        output.write_all(&pixel[..3])?;
    }
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_use_even_median_and_nearest_rank_p95() {
        let mut output = String::new();
        summary(&mut output, vec![100, 1, 3, 2]).unwrap();
        assert_eq!(
            output,
            "{\"samples\":4,\"median\":2.5,\"p95_nearest_rank\":100,\"min\":1,\"max\":100}"
        );
        output.clear();
        summary(&mut output, vec![]).unwrap();
        assert_eq!(output, "null");
    }

    #[test]
    fn strings_escape_json_control_characters_without_losing_unicode() {
        let mut output = String::new();
        quote(&mut output, "日本語\"\\\n\r\t\0\u{1f}");
        assert_eq!(output, "\"日本語\\\"\\\\\\n\\r\\t\\u0000\\u001f\"");
    }
}
