use super::*;
use crate::jobs::SharedJobs;
use sashiko_animation::PoseSample;

/// A recorded mesh palette, or an index in the supplied slice of resolved local poses.
#[derive(Clone, Copy)]
pub enum PaletteRequest {
    Frame { clip: usize, tick: u64 },
    Phase { clip: usize, phase: f64 },
    Sample(usize),
}

pub struct PaletteBatch {
    capacity: usize,
    sample_capacity: usize,
    joints: usize,
    descriptors: Vec<Clip>,
    cooked: Arc<[Transform]>,
    groups: Vec<Group>,
    inverse: Option<Arc<[Mat4]>>,
}
struct Group {
    batch: PoseBatch,
    palettes: Vec<Transform>,
    error: Option<Box<dyn std::error::Error + Send + Sync>>,
}
impl AnimatedModel {
    /// Set sample_capacity to zero for direct-only evaluation, including legacy skins.
    pub fn prepare_palette_batch(
        &self,
        names: &[&str],
        capacity: usize,
        sample_capacity: usize,
    ) -> PipelineResult<PaletteBatch> {
        if sample_capacity > capacity {
            return Err("sample capacity exceeds palette capacity".into());
        }
        let descriptors = names
            .iter()
            .map(|&n| {
                self.clips
                    .get(n)
                    .copied()
                    .ok_or_else(|| format!("unknown clip {n}"))
            })
            .collect::<Result<_, _>>()?;
        let joints = self.model.joints;
        let mut groups = Vec::new();
        let mut inverse = None;
        if sample_capacity > 0 {
            let rig = self
                .model
                .skeleton
                .as_ref()
                .ok_or("a skeleton is required for composed palettes")?;
            let layout = PoseWorkspace::all(&rig.skeleton);
            inverse = Some(rig.inverse_bind.clone());
            for start in (0..sample_capacity).step_by(4) {
                let lanes = (sample_capacity - start).min(4);
                groups.push(Group {
                    batch: PoseBatch::new(&layout, lanes),
                    palettes: vec![Transform::IDENTITY; lanes * joints],
                    error: None,
                });
            }
        }
        Ok(PaletteBatch {
            capacity,
            sample_capacity,
            joints,
            descriptors,
            cooked: self.model.poses.clone(),
            groups,
            inverse,
        })
    }
}
impl PaletteBatch {
    /// Compute each supplied pose once; requests may reuse it and interleave cooked frames.
    pub fn evaluate<'a>(
        &'a mut self,
        requests: &'a [PaletteRequest],
        samples: &[PoseSample<'_>],
        rate: NonZeroU32,
        jobs: &SharedJobs,
    ) -> PipelineResult<impl ExactSizeIterator<Item = &'a [Transform]> + 'a> {
        if requests.len() > self.capacity || samples.len() > self.sample_capacity {
            return Err("palette capacity exceeded".into());
        }
        for r in requests {
            match *r {
                PaletteRequest::Phase { phase, .. } if !(0.0..=1.0).contains(&phase) => {
                    return Err("invalid palette phase".into());
                }
                PaletteRequest::Frame { clip, .. } | PaletteRequest::Phase { clip, .. }
                    if clip >= self.descriptors.len() =>
                {
                    return Err("unknown clip index".into());
                }
                PaletteRequest::Sample(i) if i >= samples.len() => {
                    return Err("unknown pose sample".into());
                }
                _ => {}
            }
        }
        if !samples.is_empty() {
            let inverse = self
                .inverse
                .as_ref()
                .expect("capacity established at preparation");
            let groups = &mut self.groups[..samples.len().div_ceil(4)];
            let run = |i: usize, g: &mut Group| {
                let start = i * 4;
                g.error = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    let poses = g
                        .batch
                        .evaluate(&samples[start..(start + 4).min(samples.len())])?;
                    for (pose, out) in poses.iter().zip(g.palettes.chunks_exact_mut(inverse.len()))
                    {
                        write_palette(pose, inverse, out)?;
                    }
                    Ok(())
                })()
                .err();
            };
            if samples.len() >= 16
                && samples.len().saturating_mul(self.joints) >= 16 * 62
                && jobs.parallelism().get() > 1
            {
                jobs.with(|team| {
                    team.scope(|scope| {
                        scope.for_each_chunk_mut(groups, std::num::NonZeroUsize::MIN, |i, part| {
                            run(i, &mut part[0])
                        })
                    })
                });
            } else {
                for (i, g) in groups.iter_mut().enumerate() {
                    run(i, g);
                }
            }
            if let Some(e) = groups.iter_mut().find_map(|g| g.error.take()) {
                return Err(e);
            }
        }
        let batch = &*self;
        Ok(requests.iter().map(move |r| match *r {
            PaletteRequest::Frame { clip, tick } => {
                let clip = batch.descriptors[clip];
                let first = clip.first * batch.joints;
                AnimationClip {
                    clip,
                    joints: batch.joints,
                    poses: &batch.cooked[first..],
                }
                .sample(tick, rate)
            }
            PaletteRequest::Phase { clip, phase } => {
                let clip = batch.descriptors[clip];
                let index = (phase * clip.frames as f64) as usize;
                let frame = if clip.looping {
                    index % clip.frames
                } else {
                    index.min(clip.frames - 1)
                };
                let start = (clip.first + frame) * batch.joints;
                &batch.cooked[start..start + batch.joints]
            }
            PaletteRequest::Sample(i) => {
                let start = i % 4 * batch.joints;
                &batch.groups[i / 4].palettes[start..start + batch.joints]
            }
        }))
    }
}
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::animation::CharacterCatalog;
    use crate::jobs::JobSystem;
    use std::path::Path;

    #[test]
    fn interleave_shared_cooked_and_composed_outputs() -> PipelineResult<()> {
        let catalog = CharacterCatalog::open(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/survivor/characters/catalog.json"),
        )?;
        let mut model = catalog.load(catalog.id("primary").unwrap())?;
        let clip = model.prepare_pose_clip("walk")?;
        let capacity = 20;
        let requests: Vec<_> = (0..capacity)
            .map(|i| {
                if i % 3 == 0 {
                    PaletteRequest::Frame {
                        clip: 0,
                        tick: i as u64,
                    }
                } else {
                    PaletteRequest::Sample(capacity - 1 - i)
                }
            })
            .collect();
        let rate = 60.try_into()?;
        let samples: Vec<_> = (0..capacity)
            .map(|i| clip.interpolated_at(i as u64, rate))
            .collect();
        let mut batch = model.prepare_palette_batch(&["walk"], capacity, capacity)?;
        let serial: SharedJobs = JobSystem::inline().into();
        let parallel = JobSystem::new(4.try_into()?)?.into();
        let reference: Vec<_> = batch
            .evaluate(&requests, &samples, rate, &serial)?
            .map(<[_]>::to_vec)
            .collect();
        let raw = model.clip("walk").unwrap();
        for (i, (actual, expected)) in batch
            .evaluate(&requests, &samples, rate, &parallel)?
            .zip(reference)
            .enumerate()
        {
            assert_eq!(actual, expected);
            let tick = if i % 3 == 0 { i } else { capacity - 1 - i };
            let cooked = raw.sample(tick as u64, rate);
            if i % 3 == 0 {
                assert!(std::ptr::eq(actual, cooked));
            }
            for (a, b) in actual.iter().zip(cooked) {
                assert!(matrix(*a).abs_diff_eq(matrix(*b), 2e-4));
            }
        }
        model.model.skeleton = None;
        assert!(model.prepare_palette_batch(&["walk"], 1, 1).is_err());
        Ok(())
    }
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
        let requests: [_; 67] = std::array::from_fn(|i| PaletteRequest::Frame {
            clip: (i / 3) % 2,
            tick: (i * 13) as u64,
        });
        for legacy in [false, true] {
            if legacy {
                model.model.skeleton = None;
            }
            let mut batch = model.prepare_palette_batch(&names, requests.len(), 0)?;
            assert!(
                batch
                    .evaluate(&[requests[0]; 68], &[], rate, &shared)
                    .is_err()
            );
            assert!(
                batch
                    .evaluate(
                        &[PaletteRequest::Frame { clip: 2, tick: 0 }],
                        &[],
                        rate,
                        &shared
                    )
                    .is_err()
            );
            for n in [67, 0, 1, 2, 3, 4, 17, 66] {
                let input = &requests[..n];
                let reference: Vec<_> = batch
                    .evaluate(input, &[], rate, &inline)?
                    .map(<[_]>::to_vec)
                    .collect();
                let actual = batch.evaluate(input, &[], rate, &shared)?;
                assert_eq!(actual.len(), n);
                for ((palette, expected), r) in actual.zip(&reference).zip(input) {
                    assert_eq!(palette, expected);
                    let PaletteRequest::Frame { clip, tick } = *r else {
                        unreachable!()
                    };
                    let cooked = model.clip(names[clip]).unwrap().sample(tick, rate);
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
