//! Four independent poses of one skeleton per vector. Parent order is shared by every lane.
use super::*;
use glam::Vec4;

/// Read-only display sample. Resolving or evaluating it does not advance simulation time.
#[derive(Clone, Copy)]
pub struct PoseSample<'a> {
    pub(super) clip: &'a Clip,
    pub(super) local: &'a [LocalPose],
    pub(super) blend: Option<(&'a [LocalPose], f32)>,
}

impl PoseSample<'_> {
    pub fn evaluate(self, out: &mut PoseWorkspace) -> Result<(), AnimationError> {
        out.evaluate(self.clip, self.local, self.blend)
    }
}

/// Prepare once for a fixed upper bound and joint selection. Successful
/// evaluation returns exactly one output per request, in input order.
pub struct PoseBatch {
    skeleton: Arc<Skeleton>,
    outputs: Vec<PoseWorkspace>,
    world: Vec<Affine4>,
}
impl PoseBatch {
    pub fn new(layout: &PoseWorkspace, capacity: usize) -> Self {
        Self {
            skeleton: layout.skeleton.clone(),
            outputs: (0..capacity).map(|_| layout.clone()).collect(),
            world: if capacity >= 4 {
                vec![Affine4::default(); layout.matrices.len()]
            } else {
                Vec::new()
            },
        }
    }
    pub fn evaluate(
        &mut self,
        requests: &[PoseSample<'_>],
    ) -> Result<&[PoseWorkspace], AnimationError> {
        self.validate(requests)?;
        let outputs = &mut self.outputs[..requests.len()];
        let (groups, tail) = requests.as_chunks::<4>();
        let (dest, remainder) = outputs.as_chunks_mut::<4>();
        for (group, lanes) in groups.iter().zip(dest) {
            for &i in &lanes[0].order {
                let local = Affine4::from_joint(group, i);
                self.world[i] =
                    self.skeleton.parents[i].map_or(local, |p| self.world[p].compose(local));
                let matrices = self.world[i].unpack();
                // The evaluation order only borrows lane zero's order, not its matrices.
                lanes[0].matrices[i] = matrices[0];
                lanes[1].matrices[i] = matrices[1];
                lanes[2].matrices[i] = matrices[2];
                lanes[3].matrices[i] = matrices[3];
            }
        }
        for (sample, out) in tail.iter().zip(remainder) {
            sample.evaluate(out)?;
        }
        Ok(outputs)
    }
    fn validate(&self, requests: &[PoseSample<'_>]) -> Result<(), AnimationError> {
        if requests.len() > self.outputs.len() {
            return Err(Invalid("pose batch capacity exceeded"));
        }
        if requests
            .iter()
            .any(|r| !Arc::ptr_eq(&r.clip.skeleton, &self.skeleton))
        {
            return Err(Invalid("incompatible batch skeleton"));
        }
        Ok(())
    }

    /// Evaluate independent four-pose SIMD groups on a borrowed job scope.
    /// Only these pose jobs must complete before the output can be consumed.
    pub fn evaluate_scoped(
        &mut self,
        requests: &[PoseSample<'_>],
        jobs: &sashiko_jobs::JobScope<'_, '_>,
    ) -> Result<&[PoseWorkspace], AnimationError> {
        let joints = self.outputs.first().map_or(0, |pose| pose.order.len());
        if jobs.parallelism().get() == 1
            || requests.len() < 8
            || requests.len().saturating_mul(joints) <= 128
        {
            return self.evaluate(requests);
        }
        self.validate(requests)?;
        let outputs = &mut self.outputs[..requests.len()];
        let (groups, tail) = requests.as_chunks::<4>();
        let (dest, remainder) = outputs.as_chunks_mut::<4>();
        let rig = &self.skeleton;
        jobs.scope(|s| {
            s.for_each_chunk_mut(dest, std::num::NonZeroUsize::MIN, |i, part| {
                evaluate_independent_group(rig, &groups[i], &mut part[0]);
            });
            for (sample, out) in tail.iter().zip(remainder) {
                sample.evaluate(out)?;
            }
            Ok(())
        })?;
        Ok(outputs)
    }
}

// A packed affine fits in three of the four lane matrices. Keep intermediate
// worlds there until all parents have been read, then unpack in place. Each
// group owns its scratch even when stolen, without per-group heap allocations.
fn evaluate_independent_group(
    rig: &Skeleton,
    group: &[PoseSample<'_>; 4],
    lanes: &mut [PoseWorkspace; 4],
) {
    for &i in &lanes[0].order {
        let local = Affine4::from_joint(group, i);
        let world = rig.parents[i].map_or(local, |p| {
            let rows = std::array::from_fn(|r: usize| {
                let m = lanes[r].matrices[p];
                [m.x_axis, m.y_axis, m.z_axis, m.w_axis]
            });
            Affine4(rows).compose(local)
        });
        for (r, [x, y, z, w]) in world.0.into_iter().enumerate() {
            lanes[r].matrices[i] = Mat4::from_cols(x, y, z, w);
        }
    }
    for &i in &lanes[0].order {
        let packed = Affine4(std::array::from_fn(|r: usize| {
            let m = lanes[r].matrices[i];
            [m.x_axis, m.y_axis, m.z_axis, m.w_axis]
        }));
        let [a, b, c, d] = packed.unpack();
        lanes[0].matrices[i] = a;
        lanes[1].matrices[i] = b;
        lanes[2].matrices[i] = c;
        lanes[3].matrices[i] = d;
    }
}

// Rows of a 3x4 affine matrix. Each coefficient holds the same component of four poses.
#[derive(Clone, Copy, Default)]
struct Affine4([[Vec4; 4]; 3]);

impl Affine4 {
    #[inline]
    fn from_joint(samples: &[PoseSample<'_>; 4], i: usize) -> Self {
        let source = samples.map(|s| s.local[i]);
        let gather = |field: fn(LocalPose) -> f32| Vec4::from_array(source.map(field));
        let mut q = [
            gather(|p| p.rotation.x),
            gather(|p| p.rotation.y),
            gather(|p| p.rotation.z),
            gather(|p| p.rotation.w),
        ];
        let mut scale = [
            gather(|p| p.scale.x),
            gather(|p| p.scale.y),
            gather(|p| p.scale.z),
        ];
        let mut pos = [
            gather(|p| p.translation.x),
            gather(|p| p.translation.y),
            gather(|p| p.translation.z),
        ];
        if samples.iter().any(|s| s.blend.is_some()) {
            let ends = std::array::from_fn::<_, 4, _>(|lane| {
                samples[lane].blend.map_or(source[lane], |(to, _)| to[i])
            });
            let weight = Vec4::from_array(samples.map(|s| s.blend.map_or(0., |(_, t)| t)));
            for axis in 0..3 {
                let end_scale = Vec4::from_array(ends.map(|p| p.scale[axis]));
                let end_pos = Vec4::from_array(ends.map(|p| p.translation[axis]));
                scale[axis] += (end_scale - scale[axis]) * weight;
                pos[axis] += (end_pos - pos[axis]) * weight;
            }
            // Compute shortest-arc weights per lane; all quaternion components
            // and the hierarchy below are combined with packed arithmetic.
            let weights = std::array::from_fn::<_, 4, _>(|lane| {
                let dot = source[lane].rotation.dot(ends[lane].rotation);
                let t = weight[lane];
                let (a, b) = if dot.abs() >= 1. - f32::EPSILON {
                    (1. - t, t)
                } else {
                    let angle = dot.abs().acos();
                    let inv = angle.sin().recip();
                    (((1. - t) * angle).sin() * inv, (t * angle).sin() * inv)
                };
                (a, if dot < 0. { -b } else { b })
            });
            let a = Vec4::from_array(weights.map(|w| w.0));
            let b = Vec4::from_array(weights.map(|w| w.1));
            for (axis, v) in q.iter_mut().enumerate() {
                *v = *v * a + Vec4::from_array(ends.map(|p| p.rotation.to_array()[axis])) * b;
            }
        }
        let [x, y, z, w] = q;
        let [sx, sy, sz] = scale;
        let x2 = x + x;
        let y2 = y + y;
        let z2 = z + z;
        let one = Vec4::ONE;
        Self([
            [
                (one - (y * y2 + z * z2)) * sx,
                (x * y2 - w * z2) * sy,
                (x * z2 + w * y2) * sz,
                pos[0],
            ],
            [
                (x * y2 + w * z2) * sx,
                (one - (x * x2 + z * z2)) * sy,
                (y * z2 - w * x2) * sz,
                pos[1],
            ],
            [
                (x * z2 - w * y2) * sx,
                (y * z2 + w * x2) * sy,
                (one - (x * x2 + y * y2)) * sz,
                pos[2],
            ],
        ])
    }
    #[inline]
    fn compose(self, child: Self) -> Self {
        Self(self.0.map(|[x, y, z, t]| {
            let mut row =
                std::array::from_fn(|i| x * child.0[0][i] + y * child.0[1][i] + z * child.0[2][i]);
            row[3] += t;
            row
        }))
    }
    #[inline]
    fn unpack(self) -> [Mat4; 4] {
        let columns: [Mat4; 4] = std::array::from_fn(|i| {
            Mat4::from_cols(
                self.0[0][i],
                self.0[1][i],
                self.0[2][i],
                if i == 3 { Vec4::ONE } else { Vec4::ZERO },
            )
            .transpose()
        });
        std::array::from_fn(|lane| {
            Mat4::from_cols(
                columns[0].col(lane),
                columns[1].col(lane),
                columns[2].col(lane),
                columns[3].col(lane),
            )
        })
    }
}
