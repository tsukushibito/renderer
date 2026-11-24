//! Consumer-owned cooked data. All byte/content checks happen at this load boundary.
//!
//! The runner keeps these values private and immutable after GPU publication.
//! Replacing an asset constructs and validates a new Scene before replacing its
//! GPU owners; an old buffer identity is never used as a content certificate.

use std::{
    error::Error,
    ops::Range,
    path::{Component, Path, PathBuf},
};

use renderer::pbr::NormalEncoding;
use renderer_core::ImageFormat;
use serde_json::Value;
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(crate) struct Scene {
    pub lightmap_uv: Vec<u8>,
    pub vertices: Vec<u8>,
    pub indices: Vec<u8>,
    pub primitives: Vec<Primitive>,
    pub images: Vec<TextureImage>,
    pub materials: Vec<Material>,
    pub mesh_count: usize,
    pub source: Value,
}

pub(crate) struct Primitive {
    pub surface: Option<renderer::pbr::LightmapAddress>,
    pub lightmap: Option<(Range<usize>, [u32; 2])>,
    pub material: usize,
    pub vertices: Range<usize>,
    pub indices: Range<usize>,
    pub index_count: u32,
    pub center: [f32; 3],
}

pub(crate) struct TextureImage {
    pub width: u32,
    pub height: u32,
    pub srgb: bool,
    pub(crate) compressed: Option<CompressedImage>,
    pub mips: Vec<Range<usize>>,
    pub(crate) pixels: Pixels,
}

pub(crate) struct CompressedImage {
    format: ImageFormat,
    normal: NormalEncoding,
    maximum_alpha: Option<u8>,
    #[cfg(feature = "bake-tool")]
    bake_pixels: Pixels,
}

pub(crate) enum Pixels {
    File {
        path: PathBuf,
        sha256: String,
        byte_length: usize,
    },
    Owned(Vec<u8>),
}

impl Pixels {
    /// Called once at the upload/change boundary, then promptly dropped. The
    /// full Bistro has several GiB of decoded pixels; it must not be retained
    /// beside the GPU-resident copy or copied into a per-frame Graph.
    fn read(&self) -> Result<Vec<u8>> {
        match self {
            Pixels::Owned(bytes) => Ok(bytes.clone()),
            Pixels::File {
                path,
                sha256,
                byte_length,
            } => {
                let bytes = std::fs::read(path)?;
                if bytes.len() != *byte_length || format!("{:x}", Sha256::digest(&bytes)) != *sha256
                {
                    return Err(invalid(format!(
                        "image byte length/SHA-256 mismatch: {}",
                        path.display()
                    )));
                }
                Ok(bytes)
            }
        }
    }
}

impl TextureImage {
    #[cfg(test)]
    pub(crate) fn constant_bc5(x: u8, y: u8, normal: NormalEncoding) -> Self {
        // Each BC4 component selects its first endpoint at every texel.
        let mut block = vec![0; 16];
        block[0] = x;
        block[8] = y;
        Self {
            width: 1,
            height: 1,
            srgb: false,
            compressed: Some(CompressedImage {
                format: ImageFormat::Bc5RgUnorm,
                normal,
                maximum_alpha: None,
                #[cfg(feature = "bake-tool")]
                bake_pixels: Pixels::Owned(vec![x, y, 0, 255]),
            }),
            mips: std::iter::once(0..16).collect(),
            pixels: Pixels::Owned(block),
        }
    }
    pub fn load_bytes(&self) -> Result<Vec<u8>> {
        self.pixels.read()
    }
    pub fn format(&self) -> ImageFormat {
        match &self.compressed {
            Some(info) => info.format,
            None if self.srgb => ImageFormat::Rgba8Srgb,
            None => ImageFormat::Rgba8Unorm,
        }
    }
    pub fn desc(&self) -> renderer_core::graph::ImageDesc {
        renderer_core::graph::ImageDesc {
            mip_levels: self.mips.len() as u32,
            ..renderer_core::graph::ImageDesc::texture_2d(self.width, self.height, self.format())
        }
    }
    /// The per-mip diagnostic writes known RGBA values into an opaque base texture.
    pub fn probe_desc(&self) -> renderer_core::graph::ImageDesc {
        renderer_core::graph::ImageDesc {
            format: if self.srgb {
                ImageFormat::Rgba8Srgb
            } else {
                ImageFormat::Rgba8Unorm
            },
            ..self.desc()
        }
    }
    #[cfg(feature = "bake-tool")]
    pub fn bake_pixels(&self) -> Result<(usize, Vec<u8>)> {
        let level = bake_level(self.width, self.height);
        let bytes = match &self.compressed {
            Some(info) => info.bake_pixels.read()?,
            None => self.load_bytes()?[self.mips[level].clone()].to_vec(),
        };
        Ok((level, bytes))
    }
    fn maximum_alpha(&self) -> Result<u8> {
        if let Some(info) = &self.compressed {
            return info
                .maximum_alpha
                .ok_or_else(|| invalid("base texture lacks alpha metadata"));
        }
        Ok(self
            .load_bytes()?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[3])
            .max()
            .unwrap_or(255))
    }
    pub fn byte_length(&self) -> usize {
        self.mips.last().map_or(0, |mip| mip.end)
    }
}

pub(crate) fn staging_bytes(desc: renderer_core::graph::ImageDesc) -> usize {
    (0..desc.mip_levels).fold(0usize, |end, mip| {
        let width = (desc.width >> mip).max(1);
        let height = (desc.height >> mip).max(1);
        let row = desc.format.packed_row_bytes(width) as usize;
        end.next_multiple_of(512)
            + row.next_multiple_of(256) * desc.format.packed_rows(height) as usize
    })
}

pub(crate) fn mip_color(level: usize) -> [u8; 3] {
    [
        24 + (level as u8) * 7,
        172 - (level as u8) * 5,
        80 + (level as u8) * 3,
    ]
}

pub(crate) fn changed_texture(scene: &Scene, index: usize, original: &[u8]) -> Vec<u8> {
    let image = &scene.images[index];
    let mut changed = if image.format().is_block_compressed() {
        vec![255; image.probe_desc().byte_len().unwrap() as usize]
    } else {
        original.to_vec()
    };
    let mut remaining = changed.as_mut_slice();
    for level in 0..image.mips.len() {
        let length =
            (image.width >> level).max(1) as usize * (image.height >> level).max(1) as usize * 4;
        let (pixels, next) = remaining.split_at_mut(length);
        for rgba in pixels.as_chunks_mut::<4>().0 {
            rgba[..3].copy_from_slice(&mip_color(level));
        }
        remaining = next;
    }
    changed
}

#[derive(Clone)]
pub(crate) struct Material {
    pub uniform: [u8; 64],
    /// Base, normal, ORM and emissive image indices, with defaults resolved.
    pub textures: [usize; 4],
    pub blend: bool,
    /// Original material coverage, retained when the diagnostic override is used.
    pub alpha_zero: bool,
}

fn invalid(message: impl Into<String>) -> Box<dyn Error> {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()).into()
}

fn array(value: &Value) -> Result<&[Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| invalid("expected JSON array"))
}

fn integer(value: &Value) -> Result<usize> {
    value
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| invalid("expected addressable non-negative integer"))
}

fn u32_value(value: &Value) -> Result<u32> {
    value
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| invalid("expected u32 integer"))
}

fn float(value: &Value) -> Result<f32> {
    value
        .as_f64()
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .ok_or_else(|| invalid("expected finite f32"))
}

fn floats<const N: usize>(value: &Value) -> Result<[f32; N]> {
    let values = array(value)?;
    if values.len() != N {
        return Err(invalid("wrong vector component count"));
    }
    let mut result = [0.; N];
    for (out, value) in result.iter_mut().zip(values) {
        *out = float(value)?;
    }
    Ok(result)
}

fn text(value: &Value) -> Result<&str> {
    value.as_str().ok_or_else(|| invalid("expected string"))
}

fn boolean(value: &Value) -> Result<bool> {
    value.as_bool().ok_or_else(|| invalid("expected boolean"))
}

fn unit(value: f32) -> Result<f32> {
    if !(0.0..=1.0).contains(&value) {
        return Err(invalid("material coefficient outside [0,1]"));
    }
    Ok(value)
}

fn blob_path(root: &Path, record: &Value) -> Result<PathBuf> {
    let relative = Path::new(text(&record["path"])?);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(invalid(
            "asset path must contain only relative normal components",
        ));
    }
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(root) {
        return Err(invalid("asset path escapes manifest directory"));
    }
    let expected_len = integer(&record["byte_length"])?;
    if std::fs::metadata(&path)?.len() != expected_len as u64 {
        return Err(invalid(format!(
            "asset byte length mismatch: {}",
            relative.display()
        )));
    }
    Ok(path)
}

fn read_blob(root: &Path, record: &Value) -> Result<Vec<u8>> {
    let path = blob_path(root, record)?;
    let bytes = std::fs::read(&path)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    if bytes.len() != integer(&record["byte_length"])? || hash != text(&record["sha256"])? {
        return Err(invalid(format!(
            "asset byte length/SHA-256 mismatch: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

fn byte_range(offset: &Value, count: usize, stride: usize, limit: usize) -> Result<Range<usize>> {
    let start = integer(offset)?;
    let end = count
        .checked_mul(stride)
        .and_then(|n| start.checked_add(n))
        .filter(|end| *end <= limit)
        .ok_or_else(|| invalid("asset byte range exceeds stream"))?;
    if !start.is_multiple_of(stride) {
        return Err(invalid("asset byte offset is not stride aligned"));
    }
    Ok(start..end)
}

pub(crate) fn validate_vertex(bytes: &[u8]) -> Result<[f32; 3]> {
    let v: [f32; 12] =
        std::array::from_fn(|i| f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()));
    if v.iter().any(|x| !x.is_finite()) {
        return Err(invalid("non-finite vertex component"));
    }
    let normal = &v[3..6];
    let tangent = &v[6..9];
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(a, b)| a * b).sum::<f32>();
    if (dot(normal, normal) - 1.).abs() > 0.005
        || (dot(tangent, tangent) - 1.).abs() > 0.005
        || dot(normal, tangent).abs() > 0.005
        || (v[9] != 1. && v[9] != -1.)
    {
        return Err(invalid(
            "vertex normal/tangent basis is not orthonormal with signed handedness",
        ));
    }
    Ok([v[0], v[1], v[2]])
}

fn validate_geometry(
    doc: &Value,
    vertices: &[u8],
    indices: &[u8],
    materials: usize,
) -> Result<Vec<Primitive>> {
    let records = array(&doc["primitives"])?;
    let meshes = array(&doc["meshes"])?;
    if records.is_empty() || meshes.is_empty() {
        return Err(invalid("empty scene geometry"));
    }
    let mut owners = vec![None; records.len()];
    for (id, mesh) in meshes.iter().enumerate() {
        if integer(&mesh["id"])? != id {
            return Err(invalid("mesh ids must be dense and ordered"));
        }
        let _ = floats::<16>(&mesh["world_transform"])?;
        let primitives = array(&mesh["primitives"])?;
        if primitives.is_empty() {
            return Err(invalid("mesh has no primitives"));
        }
        for value in primitives {
            let primitive = integer(value)?;
            let owner = owners
                .get_mut(primitive)
                .ok_or_else(|| invalid("mesh references unknown primitive"))?;
            if owner.replace(id).is_some() {
                return Err(invalid("primitive listed more than once"));
            }
        }
    }
    let mut result = Vec::with_capacity(records.len());
    let (mut vertex_cursor, mut index_cursor) = (0, 0);
    for (id, primitive) in records.iter().enumerate() {
        if integer(&primitive["id"])? != id || owners[id] != Some(integer(&primitive["mesh"])?) {
            return Err(invalid("primitive id/mesh ownership mismatch"));
        }
        let material = integer(&primitive["material"])?;
        if material >= materials {
            return Err(invalid("primitive references unknown material"));
        }
        let vertex_count = integer(&primitive["vertex_count"])?;
        let index_count = u32_value(&primitive["index_count"])?;
        if vertex_count == 0 || index_count == 0 || !index_count.is_multiple_of(3) {
            return Err(invalid("primitive must contain complete indexed triangles"));
        }
        let vertex_range = byte_range(
            &primitive["vertex_offset"],
            vertex_count,
            48,
            vertices.len(),
        )?;
        let index_range = byte_range(
            &primitive["index_offset"],
            index_count as usize,
            4,
            indices.len(),
        )?;
        if vertex_range.start != vertex_cursor || index_range.start != index_cursor {
            return Err(invalid(
                "primitive streams must partition the blobs without gaps or overlap",
            ));
        }
        let (mut low, mut high) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for vertex in vertices[vertex_range.clone()].as_chunks::<48>().0 {
            let position = validate_vertex(vertex)?;
            for axis in 0..3 {
                low[axis] = low[axis].min(position[axis]);
                high[axis] = high[axis].max(position[axis]);
            }
        }
        let mut maximum = 0;
        for word in indices[index_range.clone()].as_chunks::<4>().0.iter() {
            let index = u32::from_le_bytes(*word);
            if index as usize >= vertex_count {
                return Err(invalid("index exceeds primitive vertex count"));
            }
            maximum = maximum.max(index);
        }
        if maximum != u32_value(&primitive["max_index"])? {
            return Err(invalid("incorrect maximum index summary"));
        }
        let declared_low = floats::<3>(&primitive["bounds_min"])?;
        let declared_high = floats::<3>(&primitive["bounds_max"])?;
        for axis in 0..3 {
            let tolerance = 1e-5 * high[axis].abs().max(low[axis].abs()).max(1.);
            if (low[axis] - declared_low[axis]).abs() > tolerance
                || (high[axis] - declared_high[axis]).abs() > tolerance
            {
                return Err(invalid("primitive bounds disagree with position stream"));
            }
        }
        result.push(Primitive {
            surface: None,
            lightmap: None,
            material,
            vertices: vertex_range.clone(),
            indices: index_range.clone(),
            index_count,
            center: std::array::from_fn(|axis| low[axis] * 0.5 + high[axis] * 0.5),
        });
        vertex_cursor = vertex_range.end;
        index_cursor = index_range.end;
    }
    if vertex_cursor != vertices.len() || index_cursor != indices.len() {
        return Err(invalid("unreferenced geometry bytes"));
    }
    Ok(result)
}

#[cfg(feature = "bake-tool")]
fn bake_level(width: u32, height: u32) -> usize {
    let mut edge = width.max(height);
    let mut level = 0;
    while edge > 128 {
        edge >>= 1;
        level += 1;
    }
    level
}

fn image_blob(root: &Path, record: &Value) -> Result<Pixels> {
    let path = blob_path(root, record)?;
    let sha256 = text(&record["sha256"])?;
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(invalid("invalid image SHA-256"));
    }
    Ok(Pixels::File {
        path,
        sha256: sha256.to_owned(),
        byte_length: integer(&record["byte_length"])?,
    })
}

fn image(root: &Path, record: &Value, id: usize) -> Result<TextureImage> {
    if integer(&record["id"])? != id {
        return Err(invalid("image ids must be dense and ordered"));
    }
    let width = u32_value(&record["width"])?;
    let height = u32_value(&record["height"])?;
    if width == 0 || height == 0 {
        return Err(invalid("zero image extent"));
    }
    let srgb = match text(&record["colorspace"])? {
        "srgb" => true,
        "linear" => false,
        _ => return Err(invalid("unknown image colorspace")),
    };
    let role_srgb = match text(&record["role"])? {
        "base_color" | "emissive" => true,
        "normal" | "orm" => false,
        _ => return Err(invalid("unsupported image role")),
    };
    if srgb != role_srgb {
        return Err(invalid("image colorspace contradicts its material role"));
    }
    let levels = u32::BITS - width.max(height).leading_zeros();
    let records = array(&record["mips"])?;
    if integer(&record["mip_levels"])? != levels as usize || records.len() != levels as usize {
        return Err(invalid("image must contain its complete mip chain"));
    }
    let format_name = record
        .get("format")
        .map(text)
        .transpose()?
        .unwrap_or("rgba8");
    let format = match (format_name, srgb) {
        ("rgba8", true) => ImageFormat::Rgba8Srgb,
        ("rgba8", false) => ImageFormat::Rgba8Unorm,
        ("bc1", true) => ImageFormat::Bc1RgbaSrgb,
        ("bc1", false) => ImageFormat::Bc1RgbaUnorm,
        ("bc3", true) => ImageFormat::Bc3RgbaSrgb,
        ("bc3", false) => ImageFormat::Bc3RgbaUnorm,
        ("bc7", true) => ImageFormat::Bc7RgbaSrgb,
        ("bc7", false) => ImageFormat::Bc7RgbaUnorm,
        ("bc5", false) if record["role"] == "normal" => ImageFormat::Bc5RgUnorm,
        _ => return Err(invalid("unsupported material texture format/role")),
    };
    let normal = match (
        format,
        record
            .get("normal_encoding")
            .map(text)
            .transpose()?
            .unwrap_or("xyz"),
    ) {
        (ImageFormat::Bc5RgUnorm, "xy") => NormalEncoding::Xy,
        (ImageFormat::Bc5RgUnorm, "xy_directx") => NormalEncoding::XyDirectX,
        (f, "xyz") if f != ImageFormat::Bc5RgUnorm => NormalEncoding::Xyz,
        _ => return Err(invalid("normal encoding disagrees with format")),
    };
    let mut maximum_alpha = None;
    let pixels = image_blob(root, record)?;
    let byte_length = integer(&record["byte_length"])?;
    let mut mips = Vec::with_capacity(records.len());
    let mut cursor = 0;
    for (level, mip) in records.iter().enumerate() {
        let [w, h] = [(width >> level).max(1), (height >> level).max(1)];
        let len = format
            .packed_byte_len(w, h)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| invalid("image byte size overflow"))?;
        if format.is_block_compressed() && record["role"] == "base_color" {
            let min = u8::try_from(integer(&mip["alpha_min"])?)?;
            let max = u8::try_from(integer(&mip["alpha_max"])?)?;
            if min > max {
                return Err(invalid("invalid mip alpha bounds"));
            }
            maximum_alpha = Some(maximum_alpha.unwrap_or(0).max(max));
        }
        if integer(&mip["level"])? != level
            || u32_value(&mip["width"])? != w
            || u32_value(&mip["height"])? != h
            || integer(&mip["offset"])? != cursor
            || integer(&mip["byte_length"])? != len
        {
            return Err(invalid("invalid mip geometry or byte partition"));
        }
        let end = cursor
            .checked_add(len)
            .filter(|n| *n <= byte_length)
            .ok_or_else(|| invalid("mip exceeds image blob"))?;
        mips.push(cursor..end);
        cursor = end;
    }
    if cursor != byte_length {
        return Err(invalid("unreferenced image bytes"));
    }
    let compressed = if format.is_block_compressed() {
        #[cfg(feature = "bake-tool")]
        let bake_pixels = {
            let mip = &record["bake_mip"];
            let level = bake_level(width, height);
            let w = (width >> level).max(1);
            let h = (height >> level).max(1);
            if integer(&mip["level"])? != level
                || integer(&mip["byte_length"])? != (w * h * 4) as usize
            {
                return Err(invalid("invalid decoded CPU bake mip"));
            }
            image_blob(root, mip)?
        };
        Some(CompressedImage {
            format,
            normal,
            maximum_alpha,
            #[cfg(feature = "bake-tool")]
            bake_pixels,
        })
    } else {
        None
    };
    Ok(TextureImage {
        width,
        height,
        srgb,
        compressed,
        mips,
        pixels,
    })
}

fn validate_samplers(doc: &Value) -> Result<Vec<usize>> {
    let samplers = array(&doc["samplers"])?;
    for sampler in samplers {
        for (key, expected) in [
            ("magFilter", 9729),
            ("minFilter", 9987),
            ("wrapS", 10497),
            ("wrapT", 10497),
        ] {
            if !sampler[key].is_null() && sampler[key].as_u64() != Some(expected) {
                return Err(invalid(
                    "Bistro consumer requires linear/trilinear repeat sampler",
                ));
            }
        }
    }
    let mut textures = Vec::new();
    for (id, texture) in array(&doc["textures"])?.iter().enumerate() {
        if integer(&texture["id"])? != id {
            return Err(invalid("texture ids must be dense and ordered"));
        }
        if !texture["sampler"].is_null() && integer(&texture["sampler"])? >= samplers.len() {
            return Err(invalid("texture references unknown sampler"));
        }
        textures.push(integer(&texture["source"])?);
    }
    Ok(textures)
}

pub(crate) fn fallback_images(images: &mut Vec<TextureImage>) -> [usize; 4] {
    let first = images.len();
    for (bytes, srgb) in [
        ([255, 255, 255, 255], true),
        ([128, 128, 255, 255], false),
        ([255, 255, 255, 255], false),
        ([255, 255, 255, 255], true),
    ] {
        images.push(TextureImage {
            compressed: None,
            width: 1,
            height: 1,
            srgb,
            mips: std::iter::once(0..4).collect(),
            pixels: Pixels::Owned(bytes.to_vec()),
        });
    }
    [first, first + 1, first + 2, first + 3]
}

pub(crate) fn material(
    record: &Value,
    id: usize,
    textures: &[usize],
    images: &[TextureImage],
    defaults: [usize; 4],
    force_opaque: bool,
) -> Result<Material> {
    if integer(&record["id"])? != id {
        return Err(invalid("material ids must be dense and ordered"));
    }
    let mut base = floats::<4>(&record["base_factor"])?;
    for v in base {
        unit(v)?;
    }
    let emissive = floats::<3>(&record["emissive"])?;
    if emissive.iter().any(|v| *v < 0.) {
        return Err(invalid("negative emissive coefficient"));
    }
    let declared_normal_scale = float(&record["normal_scale"])?;
    // RGBA8's nearest encoding of tangent-space (0,0,1) is slightly tilted.
    // No normal map means the unperturbed geometric normal, not that tilt.
    let normal_scale = if record["normal_texture"].is_null() {
        0.
    } else {
        declared_normal_scale
    };
    let metallic = unit(float(&record["metallic"])?)?;
    let roughness = unit(float(&record["roughness"])?)?;
    let declared_ao = unit(float(&record["occlusion_strength"])?)?;
    // An absent AO map is identity even when the packed ORM texture is present
    // for metallic/roughness. Its R channel must not become implicit occlusion.
    let ao = if record["occlusion_texture"].is_null() {
        0.
    } else {
        declared_ao
    };
    let cutoff = float(&record["alpha_cutoff"])?;
    if cutoff < 0. {
        return Err(invalid("negative alpha cutoff"));
    }
    let alpha_mode = match text(&record["alpha_mode"])? {
        "OPAQUE" => 0,
        "MASK" => 1,
        "BLEND" => 2,
        _ => return Err(invalid("unknown alpha mode")),
    };
    let double_sided = boolean(&record["double_sided"])?;
    let mut slots = defaults;
    for (slot, key) in [
        "base_color_texture",
        "normal_texture",
        "orm_texture",
        "emissive_texture",
    ]
    .into_iter()
    .enumerate()
    {
        if !record[key].is_null() {
            slots[slot] = *textures
                .get(integer(&record[key])?)
                .ok_or_else(|| invalid("material references unknown texture"))?;
            let image = images
                .get(slots[slot])
                .ok_or_else(|| invalid("texture references unknown image"))?;
            if image.srgb != matches!(slot, 0 | 3) {
                return Err(invalid("material texture colorspace mismatch"));
            }
        }
    }
    if !record["occlusion_texture"].is_null()
        && record["occlusion_texture"] != record["orm_texture"]
    {
        return Err(invalid(
            "Bistro consumer does not support separate AO and metallic-roughness textures",
        ));
    }
    let maximum_alpha = if alpha_mode == 0 {
        1.
    } else {
        f32::from(images[slots[0]].maximum_alpha()?) / 255. * base[3]
    };
    let alpha_zero =
        (alpha_mode == 1 && maximum_alpha < cutoff) || (alpha_mode == 2 && maximum_alpha == 0.);
    let effective_mode = if force_opaque {
        base[3] = 1.;
        0
    } else {
        alpha_mode
    };
    let mut uniform = [0; 64];
    for (out, value) in uniform[..48].as_chunks_mut::<4>().0.iter_mut().zip(
        base.into_iter()
            .chain(emissive)
            .chain([normal_scale, metallic, roughness, ao, cutoff]),
    ) {
        out.copy_from_slice(&value.to_le_bytes());
    }
    for (out, value) in uniform[48..].as_chunks_mut::<4>().0.iter_mut().zip([
        effective_mode,
        u32::from(double_sided),
        images[slots[1]]
            .compressed
            .as_ref()
            .map_or(NormalEncoding::Xyz, |c| c.normal) as u32,
        0,
    ]) {
        out.copy_from_slice(&value.to_le_bytes());
    }
    Ok(Material {
        uniform,
        textures: slots,
        blend: effective_mode == 2,
        alpha_zero,
    })
}

impl Scene {
    /// A sphere of known material used to inspect spatial lighting. Inserted
    /// after the bake, so it receives the same lighting as a moving object.
    pub fn add_inspection_sphere(&mut self, center: [f32; 3], roughness: f32, metallic: f32) {
        let textures = fallback_images(&mut self.images);
        let factors = renderer::pbr::MaterialFactors {
            roughness,
            metallic,
            normal_scale: 0.,
            ..Default::default()
        };
        let material = self.materials.len();
        self.materials.push(Material {
            uniform: factors.encode().expect("constant sphere material"),
            textures,
            blend: false,
            alpha_zero: false,
        });
        let start = self.vertices.len();
        let index_start = self.indices.len();
        const RINGS: u32 = 20;
        const SLICES: u32 = 32;
        for y in 0..=RINGS {
            let (st, ct) = (std::f32::consts::PI * y as f32 / RINGS as f32).sin_cos();
            for x in 0..=SLICES {
                let (sp, cp) = (std::f32::consts::TAU * x as f32 / SLICES as f32).sin_cos();
                let n = [st * cp, ct, st * sp];
                let p = std::array::from_fn::<_, 3, _>(|i| center[i] + 0.4 * n[i]);
                self.vertices.extend(
                    p.into_iter()
                        .chain(n)
                        .chain([
                            -sp,
                            0.,
                            cp,
                            1.,
                            x as f32 / SLICES as f32,
                            y as f32 / RINGS as f32,
                        ])
                        .flat_map(f32::to_le_bytes),
                );
            }
        }
        for y in 0..RINGS {
            for x in 0..SLICES {
                let a = y * (SLICES + 1) + x;
                let b = a + SLICES + 1;
                if y != 0 {
                    self.indices
                        .extend([a, a + 1, b].into_iter().flat_map(u32::to_le_bytes));
                }
                if y + 1 != RINGS {
                    self.indices
                        .extend([a + 1, b + 1, b].into_iter().flat_map(u32::to_le_bytes));
                }
            }
        }
        self.primitives.push(Primitive {
            surface: None,
            lightmap: None,
            material,
            vertices: start..self.vertices.len(),
            indices: index_start..self.indices.len(),
            index_count: ((self.indices.len() - index_start) / 4) as u32,
            center,
        });
        self.mesh_count += 1;
    }

    /// Apply the Bistro AO policy at publication, preserving source ORM pixels.
    pub fn disable_environment_occlusion(&mut self) {
        for material in &mut self.materials {
            material.uniform[40..44].copy_from_slice(&0.0f32.to_le_bytes());
        }
        self.source["applied_material_policy"] = serde_json::json!({
            "ambient_occlusion_enabled": false,
            "reference": "Falcor 4.0.1 default; c53908dd7930c031eb28b7ee49025eb27a833d64",
            "source_pixels_and_references_preserved": true
        });
    }
    pub fn load(manifest: &Path, force_opaque: bool) -> Result<Self> {
        let manifest = manifest.canonicalize()?;
        let root = manifest
            .parent()
            .ok_or_else(|| invalid("manifest has no parent"))?;
        let manifest_bytes = std::fs::read(&manifest)?;
        let doc: Value = serde_json::from_slice(&manifest_bytes)?;
        if integer(&doc["schema_version"])? != 1 || integer(&doc["vertex_stride"])? != 48 {
            return Err(invalid("unsupported cooked scene schema or vertex stride"));
        }
        let vertices = read_blob(root, &doc["vertices"])?;
        let indices = read_blob(root, &doc["indices"])?;
        if integer(&doc["vertices"]["count"])?.checked_mul(48) != Some(vertices.len())
            || integer(&doc["indices"]["count"])?.checked_mul(4) != Some(indices.len())
        {
            return Err(invalid("stream count/stride mismatch"));
        }
        let image_records = array(&doc["images"])?;
        let mut images = Vec::with_capacity(image_records.len() + 4);
        for (id, record) in image_records.iter().enumerate() {
            images.push(image(root, record, id)?);
        }
        let textures = validate_samplers(&doc)?;
        if textures.iter().any(|id| *id >= image_records.len()) {
            return Err(invalid("texture references unknown source image"));
        }
        let defaults = fallback_images(&mut images);
        let records = array(&doc["materials"])?;
        let mut materials = Vec::with_capacity(records.len());
        for (id, record) in records.iter().enumerate() {
            materials.push(material(
                record,
                id,
                &textures,
                &images,
                defaults,
                force_opaque,
            )?);
        }
        if materials.is_empty() {
            return Err(invalid("scene has no materials"));
        }
        let mut primitives = validate_geometry(&doc, &vertices, &indices, materials.len())?;
        let lightmap_uv = if doc["lightmap_uv"].is_null() {
            Vec::new()
        } else {
            let blob = read_blob(root, &doc["lightmap_uv"])?;
            if !blob.len().is_multiple_of(8)
                || !blob
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|b| (0.0..=1.0).contains(&f32::from_le_bytes(*b)))
            {
                return Err(invalid("invalid cooked lightmap UV stream"));
            }
            for (p, record) in primitives.iter_mut().zip(array(&doc["primitives"])?) {
                let layout = &record["lightmap"];
                if layout.is_null() {
                    continue;
                }
                let range = byte_range(&layout["uv_offset"], p.vertices.len() / 48, 8, blob.len())?;
                let e = array(&layout["extent"])?;
                if e.len() != 2 {
                    return Err(invalid("invalid lightmap extent"));
                }
                let extent = [u32_value(&e[0])?, u32_value(&e[1])?];
                if extent.contains(&0) || !range.start.is_multiple_of(8) {
                    return Err(invalid("invalid cooked lightmap layout"));
                }
                p.lightmap = Some((range, extent));
            }
            blob
        };
        let mesh_count = array(&doc["meshes"])?.len();
        let triangle_count = primitives
            .iter()
            .try_fold(0_usize, |total, primitive| {
                total.checked_add(primitive.index_count as usize / 3)
            })
            .ok_or_else(|| invalid("triangle count overflow"))?;
        for (key, actual) in [
            ("mesh_count", mesh_count),
            ("primitive_count", primitives.len()),
            ("material_count", materials.len()),
            ("image_count", image_records.len()),
            ("triangles", triangle_count),
        ] {
            if integer(&doc["summary"][key])? != actual {
                return Err(invalid("scene summary count mismatch"));
            }
        }
        let mut source = doc["source"].clone();
        if !source.is_object() {
            return Err(invalid("scene source identity must be an object"));
        }
        source["cooked_manifest_sha256"] =
            Value::String(format!("{:x}", Sha256::digest(&manifest_bytes)));
        if let Some(format) = doc.pointer("/recipe/texture_policy/reflection_probes") {
            if !matches!(format.as_str(), Some("bc6h" | "rgba32f")) {
                return Err(invalid("unknown reflection probe cook format"));
            }
            source["reflection_probe_format"] = format.clone();
        }
        source["image_count"] = Value::from(image_records.len());
        source["texture_count"] = Value::from(textures.len());
        let source_images = &images[..image_records.len()];
        source["material_texture_mips"] =
            Value::from(source_images.iter().map(|i| i.mips.len()).sum::<usize>());
        source["material_texture_bytes"] = Value::from(
            source_images
                .iter()
                .map(TextureImage::byte_length)
                .sum::<usize>(),
        );
        source["primitive_count"] = Value::from(primitives.len());
        source["triangle_count"] = Value::from(triangle_count);
        Ok(Self {
            lightmap_uv,
            vertices,
            indices,
            primitives,
            images,
            materials,
            mesh_count,
            source,
        })
    }

    /// Small consumer-owned input: opaque dielectric/metal, normal map/emissive,
    /// masked alpha-zero, and blended alpha-zero. The last two remain selectable
    /// after the explicit opaque override for mutation-sensitivity checks.
    pub fn fixture(force_opaque: bool) -> Self {
        let mut images = Vec::new();
        let defaults = fallback_images(&mut images);
        let zero = images.len();
        images.push(TextureImage {
            compressed: None,
            width: 1,
            height: 1,
            srgb: true,
            mips: std::iter::once(0..4).collect(),
            pixels: Pixels::Owned(vec![255, 128, 64, 0]),
        });
        let tilted = images.len();
        images.push(TextureImage {
            compressed: None,
            width: 1,
            height: 1,
            srgb: false,
            mips: std::iter::once(0..4).collect(),
            pixels: Pixels::Owned(vec![220, 128, 220, 255]),
        });
        let mut materials = Vec::new();
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut primitives = Vec::new();
        for id in 0..8 {
            let x = (id % 4) as f32 * 0.8 - 1.2;
            let y = if id < 4 { 0.5 } else { -0.5 };
            let vstart = vertices.len();
            let istart = indices.len();
            for [px, py, u, v] in [
                [-0.35, -0.4, 0., 1.],
                [0.35, -0.4, 1., 1.],
                [0.35, 0.4, 1., 0.],
                [-0.35, 0.4, 0., 0.],
            ] {
                for value in [x + px, y + py, 0., 0., 0., 1., 1., 0., 0., 1., u, v] {
                    vertices.extend_from_slice(&f32::to_le_bytes(value));
                }
            }
            let winding = if matches!(id, 3 | 6) {
                [0_u32, 2, 1, 0, 3, 2]
            } else {
                [0, 1, 2, 0, 2, 3]
            };
            for word in winding {
                indices.extend_from_slice(&word.to_le_bytes());
            }
            let mut record = serde_json::json!({"id":id,"base_factor":[0.8,0.4,0.2,1.0],"emissive":[0.0,0.0,0.0],
                "normal_scale":1.0,"metallic":0.0,"roughness":0.6,"occlusion_strength":1.0,"alpha_cutoff":0.5,
                "alpha_mode":"OPAQUE","double_sided":false,"base_color_texture":null,"normal_texture":null,
                "orm_texture":null,"occlusion_texture":null,"emissive_texture":null});
            if id == 1 {
                record["metallic"] = Value::from(1.0);
                record["roughness"] = Value::from(0.2);
            }
            if id == 2 {
                record["normal_texture"] = Value::from(1);
                record["emissive"] = serde_json::json!([0.15, 0.05, 0.]);
            }
            if id == 3 {
                record["double_sided"] = Value::Bool(true);
            }
            if matches!(id, 4 | 5) {
                record["base_color_texture"] = Value::from(0);
                record["alpha_mode"] = Value::from(if id == 4 { "MASK" } else { "BLEND" });
            }
            if id == 7 {
                record["alpha_mode"] = Value::from("BLEND");
                record["base_factor"] = serde_json::json!([0.1, 0.4, 0.8, 0.5]);
            }
            let mat = material(
                &record,
                id,
                &[zero, tilted],
                &images,
                defaults,
                force_opaque,
            )
            .expect("audited fixture material");
            materials.push(mat);
            primitives.push(Primitive {
                surface: None,
                lightmap: Some((0..32, [4, 4])),
                material: id,
                vertices: vstart..vertices.len(),
                indices: istart..indices.len(),
                index_count: 6,
                center: [x, y, 0.],
            });
        }
        Self {
            lightmap_uv: [0f32, 1., 1., 1., 1., 0., 0., 0.]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
            vertices,
            indices,
            primitives,
            images,
            materials,
            mesh_count: 8,
            source: serde_json::json!({"fixture":"eight-material-v1","image_count":6,"texture_count":2,"primitive_count":8}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_bistro_ao_policy_only_changes_the_environment_contribution() {
        let mut scene = Scene::fixture(false);
        let original: Vec<_> = scene
            .materials
            .iter()
            .map(|m| (m.uniform, m.textures))
            .collect();
        let image_count = scene.images.len();
        scene.disable_environment_occlusion();
        for (material, (uniform, textures)) in scene.materials.iter().zip(original) {
            assert_eq!(material.uniform[..40], uniform[..40]);
            assert_eq!(material.uniform[44..], uniform[44..]);
            assert_eq!(material.uniform[40..44], 0f32.to_le_bytes());
            assert_eq!(material.textures, textures);
        }
        assert_eq!(scene.images.len(), image_count);
        assert_eq!(
            scene.source["applied_material_policy"]["ambient_occlusion_enabled"],
            false
        );
    }

    struct CookedFixture {
        root: PathBuf,
        doc: Value,
    }
    impl CookedFixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "renderer-bistro-loader-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            let scene = Scene::fixture(false);
            let blob = |path: &str, bytes: &[u8]| {
                std::fs::write(root.join(path), bytes).unwrap();
                serde_json::json!({"path":path,"byte_length":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))})
            };
            let mut vertices = blob("vertices.bin", &scene.vertices);
            vertices["count"] = Value::from(scene.vertices.len() / 48);
            let mut indices = blob("indices.bin", &scene.indices);
            indices["count"] = Value::from(scene.indices.len() / 4);
            let images:Vec<_>=scene.images.iter().enumerate().map(|(id,image)| {
                let mut record=blob(&format!("image-{id}.bin"),&image.load_bytes().unwrap());
                record["id"]=Value::from(id);
                record["role"]=Value::from(match id {0|4=>"base_color",1|5=>"normal",2=>"orm",_=>"emissive"});
                record["colorspace"]=Value::from(if image.srgb {"srgb"} else {"linear"});
                record["width"]=Value::from(1);record["height"]=Value::from(1);
                record["mip_levels"]=Value::from(1);
                record["mips"]=serde_json::json!([{"level":0,"width":1,"height":1,"offset":0,"byte_length":4}]);
                record
            }).collect();
            let materials:Vec<_>=scene.materials.iter().enumerate().map(|(id,m)| {
                let f=|i:usize| f32::from_le_bytes(m.uniform[i*4..i*4+4].try_into().unwrap());
                let mode=u32::from_le_bytes(m.uniform[48..52].try_into().unwrap());
                serde_json::json!({"id":id,"base_factor":[f(0),f(1),f(2),f(3)],"emissive":[f(4),f(5),f(6)],
                    "normal_scale":f(7),"metallic":f(8),"roughness":f(9),"occlusion_strength":f(10),"alpha_cutoff":f(11),
                    "alpha_mode":(["OPAQUE","MASK","BLEND"][mode as usize]),"double_sided":m.uniform[52]!=0,
                    "base_color_texture":m.textures[0],"normal_texture":m.textures[1],"orm_texture":m.textures[2],
                    "emissive_texture":m.textures[3],"occlusion_texture":m.textures[2]})
            }).collect();
            let primitives:Vec<_>=scene.primitives.iter().enumerate().map(|(id,p)| {
                let mut low=[f32::INFINITY;3];let mut high=[f32::NEG_INFINITY;3];
                for vertex in scene.vertices[p.vertices.clone()].as_chunks::<48>().0 {
                    let position=validate_vertex(vertex).unwrap();
                    for i in 0..3 {low[i]=low[i].min(position[i]);high[i]=high[i].max(position[i]);}
                }
                serde_json::json!({"id":id,"mesh":id,"material":p.material,"vertex_offset":p.vertices.start,
                    "vertex_count":4,"index_offset":p.indices.start,"index_count":6,"max_index":3,"bounds_min":low,"bounds_max":high})
            }).collect();
            let meshes: Vec<_> = (0..scene.mesh_count)
                .map(|id| {
                    serde_json::json!({"id":id,"primitives":[id],
                "world_transform":[1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1]})
                })
                .collect();
            let textures: Vec<_> = (0..images.len())
                .map(|id| serde_json::json!({"id":id,"source":id,"sampler":0}))
                .collect();
            let doc = serde_json::json!({"schema_version":1,"vertex_stride":48,"vertices":vertices,"indices":indices,
                "images":images,"materials":materials,"primitives":primitives,"meshes":meshes,"textures":textures,
                "samplers":[{"magFilter":9729,"minFilter":9987}],"source":{"fixture":"roundtrip"},
                "summary":{"mesh_count":8,"primitive_count":8,"material_count":8,"image_count":6,"triangles":16}});
            Self { root, doc }
        }
        fn manifest(&self) -> PathBuf {
            let path = self.root.join("manifest.json");
            std::fs::write(&path, serde_json::to_vec(&self.doc).unwrap()).unwrap();
            path
        }
    }
    impl Drop for CookedFixture {
        fn drop(&mut self) {
            // This unique directory was created by this instance, never reused.
            assert!(self.root.starts_with(std::env::temp_dir()));
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn fixture_preserves_alpha_and_override_changes_only_effective_material() {
        let original = Scene::fixture(false);
        let opaque = Scene::fixture(true);
        assert_eq!(original.primitives.len(), 8);
        assert_eq!(
            original.materials.iter().filter(|m| m.alpha_zero).count(),
            2
        );
        assert!(original.materials[5].blend);
        for (a, b) in original.materials.iter().zip(&opaque.materials) {
            assert_eq!(a.alpha_zero, b.alpha_zero);
            assert!(!b.blend);
            assert_eq!(u32::from_le_bytes(b.uniform[48..52].try_into().unwrap()), 0);
        }
        assert_eq!(original.vertices, opaque.vertices);
        assert_eq!(original.indices, opaque.indices);
        assert_eq!(
            original.images[4].load_bytes().unwrap(),
            opaque.images[4].load_bytes().unwrap()
        );
    }

    #[test]
    fn invalid_vertex_basis_and_nonfinite_components_are_rejected() {
        let scene = Scene::fixture(false);
        assert!(validate_vertex(&scene.vertices[..48]).is_ok());
        for (offset, value) in [(12, 2_f32), (24, 0.), (36, 0.), (0, f32::NAN)] {
            let mut bad = scene.vertices[..48].to_vec();
            bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(validate_vertex(&bad).is_err());
        }
    }

    #[test]
    fn separate_ao_and_wrong_colorspace_are_explicit_errors() {
        let mut images = Vec::new();
        let defaults = fallback_images(&mut images);
        let mut material_doc = serde_json::json!({"id":0,"base_factor":[1.,1.,1.,1.],"emissive":[0.,0.,0.],
            "normal_scale":1.,"metallic":0.,"roughness":1.,"occlusion_strength":1.,"alpha_cutoff":0.5,
            "alpha_mode":"OPAQUE","double_sided":false,"base_color_texture":null,"normal_texture":null,
            "orm_texture":null,"occlusion_texture":0,"emissive_texture":null});
        assert!(material(&material_doc, 0, &[defaults[2]], &images, defaults, false).is_err());
        material_doc["occlusion_texture"] = Value::Null;
        material_doc["base_color_texture"] = Value::from(0);
        assert!(material(&material_doc, 0, &[defaults[2]], &images, defaults, false).is_err());
    }

    #[test]
    fn absent_maps_are_identity_and_cutoff_above_one_is_fully_masked() {
        let mut images = Vec::new();
        let defaults = fallback_images(&mut images);
        let doc = serde_json::json!({"id":0,"base_factor":[1.,1.,1.,1.],"emissive":[0.5,0.25,0.125],
            "normal_scale":1.,"metallic":0.5,"roughness":0.5,"occlusion_strength":1.,"alpha_cutoff":2.,
            "alpha_mode":"MASK","double_sided":false,"base_color_texture":null,"normal_texture":null,
            "orm_texture":0,"occlusion_texture":null,"emissive_texture":null});
        let material = material(&doc, 0, &[defaults[2]], &images, defaults, false).unwrap();
        assert_eq!(
            f32::from_le_bytes(material.uniform[28..32].try_into().unwrap()),
            0.,
            "no normal perturbation"
        );
        assert_eq!(
            f32::from_le_bytes(material.uniform[40..44].try_into().unwrap()),
            0.,
            "no occlusion"
        );
        assert_eq!(
            images[material.textures[3]].load_bytes().unwrap(),
            [255; 4],
            "emissive factor remains authoritative"
        );
        assert!(
            material.alpha_zero,
            "a legal cutoff greater than one masks the complete surface"
        );
    }

    #[test]
    fn cooked_roundtrip_establishes_geometry_and_original_alpha_once() {
        let fixture = CookedFixture::new();
        let scene = Scene::load(&fixture.manifest(), false).unwrap();
        assert_eq!(scene.mesh_count, 8);
        assert_eq!(scene.source["image_count"], 6);
        assert_eq!(scene.primitives.len(), 8);
        assert_eq!(scene.materials.iter().filter(|m| m.alpha_zero).count(), 2);
        assert_eq!(scene.images[0].byte_length(), 4);
        // Non-alpha images are streamed and checked at their GPU-upload boundary.
        std::fs::write(fixture.root.join("image-0.bin"), [0; 4]).unwrap();
        assert!(scene.images[0].load_bytes().is_err());
        assert_eq!(scene.images[4].load_bytes().unwrap(), [255, 128, 64, 0]);
    }

    #[test]
    fn compressed_alpha_and_bake_mip_use_cooked_semantics() {
        let mut fixture = CookedFixture::new();
        let mut gpu = [0xff; 16];
        // BC3 alpha endpoints and indices select zero; the color block is opaque white.
        gpu[..8].fill(0);
        let rgba = [255, 255, 255, 0];
        std::fs::write(fixture.root.join("image-4.bin"), gpu).unwrap();
        std::fs::write(fixture.root.join("small.rgba"), rgba).unwrap();
        let image = &mut fixture.doc["images"][4];
        image["format"] = Value::from("bc3");
        image["byte_length"] = Value::from(16);
        image["sha256"] = Value::from(format!("{:x}", Sha256::digest(gpu)));
        image["mips"][0]["byte_length"] = Value::from(16);
        image["mips"][0]["alpha_min"] = Value::from(0);
        image["mips"][0]["alpha_max"] = Value::from(0);
        image["bake_mip"] = serde_json::json!({"level":0,"path":"small.rgba","byte_length":4,"sha256":format!("{:x}",Sha256::digest(rgba))});
        let scene = Scene::load(&fixture.manifest(), false).unwrap();
        assert_eq!(scene.images[4].format(), ImageFormat::Bc3RgbaSrgb);
        assert_eq!(scene.images[4].load_bytes().unwrap(), gpu);
        assert_eq!(scene.materials.iter().filter(|m| m.alpha_zero).count(), 2);
        #[cfg(feature = "bake-tool")]
        {
            assert_eq!(scene.images[4].bake_pixels().unwrap(), (0, rgba.to_vec()));
            std::fs::write(fixture.root.join("small.rgba"), [1; 4]).unwrap();
            assert!(scene.images[4].bake_pixels().is_err());
        }
        fixture.doc["images"][4]["mips"][0]["byte_length"] = Value::from(4);
        assert!(Scene::load(&fixture.manifest(), false).is_err());
    }

    #[test]
    fn malformed_ranges_proofs_mips_and_external_paths_fail_before_publication() {
        let mut fixture = CookedFixture::new();
        let original = fixture.doc.clone();
        for case in 0..6 {
            fixture.doc = original.clone();
            match case {
                0 => fixture.doc["primitives"][0]["max_index"] = Value::from(2),
                1 => fixture.doc["primitives"][1]["vertex_offset"] = Value::from(0),
                2 => fixture.doc["meshes"][0]["primitives"] = serde_json::json!([0, 1]),
                3 => fixture.doc["images"][0]["mip_levels"] = Value::from(2),
                4 => fixture.doc["vertices"]["path"] = Value::from("../outside.bin"),
                _ => fixture.doc["materials"][0]["base_color_texture"] = Value::from(1000),
            }
            assert!(
                Scene::load(&fixture.manifest(), false).is_err(),
                "case {case}"
            );
        }
    }

    #[test]
    fn geometry_hash_failure_cannot_be_overridden_by_plausible_summaries() {
        let fixture = CookedFixture::new();
        let mut bytes = std::fs::read(fixture.root.join("indices.bin")).unwrap();
        bytes[..4].copy_from_slice(&3_u32.to_le_bytes());
        std::fs::write(fixture.root.join("indices.bin"), bytes).unwrap();
        assert!(Scene::load(&fixture.manifest(), false).is_err());
    }
}
