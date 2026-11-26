//! Fixed-tick animation. Assets are immutable; playback and pose storage belong to the caller.
use glam::{DVec3, Mat4, Quat, Vec3};
use std::{num::NonZeroU32, sync::Arc};

mod sync;
pub use sync::PhaseMap;
mod blend;
pub use blend::{BlendLine, BlendSpace, JointMask, LocalBuffer};
mod batch;
pub use batch::{PoseBatch, PoseSample};
mod playback;
pub use playback::{Advance, Playback};

#[derive(Debug, PartialEq, Eq)]
pub enum AnimationError {
    Invalid(&'static str),
    EventCapacity { required: u128 },
}

impl std::fmt::Display for AnimationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => f.write_str(reason),
            Self::EventCapacity { required } => {
                write!(f, "advance requires {required} event slots")
            }
        }
    }
}
impl std::error::Error for AnimationError {}
use AnimationError::Invalid;

pub struct Skeleton {
    parents: Box<[Option<usize>]>,
    order: Vec<usize>,
}

impl Skeleton {
    pub fn new(parents: Vec<Option<usize>>) -> Result<Arc<Self>, AnimationError> {
        if parents.is_empty() {
            return Err(Invalid("empty skeleton"));
        }
        let mut children = vec![Vec::new(); parents.len()];
        let mut order = Vec::with_capacity(parents.len());
        for (i, &parent) in parents.iter().enumerate() {
            if let Some(p) = parent {
                children
                    .get_mut(p)
                    .ok_or(Invalid("unknown parent"))?
                    .push(i);
            } else {
                order.push(i);
            }
        }
        let mut next = 0;
        while let Some(&parent) = order.get(next) {
            order.extend_from_slice(&children[parent]);
            next += 1;
        }
        if order.len() != parents.len() {
            return Err(Invalid("cyclic skeleton"));
        }
        Ok(Arc::new(Self {
            parents: parents.into(),
            order,
        }))
    }

    pub fn joint_count(&self) -> usize {
        self.parents.len()
    }
}

/// Frame-major local translation, rotation and positive scale.
#[derive(Clone, Copy, Debug)]
pub struct LocalPose {
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
}
impl LocalPose {
    /// A local transform with an XYZW quaternion. Clip preparation validates its components.
    pub fn new(translation: [f32; 3], rotation: [f32; 4], scale: [f32; 3]) -> Self {
        Self {
            translation: Vec3::from_array(translation),
            rotation: Quat::from_array(rotation),
            scale: Vec3::from_array(scale),
        }
    }

    fn matrix(self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
    fn blend(self, to: Self, t: f32) -> Self {
        Self {
            translation: self.translation.lerp(to.translation, t),
            rotation: self.rotation.slerp(to.rotation, t),
            scale: self.scale.lerp(to.scale, t),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Marker {
    pub frame: usize,
    pub id: u32,
}

pub struct Clip {
    skeleton: Arc<Skeleton>,
    local: Vec<LocalPose>,
    timeline: ClipTimeline,
}

/// Timing and authored tracks can be evaluated without loading skeletal poses.
#[derive(Clone, Debug)]
pub struct ClipTimeline {
    frames: usize,
    hz: NonZeroU32,
    looping: bool,
    markers: Vec<Marker>,
    sync: Vec<Marker>,
    root: Vec<DVec3>,
}
impl Clip {
    /// Optional root translations are cumulative samples at frame boundaries,
    /// starting at zero and including the loop endpoint. Local poses are in-place.
    pub fn new(
        skeleton: Arc<Skeleton>,
        local: Vec<LocalPose>,
        hz: NonZeroU32,
        looping: bool,
        markers: Vec<Marker>,
        root: Vec<[f64; 3]>,
    ) -> Result<Self, AnimationError> {
        let frames = local.len() / skeleton.joint_count();
        let timeline = ClipTimeline::new(frames, hz, looping, markers, root)?;
        Self::from_timeline(skeleton, local, timeline)
    }

    pub fn from_timeline(
        skeleton: Arc<Skeleton>,
        mut local: Vec<LocalPose>,
        timeline: ClipTimeline,
    ) -> Result<Self, AnimationError> {
        if timeline.frames.checked_mul(skeleton.joint_count()) != Some(local.len()) {
            return Err(Invalid("incomplete pose frame"));
        }
        for p in &mut local {
            if !p.translation.is_finite()
                || !p.scale.is_finite()
                || p.scale.min_element() <= 0.
                || !p.rotation.is_normalized()
            {
                return Err(Invalid("invalid local TRS"));
            }
            p.rotation = p.rotation.normalize();
        }
        Ok(Self {
            skeleton,
            local,
            timeline,
        })
    }
    pub fn timeline(&self) -> &ClipTimeline {
        &self.timeline
    }
    pub fn skeleton(&self) -> &Arc<Skeleton> {
        &self.skeleton
    }
    fn pose(&self, tick: u64, rate: NonZeroU32) -> &[LocalPose] {
        let n = self.timeline.frame(tick, rate);
        let i = if self.timeline.looping {
            n % self.timeline.frames as u128
        } else {
            n.min(self.timeline.frames as u128 - 1)
        };
        let joints = self.skeleton.joint_count();
        let start = i as usize * joints;
        &self.local[start..start + joints]
    }
    pub fn sample_at(&self, tick: u64, rate: NonZeroU32) -> PoseSample<'_> {
        PoseSample {
            skeleton: &self.skeleton,
            local: self.pose(tick, rate),
            blend: None,
        }
    }
    /// Interpolate neighboring local frames at a fixed-clock display time.
    pub fn interpolated_at(&self, tick: u64, rate: NonZeroU32) -> PoseSample<'_> {
        let units = u128::from(tick) * u128::from(self.timeline.hz.get());
        let divisor = u128::from(rate.get());
        let frame = units / divisor;
        let count = self.timeline.frames as u128;
        let index = if self.timeline.looping {
            frame % count
        } else {
            frame.min(count - 1)
        };
        self.frame_sample(index as usize, (units % divisor) as f32 / divisor as f32)
    }

    /// Display frame position relative to the start, useful for synchronized clips.
    pub fn sample_phase(&self, phase: f64) -> Result<PoseSample<'_>, AnimationError> {
        if !(0.0..=1.0).contains(&phase) {
            return Err(Invalid("phase outside [0, 1]"));
        }
        let frame = phase * self.timeline.frames as f64;
        let index = frame as usize;
        if index >= self.timeline.frames {
            return Ok(self.frame_sample(
                if self.timeline.looping {
                    0
                } else {
                    self.timeline.frames - 1
                },
                0.,
            ));
        }
        Ok(self.frame_sample(index, frame.fract() as f32))
    }

    fn frame_sample(&self, index: usize, fraction: f32) -> PoseSample<'_> {
        let joints = self.skeleton.joint_count();
        let frame = |i: usize| &self.local[i * joints..(i + 1) * joints];
        let next = if index + 1 < self.timeline.frames {
            index + 1
        } else if self.timeline.looping {
            0
        } else {
            index
        };
        PoseSample {
            skeleton: &self.skeleton,
            local: frame(index),
            blend: (fraction > 0. && next != index).then(|| (frame(next), fraction)),
        }
    }
    /// Stateless display sampling permits seeking and repeated rendering.
    pub fn evaluate_at(
        &self,
        tick: u64,
        rate: NonZeroU32,
        out: &mut PoseWorkspace,
    ) -> Result<(), AnimationError> {
        self.sample_at(tick, rate).evaluate(out)
    }
}
impl ClipTimeline {
    pub fn new(
        frames: usize,
        hz: NonZeroU32,
        looping: bool,
        mut markers: Vec<Marker>,
        root: Vec<[f64; 3]>,
    ) -> Result<Self, AnimationError> {
        if frames == 0 {
            return Err(Invalid("empty clip timeline"));
        }
        if markers.iter().any(|e| e.frame >= frames) {
            return Err(Invalid("marker outside clip"));
        }
        let root: Vec<_> = root.into_iter().map(DVec3::from_array).collect();
        markers.sort_by_key(|e| e.frame);
        if !root.is_empty()
            && (frames.checked_add(1) != Some(root.len())
                || root[0] != DVec3::ZERO
                || root.iter().any(|v| !v.is_finite()))
        {
            return Err(Invalid("invalid root translation curve"));
        }
        Ok(Self {
            frames,
            hz,
            looping,
            markers,
            sync: Vec::new(),
            root,
        })
    }
    fn frame(&self, tick: u64, rate: NonZeroU32) -> u128 {
        u128::from(tick) * u128::from(self.hz.get()) / u128::from(rate.get())
    }
}

struct JointSlot {
    joint: usize,
    parent: Option<usize>,
}

struct Layout {
    slots: Vec<JointSlot>,
    by_joint: Vec<Option<usize>>,
}

#[derive(Clone)]
pub struct PoseWorkspace {
    skeleton: Arc<Skeleton>,
    layout: Arc<Layout>,
    matrices: Vec<Mat4>,
}
impl PoseWorkspace {
    pub fn all(skeleton: &Arc<Skeleton>) -> Self {
        Self::prepare(skeleton, skeleton.order.iter().copied())
    }
    fn prepare(skeleton: &Arc<Skeleton>, order: impl Iterator<Item = usize>) -> Self {
        let mut by_joint = vec![None; skeleton.joint_count()];
        let slots: Vec<_> = order
            .enumerate()
            .map(|(slot, joint)| {
                let parent =
                    skeleton.parents[joint].map(|p| by_joint[p].expect("parent precedes child"));
                by_joint[joint] = Some(slot);
                JointSlot { joint, parent }
            })
            .collect();
        Self {
            skeleton: skeleton.clone(),
            matrices: vec![Mat4::IDENTITY; slots.len()],
            layout: Arc::new(Layout { slots, by_joint }),
        }
    }
    pub fn subset(skeleton: &Arc<Skeleton>, requested: &[usize]) -> Result<Self, AnimationError> {
        let mut selected = vec![false; skeleton.joint_count()];
        for &joint in requested {
            let mut next = Some(joint);
            while let Some(j) = next {
                let mark = selected
                    .get_mut(j)
                    .ok_or(Invalid("unknown requested joint"))?;
                if *mark {
                    break;
                }
                *mark = true;
                next = skeleton.parents[j];
            }
        }
        Ok(Self::prepare(
            skeleton,
            skeleton.order.iter().copied().filter(|&j| selected[j]),
        ))
    }
    /// Model-space joint transform, in column-major order, when included in this workspace.
    pub fn joint(&self, joint: usize) -> Option<[[f32; 4]; 4]> {
        self.layout
            .by_joint
            .get(joint)?
            .map(|slot| self.matrices[slot].to_cols_array_2d())
    }
    fn evaluate(
        &mut self,
        skeleton: &Arc<Skeleton>,
        local: &[LocalPose],
        target: Option<(&[LocalPose], f32)>,
    ) -> Result<(), AnimationError> {
        if !Arc::ptr_eq(&self.skeleton, skeleton) {
            return Err(Invalid("pose and clip skeletons differ"));
        }
        for (i, slot) in self.layout.slots.iter().enumerate() {
            let j = slot.joint;
            let p = target
                .map_or(local[j], |(to, t)| local[j].blend(to[j], t))
                .matrix();
            self.matrices[i] = slot.parent.map_or(p, |parent| self.matrices[parent] * p);
        }
        Ok(())
    }
}
