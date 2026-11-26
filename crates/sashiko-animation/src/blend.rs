//! Prepared blend coordinates and reusable, parent-relative pose composition.
use super::*;
use glam::DVec2;

/// Ordered sample coordinates for a clamped one-dimensional blend.
pub struct BlendLine(Vec<f32>);
impl BlendLine {
    pub fn new(points: Vec<f32>) -> Result<Self, AnimationError> {
        if points.len() < 2
            || points.iter().any(|x| !x.is_finite())
            || points.windows(2).any(|p| p[0] >= p[1])
        {
            return Err(Invalid("expected increasing blend coordinates"));
        }
        Ok(Self(points))
    }
    pub fn weights(&self, x: f32) -> Result<[(usize, f32); 2], AnimationError> {
        if !x.is_finite() {
            return Err(Invalid("invalid blend coordinate"));
        }
        let hi = self
            .0
            .partition_point(|p| *p <= x)
            .clamp(1, self.0.len() - 1);
        let lo = f64::from(self.0[hi - 1]);
        let t = ((f64::from(x) - lo) / (f64::from(self.0[hi]) - lo)).clamp(0., 1.) as f32;
        Ok([(hi - 1, 1. - t), (hi, t)])
    }
}

/// Triangles refer to the caller's sample order. Outside the mesh, use its closest edge.
pub struct BlendSpace {
    points: Vec<DVec2>,
    triangles: Vec<[usize; 3]>,
}
impl BlendSpace {
    pub fn new(points: Vec<[f32; 2]>, triangles: Vec<[usize; 3]>) -> Result<Self, AnimationError> {
        let points: Vec<_> = points
            .into_iter()
            .map(|p| glam::Vec2::from_array(p).as_dvec2())
            .collect();
        if points.iter().any(|p| !p.is_finite()) || triangles.is_empty() {
            return Err(Invalid("invalid blend mesh"));
        }
        for tri in &triangles {
            if tri.iter().any(|&i| i >= points.len()) {
                return Err(Invalid("unknown blend vertex"));
            }
            let [a, b, c] = tri.map(|i| points[i]);
            if (b - a).perp_dot(c - a) == 0. {
                return Err(Invalid("degenerate blend triangle"));
            }
        }
        Ok(Self { points, triangles })
    }
    pub fn weights(&self, position: [f32; 2]) -> Result<[(usize, f32); 3], AnimationError> {
        let p = glam::Vec2::from_array(position).as_dvec2();
        if !p.is_finite() {
            return Err(Invalid("invalid blend coordinate"));
        }
        let mut distance = f64::INFINITY;
        let mut closest = [(0, 0.); 3];
        for &tri in &self.triangles {
            let [a, b, c] = tri.map(|i| self.points[i]);
            let area = (b - a).perp_dot(c - a);
            let v = (p - a).perp_dot(c - a) / area;
            let w = (b - a).perp_dot(p - a) / area;
            let u = 1. - v - w;
            if u.min(v).min(w) >= 0. {
                return Ok([(tri[0], u as f32), (tri[1], v as f32), (tri[2], w as f32)]);
            }
            for [i, j, k] in [[0, 1, 2], [1, 2, 0], [2, 0, 1]] {
                let start = self.points[tri[i]];
                let edge = self.points[tri[j]] - start;
                let t = ((p - start).dot(edge) / edge.length_squared()).clamp(0., 1.);
                let d = p.distance_squared(start + edge * t);
                if d < distance {
                    distance = d;
                    closest = [(tri[i], (1. - t) as f32), (tri[j], t as f32), (tri[k], 0.)];
                }
            }
        }
        Ok(closest)
    }
}

/// Per-joint layer strengths in skeleton index order.
pub struct JointMask {
    rig: Arc<Skeleton>,
    weights: Vec<f32>,
}
impl JointMask {
    pub fn skeleton(&self) -> &Arc<Skeleton> {
        &self.rig
    }

    pub fn new(rig: &Arc<Skeleton>, weights: Vec<f32>) -> Result<Self, AnimationError> {
        if weights.len() != rig.joint_count() || weights.iter().any(|w| !(0.0..=1.0).contains(w)) {
            return Err(Invalid("invalid joint mask"));
        }
        Ok(Self {
            rig: rig.clone(),
            weights,
        })
    }
}

impl PoseSample<'_> {
    fn joint_local(self, joint: usize) -> LocalPose {
        let p = self.local[joint];
        self.blend.map_or(p, |(to, t)| p.blend(to[joint], t))
    }
}

/// Full local pose retained for layering or submission to a pose/palette batch.
pub struct LocalBuffer {
    rig: Arc<Skeleton>,
    poses: Vec<LocalPose>,
    scratch: Vec<LocalPose>,
}
impl LocalBuffer {
    pub fn new(rig: &Arc<Skeleton>) -> Self {
        let poses = vec![
            LocalPose {
                scale: Vec3::ONE,
                rotation: Quat::IDENTITY,
                translation: Vec3::ZERO
            };
            rig.joint_count()
        ];
        Self {
            rig: rig.clone(),
            scratch: poses.clone(),
            poses,
        }
    }
    pub fn sample(&self) -> PoseSample<'_> {
        PoseSample {
            skeleton: &self.rig,
            local: &self.poses,
            blend: None,
        }
    }
    /// Normalize nonnegative weights. Combine rotations by shortest arcs in input order.
    pub fn mix(&mut self, inputs: &[(PoseSample<'_>, f32)]) -> Result<(), AnimationError> {
        let mut total = 0f64;
        for &(s, w) in inputs {
            if !Arc::ptr_eq(&self.rig, s.skeleton) || !w.is_finite() || w < 0. {
                return Err(Invalid("invalid blend input"));
            }
            total += f64::from(w);
        }
        if total == 0. {
            return Err(Invalid("blend needs a positive weight"));
        }
        let mut accumulated = 0f64;
        for &(s, w) in inputs.iter().filter(|(_, w)| *w > 0.) {
            accumulated += f64::from(w);
            let t = (f64::from(w) / accumulated) as f32;
            for (j, p) in self.poses.iter_mut().enumerate() {
                let next = s.joint_local(j);
                *p = if accumulated == f64::from(w) {
                    next
                } else {
                    p.blend(next, t)
                };
            }
        }
        Ok(())
    }
    fn check_layer(
        &self,
        pose: PoseSample<'_>,
        mask: &JointMask,
        strength: f32,
    ) -> Result<(), AnimationError> {
        if !Arc::ptr_eq(&self.rig, pose.skeleton)
            || !Arc::ptr_eq(&self.rig, &mask.rig)
            || !(0.0..=1.0).contains(&strength)
        {
            return Err(Invalid("incompatible layer"));
        }
        Ok(())
    }
    /// Override the current local pose by the masked sample.
    pub fn overlay(
        &mut self,
        pose: PoseSample<'_>,
        mask: &JointMask,
        strength: f32,
    ) -> Result<(), AnimationError> {
        self.check_layer(pose, mask, strength)?;
        for (j, p) in self.poses.iter_mut().enumerate() {
            let w = mask.weights[j] * strength;
            if w > 0. {
                *p = p.blend(pose.joint_local(j), w);
            }
        }
        Ok(())
    }
    /// Apply a reference-relative local layer. An unrepresentable result preserves the previous pose.
    pub fn additive(
        &mut self,
        pose: PoseSample<'_>,
        reference: PoseSample<'_>,
        mask: &JointMask,
        strength: f32,
    ) -> Result<(), AnimationError> {
        self.check_layer(pose, mask, strength)?;
        self.check_layer(reference, mask, strength)?;
        for (j, &base) in self.poses.iter().enumerate() {
            let w = mask.weights[j] * strength;
            if w == 0. {
                self.scratch[j] = base;
                continue;
            }
            let a = pose.joint_local(j);
            let r = reference.joint_local(j);
            let t = f64::from(w);
            let scale = (base.scale.as_dvec3()
                * DVec3::ONE.lerp(a.scale.as_dvec3() / r.scale.as_dvec3(), t))
            .as_vec3();
            let translation = (base.translation.as_dvec3()
                + (a.translation.as_dvec3() - r.translation.as_dvec3()) * t)
                .as_vec3();
            if !scale.is_finite() || scale.min_element() <= 0. || !translation.is_finite() {
                return Err(Invalid("additive pose outside representable range"));
            }
            let rotation = (base.rotation
                * Quat::IDENTITY.slerp((r.rotation.conjugate() * a.rotation).normalize(), w))
            .normalize();
            self.scratch[j] = LocalPose {
                scale,
                translation,
                rotation,
            };
        }
        std::mem::swap(&mut self.poses, &mut self.scratch);
        Ok(())
    }
}
