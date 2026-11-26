//! Cooked model-space skinning streams. Matrices are affine, three rows by four columns.
//! Version 1 stores sampled palettes. Version 2 adds retained parents and bind transforms.
use crate::{MeshVertex, invalid};
use std::io;

/// One vertex with four mesh-local joint indices and skinning weights.
pub struct SkinVertex {
    pub surface: MeshVertex,
    pub joints: [u16; 4],
    pub weights: [f32; 4],
}

/// Retained parent and model-space bind transform. Roots have no parent.
pub struct BindJoint {
    pub parent: Option<usize>,
    pub transform: [[f32; 4]; 3],
}

/// A decoded mesh and borrowed frame-major palette stream. Each frame contains
/// `joint_count` transforms in mesh joint order. Pose and bind inversion are
/// checked when preparing a rig for a particular animation/rendering consumer.
pub struct Skin<'a> {
    pub vertices: Vec<SkinVertex>,
    pub indices: Vec<u32>,
    pub joint_count: usize,
    pub frame_count: usize,
    pub bind_joints: Option<Vec<BindJoint>>,
    poses: &'a [[u8; 48]],
}

fn records<'a, const STRIDE: usize>(
    input: &mut &'a [u8],
    count: usize,
) -> io::Result<&'a [[u8; STRIDE]]> {
    let size = count
        .checked_mul(STRIDE)
        .ok_or_else(|| invalid("skin size overflow"))?;
    let bytes = input
        .split_off(..size)
        .ok_or_else(|| invalid("short skin stream"))?;
    Ok(bytes.as_chunks::<STRIDE>().0)
}

fn floats<const N: usize>(record: &[u8], offset: usize) -> [f32; N] {
    std::array::from_fn(|i| {
        let start = offset + 4 * i;
        f32::from_le_bytes(record[start..start + 4].try_into().unwrap())
    })
}

fn rows(bytes: &[u8]) -> [[f32; 4]; 3] {
    std::array::from_fn(|i| floats(bytes, i * 16))
}

impl<'a> Skin<'a> {
    pub fn decode(input: &'a [u8]) -> io::Result<Self> {
        let (version, mut data) = input
            .split_at_checked(8)
            .ok_or_else(|| invalid("short skin header"))?;
        let has_skeleton = match version {
            b"SURSKIN1" => false,
            b"SURSKIN2" => true,
            _ => return Err(invalid("unknown skin format")),
        };
        let counts = records::<4>(&mut data, 4)?;
        let [points, elements, joints, frames] =
            std::array::from_fn(|i| u32::from_le_bytes(counts[i]) as usize);
        if joints == 0 || frames == 0 {
            return Err(invalid("empty skin palette"));
        }
        let vertex_records = records::<72>(&mut data, points)?;
        let index_records = records::<4>(&mut data, elements)?;
        let matrices = frames
            .checked_mul(joints)
            .ok_or_else(|| invalid("skin palette overflow"))?;
        let poses = records::<48>(&mut data, matrices)?;
        let bind_records = if has_skeleton {
            Some(records::<52>(&mut data, joints)?)
        } else {
            None
        };
        if !data.is_empty() {
            return Err(invalid("trailing skin data"));
        }
        let mut vertices = Vec::with_capacity(points);
        for record in vertex_records {
            let vertex = SkinVertex {
                surface: MeshVertex {
                    position: floats(record, 0),
                    normal: floats(record, 12),
                    tangent: floats(record, 24),
                    uv: floats(record, 40),
                },
                joints: std::array::from_fn(|i| {
                    u16::from_le_bytes([record[48 + 2 * i], record[49 + 2 * i]])
                }),
                weights: floats(record, 56),
            };
            if vertex.joints.iter().any(|&j| usize::from(j) >= joints)
                || vertex.weights.iter().any(|&w| !w.is_finite() || w < 0.)
                || vertex.weights.iter().all(|&w| w == 0.)
            {
                return Err(invalid("invalid cooked joint indices or weights"));
            }
            vertices.push(vertex);
        }
        let indices: Vec<_> = index_records
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .collect();
        if indices.is_empty()
            || !indices.len().is_multiple_of(3)
            || indices.iter().any(|&i| i as usize >= points)
        {
            return Err(invalid("invalid cooked triangle indices"));
        }
        let bind_joints = bind_records
            .map(|records| {
                records
                    .iter()
                    .map(|record| {
                        let parent = u32::from_le_bytes(record[..4].try_into().unwrap());
                        let parent = (parent != u32::MAX).then_some(parent as usize);
                        if parent.is_some_and(|id| id >= joints) {
                            return Err(invalid("invalid bind parent"));
                        }
                        Ok(BindJoint {
                            parent,
                            transform: rows(&record[4..]),
                        })
                    })
                    .collect::<io::Result<Vec<_>>>()
            })
            .transpose()?;
        Ok(Self {
            vertices,
            indices,
            joint_count: joints,
            frame_count: frames,
            bind_joints,
            poses,
        })
    }

    pub fn poses(&self) -> impl ExactSizeIterator<Item = [[f32; 4]; 3]> + '_ {
        self.poses.iter().map(|record| rows(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &[u8] = include_bytes!("../../../assets/verification/animation/v1.skin");
    const V2: &[u8] = include_bytes!("../../../assets/verification/animation/v2.skin");

    #[test]
    fn reads_the_cooker_fixtures_with_and_without_bind_joints() -> io::Result<()> {
        for (bytes, rigged) in [(V1, false), (V2, true)] {
            let skin = Skin::decode(bytes)?;
            assert_eq!((skin.joint_count, skin.frame_count), (2, 3));
            assert_eq!(skin.indices, [2, 0, 1]);
            let vertex = &skin.vertices[2];
            assert_eq!(vertex.surface.position, [0., 1., 0.]);
            assert_eq!(vertex.surface.normal, [0., 0., 1.]);
            assert_eq!(vertex.surface.tangent, [1., 0., 0., -1.]);
            assert_eq!(vertex.surface.uv, [0., 1.]);
            assert_eq!(vertex.joints, [1, 0, 0, 0]);
            assert_eq!(vertex.weights, [0.75, 0.25, 0., 0.]);
            assert_eq!(
                skin.poses().map(|m| m[0][3]).collect::<Vec<_>>(),
                [0., 1., 2., 3., 4., 5.]
            );
            assert_eq!(skin.bind_joints.is_some(), rigged);
            if let Some(bind) = skin.bind_joints {
                assert_eq!(bind[0].parent, None);
                assert_eq!(bind[1].parent, Some(0));
                assert_eq!(bind[1].transform[0][3], 1.);
            }
        }
        Ok(())
    }

    #[test]
    fn refuses_truncated_extra_or_out_of_bounds_data() {
        let mut extra = V1.to_vec();
        extra.push(0);
        let mut bad_joint = V2.to_vec();
        bad_joint[24 + 48..24 + 50].copy_from_slice(&2u16.to_le_bytes());
        let mut bad_index = V2.to_vec();
        let first_index = 24 + 72 * 3;
        bad_index[first_index..first_index + 4].copy_from_slice(&3u32.to_le_bytes());
        let mut huge = V2.to_vec();
        huge[16..24].fill(255);
        for bytes in [&V2[..V2.len() - 1], &extra, &bad_joint, &bad_index, &huge] {
            assert!(Skin::decode(bytes).is_err());
        }
    }
}
