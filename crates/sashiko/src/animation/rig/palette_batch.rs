use super::*;
use sashiko_renderer::core::jobs::RendererJobs;

/// Index into the clip names supplied at preparation, and its independent display time.
#[derive(Clone, Copy)]
pub struct PaletteRequest {
    pub clip: usize,
    pub tick: u64,
}

/// Fixed-capacity output for independent samples of one animated model.
pub struct PaletteBatch {
    capacity: usize,
    joints: usize,
    data: BatchData,
}

enum BatchData {
    Rigged {
        clips: Vec<PoseClip>,
        inverse: Arc<[Mat4]>,
        groups: Vec<Group>,
    },
    Legacy(Vec<(Clip, Vec<Transform>)>),
}

struct Group {
    batch: PoseBatch,
    palettes: Vec<Transform>,
    error: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl AnimatedModel {
    /// Prepare the named clips and all evaluation/output storage before the frame loop.
    pub fn prepare_palette_batch(
        &self,
        names: &[&str],
        capacity: usize,
    ) -> PipelineResult<PaletteBatch> {
        let joints = self.model.joints;
        let source = |name: &str| {
            self.clip(name)
                .ok_or_else(|| format!("unknown clip {name}"))
        };
        let data = if let Some(rig) = &self.model.skeleton {
            let clips = names
                .iter()
                .map(|&name| rig.clip(source(name)?, self.timelines[name].clone()))
                .collect::<PipelineResult<_>>()?;
            let layout = PoseWorkspace::all(&rig.skeleton);
            let groups = (0..capacity)
                .step_by(4)
                .map(|start| {
                    let lanes = (capacity - start).min(4);
                    Group {
                        batch: PoseBatch::new(&layout, lanes),
                        palettes: vec![Transform::IDENTITY; lanes * joints],
                        error: None,
                    }
                })
                .collect();
            BatchData::Rigged {
                clips,
                inverse: rig.inverse_bind.clone(),
                groups,
            }
        } else {
            BatchData::Legacy(
                names
                    .iter()
                    .map(|&name| {
                        let asset = source(name)?;
                        Ok((asset.clip, asset.poses.to_vec()))
                    })
                    .collect::<PipelineResult<_>>()?,
            )
        };
        Ok(PaletteBatch {
            capacity,
            joints,
            data,
        })
    }
}

impl Group {
    fn evaluate(
        &mut self,
        requests: &[PaletteRequest],
        clips: &[PoseClip],
        rate: NonZeroU32,
        inverse: &[Mat4],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let sample = |r: PaletteRequest| clips[r.clip].sample_at(r.tick, rate);
        let mut samples = [sample(requests[0]); 4];
        for (dest, r) in samples.iter_mut().skip(1).zip(&requests[1..]) {
            *dest = sample(*r);
        }
        let poses = self.batch.evaluate(&samples[..requests.len()])?;
        for (pose, out) in poses
            .iter()
            .zip(self.palettes.chunks_exact_mut(inverse.len()))
        {
            write_palette(pose, inverse, out)?;
        }
        Ok(())
    }
}

impl PaletteBatch {
    /// Evaluate on the host's shared team and return complete palettes in request order.
    /// Legacy assets borrow cooked palettes. Empty and small batches stay on the caller.
    pub fn evaluate<'a>(
        &'a mut self,
        requests: &'a [PaletteRequest],
        rate: NonZeroU32,
        jobs: &RendererJobs,
    ) -> PipelineResult<impl ExactSizeIterator<Item = &'a [Transform]> + 'a> {
        if requests.len() > self.capacity {
            return Err("palette batch capacity exceeded".into());
        }
        let clips = match &self.data {
            BatchData::Rigged { clips, .. } => clips.len(),
            BatchData::Legacy(clips) => clips.len(),
        };
        if requests.iter().any(|r| r.clip >= clips) {
            return Err("unknown palette batch clip index".into());
        }
        if let BatchData::Rigged {
            clips,
            inverse,
            groups,
        } = &mut self.data
        {
            let active = &mut groups[..requests.len().div_ceil(4)];
            let run = |i: usize, group: &mut Group| {
                let start = i * 4;
                group.error = group
                    .evaluate(
                        &requests[start..requests.len().min(start + 4)],
                        clips,
                        rate,
                        inverse,
                    )
                    .err();
            };
            // First consistent p95 win on the 62-joint Survivor rig: four full SIMD groups.
            if requests.len() >= 16
                && requests.len().saturating_mul(self.joints) >= 16 * 62
                && jobs.parallelism().get() > 1
            {
                jobs.with(|team| {
                    team.scope(|scope| {
                        scope.for_each_chunk_mut(active, std::num::NonZeroUsize::MIN, |i, part| {
                            run(i, &mut part[0])
                        });
                    })
                });
            } else {
                for (i, group) in active.iter_mut().enumerate() {
                    run(i, group);
                }
            }
            if let Some(error) = active.iter_mut().find_map(|g| g.error.take()) {
                return Err(error);
            }
        }
        let data = &self.data;
        let joints = self.joints;
        Ok(requests.iter().enumerate().map(move |(i, r)| match data {
            BatchData::Rigged { groups, .. } => {
                let start = (i % 4) * joints;
                &groups[i / 4].palettes[start..start + joints]
            }
            BatchData::Legacy(clips) => {
                let (clip, poses) = &clips[r.clip];
                AnimationClip {
                    clip: *clip,
                    joints,
                    poses,
                }
                .sample(r.tick, rate)
            }
        }))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::animation::CharacterCatalog;
    use sashiko_renderer::core::jobs::JobSystem;
    use std::path::Path;

    #[test]
    fn mixed_clips_keep_order_across_workers_tail_and_retry() -> PipelineResult<()> {
        let catalog = CharacterCatalog::open(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/survivor/characters/catalog.json"),
        )?;
        let mut model = catalog.load(catalog.id("primary").unwrap())?;
        let rate = 60.try_into()?;
        let inline = JobSystem::inline().into();
        let shared = JobSystem::new(4.try_into()?)?.into();
        let names = ["jog", "walk"];
        let requests: [_; 67] = std::array::from_fn(|i| PaletteRequest {
            clip: (i / 3) % 2,
            tick: (i * 13) as u64,
        });
        for legacy in [false, true] {
            if legacy {
                model.model.skeleton = None;
            }
            let mut batch = model.prepare_palette_batch(&names, requests.len())?;
            assert!(batch.evaluate(&[requests[0]; 68], rate, &shared).is_err());
            assert!(
                batch
                    .evaluate(&[PaletteRequest { clip: 2, tick: 0 }], rate, &shared)
                    .is_err()
            );
            for n in [67, 0, 1, 2, 3, 4, 17, 66] {
                let input = &requests[..n];
                let reference: Vec<_> = batch
                    .evaluate(input, rate, &inline)?
                    .map(<[_]>::to_vec)
                    .collect();
                let actual = batch.evaluate(input, rate, &shared)?;
                assert_eq!(actual.len(), n);
                for ((palette, expected), r) in actual.zip(&reference).zip(input) {
                    assert_eq!(palette, expected);
                    let cooked = model.clip(names[r.clip]).unwrap().sample(r.tick, rate);
                    if legacy {
                        assert_eq!(palette, cooked);
                    }
                    assert_eq!(palette.len(), cooked.len());
                    for (a, b) in palette.iter().zip(cooked) {
                        assert!(
                            a.columns()
                                .into_iter()
                                .flatten()
                                .zip(b.columns().into_iter().flatten())
                                .all(|(a, b)| (a - b).abs() < 2e-4)
                        );
                    }
                }
            }
        }
        Ok(())
    }
}
