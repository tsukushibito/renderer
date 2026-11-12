//! Repeatable game integration workloads, with correctness separate from timing.
//!
//! `--verify report.json` requires effective Core and Synchronization Validation.
//! `--benchmark report.json` explicitly requests Disabled validation and records
//! raw warm samples; neither mode measures presentation or claims an FPS rate.

use std::{error::Error, path::PathBuf};

use renderer::engine;

#[path = "support/engine_geometry.rs"]
mod engine_geometry;
#[path = "support/engine_workloads.rs"]
mod engine_workloads;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let mode = match arguments.next().as_deref() {
        Some("--verify") => engine_workloads::Mode::Verify,
        Some("--benchmark") => engine_workloads::Mode::Benchmark,
        _ => return Err("usage: engine_workloads --verify|--benchmark report.json".into()),
    };
    let output = PathBuf::from(arguments.next().ok_or("missing JSON report path")?);
    if arguments.next().is_some() {
        return Err("unexpected argument".into());
    }
    let report = engine_workloads::run(mode)?;
    std::fs::write(&output, report)?;
    println!("wrote {}", output.display());
    Ok(())
}
