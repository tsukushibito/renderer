#![cfg(feature = "diagnostics")]

use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(path: &Path, op: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_survivor"))
        .args(["--headless", op])
        .arg(path)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn report_replays_in_another_process_and_locates_the_first_difference() {
    let root = std::env::temp_dir().join(format!("survivor-replay-{}", std::process::id()));
    let record = run(&root, "--record-report", &["--ticks", "8"]);
    assert!(
        record.status.success(),
        "{}",
        String::from_utf8_lossy(&record.stderr)
    );
    let verify = || run(&root, "--verify-report", &[]);
    let success = verify();
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&success.stdout).unwrap()["verified_tick"],
        "8"
    );
    let tape = root.join("replay.json");
    let mut replay: Value = serde_json::from_slice(&fs::read(&tape).unwrap()).unwrap();
    replay["inputs"][3]["movement"] = serde_json::json!([0, 0]);
    fs::write(&tape, serde_json::to_vec(&replay).unwrap()).unwrap();
    let different = verify();
    assert!(!different.status.success());
    assert!(String::from_utf8_lossy(&different.stderr).contains("first state mismatch at tick 4"));
    replay["initial"]["arena"] = Value::Null;
    fs::write(&tape, serde_json::to_vec(&replay).unwrap()).unwrap();
    let missing = verify();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("initial assets"));
    fs::write(&tape, b"[]").unwrap();
    let missing = verify();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("lacks its initial state"));
    for name in ["replay.json", "ticks.ndjson", "manifest.json"] {
        fs::remove_file(root.join(name)).unwrap();
    }
    fs::remove_dir(root).unwrap();
}
