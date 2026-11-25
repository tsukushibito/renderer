//! Linear blend skinning from the final mesh-space joint palette supplied by the host.
use super::*;
mod bounds;
mod compute;
use crate::{pbr::Transform, pipeline::RenderObject};
use bounds::JointBounds;
pub use compute::PreparedComputeSkin;
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
    compute: Option<(PreparedComputeSkin, JointBounds)>,
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
            compute: None,
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
    data: PoseData,
}

enum PoseData {
    Cpu { bytes: Vec<u8>, scratch: Vec<u8> },
    Compute(Vec<Transform>),
}

impl Meshes {
    /// Bytes of posed vertex or palette uploads recorded since the previous observation.
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
        let (data, bounds) = if let Some((_, ranges)) = &skin.compute {
            let palette = vec![Transform::IDENTITY; skin.joints];
            let bounds = ranges.transform(&palette)?;
            (PoseData::Compute(palette), bounds)
        } else {
            let bytes: Vec<_> = skin
                .vertices
                .iter()
                .flat_map(|(vertex, _)| vertex.encode())
                .collect();
            (
                PoseData::Cpu {
                    scratch: vec![0; bytes.len()],
                    bytes,
                },
                layout.bounds,
            )
        };
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
                bounds,
                data,
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
        match &mut pose.data {
            PoseData::Compute(stored) => {
                let (_, bounds) = pose.skin.compute.as_ref().expect("compute pose");
                pose.bounds = bounds.transform(palette)?;
                stored.copy_from_slice(palette);
            }
            PoseData::Cpu { bytes, scratch } => {
                let mut low = [f32::INFINITY; 3];
                let mut high = [f32::NEG_INFINITY; 3];
                for ((vertex, influence), dst) in pose.skin.vertices.iter().zip(
                    scratch
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
                std::mem::swap(scratch, bytes);
            }
        }
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

    pub(crate) fn pose_metadata(
        &self,
        id: SkinPoseId,
        mesh: MeshHandle,
    ) -> Result<(Bounds, u64), GraphError> {
        let asset = self
            .assets
            .get(&mesh)
            .ok_or_else(|| GraphError::invalid_input("unknown skin mesh"))?;
        let pose = self.pose(id, mesh, asset.drawable_layout())?;
        Ok((pose.bounds, pose.revision))
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
                if !seen.insert(id) {
                    continue;
                }
                if let Some((prepared, _)) = &pose.skin.compute {
                    prepared.reserve_pose(prep)?;
                    prep.add_graph_requirements(GraphRequirements {
                        resources: 1,
                        name_bytes: "skin bind".len(),
                        ..Default::default()
                    })?;
                } else if let PoseData::Cpu { bytes, .. } = &pose.data {
                    prep.reserve_buffer(
                        BufferDesc {
                            size: bytes.len() as u64,
                        },
                        1,
                    )?;
                    prep.add_graph_requirements(GraphRequirements {
                        resources: 1,
                        passes: 1,
                        accesses: 1,
                        name_bytes: STREAM.len() + UPLOAD.len(),
                        upload_bytes: bytes.len(),
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
    graph: u64,
}
#[derive(Clone, Copy)]
pub(crate) struct PosedStream {
    mesh: MeshHandle,
    pub(crate) revision: u64,
    pub(crate) vertices: BufferHandle,
    pub(super) bounds: Bounds,
}
impl FramePoses {
    pub(crate) fn reserve(&mut self, poses: usize) {
        self.streams.clear();
        self.streams.reserve(poses);
    }

    pub(crate) fn begin(&mut self, graph: u64) {
        self.graph = graph;
        self.streams.clear();
    }

    pub(crate) fn record(
        &mut self,
        graph: &mut Graph,
        meshes: &Meshes,
        id: SkinPoseId,
        mesh: MeshHandle,
    ) -> Result<PosedStream, GraphError> {
        if self.graph != graph.graph_id() {
            return Err(GraphError::invalid_input(
                "pose requested outside the current frame graph",
            ));
        }
        let asset = meshes
            .assets
            .get(&mesh)
            .ok_or_else(|| GraphError::invalid_input("unknown skin mesh"))?;
        let pose = meshes.pose(id, mesh, asset.drawable_layout())?;
        if let Some(stream) = self.streams.get(&id)
            && stream.revision == pose.revision
            && stream.mesh == mesh
        {
            return Ok(*stream);
        }
        if self.streams.len() == self.streams.capacity() && !self.streams.contains_key(&id) {
            return Err(GraphError::capacity_error(
                GraphCapacityKind::Resources,
                self.streams.capacity(),
                Some(self.streams.len() + 1),
            ));
        }
        let (vertices, _uploaded) = match &pose.data {
            PoseData::Compute(palette) => {
                let bind = asset
                    .active
                    .as_ref()
                    .and_then(|a| a.buffers[3].as_ref())
                    .ok_or_else(|| {
                        GraphError::invalid_input("prepare the compute skin before drawing")
                    })?;
                let bind = graph.import_readonly_buffer("skin bind", bind)?;
                let (prepared, _) = pose.skin.compute.as_ref().expect("compute pose");
                (
                    prepared.add_pose(graph, bind, palette)?,
                    compute::PALETTE_BYTES,
                )
            }
            PoseData::Cpu { bytes, .. } => {
                let vertices = graph.buffer(
                    STREAM,
                    BufferDesc {
                        size: bytes.len() as u64,
                    },
                )?;
                graph.upload_buffer(UPLOAD, vertices, 0, bytes)?;
                (vertices, bytes.len())
            }
        };
        #[cfg(feature = "memory-profiling")]
        meshes
            .recorded_pose_bytes
            .fetch_add(_uploaded as u64, Ordering::Relaxed);
        let stream = PosedStream {
            mesh,
            revision: pose.revision,
            vertices,
            bounds: pose.bounds,
        };
        self.streams.insert(id, stream);
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_bounds_cover_blended_positions_and_failed_update_keeps_palette() {
        let vertices = [[-2., 1., 0.], [5., -3., 0.], [1., 2., 0.]]
            .map(|p| PbrVertex::new(p, [0., 0., 1.], [1., 0., 0., 1.], [0.; 2]).unwrap());
        let weights = [0., 0.25, 1.].map(|w| JointWeights {
            joints: [0, 1, u16::MAX, 2],
            weights: [w, 1. - w, 0., 0.],
        });
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 1,
            retired_mesh_versions: 1,
        })
        .unwrap();
        let data = MeshData::skinned_pbr(&vertices, &[0, 1, 2], 3, &weights)
            .unwrap()
            .with_compute_skin()
            .unwrap();
        let mesh = meshes.create(data).unwrap();
        let id = meshes.create_pose(mesh).unwrap();
        let palette = [
            Transform::new([[-2., 0.5, 0., 3.], [0., 1., 0., -2.], [0., 0., 1., 0.]]).unwrap(),
            Transform::IDENTITY,
            Transform::new([[1., 0., 0., 1e20], [0., 1., 0., 0.], [0., 0., 1., 0.]]).unwrap(),
        ];
        meshes.set_joint_transforms(id, &palette).unwrap();
        let pose = &meshes.poses[&id];
        let bounds = pose.bounds;
        let revision = pose.revision;
        assert!(bounds.max()[0] < 100.);
        for (v, w) in vertices.iter().zip(weights) {
            let p = v
                .skinned(palette.into_iter().zip(w.weights))
                .unwrap()
                .position();
            for (i, x) in p.into_iter().enumerate() {
                assert!((bounds.min()[i]..=bounds.max()[i]).contains(&x));
            }
        }
        let extreme =
            Transform::new([[f32::MAX, 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]]).unwrap();
        assert!(meshes.set_joint_transforms(id, &[extreme; 3]).is_err());
        let pose = &meshes.poses[&id];
        assert_eq!(pose.revision, revision);
        assert_eq!(pose.bounds.min(), bounds.min());
        let PoseData::Compute(stored) = &pose.data else {
            unreachable!()
        };
        assert_eq!(stored.as_slice(), palette);
    }

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
        let PoseData::Cpu { bytes, .. } = &previous.data else {
            unreachable!()
        };
        let (bytes, bounds, revision) = (bytes.clone(), previous.bounds, previous.revision);
        // Only the last vertex overflows. Earlier vertices already touched the scratch buffer.
        let overflow =
            Transform::new([[f32::MAX, 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]]).unwrap();
        assert!(meshes.set_joint_transforms(id, &[]).is_err());
        assert!(meshes.set_joint_transforms(id, &[overflow]).is_err());
        let current = &meshes.poses[&id];
        let PoseData::Cpu {
            bytes: current_bytes,
            ..
        } = &current.data
        else {
            unreachable!()
        };
        assert_eq!(current_bytes, &bytes);
        assert_eq!(current.bounds.min(), bounds.min());
        assert_eq!(current.bounds.max(), bounds.max());
        assert_eq!(current.revision, revision);
        meshes.remove_pose(id);
        let fresh = meshes.create_pose(mesh).unwrap();
        assert_ne!(fresh, id);
        assert!(meshes.set_joint_transforms(id, &[translated]).is_err());
    }
}
