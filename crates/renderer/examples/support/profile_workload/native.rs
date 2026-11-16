use super::backend::Backend;
#[cfg(feature = "d3d12")]
use super::backend::D3d12;
#[cfg(feature = "vulkan")]
use super::backend::Vulkan;

use super::{Api, BYTES, Error, GraphPool, Options, Poll, build, capacity, clock, payload};
use std::{
    io::{Read, Write},
    sync::mpsc,
    time::{Duration, Instant},
};

// All counters and timing accumulators are fixed-size caller storage.
struct Totals {
    frames: u64,
    polls: u64,
    digest: u64,
    elapsed: Duration,
    phases: [Duration; 6],
}
impl Default for Totals {
    fn default() -> Self {
        Self {
            frames: 0,
            polls: 0,
            digest: 0xcbf29ce484222325,
            elapsed: Duration::ZERO,
            phases: [Duration::ZERO; 6],
        }
    }
}
impl Totals {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({ "frames": self.frames, "poll_calls": self.polls,
            "payload_digest_fnv1a64": format!("{:016x}", self.digest), "elapsed_ns": self.elapsed.as_nanos(),
            "phase_elapsed_ns": { "build": self.phases[0].as_nanos(), "compile": self.phases[1].as_nanos(),
                "prepare_record_submit": self.phases[2].as_nanos(), "poll_api": self.phases[3].as_nanos(),
                "caller_poll_pacing": self.phases[4].as_nanos(), "readback_verify_return": self.phases[5].as_nanos() },
            "phase_time_kind": "elapsed wall time; phases exclude loop/timer overhead; not CPU execution time" })
    }
}

fn batch<B: Backend>(
    backend: &mut B,
    pool: &mut GraphPool,
    options: &Options,
    duration: Duration,
    target: Option<u64>,
) -> Result<Totals, Error> {
    let start = Instant::now();
    let mut totals = Totals::default();
    while totals.frames == 0
        || (start.elapsed() < duration && target.is_none_or(|n| totals.frames < n))
    {
        let bytes = payload(options.seed, totals.frames);
        let phase = Instant::now();
        let (graph, readback) = build(pool, options.resources, &bytes)?;
        totals.phases[0] += phase.elapsed();
        let phase = Instant::now();
        let plan = graph.compile()?;
        totals.phases[1] += phase.elapsed();
        let phase = Instant::now();
        let mut pending = backend.submit(&plan)?;
        totals.phases[2] += phase.elapsed();
        let poll_start = Instant::now();
        let mut result = loop {
            let phase = Instant::now();
            let completed = B::poll(&mut pending)?;
            totals.phases[3] += phase.elapsed();
            totals.polls = totals.polls.checked_add(1).ok_or("poll count overflow")?;
            if let Some(result) = completed {
                break result;
            }
            if poll_start.elapsed() >= Duration::from_secs(30) {
                return Err("nonblocking GPU polling timed out after 30 seconds".into());
            }
            let phase = Instant::now();
            match options.poll {
                Poll::Busy => {}
                Poll::Yield => std::thread::yield_now(),
                Poll::Sleep => std::thread::sleep(Duration::from_millis(1)),
            }
            totals.phases[4] += phase.elapsed();
        };
        let phase = Instant::now();
        let mut actual = [0; BYTES];
        B::readback(&mut result, readback, &mut actual)?;
        if actual != bytes {
            return Err(format!("readback mismatch at frame {}", totals.frames).into());
        }
        for byte in actual {
            totals.digest = (totals.digest ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        drop(result);
        drop(pending);
        drop(plan);
        if pool.leased_slots() != 0 {
            return Err("frame retained a Graph pool lease".into());
        }
        totals.phases[5] += phase.elapsed();
        totals.frames = totals.frames.checked_add(1).ok_or("frame count overflow")?;
    }
    totals.elapsed = start.elapsed();
    if target.is_some_and(|target| totals.frames != target || totals.elapsed > duration) {
        return Err("duration expired before --frames target completed".into());
    }
    Ok(totals)
}

fn read_command(mut input: impl Read, expected: &[u8]) -> Result<(), &'static str> {
    let mut line = [0_u8; 16];
    let mut length = 0;
    loop {
        let mut byte = [0];
        input
            .read_exact(&mut byte)
            .map_err(|_| "capture handshake EOF/read error")?;
        if byte[0] == b'\n' {
            if length != 0 && line[length - 1] == b'\r' {
                length -= 1;
            }
            return if &line[..length] == expected {
                Ok(())
            } else {
                Err("unexpected capture handshake command")
            };
        }
        if length == line.len() {
            return Err("capture handshake command too long");
        }
        line[length] = byte[0];
        length += 1;
    }
}
fn handshake(expected: &'static [u8], seconds: u64) -> Result<(), Error> {
    let (send, receive) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let command = read_command(stdin.lock(), expected);
        let _ = send.send(command);
    });
    let command = receive.recv_timeout(Duration::from_secs(seconds))?;
    // Finish stdin/thread/channel cleanup before starting a measurement. On a
    // timeout no measurement follows; the caller capture tool owns child cleanup.
    reader
        .join()
        .map_err(|_| "capture handshake reader panicked")?;
    command?;
    Ok(())
}
fn emit(prefix: &str, value: &serde_json::Value) -> Result<(), Error> {
    println!("{prefix} {value}");
    std::io::stdout().flush()?;
    Ok(())
}

#[cfg(feature = "memory-profiling")]
fn memory_json(memory: renderer::measurement::Snapshot) -> serde_json::Value {
    serde_json::json!({ "allocation_free": memory.is_allocation_free(),
        "overflowed": memory.overflowed,
        "rust_calls_allocate_zeroed_reallocate_deallocate": memory.rust.map(|event| event.calls),
        "rust_requested_bytes": memory.rust.map(|event| event.bytes),
        "vma_host_calls_allocate_reallocate_deallocate": memory.vma_host.map(|event| event.calls),
        "vma_host_requested_bytes": memory.vma_host.map(|event| event.bytes),
        "native_create_destroy_resource_create_destroy_memory_alloc_free_descriptor_alloc_free_reset": memory.native })
}

fn execute<B: Backend>(options: &Options) -> Result<(), Error> {
    let frequency = clock::frequency()?;
    let thread_id = clock::thread_id();
    #[cfg(feature = "memory-profiling")]
    let setup_start = renderer::measurement::Snapshot::now();
    let mut backend = B::create(options)?;
    let info = backend.info();
    let graph_capacity = capacity(options.resources)?;
    let mut pool = GraphPool::new(graph_capacity, 1)?;
    let warmup = batch(&mut backend, &mut pool, options, options.warmup, None)?;
    #[cfg(feature = "memory-profiling")]
    let setup_memory = renderer::measurement::Snapshot::now().since(setup_start);
    let configuration = serde_json::json!({ "api": options.api.name(), "resources": options.resources,
        "buffer_bytes": BYTES, "seed": options.seed, "warmup_ms": options.warmup.as_millis(),
        "duration_ms": options.duration.as_millis(), "target_frames": options.frames,
        "in_flight": 1, "headless": true, "vsync": null, "caller_poll_policy": options.poll.name(),
        "caller_poll_sleep_ms": if options.poll == Poll::Sleep { 1 } else { 0 },
        "gpu_timestamps": false, "native_metrics": false, "capture_handshake": options.capture_handshake,
        "development_validation": cfg!(feature = "development-validation"),
        "vulkan_diagnostics": cfg!(feature = "vulkan-diagnostics"),
        "d3d12_diagnostics": cfg!(feature = "d3d12-diagnostics"),
        "graph_diagnostics": cfg!(feature = "graph-diagnostics"),
        "debug_assertions": cfg!(debug_assertions),
        "memory_profiling": cfg!(feature = "memory-profiling"),
        "graph_capacity": { "resources": graph_capacity.resources, "passes": graph_capacity.passes,
            "accesses": graph_capacity.accesses, "name_bytes": graph_capacity.name_bytes,
            "upload_bytes": graph_capacity.upload_bytes, "edges": graph_capacity.edges,
            "segments": graph_capacity.segments, "readers": graph_capacity.readers },
        "prepared_capacity": { "execution_slots": 1, "result_slots": 1, "graph_slots": 1,
            "resource_capacity": options.resources, "buffer_instances": options.resources,
            "initialized_ranges_per_resource": 1, "range_snapshots": 1, "provenance_segments_per_resource": 0,
            "vulkan_work_slots": 1, "vulkan_metadata_slots": 1, "vulkan_max_shader_bindings": 1,
            "vulkan_staging_bytes_per_arena": 16384, "vulkan_staging_ranges_per_arena": 1,
            "d3d12_operations": graph_capacity.passes, "d3d12_upload_bytes": BYTES,
            "d3d12_readback_bytes": BYTES, "d3d12_scratch_bytes": BYTES } });
    if options.capture_handshake {
        emit(
            "PROFILE_WORKLOAD_READY",
            &serde_json::json!({ "schema": 1, "pid": std::process::id(),
            "tid": thread_id, "qpc_frequency": frequency, "warmup_done": true,
            "configuration": configuration, "warmup": warmup.json(), "device": info }),
        )?;
        handshake(b"GO", 30)?;
    }
    #[cfg(feature = "memory-profiling")]
    let memory_start = renderer::measurement::Snapshot::now();
    let start = clock::Boundary::capture()?;
    let measured = batch(
        &mut backend,
        &mut pool,
        options,
        options.duration,
        options.frames,
    )?;
    let end = clock::Boundary::capture()?;
    #[cfg(feature = "memory-profiling")]
    let memory = renderer::measurement::Snapshot::now().since(memory_start);
    let mut result = serde_json::json!({ "schema": 1, "pid": std::process::id(), "tid": thread_id,
        "configuration": configuration, "device": info, "warmup": warmup.json(),
        "measurement": measured.json(), "window": end.difference(start, frequency)?,
        "content_verified_bytes_per_frame": BYTES, "measured_payload_first_frame": 0,
        "retained_graph_slots": pool.leased_slots() });
    #[cfg(feature = "memory-profiling")]
    {
        result["memory"] = memory_json(memory);
        result["setup_and_warmup_memory_positive_control"] = memory_json(setup_memory);
    }
    if options.capture_handshake {
        emit("PROFILE_WORKLOAD_DONE", &result)?;
        handshake(b"END", 180)?;
    }
    #[cfg(feature = "memory-profiling")]
    let shutdown_start = renderer::measurement::Snapshot::now();
    drop(pool);
    let validation = backend.close()?;
    #[cfg(feature = "memory-profiling")]
    let shutdown_memory = renderer::measurement::Snapshot::now().since(shutdown_start);
    result["validation"] = validation;
    #[cfg(feature = "memory-profiling")]
    {
        result["shutdown_memory_positive_control"] = memory_json(shutdown_memory);
    }
    emit("PROFILE_WORKLOAD_REPORT", &result)?;
    #[cfg(feature = "memory-profiling")]
    {
        use renderer::measurement::NativeEvent;
        if setup_memory.overflowed
            || shutdown_memory.overflowed
            || setup_memory.rust[0].calls == 0
            || setup_memory.native[NativeEvent::ResourceCreate as usize] == 0
            || shutdown_memory.native[NativeEvent::ResourceDestroy as usize] == 0
        {
            return Err(
                "allocation/native hooks lack setup/shutdown positive controls; see report".into(),
            );
        }
        if !memory.is_allocation_free() {
            return Err(
            "measured window contained allocation/free or native create/destroy events; see report"
                .into(),
        );
        }
    }
    Ok(())
}

pub(super) fn run(options: &Options) -> Result<(), Error> {
    match options.api {
        #[cfg(feature = "vulkan")]
        Api::Vulkan => execute::<Vulkan>(options),
        #[cfg(not(feature = "vulkan"))]
        Api::Vulkan => Err("--api vulkan requires feature vulkan".into()),
        #[cfg(feature = "d3d12")]
        Api::D3d12 => execute::<D3d12>(options),
        #[cfg(not(feature = "d3d12"))]
        Api::D3d12 => Err("--api d3d12 requires feature d3d12".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_commands_require_complete_matching_lines() {
        assert!(read_command(&b"GO\n"[..], b"GO").is_ok());
        assert!(read_command(&b"END\r\n"[..], b"END").is_ok());
        for input in [
            b"GO".as_slice(),
            b"".as_slice(),
            b"END\n".as_slice(),
            b"GO extra\n".as_slice(),
            b"01234567890123456\n".as_slice(),
        ] {
            assert!(read_command(input, b"GO").is_err());
        }
    }
    fn acceptance(api: Api) {
        for resources in [4, 256] {
            let options = Options {
                api,
                resources,
                seed: 42,
                warmup: Duration::from_millis(20),
                duration: Duration::from_secs(10),
                frames: Some(8),
                poll: Poll::Yield,
                capture_handshake: false,
            };
            run(&options).unwrap();
        }
    }
    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a real Vulkan GPU and configured native validation; run explicitly"]
    fn vulkan_small_and_large_changed_payloads() {
        acceptance(Api::Vulkan);
    }
    #[cfg(feature = "d3d12")]
    #[test]
    #[ignore = "requires a real D3D12 GPU and configured native validation; run explicitly"]
    fn d3d12_small_and_large_changed_payloads() {
        acceptance(Api::D3d12);
    }
}
