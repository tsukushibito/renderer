//! Convert cooked skin matrices into skeleton poses at the asset boundary.
use super::{AnimatedModel, AnimationClip, Clip, Transform, floats};
use glam::{DMat4, Mat4};
use sashiko_animation::{Clip as PoseClip, LocalPose, Playback, PoseWorkspace, Skeleton};
use sashiko_renderer::pipeline::PipelineResult;
use std::{num::NonZeroU32, sync::Arc};

pub(super) struct Rig {
    skeleton: Arc<Skeleton>,
    parents: Vec<Option<usize>>,
    bind: Vec<DMat4>,
    inverse_bind: Arc<[DMat4]>,
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
        let inverse_bind = bind.iter().map(DMat4::inverse).collect();
        Ok(Self {
            skeleton,
            parents,
            bind,
            inverse_bind,
        })
    }

    fn clip(&self, source: AnimationClip<'_>) -> PipelineResult<PoseClip> {
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
                    scale,
                    rotation: rotation.normalize(),
                    translation,
                });
            }
        }
        Ok(PoseClip::new(
            self.skeleton.clone(),
            local,
            source.clip.hz,
            source.clip.looping,
            vec![],
            vec![],
        )?)
    }
}

fn matrix(transform: Transform) -> DMat4 {
    Mat4::from_cols_array_2d(&transform.columns()).as_dmat4()
}

impl AnimatedModel {
    /// Resolve once for all display phases that use this clip.
    pub fn prepare_animation(&self, name: &str) -> PipelineResult<PreparedAnimation> {
        let source = self
            .clip(name)
            .ok_or_else(|| format!("unknown clip {name}"))?;
        let data = if let Some(rig) = &self.model.skeleton {
            PreparedData::Rigged(Box::new(Rigged {
                clip: rig.clip(source)?,
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
    inverse_bind: Arc<[DMat4]>,
    palette: Vec<Transform>,
}
enum PreparedData {
    Rigged(Box<Rigged>),
    Legacy {
        descriptor: Clip,
        joints: usize,
        poses: Vec<Transform>,
    },
}
pub struct PreparedAnimation(PreparedData);

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
        for (i, (dest, &inverse_bind)) in self
            .palette
            .iter_mut()
            .zip(self.inverse_bind.iter())
            .enumerate()
        {
            let model = self.workspace.joint(i).expect("full skeleton workspace");
            let cols = (model * inverse_bind).as_mat4().to_cols_array_2d();
            *dest = Transform::new(std::array::from_fn(|row| cols.map(|c| c[row])))?;
        }
        Ok(&self.palette)
    }
}
