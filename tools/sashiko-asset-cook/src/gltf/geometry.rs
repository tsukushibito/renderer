use super::Result;
use crate::geometry::unit;
use glam::{Mat4, Vec3};
use gltf::{
    Accessor, Primitive, Semantic,
    accessor::{DataType, Dimensions},
};
use sha2::{Digest, Sha256};

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
    let indices: Vec<_> = match reader.read_indices() {
        Some(i) => i.into_u32().collect(),
        None => (0..u32::try_from(positions.len())?).collect(),
    };
    crate::geometry::triangles(positions, normals, uvs, authored, indices, transform)
}
