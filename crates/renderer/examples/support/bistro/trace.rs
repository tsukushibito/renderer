//! Optional collector coordination; all I/O and waits stay outside measurement.

use super::{Error, Options};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    sync::mpsc,
    time::{Duration, Instant},
};

pub(super) fn measurement_location() -> &'static tracy_client::SpanLocation {
    tracy_client::span_location!("bistro.measurement")
}

pub(super) fn frame_location() -> &'static tracy_client::SpanLocation {
    tracy_client::span_location!("bistro.frame")
}

pub(super) fn drain_location() -> &'static tracy_client::SpanLocation {
    tracy_client::span_location!("bistro.final_drain")
}

pub(super) fn prepare_locations() {
    // span_location! lazily owns its function-name CString. Warmup does not
    // enter these measured-only zones, so resolve their exact callsites here.
    let _ = (measurement_location(), frame_location(), drain_location());
}

fn validate_capture_id(value: &str) -> Result<(), Error> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("RENDERER_TRACY_CAPTURE_ID must contain exactly 32 hexadecimal digits".into());
    }
    Ok(())
}

pub(super) fn capture_id() -> Result<Option<String>, Error> {
    let value = match std::env::var("RENDERER_TRACY_CAPTURE_ID") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    validate_capture_id(&value)?;
    Ok(Some(value))
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

fn receive_command(
    receive: &mpsc::Receiver<Result<(), &'static str>>,
    timeout: Duration,
) -> Result<(), Error> {
    receive.recv_timeout(timeout)??;
    Ok(())
}

fn handshake(expected: &'static [u8], timeout: Duration) -> Result<(), Error> {
    let (send, receive) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let command = read_command(stdin.lock(), expected);
        let _ = send.send(command);
    });
    // A timeout ends this execution; the collector owns child cleanup. Joining
    // a reader still blocked in stdin would defeat the explicit timeout.
    let command = receive_command(&receive, timeout);
    if command.is_err() && !reader.is_finished() {
        return command;
    }
    reader
        .join()
        .map_err(|_| "capture handshake reader panicked")?;
    command
}

fn wait_connected(mut connected: impl FnMut() -> bool, timeout: Duration) -> Result<(), Error> {
    let started = Instant::now();
    while !connected() {
        if started.elapsed() >= timeout {
            return Err("Tracy collector did not connect before measurement timeout".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn emit(prefix: &str, value: &Value) -> Result<(), Error> {
    println!("{prefix} {value}");
    std::io::stdout().flush()?;
    Ok(())
}

fn boundary() -> Result<Value, Error> {
    #[cfg(windows)]
    return Ok(json!({
        "qpc": super::clock::Boundary::capture()?.qpc,
        "qpc_frequency": super::clock::frequency()?,
        "tid": super::clock::thread_id(),
    }));
    #[cfg(not(windows))]
    Ok(Value::Null)
}

pub(super) fn ready(
    options: &Options,
    identity: &Value,
    capture_id: Option<&str>,
) -> Result<(), Error> {
    emit(
        "BISTRO_READY",
        &json!({
            "schema_version": 1, "pid": std::process::id(), "api": options.api,
            "capture_id": capture_id,
            "warmup_done": true, "warmup_frames": options.warmup,
            "measured_frames": options.frames, "fixture": options.fixture,
            "identity": identity,
            "conditions_path": concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/bistro/conditions.json"),
            "conditions_sha256": identity["conditions_sha256"],
            "cook_manifest_path": options.manifest.as_ref().map(std::path::absolute).transpose()?,
            "cook_manifest_sha256": identity["cook_manifest_sha256"],
            "report_path": std::path::absolute(&options.output)?.join("report.json"),
            "measurement_zone": "bistro.measurement", "frame_zone": "bistro.frame",
            "frame_scope": "one CPU processing iteration, including oldest completion when at in-flight capacity; final drain is separate; not queued GPU completion latency",
            "tracy_connected": tracy_client::Client::is_connected(), "clock": boundary()?,
        }),
    )?;
    handshake(b"GO", Duration::from_secs(30))?;
    // GO alone cannot guarantee on-demand source locations or the first zone
    // reach the collector. Establish the connection before entering the ROI.
    wait_connected(tracy_client::Client::is_connected, Duration::from_secs(30))?;
    if let Some(capture_id) = capture_id {
        let client =
            tracy_client::Client::running().ok_or("Tracy client stopped before capture")?;
        let identity = client.span(tracy_client::span_location!("bistro.capture_identity"), 0);
        // Bind the raw trace to this collector-owned process/run before the
        // measured root. The optional text copy is outside CPU/memory timers.
        identity.emit_text(capture_id);
    }
    Ok(())
}

pub(super) fn done(options: &Options, measured: &Value, tracy: &Value) -> Result<(), Error> {
    if !tracy_client::Client::is_connected() {
        return Err("Tracy collector disconnected during measurement".into());
    }
    emit(
        "BISTRO_DONE",
        &json!({
            "schema_version": 1, "pid": std::process::id(), "api": options.api,
            "capture_id": tracy["capture_id"],
            "measured_frames": options.frames, "measurement": measured,
            "tracy": tracy,
            "report_path": std::path::absolute(&options.output)?.join("report.json"),
            "report_written_after_end": true, "tracy_connected": true,
            "clock": boundary()?,
        }),
    )?;
    // Keep Renderer resources alive until capture has drained and disconnected;
    // image readback and original teardown follow END outside the captured ROI.
    handshake(b"END", Duration::from_secs(180))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_reject_eof_unexpected_and_overlong_input() {
        assert!(read_command(&b"GO\n"[..], b"GO").is_ok());
        assert!(read_command(&b"END\r\n"[..], b"END").is_ok());
        for input in [
            b"".as_slice(),
            b"GO".as_slice(),
            b"END\n".as_slice(),
            b"GO extra\n".as_slice(),
            b"01234567890123456\n".as_slice(),
        ] {
            assert!(read_command(input, b"GO").is_err());
        }
    }

    #[test]
    fn command_wait_is_bounded_and_propagates_reader_failure() {
        let (send, receive) = mpsc::sync_channel(1);
        assert!(receive_command(&receive, Duration::ZERO).is_err());
        send.send(Err("capture handshake EOF/read error")).unwrap();
        assert!(receive_command(&receive, Duration::ZERO).is_err());
        send.send(Ok(())).unwrap();
        assert!(receive_command(&receive, Duration::ZERO).is_ok());
        drop(send);
        assert!(receive_command(&receive, Duration::ZERO).is_err());
    }

    #[test]
    fn measurement_requires_connection_before_the_timeout() {
        assert!(wait_connected(|| true, Duration::ZERO).is_ok());
        assert!(wait_connected(|| false, Duration::ZERO).is_err());
    }

    #[test]
    fn capture_identity_is_bounded_hexadecimal_text() {
        assert!(validate_capture_id("0123456789abcdef0123456789ABCDEF").is_ok());
        for value in [
            "",
            "abc",
            "0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert!(validate_capture_id(value).is_err());
        }
    }
}
