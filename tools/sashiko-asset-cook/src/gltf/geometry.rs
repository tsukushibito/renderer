use super::Result;
use glam::{Mat3, Mat4, Vec2, Vec3};
use gltf::{
    Accessor, Primitive, Semantic,
    accessor::{DataType, Dimensions},
};
use sashiko_asset::MeshBlob;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

fn checked_accessor(accessor: &Accessor<'_>, buffers: &[Vec<u8>]) -> Result<()> {
    if accessor.sparse().is_some() {
        return Err(format!(
            "accessor {} uses unsupported sparse storage",
            accessor.index()
        )
        .into());
    }
    let view = accessor.view().ok_or("accessor has no bufferView")?;
    let size = accessor.size();
    let stride = view.stride().unwrap_or(size);
    let last = accessor
        .count()
        .checked_sub(1)
        .and_then(|count| count.checked_mul(stride))
        .and_then(|n| n.checked_add(size))
        .and_then(|n| n.checked_add(accessor.offset()));
    if stride < size
        || last.is_none_or(|end| end > view.length())
        || view
            .offset()
            .checked_add(view.length())
            .is_none_or(|end| end > buffers[view.buffer().index()].len())
    {
        return Err(format!(
            "accessor {} exceeds its bufferView or has invalid stride",
            accessor.index()
        )
        .into());
    }
    Ok(())
}

fn hash_accessor(hash: &mut Sha256, accessor: Accessor<'_>, buffers: &[Vec<u8>]) -> Result<()> {
    checked_accessor(&accessor, buffers)?;
    let view = accessor.view().unwrap();
    let size = accessor.size();
    let start = view.offset() + accessor.offset();
    hash.update(accessor.data_type().as_gl_enum().to_le_bytes());
    hash.update([accessor.normalized().into(), accessor.dimensions() as u8]);
    hash.update((accessor.count() as u64).to_le_bytes());
    let stride = view.stride().unwrap_or(size);
    for i in 0..accessor.count() {
        let pos = start + i * stride;
        hash.update(&buffers[view.buffer().index()][pos..pos + size]);
    }
    Ok(())
}

pub(super) fn hash_inputs(
    primitive: Primitive<'_>,
    buffers: &[Vec<u8>],
    hash: &mut Sha256,
) -> Result<()> {
    for accessor in [
        primitive.get(&Semantic::Positions),
        primitive.get(&Semantic::Normals),
        primitive.get(&Semantic::Tangents),
        primitive.get(&Semantic::TexCoords(0)),
        primitive.indices(),
    ] {
        hash.update([u8::from(accessor.is_some())]);
        if let Some(accessor) = accessor {
            hash_accessor(hash, accessor, buffers)?;
        }
    }
    Ok(())
}

struct TangentGeometry<'a> {
    triangles: &'a [[u32; 3]],
    positions: &'a [[f32; 3]],
    normals: &'a [Vec3],
    uvs: &'a [[f32; 2]],
    tangents: Vec<[f32; 4]>,
}

impl bevy_mikktspace::Geometry for TangentGeometry<'_> {
    fn num_faces(&self) -> usize {
        self.triangles.len()
    }

    fn num_vertices_of_face(&self, _: usize) -> usize {
        3
    }

    fn position(&self, face: usize, corner: usize) -> [f32; 3] {
        self.positions[self.triangles[face][corner] as usize]
    }

    fn normal(&self, face: usize, corner: usize) -> [f32; 3] {
        self.normals[self.triangles[face][corner] as usize].to_array()
    }

    fn tex_coord(&self, face: usize, corner: usize) -> [f32; 2] {
        self.uvs[self.triangles[face][corner] as usize]
    }

    fn set_tangent(
        &mut self,
        tangent: Option<bevy_mikktspace::TangentSpace>,
        face: usize,
        corner: usize,
    ) {
        // A degenerate corner with no neighbor uses the reference MikkTSpace default.
        self.tangents[3 * face + corner] = tangent.unwrap_or_default().tangent_encoded();
    }
}

fn generate_tangents(
    indices: &[u32],
    positions: &[[f32; 3]],
    normals: &[Vec3],
    uvs: &[[f32; 2]],
) -> Result<Vec<[f32; 4]>> {
    let mut geometry = TangentGeometry {
        triangles: indices.as_chunks::<3>().0,
        positions,
        normals,
        uvs,
        tangents: vec![[0.; 4]; indices.len()],
    };
    bevy_mikktspace::generate_tangents(&mut geometry)?;
    Ok(geometry.tangents)
}

fn unit(v: Vec3, name: &str) -> Result<Vec3> {
    v.try_normalize()
        .ok_or_else(|| format!("invalid {name}").into())
}

pub(super) fn cook(
    primitive: Primitive<'_>,
    buffers: &[Vec<u8>],
    transform: Mat4,
) -> Result<Vec<u8>> {
    for (semantic, accessor) in primitive.attributes() {
        checked_accessor(&accessor, buffers)?;
        let f32_type = accessor.data_type() == DataType::F32 && !accessor.normalized();
        let dims = accessor.dimensions();
        let valid = match semantic {
            Semantic::Positions | Semantic::Normals => f32_type && dims == Dimensions::Vec3,
            Semantic::Tangents => f32_type && dims == Dimensions::Vec4,
            Semantic::TexCoords(0) => {
                dims == Dimensions::Vec2
                    && (f32_type
                        || (matches!(accessor.data_type(), DataType::U8 | DataType::U16)
                            && accessor.normalized()))
            }
            _ => false,
        };
        if !valid {
            return Err(format!("invalid {semantic:?} accessor {}", accessor.index()).into());
        }
    }
    if let Some(accessor) = primitive.indices() {
        checked_accessor(&accessor, buffers)?;
        if !matches!(
            accessor.data_type(),
            DataType::U8 | DataType::U16 | DataType::U32
        ) || accessor.dimensions() != Dimensions::Scalar
            || accessor.normalized()
        {
            return Err("invalid triangle index accessor".into());
        }
    }
    let reader = primitive.reader(|b| Some(&buffers[b.index()]));
    let positions: Vec<_> = reader.read_positions().ok_or("missing POSITION")?.collect();
    let normals: Vec<_> = reader
        .read_normals()
        .ok_or("missing NORMAL")?
        .map(|p| unit(Vec3::from(p), "normal"))
        .collect::<Result<_>>()?;
    let uvs: Vec<_> = reader
        .read_tex_coords(0)
        .ok_or("missing TEXCOORD_0")?
        .into_f32()
        .collect();
    let authored = reader.read_tangents().map(|v| v.collect::<Vec<_>>());
    if positions.len() != normals.len()
        || positions.len() != uvs.len()
        || authored
            .as_ref()
            .is_some_and(|t| t.len() != positions.len())
        || positions.iter().any(|v| !Vec3::from(*v).is_finite())
        || uvs.iter().any(|v| !Vec2::from(*v).is_finite())
    {
        return Err("invalid or mismatched vertex attributes".into());
    }
    let indices: Vec<_> = match reader.read_indices() {
        Some(i) => i.into_u32().collect(),
        None => (0..u32::try_from(positions.len())?).collect(),
    };
    if indices.is_empty()
        || !indices.len().is_multiple_of(3)
        || indices.iter().any(|&i| i as usize >= positions.len())
    {
        return Err("incomplete or out-of-range triangle indices".into());
    }
    let generated = if authored.is_none() {
        Some(generate_tangents(&indices, &positions, &normals, &uvs)?)
    } else {
        None
    };
    let linear = Mat3::from_mat4(transform);
    let det = linear.determinant();
    if !det.is_finite() || det.abs() < 1e-12 {
        return Err("singular node transform".into());
    }
    let normal_matrix = linear.inverse().transpose();
    let mut vertices = Vec::new();
    let mut remap = HashMap::new();
    let mut result = Vec::with_capacity(indices.len());
    for (corner, &index) in indices.iter().enumerate() {
        let i = index as usize;
        let [x, y, z, sign] = match &authored {
            Some(t) => t[i],
            None => generated.as_ref().unwrap()[corner],
        };
        if !(sign == 1. || sign == -1.) || !Vec3::new(x, y, z).is_finite() {
            return Err("invalid tangent".into());
        }
        let n = normals[i];
        let t = Vec3::new(x, y, z);
        let t = match (t - n * n.dot(t)).try_normalize() {
            Some(value) => value,
            None if generated.is_some() => n.any_orthonormal_vector(),
            None => return Err("authored tangent parallel to normal".into()),
        };
        let pos = transform.transform_point3(Vec3::from(positions[i]));
        if !pos.is_finite() {
            return Err("transformed position is not finite".into());
        }
        let normal = unit(normal_matrix * n, "transformed normal")?;
        let tangent = linear * t;
        let t = unit(
            tangent - normal * normal.dot(tangent),
            "transformed tangent",
        )?;
        let uv = uvs[i];
        let vertex = [
            pos.x,
            pos.y,
            pos.z,
            normal.x,
            normal.y,
            normal.z,
            t.x,
            t.y,
            t.z,
            sign * det.signum(),
            uv[0],
            uv[1],
        ];
        let key = vertex.map(f32::to_bits);
        let id = match remap.entry(key) {
            std::collections::hash_map::Entry::Occupied(e) => *e.get(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let id = u32::try_from(vertices.len())?;
                vertices.push(vertex);
                *e.insert(id)
            }
        };
        result.push(id);
    }
    if det < 0. {
        for tri in result.as_chunks_mut::<3>().0 {
            tri.swap(1, 2);
        }
    }
    Ok(MeshBlob {
        vertices,
        indices: result,
    }
    .encode()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrored_uv_faces_keep_separate_corner_tangents() -> Result<()> {
        let positions = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [1., 1., 0.]];
        let uvs = [[0., 0.], [1., 0.], [0., 1.], [0., 0.]];
        let tangents = generate_tangents(&[0, 1, 2, 1, 3, 2], &positions, &[Vec3::Z; 4], &uvs)?;
        assert_eq!(tangents[..3], [[1., 0., 0., 1.]; 3]);
        assert_eq!(tangents[3..], [[0., -1., 0., -1.]; 3]);
        Ok(())
    }

    #[test]
    fn isolated_degenerate_triangle_uses_reference_default() -> Result<()> {
        let tangents = generate_tangents(&[0, 0, 0], &[[0.; 3]], &[Vec3::Z], &[[0.; 2]])?;
        assert_eq!(tangents, [[1., 0., 0., -1.]; 3]);
        Ok(())
    }
}
