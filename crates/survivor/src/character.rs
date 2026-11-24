//! Cooked character presentation. The simulation owns the subject's identity and rules.

use renderer::{
    mesh::{JointWeights, MeshData},
    pbr::{PbrVertex, Transform},
    pipeline::PipelineResult,
};

const PRIMARY: &[u8] = include_bytes!("../../../assets/survivor/characters/primary.skin");
const ALTERNATE: &[u8] = include_bytes!("../../../assets/survivor/characters/alternate.skin");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharacterVariant {
    Primary,
    Alternate,
}

impl CharacterVariant {
    /// Tightly packed, top-row-first 256 × 256 sRGB RGBA pixels.
    pub fn base_rgba(self) -> &'static [u8] {
        match self {
            Self::Primary => include_bytes!("../../../assets/survivor/characters/primary.rgba"),
            Self::Alternate => include_bytes!("../../../assets/survivor/characters/alternate.rgba"),
        }
    }
}

pub struct Character {
    vertices: Vec<PbrVertex>,
    weights: Vec<JointWeights>,
    indices: Vec<u32>,
    joints: usize,
    frames: usize,
    poses: Vec<Transform>,
}

fn record_bytes<'a>(input: &mut &'a [u8], count: usize, stride: usize) -> PipelineResult<&'a [u8]> {
    let size = count.checked_mul(stride).ok_or("character size overflow")?;
    let (records, next) = input.split_at_checked(size).ok_or("short character data")?;
    *input = next;
    Ok(records)
}

fn floats<const N: usize>(record: &[u8], offset: usize) -> [f32; N] {
    std::array::from_fn(|i| {
        let start = offset + i * 4;
        f32::from_le_bytes(record[start..start + 4].try_into().unwrap())
    })
}

impl Character {
    pub fn load(variant: CharacterVariant) -> PipelineResult<Self> {
        let input = match variant {
            CharacterVariant::Primary => PRIMARY,
            CharacterVariant::Alternate => ALTERNATE,
        };
        let mut data = input.strip_prefix(b"SURSKIN1").ok_or("character schema")?;
        let header = record_bytes(&mut data, 4, 4)?;
        let [points, elements, joints, frames] = std::array::from_fn(|i| {
            u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap()) as usize
        });
        if joints == 0 || frames == 0 {
            return Err("empty walk clip".into());
        }
        let vertex_bytes = record_bytes(&mut data, points, 72)?;
        let index_bytes = record_bytes(&mut data, elements, 4)?;
        let all_joints = frames.checked_mul(joints).ok_or("walk clip overflow")?;
        let matrices = record_bytes(&mut data, all_joints, 48)?;
        if !data.is_empty() {
            return Err("trailing character data".into());
        }
        let mut vertices = Vec::with_capacity(points);
        let mut weights = Vec::with_capacity(points);
        for v in vertex_bytes.as_chunks::<72>().0 {
            vertices.push(PbrVertex::new(
                floats(v, 0),
                floats(v, 12),
                floats(v, 24),
                floats(v, 40),
            )?);
            weights.push(JointWeights {
                joints: std::array::from_fn(|i| u16::from_le_bytes([v[48 + 2 * i], v[49 + 2 * i]])),
                weights: floats(v, 56),
            });
        }
        let indices = index_bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|x| u32::from_le_bytes(*x))
            .collect();
        let mut poses = Vec::with_capacity(all_joints);
        for matrix in matrices.as_chunks::<48>().0 {
            poses.push(Transform::new(std::array::from_fn(|i| {
                floats(matrix, i * 16)
            }))?);
        }
        Ok(Self {
            vertices,
            indices,
            weights,
            joints,
            frames,
            poses,
        })
    }

    pub fn mesh_data(&self) -> PipelineResult<MeshData> {
        Ok(MeshData::skinned_pbr(
            &self.vertices,
            &self.indices,
            self.joints,
            &self.weights,
        )?)
    }

    /// One loop of the authored walk animation, sampled at the fixed 60 Hz game rate.
    pub fn palette(&self, tick: u64) -> &[Transform] {
        let start = (tick % self.frames as u64) as usize * self.joints;
        &self.poses[start..start + self.joints]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::mesh::{MeshCapacity, Meshes};

    #[test]
    fn both_subject_appearances_play_the_walk_cycle_in_renderer() -> PipelineResult<()> {
        for variant in [CharacterVariant::Primary, CharacterVariant::Alternate] {
            let subject = Character::load(variant)?;
            let mut meshes = Meshes::new(MeshCapacity {
                meshes: 1,
                retired_mesh_versions: 0,
            })?;
            let mesh = meshes.create(subject.mesh_data()?)?;
            let pose = meshes.create_pose(mesh)?;
            let frames = subject.frames as u64;
            for tick in [0, frames / 4, frames / 2, frames - 1] {
                meshes.set_joint_transforms(pose, subject.palette(tick))?;
            }
            // The simulation always puts a subject's origin on the floor.
            // A replacement skin must remain planted throughout the walk.
            for tick in 0..frames {
                let palette = subject.palette(tick);
                let sole = subject
                    .vertices
                    .iter()
                    .zip(&subject.weights)
                    .map(|(v, weights)| {
                        weights
                            .joints
                            .iter()
                            .zip(weights.weights)
                            .map(|(&j, w)| {
                                w * palette[usize::from(j)].transform_point(v.position())[1]
                            })
                            .sum::<f32>()
                    })
                    .fold(f32::INFINITY, f32::min);
                assert!(sole.abs() < 0.025, "{variant:?} at tick {tick}: {sole} m");
            }
            assert_ne!(subject.palette(0), subject.palette(frames / 2));
            assert_eq!(subject.palette(0), subject.palette(frames));
            assert_eq!(variant.base_rgba().len(), 256 * 256 * 4);
        }
        assert_ne!(PRIMARY, ALTERNATE);
        Ok(())
    }
}
