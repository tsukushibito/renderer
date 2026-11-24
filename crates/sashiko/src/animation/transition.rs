//! Optional local-space animation blending. Prepare only the requested pair.
use super::{AnimatedModel, AnimationClip, floats};
use glam::{DMat4, DQuat, DVec3, Mat4};
use renderer::{pbr::Transform, pipeline::PipelineResult};
use std::num::{NonZeroU32, NonZeroU64};

struct Joint {
    parent: Option<usize>,
    bind: DMat4,
    inverse_bind: DMat4,
}

pub(super) struct Skeleton {
    joints: Vec<Joint>,
    order: Vec<usize>,
}

impl Skeleton {
    pub(super) fn decode(bytes: &[u8]) -> PipelineResult<Self> {
        let count = bytes.len() / 52;
        let mut joints = Vec::with_capacity(count);
        let mut children = vec![Vec::new(); count];
        let mut order = Vec::with_capacity(count);
        for (index, record) in bytes.as_chunks::<52>().0.iter().enumerate() {
            let parent = u32::from_le_bytes(record[..4].try_into()?);
            let parent = (parent != u32::MAX).then_some(parent as usize);
            if let Some(parent) = parent {
                children
                    .get_mut(parent)
                    .ok_or("unknown skeleton parent")?
                    .push(index);
            } else {
                order.push(index);
            }
            let rows = std::array::from_fn(|row| floats::<4>(record, 4 + row * 16));
            let bind = matrix(Transform::new(rows)?);
            joints.push(Joint {
                parent,
                bind,
                inverse_bind: bind.inverse(),
            });
        }
        let mut next = 0;
        while let Some(&index) = order.get(next) {
            order.extend_from_slice(&children[index]);
            next += 1;
        }
        if order.len() != count {
            return Err("cycle in skeleton parents".into());
        }
        Ok(Self { joints, order })
    }
}

fn matrix(transform: Transform) -> DMat4 {
    Mat4::from_cols_array_2d(&transform.columns()).as_dmat4()
}

struct Local {
    scale: DVec3,
    rotation: DQuat,
    translation: DVec3,
}

impl Local {
    fn decompose(matrix: DMat4) -> PipelineResult<Self> {
        // Accept rotations and positive scales. Check each basis vector
        // independently so large positions/scales cannot mask shear elsewhere.
        let basis = [
            matrix.x_axis.truncate(),
            matrix.y_axis.truncate(),
            matrix.z_axis.truncate(),
        ];
        let axes = basis.map(|col| col.try_normalize().ok_or("degenerate joint basis"));
        let [x, y, z] = axes;
        let (x, y, z) = (x?, y?, z?);
        if x.dot(y).abs().max(y.dot(z).abs()).max(z.dot(x).abs()) > 1e-4 || x.cross(y).dot(z) <= 0.
        {
            return Err(
                "crossfade requires joint-local rotation and positive scale without shear".into(),
            );
        }
        let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
        Ok(Self {
            scale,
            rotation: rotation.normalize(),
            translation,
        })
    }

    fn blend(&self, other: &Self, t: f64) -> DMat4 {
        DMat4::from_scale_rotation_translation(
            self.scale.lerp(other.scale, t),
            self.rotation.slerp(other.rotation, t),
            self.translation.lerp(other.translation, t),
        )
    }
}

struct BlendTrack<'a> {
    clip: AnimationClip<'a>,
    local: Vec<Local>,
}

impl<'a> BlendTrack<'a> {
    fn prepare(clip: AnimationClip<'a>, skeleton: &Skeleton) -> PipelineResult<Self> {
        let mut local = Vec::with_capacity(clip.poses.len());
        let mut world = vec![DMat4::IDENTITY; clip.joints];
        for frame in clip.poses.chunks_exact(clip.joints) {
            for ((pose, joint), dest) in frame.iter().zip(&skeleton.joints).zip(&mut world) {
                *dest = matrix(*pose) * joint.bind;
            }
            for (i, joint) in skeleton.joints.iter().enumerate() {
                let relative = match joint.parent {
                    Some(parent) => world[parent].inverse() * world[i],
                    None => world[i],
                };
                local.push(Local::decompose(relative)?);
            }
        }
        Ok(Self { clip, local })
    }
}

/// Prepared clip pair and reusable scratch space for one skeleton. Samples
/// rotations along the shortest quaternion arc, translations and scale linearly.
pub struct Crossfade<'a> {
    skeleton: &'a Skeleton,
    source: BlendTrack<'a>,
    target: BlendTrack<'a>,
    world: Vec<DMat4>,
    palette: Vec<Transform>,
}

impl AnimatedModel {
    /// Resolve both clips and recover joint-local transforms once at preparation.
    /// Cooked skins with bind frames and parent information support blending.
    pub fn prepare_crossfade(&self, source: &str, target: &str) -> PipelineResult<Crossfade<'_>> {
        let skeleton = self
            .model
            .skeleton
            .as_ref()
            .ok_or("recook character with skeleton metadata for crossfade")?;
        let prepare = |name| {
            let clip = self
                .clip(name)
                .ok_or_else(|| format!("unknown animation clip {name}"))?;
            BlendTrack::prepare(clip, skeleton)
        };
        let source = prepare(source)?;
        let target = prepare(target)?;
        Ok(Crossfade {
            skeleton,
            source,
            target,
            world: vec![DMat4::IDENTITY; skeleton.joints.len()],
            palette: vec![Transform::IDENTITY; skeleton.joints.len()],
        })
    }
}

impl Crossfade<'_> {
    /// Both clips advance during the blend. Source starts at the supplied tick
    /// offset, target starts at zero. At the endpoints return the authored pose.
    /// Reuse the prepared pair for repeated transitions with different timings.
    pub fn sample(
        &mut self,
        source_start_tick: u64,
        elapsed_ticks: u64,
        duration: NonZeroU64,
        tick_rate: NonZeroU32,
    ) -> PipelineResult<&[Transform]> {
        if elapsed_ticks == 0 {
            return Ok(self.source.clip.sample(source_start_tick, tick_rate));
        }
        if elapsed_ticks >= duration.get() {
            return Ok(self.target.clip.sample(elapsed_ticks, tick_rate));
        }
        let source = self.source.clip.sample_index(
            u128::from(source_start_tick) + u128::from(elapsed_ticks),
            tick_rate,
        ) * self.source.clip.joints;
        let target = self
            .target
            .clip
            .sample_index(u128::from(elapsed_ticks), tick_rate)
            * self.target.clip.joints;
        let t = elapsed_ticks as f64 / duration.get() as f64;
        for &i in &self.skeleton.order {
            let joint = &self.skeleton.joints[i];
            let local = self.source.local[source + i].blend(&self.target.local[target + i], t);
            self.world[i] = match joint.parent {
                Some(parent) => self.world[parent] * local,
                None => local,
            };
            let columns = (self.world[i] * joint.inverse_bind)
                .as_mat4()
                .to_cols_array_2d();
            self.palette[i] =
                Transform::new(std::array::from_fn(|row| columns.map(|col| col[row])))?;
        }
        Ok(&self.palette)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::{Clip, SkinnedModel};
    use std::collections::BTreeMap;

    #[test]
    fn blend_turning_parent_preserves_child_offset_and_exact_endpoints() -> PipelineResult<()> {
        let child_bind = DMat4::from_translation(DVec3::X * 2.);
        // Child stored first. Its local counter-rotation cancels the parent's
        // turn in the target pose. Its joint pivot must still follow the parent.
        let skeleton = Skeleton {
            joints: vec![
                Joint {
                    parent: Some(1),
                    bind: child_bind,
                    inverse_bind: child_bind.inverse(),
                },
                Joint {
                    parent: None,
                    bind: DMat4::IDENTITY,
                    inverse_bind: DMat4::IDENTITY,
                },
            ],
            order: vec![1, 0],
        };
        let child_end = Transform::new([[1., 0., 0., -4.], [0., 1., 0., 0.], [0., 0., 1., 0.]])?;
        let parent_end = Transform::new([[-1., 0., 0., 0.], [0., -1., 0., 0.], [0., 0., 1., 0.]])?;
        let hz = NonZeroU32::new(60).unwrap();
        let duration = NonZeroU64::new(8).unwrap();
        let clip = Clip {
            first: 0,
            frames: 1,
            hz,
            looping: true,
        };
        let model = AnimatedModel {
            content: [renderer_asset::Digest::of(&[]); 2],
            model: SkinnedModel {
                indices: vec![],
                vertices: vec![],
                weights: vec![],
                joints: 2,
                frames: 2,
                poses: vec![
                    Transform::IDENTITY,
                    Transform::IDENTITY,
                    child_end,
                    parent_end,
                ],
                skeleton: Some(skeleton),
            },
            color: vec![],
            extent: [0, 0],
            clips: BTreeMap::from([
                ("start".into(), clip),
                ("end".into(), Clip { first: 1, ..clip }),
            ]),
        };
        let mut blend = model.prepare_crossfade("start", "end")?;
        assert_eq!(
            blend.sample(0, 0, duration, hz)?,
            model.clip("start").unwrap().sample(0, hz)
        );
        let mid = blend.sample(u64::MAX, 4, duration, hz)?;
        let [x, y, z] = mid[0].transform_point([2., 0., 0.]);
        assert!(x.abs() < 1e-5 && (y.abs() - 2.).abs() < 1e-5 && z.abs() < 1e-5);
        let rotated = mid[1].transform_point([1., 0., 0.]);
        assert!(rotated[0].abs() < 1e-5 && (rotated[1].abs() - 1.).abs() < 1e-5);
        assert_eq!(blend.sample(0, 8, duration, hz)?, &[child_end, parent_end]);
        assert_eq!(blend.sample(0, 9, duration, hz)?, &[child_end, parent_end]);
        Ok(())
    }
}
