//! Convert cooked skin matrices into skeleton poses at the asset boundary.
mod palette_batch;
use super::{AnimatedModel, AnimationClip, Clip, Transform, floats};
use glam::{DMat4, Mat4};
pub use palette_batch::{PaletteBatch, PaletteRequest};
use sashiko_animation::{
    Clip as PoseClip, LocalPose, Playback, PoseBatch, PoseWorkspace, Skeleton,
};
use sashiko_renderer::pipeline::PipelineResult;
use std::{num::NonZeroU32, sync::Arc};

pub(super) struct Rig {
    skeleton: Arc<Skeleton>,
    parents: Vec<Option<usize>>,
    bind: Vec<DMat4>,
    inverse_bind: Arc<[Mat4]>,
}

impl Rig {
    pub(super) fn decode(bytes: &[u8]) -> PipelineResult<Self> {
        let mut parents = Vec::new();
        let mut bind = Vec::new();
        for record in bytes.as_chunks::<52>().0 {
            let p = u32::from_le_bytes(record[..4].try_into()?);
            parents.push((p != u32::MAX).then_some(p as usize));
            let rows = std::array::from_fn(|i| floats(record, 4 + i * 16));
            bind.push(matrix(Transform::new(rows)?));
        }
        let skeleton = Skeleton::new(parents.clone())?;
        let inverse_bind = bind
            .iter()
            .map(|m| m.inverse().as_mat4())
            .collect::<Arc<[_]>>();
        if inverse_bind
            .iter()
            .any(|m| !m.is_finite() || !m.as_dmat4().inverse().is_finite())
        {
            return Err("inverse bind cannot be represented in single precision".into());
        }
        Ok(Self {
            skeleton,
            parents,
            bind,
            inverse_bind,
        })
    }

    fn clip(
        &self,
        source: AnimationClip<'_>,
        timeline: super::ClipTimeline,
    ) -> PipelineResult<PoseClip> {
        let mut local = Vec::with_capacity(source.poses.len());
        let mut world = vec![DMat4::IDENTITY; source.joints];
        for frame in source.poses.chunks_exact(source.joints) {
            for ((dest, &skin), &bind) in world.iter_mut().zip(frame).zip(&self.bind) {
                *dest = matrix(skin) * bind;
            }
            for (i, &pose) in world.iter().enumerate() {
                let relative = self.parents[i].map_or(pose, |p| world[p].inverse() * pose);
                let basis = [relative.x_axis, relative.y_axis, relative.z_axis]
                    .map(|axis| axis.truncate().try_normalize());
                let [Some(x), Some(y), Some(z)] = basis else {
                    return Err("degenerate joint basis".into());
                };
                if x.dot(y).abs().max(y.dot(z).abs()).max(z.dot(x).abs()) > 1e-4
                    || x.cross(y).dot(z) <= 0.
                {
                    return Err(
                        "joint-local transform requires positive scale without shear".into(),
                    );
                }
                let (scale, rotation, translation) = relative.to_scale_rotation_translation();
                local.push(LocalPose {
                    scale: scale.as_vec3(),
                    rotation: rotation.normalize().as_quat(),
                    translation: translation.as_vec3(),
                });
            }
        }
        Ok(PoseClip::from_timeline(
            self.skeleton.clone(),
            local,
            timeline,
        )?)
    }
}

fn matrix(transform: Transform) -> DMat4 {
    Mat4::from_cols_array_2d(&transform.columns()).as_dmat4()
}

impl AnimatedModel {
    /// Extract local frames only for clips the caller will interpolate or combine.
    pub fn prepare_pose_clip(&self, name: &str) -> PipelineResult<PoseClip> {
        let source = self
            .clip(name)
            .ok_or_else(|| format!("unknown clip {name}"))?;
        self.model
            .skeleton
            .as_ref()
            .ok_or("a skeleton is required for local pose evaluation")?
            .clip(source, self.timelines[name].clone())
    }

    /// Resolve once for all display phases that use this clip.
    pub fn prepare_animation(&self, name: &str) -> PipelineResult<PreparedAnimation> {
        let source = self
            .clip(name)
            .ok_or_else(|| format!("unknown clip {name}"))?;
        let data = if let Some(rig) = &self.model.skeleton {
            PreparedData::Rigged(Box::new(Rigged {
                clip: rig.clip(source, self.timelines[name].clone())?,
                output: PaletteWorkspace {
                    workspace: PoseWorkspace::all(&rig.skeleton),
                    inverse_bind: rig.inverse_bind.clone(),
                    palette: vec![Transform::IDENTITY; source.joints],
                },
            }))
        } else {
            PreparedData::Legacy {
                descriptor: source.clip,
                joints: source.joints,
                poses: source.poses.to_vec(),
            }
        };
        Ok(PreparedAnimation(data))
    }
}

struct Rigged {
    clip: PoseClip,
    output: PaletteWorkspace,
}
/// Prepared model-space pose storage and mesh-specific skin palette.
pub struct PaletteWorkspace {
    workspace: PoseWorkspace,
    inverse_bind: Arc<[Mat4]>,
    palette: Vec<Transform>,
}
enum PreparedData<R> {
    Rigged(Box<R>),
    Legacy {
        descriptor: Clip,
        joints: usize,
        poses: Vec<Transform>,
    },
}
pub struct PreparedAnimation(PreparedData<Rigged>);

impl PreparedAnimation {
    /// Borrow the immutable clip and its independent mesh output for stateful playback.
    pub fn rigged(&mut self) -> Option<(&PoseClip, &mut PaletteWorkspace)> {
        match &mut self.0 {
            PreparedData::Rigged(rig) => Some((&rig.clip, &mut rig.output)),
            PreparedData::Legacy { .. } => None,
        }
    }
    pub fn sample(&mut self, tick: u64, rate: NonZeroU32) -> PipelineResult<&[Transform]> {
        match &mut self.0 {
            PreparedData::Legacy {
                descriptor,
                joints,
                poses,
            } => Ok(AnimationClip {
                clip: *descriptor,
                joints: *joints,
                poses,
            }
            .sample(tick, rate)),
            PreparedData::Rigged(rig) => {
                rig.clip
                    .evaluate_at(tick, rate, &mut rig.output.workspace)?;
                rig.output.palette()
            }
        }
    }
}
impl PaletteWorkspace {
    pub fn evaluate(&mut self, playback: &Playback<'_>) -> PipelineResult<&[Transform]> {
        playback.evaluate_pose(&mut self.workspace)?;
        self.palette()
    }

    fn palette(&mut self) -> PipelineResult<&[Transform]> {
        write_palette(&self.workspace, &self.inverse_bind, &mut self.palette)?;
        Ok(&self.palette)
    }
}
fn write_palette(
    workspace: &PoseWorkspace,
    inverse: &[Mat4],
    palette: &mut [Transform],
) -> Result<(), sashiko_renderer::core::graph::GraphError> {
    for (i, (dest, &inverse_bind)) in palette.iter_mut().zip(inverse).enumerate() {
        let model = workspace.joint(i).expect("full skeleton workspace");
        let cols = (model * inverse_bind).to_cols_array_2d();
        *dest = Transform::new(std::array::from_fn(|row| cols.map(|c| c[row])))?;
    }
    Ok(())
}

/// Four display times of one clip, with independently writable mesh palettes.
pub struct PhaseAnimation(PreparedData<FourPhases>);
struct FourPhases {
    clip: PoseClip,
    batch: PoseBatch,
    inverse: Arc<[Mat4]>,
    palettes: [Vec<Transform>; 4],
}
impl PreparedAnimation {
    pub fn into_four_phases(self) -> PhaseAnimation {
        let data = match self.0 {
            PreparedData::Legacy {
                descriptor,
                joints,
                poses,
            } => PreparedData::Legacy {
                descriptor,
                joints,
                poses,
            },
            PreparedData::Rigged(rig) => PreparedData::Rigged(Box::new(FourPhases {
                batch: PoseBatch::new(&rig.output.workspace, 4),
                palettes: std::array::from_fn(|_| {
                    vec![Transform::IDENTITY; rig.clip.skeleton().joint_count()]
                }),
                inverse: rig.output.inverse_bind,
                clip: rig.clip,
            })),
        };
        PhaseAnimation(data)
    }
}
impl PhaseAnimation {
    pub fn sample(
        &mut self,
        ticks: [u64; 4],
        rate: NonZeroU32,
    ) -> PipelineResult<[&[Transform]; 4]> {
        match &mut self.0 {
            PreparedData::Legacy {
                descriptor,
                joints,
                poses,
            } => {
                let clip = AnimationClip {
                    clip: *descriptor,
                    joints: *joints,
                    poses,
                };
                Ok(ticks.map(|tick| clip.sample(tick, rate)))
            }
            PreparedData::Rigged(rig) => {
                let FourPhases {
                    clip,
                    batch,
                    inverse,
                    palettes,
                } = &mut **rig;
                let requests = ticks.map(|t| clip.sample_at(t, rate));
                for (out, pose) in palettes.iter_mut().zip(batch.evaluate(&requests)?) {
                    write_palette(pose, inverse, out)?;
                }
                Ok(palettes.each_ref().map(Vec::as_slice))
            }
        }
    }
}

#[cfg(all(test, feature = "memory-profiling"))]
mod measurements;
#[cfg(test)]
mod precision;
