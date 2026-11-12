//! std-only capture output; raw samples accompany the summaries.

use std::{
    collections::BTreeMap, error::Error, fmt::Write as _, io::Write as _, path::Path,
    process::Command,
};

use renderer::{MemoryStats, RuntimeStats};

use super::{
    HEIGHT, IN_FLIGHT, MAX_SHADER_BINDINGS, Options, RESOURCE_CAPACITY, RunReport, STAGING_BYTES,
    Sample, WIDTH,
};

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
    let mut json = String::from("{\n\"schema\":1,\"mode\":");
    quote(
        &mut json,
        if options.benchmark {
            "benchmark"
        } else {
            "verify"
        },
    );
    write!(
        json,
        ",\"workload\":{{\"width\":{WIDTH},\"height\":{HEIGHT},\"in_flight_capacity\":{IN_FLIGHT},\"resource_capacity\":{RESOURCE_CAPACITY},\"upload_and_readback_capacity_bytes_each\":{STAGING_BYTES},\"max_shader_bindings\":{MAX_SHADER_BINDINGS},\"frames_per_run\":{},\"warmup_frames_per_run\":{},\"runs\":{},\"history\":{},\"custom_swap_red_blue\":{},\"asset_churn\":{},\"seed\":0,\"readback\":\"every frame color plus optional 1x1 history; one completed result held until next completion\",\"presentation\":\"headless; no VSync\",\"timestamp_scope\":\"whole Graph including readback copies\",\"measured_loop_scope\":\"first measured build through all observed completions; includes in-flight warmup tail and CPU readback checks\"}},\"host\":{{",
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
            "]}},\"polling\":{{\"calls\":{},\"pending_calls\":{},\"total_cpu_ns\":{},\"peak_pending_frames\":{},\"measured_loop_ns\":{}}},\"creation_plateau_checked\":{},\"warmed_stats\":",
            report.poll_calls,
            report.pending_polls,
            report.poll_total_ns,
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
        json.push_str(",\"summary_ns\":{");
        for (index, field) in [
            "cpu_build",
            "cpu_compile",
            "cpu_prepare",
            "cpu_record",
            "cpu_submit",
            "cpu_retire",
            "completed_poll",
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
                "{{\"frame\":{},\"cpu_build_ns\":{},\"cpu_compile_ns\":{},\"completed_poll_ns\":{},\"submit_return_to_observed_ns\":{},\"output_checksum_fnv1a64\":\"{:016x}\",\"profile\":",
                sample.frame,
                sample.build_ns,
                sample.compile_ns,
                sample.completed_poll_ns,
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
        "cpu_build" => Some(s.build_ns),
        "cpu_compile" => Some(s.compile_ns),
        "completed_poll" => Some(s.completed_poll_ns),
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
