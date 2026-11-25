//! Fixed-tick animation. Assets are immutable; playback and pose storage belong to the caller.
use glam::{DMat4, DQuat, DVec3};
use std::{num::NonZeroU32, sync::Arc};

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
    pub translation: DVec3,
    pub rotation: DQuat,
    pub scale: DVec3,
}
impl LocalPose {
    fn matrix(self) -> DMat4 {
        DMat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
    fn blend(self, to: Self, t: f64) -> Self {
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
        mut local: Vec<LocalPose>,
        hz: NonZeroU32,
        looping: bool,
        mut markers: Vec<Marker>,
        root: Vec<DVec3>,
    ) -> Result<Self, AnimationError> {
        let joints = skeleton.joint_count();
        if local.is_empty() || !local.len().is_multiple_of(joints) {
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
        let frames = local.len() / joints;
        if markers.iter().any(|e| e.frame >= frames) {
            return Err(Invalid("marker outside clip"));
        }
        markers.sort_by_key(|e| e.frame);
        if !root.is_empty()
            && (root.len() != frames + 1
                || root[0] != DVec3::ZERO
                || root.iter().any(|v| !v.is_finite()))
        {
            return Err(Invalid("invalid root translation curve"));
        }
        Ok(Self {
            skeleton,
            local,
            frames,
            hz,
            looping,
            markers,
            root,
        })
    }
    pub fn skeleton(&self) -> &Arc<Skeleton> {
        &self.skeleton
    }
    fn frame(&self, tick: u64, rate: NonZeroU32) -> u128 {
        u128::from(tick) * u128::from(self.hz.get()) / u128::from(rate.get())
    }
    fn pose(&self, tick: u64, rate: NonZeroU32) -> &[LocalPose] {
        let n = self.frame(tick, rate);
        let i = if self.looping {
            n % self.frames as u128
        } else {
            n.min(self.frames as u128 - 1)
        };
        let joints = self.skeleton.joint_count();
        let start = i as usize * joints;
        &self.local[start..start + joints]
    }
    /// Stateless display sampling permits seeking and repeated rendering.
    pub fn evaluate_at(
        &self,
        tick: u64,
        rate: NonZeroU32,
        out: &mut PoseWorkspace,
    ) -> Result<(), AnimationError> {
        out.evaluate(self, self.pose(tick, rate), None)
    }
}

pub struct PoseWorkspace {
    skeleton: Arc<Skeleton>,
    order: Vec<usize>,
    matrices: Vec<DMat4>,
    selected: Vec<bool>,
}
impl PoseWorkspace {
    pub fn all(skeleton: &Arc<Skeleton>) -> Self {
        Self {
            skeleton: skeleton.clone(),
            order: skeleton.order.clone(),
            selected: vec![true; skeleton.joint_count()],
            matrices: vec![DMat4::IDENTITY; skeleton.joint_count()],
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
    pub fn joint(&self, joint: usize) -> Option<DMat4> {
        self.selected
            .get(joint)
            .filter(|&&s| s)
            .map(|_| self.matrices[joint])
    }
    fn evaluate(
        &mut self,
        clip: &Clip,
        local: &[LocalPose],
        target: Option<(&[LocalPose], f64)>,
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
