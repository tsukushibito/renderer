//! Allocation gate across warmed, actually presented Survivor frames.
#![cfg(all(
    windows,
    feature = "presentation",
    feature = "memory-profiling",
    not(feature = "development-validation"),
    any(feature = "vulkan", feature = "d3d12")
))]
use std::{error::Error, fs, process::Command, time::SystemTime};

fn check(backend: &str) -> Result<(), Box<dyn Error>> {
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "survivor-memory-{backend}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&dir)?;
    for skin in ["cpu", "compute"] {
        for timings in [false, true] {
            let csv = dir.join(format!("{skin}-{timings}.csv"));
            // Measured runs use a transient profile. Saving a first unlock is a separate IO boundary.
            let mut game = Command::new(env!("CARGO_BIN_EXE_survivor"));
            game.args([
                "--window",
                "--backend",
                backend,
                "--scenario",
                "endgame",
                "--seed",
                "506",
                "--skinning",
                skin,
                "--character-shadows",
                "--width",
                "640",
                "--height",
                "360",
                "--warmup",
                "12",
                "--frames",
                "40",
                "--volume",
                "0",
            ])
            .arg("--metrics")
            .arg(&csv);
            if timings {
                game.arg("--gpu-profile");
            }
            let output = game.output()?;
            assert!(
                output.status.success(),
                "{backend} {skin} timings={timings}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let data = fs::read_to_string(&csv)?;
            let mut lines = data.lines();
            let columns: Vec<_> = lines.next().ok_or("missing header")?.split(',').collect();
            let index = |field| {
                columns
                    .iter()
                    .position(|&s| s == field)
                    .unwrap_or_else(|| panic!("missing {field}"))
            };
            let events = [
                "alloc",
                "alloc_zeroed",
                "realloc",
                "free",
                "object_create",
                "object_destroy",
                "resource_create",
                "resource_destroy",
                "memory_allocate",
                "memory_free",
                "descriptor_allocate",
                "descriptor_free",
                "vma_host_alloc",
                "vma_host_realloc",
                "vma_host_free",
            ]
            .map(index);
            let mut frames = 0;
            for line in lines {
                let row: Vec<_> = line.split(',').collect();
                for col in events {
                    assert_eq!(
                        row[col],
                        "0",
                        "{} frame {frames} {}",
                        csv.display(),
                        columns[col]
                    );
                }
                assert!(row[index("native_reset")].parse::<u64>()? > 0);
                assert_eq!(!row[index("graph_gpu_ms")].is_empty(), timings);
                assert_eq!(row[index("active")], "500");
                assert_eq!(row[index("tick")].parse::<usize>()?, 13 + frames);
                assert!(row[index("recorded_pose_bytes")].parse::<u64>()? > 0);
                frames += 1;
            }
            assert_eq!(frames, 40);
        }
    }
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Windows display and Vulkan GPU"]
fn vulkan_warmed_presentations() -> Result<(), Box<dyn Error>> {
    check("vulkan")
}

#[cfg(feature = "d3d12")]
#[test]
#[ignore = "requires a Windows display and D3D12 GPU"]
fn d3d12_warmed_presentations() -> Result<(), Box<dyn Error>> {
    check("d3d12")
}
