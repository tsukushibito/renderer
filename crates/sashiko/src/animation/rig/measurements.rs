//! Run alone in release mode so process-wide heap counters cover this test only.
use super::*;
use crate::animation::CharacterCatalog;
use sashiko_renderer::{
    core::measurement::{CountingAllocator, Snapshot},
    mesh::{MeshCapacity, Meshes},
};
use std::{alloc::System, hint::black_box, path::Path, time::Instant};

#[global_allocator]
static HEAP: CountingAllocator<System> = CountingAllocator::new(System);

fn measure(label: &str, mut run: impl FnMut(u64)) {
    for tick in 0..12 {
        run(tick);
    }
    let mut samples = [0u128; 64];
    let before = Snapshot::now();
    for (tick, sample) in samples.iter_mut().enumerate() {
        let start = Instant::now();
        run(12 + tick as u64);
        *sample = start.elapsed().as_nanos();
    }
    let heap = Snapshot::now().since(before);
    assert!(heap.is_allocation_free(), "{label}: {heap:?}");
    samples.sort_unstable();
    println!("timing,{label},{},{}", samples[31], samples[60]);
}

#[test]
#[ignore = "release animation timing and isolated process-wide counters"]
fn measure_animation() -> PipelineResult<()> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/survivor/characters/catalog.json");
    let catalog = CharacterCatalog::open(&path)?;
    let rate = 60.try_into()?;
    for name in catalog.names() {
        let model = catalog.load(catalog.id(name).unwrap())?;
        for clip_name in model.clip_names() {
            let before = Snapshot::now();
            let mut prepared = model.prepare_animation(clip_name)?;
            let used = Snapshot::now().since(before).rust;
            println!(
                "prepared,{name},{clip_name},{}",
                used[0].bytes + used[1].bytes - used[3].bytes
            );
            let mut other =
                model.prepare_animation(if clip_name == "walk" { "jog" } else { "walk" })?;
            let (clip, output) = prepared.rigged().unwrap();
            let (target, _) = other.rigged().unwrap();
            let mut meshes = Meshes::new(MeshCapacity {
                meshes: 1,
                retired_mesh_versions: 0,
            })?;
            let mesh = meshes.create(model.model.mesh_data()?)?;
            let pose = meshes.create_pose(mesh)?;
            for count in [1, 4, 64, 256] {
                let label = format!("{name},{clip_name},{count}");
                measure(&format!("{label},pose"), |tick| {
                    for i in 0..count {
                        clip.evaluate_at(tick + i * 13, rate, &mut output.workspace)
                            .unwrap();
                        black_box(&output.workspace);
                    }
                });
                let mut players = (0..count)
                    .map(|i| {
                        let mut p = Playback::new(clip, rate);
                        p.advance_to(i * 13, &mut []).unwrap();
                        p.crossfade(target, 1024.try_into().unwrap()).unwrap();
                        p
                    })
                    .collect::<Vec<_>>();
                measure(&format!("{label},fade"), |tick| {
                    for (i, p) in players.iter_mut().enumerate() {
                        p.advance_to(i as u64 * 13 + tick, &mut []).unwrap();
                        p.evaluate_pose(&mut output.workspace).unwrap();
                        black_box(&output.workspace);
                    }
                });
                measure(&format!("{label},palette"), |_| {
                    for _ in 0..count {
                        black_box(output.palette().unwrap());
                    }
                });
                measure(&format!("{label},pose_palette"), |tick| {
                    for i in 0..count {
                        clip.evaluate_at(tick + i * 13, rate, &mut output.workspace)
                            .unwrap();
                        black_box(output.palette().unwrap());
                    }
                });
                let reference = model.clip(clip_name).unwrap();
                measure(&format!("{label},skin"), |tick| {
                    for i in 0..count {
                        meshes
                            .set_joint_transforms(pose, reference.sample(tick + i * 13, rate))
                            .unwrap();
                    }
                });
            }
        }
    }
    Ok(())
}
