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
            let mut phases = model.prepare_animation(clip_name)?.into_four_phases();
            let label = format!("{name},{clip_name},4");
            measure(&format!("{label},survivor_scalar"), |tick| {
                for offset in [0, 13, 26, 39] {
                    black_box(prepared.sample(tick + offset, rate).unwrap());
                }
            });
            measure(&format!("{label},survivor_batch"), |tick| {
                black_box(
                    phases
                        .sample([tick, tick + 13, tick + 26, tick + 39], rate)
                        .unwrap(),
                );
            });
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
                        p.advance_to(i * 13, &mut [0; 128]).unwrap();
                        p.crossfade(target, 1024.try_into().unwrap()).unwrap();
                        p
                    })
                    .collect::<Vec<_>>();
                measure(&format!("{label},fade"), |tick| {
                    for (i, p) in players.iter_mut().enumerate() {
                        p.advance_to(i as u64 * 13 + tick, &mut [0; 128]).unwrap();
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
                let before = Snapshot::now();
                let mut batch = PoseBatch::new(&output.workspace, count as usize);
                let heap = Snapshot::now().since(before).rust;
                println!(
                    "workspace,{label},{}",
                    heap[0].bytes + heap[1].bytes - heap[3].bytes
                );
                let mut requests = vec![clip.sample_at(0, rate); count as usize];
                for with_palette in [false, true] {
                    let stage = if with_palette {
                        "batch_palette"
                    } else {
                        "batch_pose"
                    };
                    measure(&format!("{label},{stage}"), |tick| {
                        for (i, r) in requests.iter_mut().enumerate() {
                            *r = clip.sample_at(tick + i as u64 * 13, rate);
                        }
                        let poses = batch.evaluate(&requests).unwrap();
                        if with_palette {
                            for pose in poses {
                                write_palette(pose, &output.inverse_bind, &mut output.palette)
                                    .unwrap();
                                black_box(&output.palette);
                            }
                        } else {
                            black_box(poses);
                        }
                    });
                }
                // Give the second run exactly the same progression as the scalar fade.
                for (i, p) in players.iter_mut().enumerate() {
                    *p = Playback::new(clip, rate);
                    p.advance_to(i as u64 * 13, &mut [0; 128]).unwrap();
                    p.crossfade(target, 1024.try_into().unwrap()).unwrap();
                }
                measure(&format!("{label},batch_fade"), |tick| {
                    for (i, (p, r)) in players.iter_mut().zip(&mut requests).enumerate() {
                        p.advance_to(i as u64 * 13 + tick, &mut [0; 128]).unwrap();
                        *r = p.sample();
                    }
                    black_box(batch.evaluate(&requests).unwrap());
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

/// Alternate execution order each tick so both policies sample the same mixed clips and times.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "release animation timing and isolated process-wide counters"]
fn measure_palette_jobs() -> PipelineResult<()> {
    use sashiko_renderer::core::jobs::{JobSystem, RendererJobs};
    let catalog = CharacterCatalog::open(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/survivor/characters/catalog.json"),
    )?;
    let model = catalog.load(catalog.id("primary").unwrap())?;
    let teams = [
        RendererJobs::new(JobSystem::inline()),
        JobSystem::new(4.try_into()?)?.into(),
    ];
    let rate = 60.try_into()?;
    for count in [1, 4, 5, 8, 9, 16, 32, 64, 256] {
        let mut batch = model.prepare_palette_batch(&["walk", "jog"], count)?;
        let mut requests = vec![PaletteRequest { clip: 0, tick: 0 }; count];
        let mut times = [[0u128; 256]; 2];
        let mut run = |tick: usize, mode: usize| -> PipelineResult<u128> {
            let start = Instant::now();
            for (i, r) in requests.iter_mut().enumerate() {
                *r = PaletteRequest {
                    clip: i % 2,
                    tick: (tick + i * 13) as u64,
                };
            }
            for palette in batch.evaluate(&requests, rate, &teams[mode])? {
                black_box(palette);
            }
            Ok(start.elapsed().as_nanos())
        };
        for tick in 0..32 {
            run(tick, 0)?;
            run(tick, 1)?;
        }
        let before = Snapshot::now();
        let [inline, shared] = &mut times;
        for (tick, (inline_ns, shared_ns)) in inline.iter_mut().zip(shared).enumerate() {
            let mut order = [(0, inline_ns), (1, shared_ns)];
            if tick % 2 == 1 {
                order.swap(0, 1);
            }
            for (mode, ns) in order {
                *ns = run(32 + tick, mode)?;
            }
        }
        let delta = Snapshot::now().since(before);
        assert!(delta.is_allocation_free(), "{count}: {delta:?}");
        for (participants, mut ns) in [1, 4].into_iter().zip(times) {
            ns.sort_unstable();
            println!(
                "palette_jobs,{count},{participants},{},{}",
                ns[127], ns[243]
            );
        }
    }
    Ok(())
}
