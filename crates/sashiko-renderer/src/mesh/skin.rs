//! Linear blend skinning from the final mesh-space joint palette supplied by the host.
use super::*;
use crate::{pbr::Transform, pipeline::RenderObject};
use sashiko_renderer_core::graph::{BufferHandle, GraphCapacityKind};
use std::{
    collections::HashSet,
    sync::atomic::{AtomicU64, Ordering},
};

const STREAM: &str = "posed vertices";
const UPLOAD: &str = "upload posed vertices";

#[derive(Clone, Copy)]
pub struct JointWeights {
    pub joints: [u16; 4],
    pub weights: [f32; 4],
}

pub(super) struct Skin {
    joints: usize,
    vertices: Vec<(PbrVertex, JointWeights)>,
}

impl MeshData {
    /// Bind positions and weights share vertex order. The host supplies its
    /// final joint matrices (including inverse bind transforms) at pose update.
    pub fn skinned_pbr(
        vertices: &[PbrVertex],
        indices: &[u32],
        joint_count: usize,
        influences: &[JointWeights],
    ) -> Result<Self, GraphError> {
        if influences.len() != vertices.len() || !(1..=65536).contains(&joint_count) {
            return Err(GraphError::invalid_input(
                "incompatible skin joint or vertex count",
            ));
        }
        let mut data = Self::pbr(vertices, indices)?;
        let mut weighted = Vec::with_capacity(vertices.len());
        for (&vertex, &influence) in vertices.iter().zip(influences) {
            let total = influence.weights.into_iter().map(f64::from).sum::<f64>();
            if !total.is_finite()
                || total <= 0.
                || influence
                    .weights
                    .iter()
                    .zip(influence.joints)
                    .any(|(&w, j)| w < 0. || (w > 0. && usize::from(j) >= joint_count))
            {
                return Err(GraphError::invalid_input(
                    "invalid skin joint indices or weights",
                ));
            }
            let weights = influence.weights.map(|w| (f64::from(w) / total) as f32);
            weighted.push((
                vertex,
                JointWeights {
                    weights,
                    ..influence
                },
            ));
        }
        data.skin = Some(Arc::new(Skin {
            joints: joint_count,
            vertices: weighted,
        }));
        Ok(data)
    }
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub struct SkinPoseId(u64);

pub(super) struct Pose {
    pub(super) mesh: MeshHandle,
    skin: Arc<Skin>,
    revision: u64,
    bounds: Bounds,
    bytes: Vec<u8>,
    scratch: Vec<u8>,
}

impl Meshes {
    /// Bytes of posed vertex uploads recorded since the previous observation.
    #[cfg(feature = "memory-profiling")]
    pub fn take_recorded_pose_bytes(&self) -> u64 {
        self.recorded_pose_bytes.swap(0, Ordering::Relaxed)
    }
    pub(crate) fn pose_count(&self) -> usize {
        self.poses.len()
    }

    /// Allocate one independently animated instance of a registered skin at load time.
    pub fn create_pose(&mut self, mesh: MeshHandle) -> Result<SkinPoseId, GraphError> {
        let layout = &self
            .assets
            .get(&mesh)
            .ok_or_else(|| GraphError::invalid_input("unknown skin mesh"))?
            .layout;
        let skin = layout
            .skin
            .as_ref()
            .ok_or_else(|| GraphError::invalid_input("mesh has no skin"))?;
        let bytes: Vec<_> = skin
            .vertices
            .iter()
            .flat_map(|(vertex, _)| vertex.encode())
            .collect();
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = SkinPoseId(
            NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .expect("skin pose IDs exhausted"),
        );
        self.poses.insert(
            id,
            Pose {
                mesh,
                skin: Arc::clone(skin),
                revision: 1,
                bounds: layout.bounds,
                scratch: vec![0; bytes.len()],
                bytes,
            },
        );
        Ok(id)
    }

    pub fn remove_pose(&mut self, id: SkinPoseId) -> bool {
        self.poses.remove(&id).is_some()
    }

    /// Deform a pose once for all views, submeshes and shadow passes. Failed
    /// updates leave the previous vertex stream and its bounds available.
    pub fn set_joint_transforms(
        &mut self,
        id: SkinPoseId,
        palette: &[Transform],
    ) -> Result<(), GraphError> {
        let pose = self
            .poses
            .get_mut(&id)
            .ok_or_else(|| GraphError::invalid_input("unknown skin pose"))?;
        if palette.len() != pose.skin.joints {
            return Err(GraphError::invalid_input(
                "joint palette size differs from the skin",
            ));
        }
        let mut low = [f32::INFINITY; 3];
        let mut high = [f32::NEG_INFINITY; 3];
        for ((vertex, influence), dst) in pose.skin.vertices.iter().zip(
            pose.scratch
                .as_chunks_mut::<{ crate::passes::pbr::VERTEX_BYTES }>()
                .0,
        ) {
            let joints = influence
                .joints
                .into_iter()
                .zip(influence.weights)
                .filter(|(_, w)| *w > 0.)
                .map(|(i, w)| (palette[usize::from(i)], w));
            let result = vertex.skinned(joints)?;
            for ((min, max), v) in low.iter_mut().zip(&mut high).zip(result.position()) {
                *min = min.min(v);
                *max = max.max(v);
            }
            *dst = result.encode();
        }
        pose.bounds = Bounds::new(low, high)?;
        std::mem::swap(&mut pose.scratch, &mut pose.bytes);
        pose.revision = pose
            .revision
            .checked_add(1)
            .expect("pose revisions exhausted");
        Ok(())
    }

    fn pose(&self, id: SkinPoseId, mesh: MeshHandle, layout: &Layout) -> Result<&Pose, GraphError> {
        let pose = self
            .poses
            .get(&id)
            .ok_or_else(|| GraphError::invalid_input("unknown skin pose"))?;
        if pose.mesh != mesh
            || !layout
                .skin
                .as_ref()
                .is_some_and(|skin| Arc::ptr_eq(skin, &pose.skin))
        {
            return Err(GraphError::invalid_input(
                "pose belongs to a different mesh or skin revision",
            ));
        }
        Ok(pose)
    }

    pub(crate) fn reserve_poses(
        &self,
        prep: &mut Preparation,
        objects: &[RenderObject],
    ) -> Result<(), GraphError> {
        let mut seen = HashSet::new();
        for object in objects {
            if let Some(id) = object.pose {
                let asset = self
                    .assets
                    .get(&object.mesh)
                    .ok_or_else(|| GraphError::invalid_input("unknown skin mesh"))?;
                let pose = self.pose(id, object.mesh, &asset.layout)?;
                if seen.insert(id) {
                    prep.reserve_buffer(
                        BufferDesc {
                            size: pose.bytes.len() as u64,
                        },
                        1,
                    )?;
                    prep.add_graph_requirements(GraphRequirements {
                        resources: 1,
                        passes: 1,
                        accesses: 1,
                        name_bytes: STREAM.len() + UPLOAD.len(),
                        upload_bytes: pose.bytes.len(),
                        ..Default::default()
                    })?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct FramePoses {
    streams: HashMap<SkinPoseId, PosedStream>,
}
#[derive(Clone, Copy)]
pub(crate) struct PosedStream {
    mesh: MeshHandle,
    pub(crate) revision: u64,
    pub(super) vertices: BufferHandle,
    pub(super) bounds: Bounds,
}
impl FramePoses {
    pub(crate) fn reserve(&mut self, poses: usize) {
        self.streams.clear();
        self.streams.reserve(poses);
    }

    pub(crate) fn clear(&mut self) {
        self.streams.clear();
    }

    pub(crate) fn record(
        &mut self,
        graph: &mut Graph,
        meshes: &Meshes,
        objects: &[RenderObject],
    ) -> Result<(), GraphError> {
        if meshes.poses.is_empty() && self.streams.is_empty() {
            return Ok(());
        }
        for object in objects {
            let Some(id) = object.pose else { continue };
            let asset = meshes
                .assets
                .get(&object.mesh)
                .ok_or_else(|| GraphError::invalid_input("unknown skin mesh"))?;
            let pose = meshes.pose(id, object.mesh, asset.drawable_layout())?;
            if self
                .streams
                .get(&id)
                .is_some_and(|stream| stream.revision == pose.revision)
            {
                continue;
            }
            if self.streams.len() == self.streams.capacity() && !self.streams.contains_key(&id) {
                return Err(GraphError::capacity_error(
                    GraphCapacityKind::Resources,
                    self.streams.capacity(),
                    Some(self.streams.len() + 1),
                ));
            }
            let vertices = graph.buffer(
                STREAM,
                BufferDesc {
                    size: pose.bytes.len() as u64,
                },
            )?;
            graph.upload_buffer(UPLOAD, vertices, 0, &pose.bytes)?;
            #[cfg(feature = "memory-profiling")]
            meshes
                .recorded_pose_bytes
                .fetch_add(pose.bytes.len() as u64, Ordering::Relaxed);
            self.streams.insert(
                id,
                PosedStream {
                    mesh: object.mesh,
                    revision: pose.revision,
                    vertices,
                    bounds: pose.bounds,
                },
            );
        }
        Ok(())
    }

    pub(crate) fn stream(
        &self,
        id: SkinPoseId,
        mesh: MeshHandle,
    ) -> Result<PosedStream, GraphError> {
        self.streams
            .get(&id)
            .filter(|stream| stream.mesh == mesh)
            .copied()
            .ok_or_else(|| GraphError::invalid_input("pose has not been recorded for this frame"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_weights_and_preserves_committed_pose_on_update_failure() {
        let vertices = [0., 1., 2.]
            .map(|x| PbrVertex::new([x, 0., 0.], [0., 0., 1.], [0., 1., 0., 1.], [0.; 2]).unwrap());
        let weight = JointWeights {
            joints: [0; 4],
            weights: [1., 0., 0., 0.],
        };
        for invalid in [
            JointWeights {
                weights: [f32::NAN; 4],
                ..weight
            },
            JointWeights {
                joints: [1; 4],
                ..weight
            },
            JointWeights {
                weights: [0.; 4],
                ..weight
            },
        ] {
            assert!(MeshData::skinned_pbr(&vertices, &[0, 1, 2], 1, &[invalid; 3]).is_err());
        }
        let data = MeshData::skinned_pbr(&vertices, &[0, 1, 2], 1, &[weight; 3]).unwrap();
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 1,
            retired_mesh_versions: 1,
        })
        .unwrap();
        let mesh = meshes.create(data).unwrap();
        let id = meshes.create_pose(mesh).unwrap();
        let translated =
            Transform::new([[1., 0., 0., 4.], [0., 1., 0., 0.], [0., 0., 1., 0.]]).unwrap();
        meshes.set_joint_transforms(id, &[translated]).unwrap();
        let previous = &meshes.poses[&id];
        let (bytes, bounds, revision) =
            (previous.bytes.clone(), previous.bounds, previous.revision);
        // Only the last vertex overflows. Earlier vertices already touched the scratch buffer.
        let overflow =
            Transform::new([[f32::MAX, 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]]).unwrap();
        assert!(meshes.set_joint_transforms(id, &[]).is_err());
        assert!(meshes.set_joint_transforms(id, &[overflow]).is_err());
        let current = &meshes.poses[&id];
        assert_eq!(current.bytes, bytes);
        assert_eq!(current.bounds.min(), bounds.min());
        assert_eq!(current.bounds.max(), bounds.max());
        assert_eq!(current.revision, revision);
        meshes.remove_pose(id);
        let fresh = meshes.create_pose(mesh).unwrap();
        assert_ne!(fresh, id);
        assert!(meshes.set_joint_transforms(id, &[translated]).is_err());
    }
}
