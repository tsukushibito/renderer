//! Fixed-tick animation. Assets are immutable; playback and pose storage belong to the caller.
use glam::{DVec3, Mat4, Quat, Vec3};
use std::{num::NonZeroU32, sync::Arc};

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
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}
impl LocalPose {
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
        root: Vec<DVec3>,
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
            clip: self,
            local: self.pose(tick, rate),
            blend: None,
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
        root: Vec<DVec3>,
    ) -> Result<Self, AnimationError> {
        if frames == 0 {
            return Err(Invalid("empty clip timeline"));
        }
        if markers.iter().any(|e| e.frame >= frames) {
            return Err(Invalid("marker outside clip"));
        }
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
            root,
        })
    }
    fn frame(&self, tick: u64, rate: NonZeroU32) -> u128 {
        u128::from(tick) * u128::from(self.hz.get()) / u128::from(rate.get())
    }
}

#[derive(Clone)]
pub struct PoseWorkspace {
    skeleton: Arc<Skeleton>,
    order: Vec<usize>,
    matrices: Vec<Mat4>,
    selected: Vec<bool>,
}
impl PoseWorkspace {
    pub fn all(skeleton: &Arc<Skeleton>) -> Self {
        Self {
            skeleton: skeleton.clone(),
            order: skeleton.order.clone(),
            selected: vec![true; skeleton.joint_count()],
            matrices: vec![Mat4::IDENTITY; skeleton.joint_count()],
        }
    }
    pub fn subset(skeleton: &Arc<Skeleton>, requested: &[usize]) -> Result<Self, AnimationError> {
        let mut out = Self::all(skeleton);
        out.selected.fill(false);
        for &i in requested {
            let mut next = Some(i);
            while let Some(j) = next {
                let selected = out
                    .selected
                    .get_mut(j)
                    .ok_or(Invalid("unknown requested joint"))?;
                if *selected {
                    break;
                }
                *selected = true;
                next = skeleton.parents[j];
            }
        }
        out.order.retain(|&j| out.selected[j]);
        Ok(out)
    }
    pub fn joint(&self, joint: usize) -> Option<Mat4> {
        self.selected
            .get(joint)
            .filter(|&&s| s)
            .map(|_| self.matrices[joint])
    }
    fn evaluate(
        &mut self,
        clip: &Clip,
        local: &[LocalPose],
        target: Option<(&[LocalPose], f32)>,
    ) -> Result<(), AnimationError> {
        if !Arc::ptr_eq(&self.skeleton, &clip.skeleton) {
            return Err(Invalid("pose and clip skeletons differ"));
        }
        for &i in &self.order {
            let p = target
                .map_or(local[i], |(to, t)| local[i].blend(to[i], t))
                .matrix();
            self.matrices[i] =
                self.skeleton.parents[i].map_or(p, |parent| self.matrices[parent] * p);
        }
        Ok(())
    }
}
