use crate::{Error, cache::Cache, geometry};
use glam::{Mat4, Vec3};
use sashiko_asset::{AssetId, BlobRef};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

/// Binary triangle PLY used by PBRT-v4 Landscape. Reject unrecognized layouts
/// before interpreting records; indices and floats are always little endian.
struct Layout<'a> {
    vertices: &'a [u8],
    faces: &'a [u8],
    stride: usize,
}

fn layout(bytes: &[u8]) -> Result<Layout<'_>, Error> {
    let mut lines = bytes.split_inclusive(|&b| b == b'\n');
    let mut offset = 0;
    let mut vertices = None;
    let mut faces = None;
    let mut attributes = Vec::new();
    let mut layout = false;
    let mut face_indices = false;
    let mut in_vertices = false;
    let mut complete = false;
    for raw in &mut lines {
        let line = std::str::from_utf8(raw)?.trim_end();
        offset += raw.len();
        let words: Vec<_> = line.split_ascii_whitespace().collect();
        match words.as_slice() {
            ["ply"] if offset == raw.len() => {}
            ["format", "binary_little_endian", "1.0"] => layout = true,
            ["comment" | "obj_info", ..] => {}
            ["element", "vertex", n] if vertices.is_none() && faces.is_none() => {
                vertices = Some(n.parse::<usize>()?);
                in_vertices = true;
            }
            ["element", "face", n] if vertices.is_some() && faces.is_none() => {
                faces = Some(n.parse::<usize>()?);
                in_vertices = false;
            }
            ["property", "float" | "float32", field] if in_vertices => attributes.push(*field),
            [
                "property",
                "list",
                "uint8" | "uchar",
                "int" | "int32",
                "vertex_indices",
            ] if faces.is_some() && !face_indices => face_indices = true,
            ["end_header"] => {
                complete = true;
                break;
            }
            _ => return Err(format!("unsupported PLY header {line:?}").into()),
        }
    }
    let nv = vertices.ok_or("missing PLY vertex element")?;
    let nf = faces.ok_or("missing PLY face element")?;
    let normals_uv = attributes == ["x", "y", "z", "nx", "ny", "nz", "u", "v"];
    if !bytes.starts_with(b"ply")
        || !complete
        || !layout
        || !face_indices
        || (!normals_uv && attributes != ["x", "y", "z"])
    {
        return Err("unsupported PLY vertex or index layout".into());
    }
    let stride = attributes.len() * 4;
    let size = nv.checked_mul(stride).ok_or("PLY vertex size overflow")?;
    if nf
        .checked_mul(13)
        .and_then(|face_size| size.checked_add(face_size))
        != Some(bytes.len() - offset)
    {
        return Err("PLY data length disagrees with header or contains non-triangles".into());
    }
    let (vertices, faces) = bytes[offset..].split_at(size);
    Ok(Layout {
        vertices,
        faces,
        stride,
    })
}

fn component(record: &[u8], i: usize) -> f32 {
    f32::from_le_bytes(record[4 * i..4 * i + 4].try_into().unwrap())
}

pub(super) fn overlaps(
    path: &Path,
    transform: Mat4,
    origin: Vec3,
    scale: f32,
    region: [f32; 4],
) -> Result<bool, Error> {
    let bytes = fs::read(path)?;
    let input = layout(&bytes)?;
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = -lo;
    for v in input.vertices.chunks_exact(input.stride) {
        let p = (transform.transform_point3(Vec3::new(
            component(v, 0),
            component(v, 1),
            component(v, 2),
        )) - origin)
            * scale;
        if !p.is_finite() {
            return Err("invalid PLY position".into());
        }
        lo = lo.min(p);
        hi = hi.max(p);
    }
    Ok(hi.x >= region[0] && hi.z >= region[1] && lo.x < region[2] && lo.z < region[3])
}

pub(super) fn cook(
    cache: &mut Cache<'_>,
    path: &Path,
    relative_name: &Path,
    reverse: bool,
    scale: f32,
) -> Result<(BlobRef, usize, usize), Error> {
    let bytes = fs::read(path)?;
    let Layout {
        vertices: v,
        faces: f,
        stride,
    } = layout(&bytes)?;
    let nv = v.len() / stride;
    let nf = f.len() / 13;
    let normals_uv = stride == 32;
    let mut positions = Vec::with_capacity(nv);
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut zero_normals = 0;
    for record in v.chunks_exact(stride) {
        let val = |i: usize| component(record, i);
        positions.push([val(0) * scale, val(1) * scale, val(2) * scale]);
        if normals_uv {
            let n = Vec3::new(val(3), val(4), val(5));
            let n = if n == Vec3::ZERO {
                zero_normals += 1;
                n
            } else {
                geometry::unit(n, "PLY normal")?
            };
            normals.push(if reverse { -n } else { n });
            // PBRT imagemaps invert the vertical texture coordinate.
            uvs.push([val(6), 1. - val(7)]);
        }
    }
    let mut indices = Vec::with_capacity(nf * 3);
    for face in f.as_chunks::<13>().0 {
        if face[0] != 3 {
            return Err("expected triangle PLY face".into());
        }
        let mut tri = [0u32; 3];
        for (dst, word) in tri.iter_mut().zip(face[1..].as_chunks::<4>().0) {
            *dst = u32::from_le_bytes(*word);
            if *dst as usize >= nv {
                return Err("invalid PLY index".into());
            }
        }
        if reverse {
            tri.swap(1, 2);
        }
        indices.extend(tri);
    }
    if zero_normals > 0 {
        for corners in indices.as_chunks::<3>().0 {
            if corners.iter().any(|&i| normals[i as usize] == Vec3::ZERO) {
                let [a, b, c] = corners.map(|i| Vec3::from(positions[i as usize]));
                if (b - a).cross(c - a) != Vec3::ZERO {
                    return Err("zero shading normal on nondegenerate PLY triangle".into());
                }
            }
        }
        for normal in &mut normals {
            if *normal == Vec3::ZERO {
                *normal = Vec3::Y;
            }
        }
    }
    if !normals_uv {
        // PBRT uses geometric normals and per-face UVs for missing attributes.
        let original = positions;
        positions = Vec::with_capacity(indices.len());
        for tri in indices.as_chunks::<3>().0 {
            let points = [
                original[tri[0] as usize],
                original[tri[1] as usize],
                original[tri[2] as usize],
            ];
            let [a, b, c] = points.map(Vec3::from);
            let n = (b - a).cross(c - a).try_normalize().unwrap_or(Vec3::Y);
            positions.extend(points);
            normals.extend([n; 3]);
            let mut uv = [[0., 1.], [1., 1.], [1., 0.]];
            if reverse {
                uv.swap(1, 2);
            }
            uvs.extend(uv);
        }
        indices = (0..u32::try_from(positions.len())?).collect();
    }
    let name = relative_name
        .to_str()
        .ok_or("invalid source filename")?
        .replace('\\', "/");
    let identifier = Sha256::digest(name.as_bytes());
    let id = AssetId::from_u128(u128::from_be_bytes(identifier[..16].try_into()?));
    let recipe = format!("metres={:08x},reverse={reverse}", scale.to_bits());
    let mesh = cache.convert(id, "pbrt-ply", &recipe, &bytes, || {
        geometry::triangles(
            positions,
            normals,
            uvs,
            None,
            indices,
            super::change_basis(),
        )
    })?;
    Ok((mesh, nf, zero_normals))
}
