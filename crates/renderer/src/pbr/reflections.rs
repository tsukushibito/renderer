//! Linear HDR, single- or paired-scenario specular reflection atlas consumed by standard PBR.
use renderer_core::graph::{GraphError, ImageView};

/// World-space capture point and axis-aligned parallax/influence volume.
/// Influence rises linearly inward from each face over `blend_distance` metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReflectionProbeDesc {
    pub position: [f32; 3],
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub blend_distance: f32,
}

impl ReflectionProbeDesc {
    pub fn validate(self) -> Result<(), GraphError> {
        if !(self.blend_distance.is_finite() && self.blend_distance > 0.)
            || !(0..3).all(|i| {
                self.min[i].is_finite()
                    && self.max[i].is_finite()
                    && self.min[i] < self.position[i]
                    && self.position[i] < self.max[i]
                    && (self.max[i] - self.min[i]).is_finite()
            })
        {
            return Err(GraphError::invalid_input(
                "invalid reflection probe box, position or fade",
            ));
        }
        Ok(())
    }
}

/// Each probe occupies a metadata row and one or two equal stacks of GGX-filtered
/// lat-long maps. All levels span the texture width so longitude wraps with
/// ordinary hardware filtering. Latitude resolution halves at each level.
pub struct ReflectionProbes {
    pixels: Vec<u8>,
}

impl ReflectionProbes {
    pub const WIDTH: usize = 128;
    pub const HEIGHT: usize = 64;
    pub const LEVELS: usize = 7;
    pub const ROWS: usize = 1 + 2 * (2 * Self::HEIGHT - 1);
    pub const MAX_COUNT: usize = 32;
    const SINGLE_MAGIC: &[u8; 8] = b"RSPRST01";
    const MAGIC: &[u8; 8] = b"RSPROB01";

    /// Validate the native atlas at the import or offline creation boundary.
    pub fn from_rgba32f(mut pixels: Vec<u8>) -> Result<Self, GraphError> {
        let single = pixels.len().is_multiple_of(Self::WIDTH * 128 * 16);
        let rows = if single { 128 } else { Self::ROWS };
        let stride = Self::WIDTH * rows * 16;
        let mut boxes = Vec::new();
        if !pixels.len().is_multiple_of(stride)
            || !(1..=Self::MAX_COUNT).contains(&(pixels.len() / stride))
        {
            return Err(GraphError::invalid_input("invalid reflection atlas extent"));
        }
        for probe in pixels.chunks_exact(stride) {
            let f = |i: usize| f32::from_le_bytes(probe[i * 4..i * 4 + 4].try_into().unwrap());
            let xyz = |start: usize| std::array::from_fn(|i| f(start + i));
            let desc = ReflectionProbeDesc {
                position: xyz(0),
                blend_distance: f(3),
                min: xyz(4),
                max: xyz(8),
            };
            desc.validate()?;
            boxes.push(desc);
            if !probe[Self::WIDTH * 16..]
                .as_chunks::<4>()
                .0
                .iter()
                .all(|b| {
                    let x = f32::from_le_bytes(*b);
                    x.is_finite() && x >= 0.
                })
            {
                return Err(GraphError::invalid_input("invalid reflection radiance"));
            }
        }
        // Populate reserved header texels even for earlier archives.
        if boxes.len() > 4 {
            build_candidate_grid(&boxes, &mut pixels[..Self::WIDTH * 16]);
        }
        Ok(Self { pixels })
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, GraphError> {
        let (header, payload) = bytes
            .split_at_checked(8)
            .ok_or_else(|| GraphError::invalid_input("truncated reflection atlas"))?;
        let rows = if header == Self::SINGLE_MAGIC {
            128
        } else if header == Self::MAGIC {
            Self::ROWS
        } else {
            return Err(GraphError::invalid_input(
                "unknown reflection probe asset version",
            ));
        };
        let stride = rows * Self::WIDTH * 16;
        if !payload.len().is_multiple_of(stride)
            || !(1..=Self::MAX_COUNT).contains(&(payload.len() / stride))
        {
            return Err(GraphError::invalid_input("invalid reflection atlas extent"));
        }
        Self::from_rgba32f(payload.to_vec())
    }
    pub fn write(&self, mut writer: impl std::io::Write) -> std::io::Result<()> {
        writer.write_all(if self.scenario_count() == 1 {
            Self::SINGLE_MAGIC
        } else {
            Self::MAGIC
        })?;
        writer.write_all(&self.pixels)
    }
    pub fn scenario_count(&self) -> usize {
        if self.extent()[1].is_multiple_of(128) {
            1
        } else {
            2
        }
    }
    pub fn extent(&self) -> [u32; 2] {
        [
            Self::WIDTH as u32,
            (self.pixels.len() / (Self::WIDTH * 16)) as u32,
        ]
    }
    pub fn image_desc(&self) -> renderer_core::graph::ImageDesc {
        let [width, height] = self.extent();
        renderer_core::graph::ImageDesc::rgba32_float(width, height)
    }
    /// Upload once at scene preparation and retain the managed image across frames.
    pub fn upload(
        &self,
        runtime: &mut renderer_core::runtime::Runtime,
        pool: &mut renderer_core::graph::GraphPool,
    ) -> Result<
        (
            renderer_core::runtime::FrameId,
            renderer_core::runtime::ManagedImage,
        ),
        renderer_core::runtime::TextureUploadError,
    > {
        runtime.upload_texture(
            pool,
            "local reflection probes",
            self.image_desc(),
            self.rgba32f(),
        )
    }
    pub fn into_rgba32f(self) -> Vec<u8> {
        self.pixels
    }
    pub fn rgba32f(&self) -> &[u8] {
        &self.pixels
    }
}

/// A 4x4x4 grid of conservative 32-bit candidate masks. Each half mask is
/// exactly representable in FP32. Original bit order preserves overlap sums.
fn build_candidate_grid(boxes: &[ReflectionProbeDesc], header: &mut [u8]) {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for b in boxes {
        for a in 0..3 {
            lo[a] = lo[a].min(b.min[a]);
            hi[a] = hi[a].max(b.max[a]);
        }
    }
    let scale: [f32; 3] = std::array::from_fn(|a| 4. / (hi[a] - lo[a]));
    let mut store = |index: usize, rgba: [f32; 4]| {
        for (offset, value) in rgba.into_iter().enumerate() {
            let byte = index * 16 + offset * 4;
            header[byte..byte + 4].copy_from_slice(&value.to_le_bytes());
        }
    };
    // Extreme finite coordinates can produce an unrepresentable grid. The
    // unindexed shader path remains valid for these boxes.
    if !scale.iter().all(|s| s.is_normal()) {
        store(3, [0.; 4]);
        return;
    }
    store(3, [lo[0], lo[1], lo[2], 1.]);
    store(4, [scale[0], scale[1], scale[2], 0.]);
    let mut masks = [0u32; 64];
    for (index, b) in boxes.iter().enumerate() {
        let cell = |p: [f32; 3], pad: f32| -> [usize; 3] {
            std::array::from_fn(|i| (((p[i] - lo[i]) * scale[i] + pad).clamp(0., 3.)) as usize)
        };
        let first = cell(b.min, -0.00001);
        let last = cell(b.max, 0.00001);
        for z in first[2]..=last[2] {
            for y in first[1]..=last[1] {
                for x in first[0]..=last[0] {
                    masks[x + 4 * y + 16 * z] |= 1 << index;
                }
            }
        }
    }
    for (i, m) in masks.into_iter().enumerate() {
        store(5 + i, [(m & 65535) as f32, (m >> 16) as f32, 0., 0.]);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ReflectionProbeView {
    #[default]
    Shaded,
    /// Red and green distinguish the first two volumes, blue shows global weight.
    Weights,
    /// Blended incoming specular radiance before applying the surface BRDF.
    Radiance,
}
/// Probe atlas resident in this graph. Paired scenarios share one atlas during blending.
#[derive(Debug, Clone, Copy)]
pub struct LocalReflections {
    pub atlas: ImageView,
    pub blend: f32,
    pub view: ReflectionProbeView,
}

impl ReflectionProbeView {
    /// Camera environment W retains its positive-shadow convention.
    pub fn camera_parameter(self, blend: f32, shadows: bool) -> Result<f32, GraphError> {
        if !(0.0..=1.0).contains(&blend) {
            return Err(GraphError::invalid_input(
                "invalid reflection scenario blend",
            ));
        }
        let mode = match self {
            Self::Shaded => 0.,
            Self::Weights => 2.,
            Self::Radiance => 4.,
        };
        Ok((2. + mode + blend) * if shadows { 1. } else { -1. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_keep_high_indices_and_shared_cell_faces() {
        let mut probes = [ReflectionProbeDesc {
            position: [0.; 3],
            min: [-1.; 3],
            max: [1.; 3],
            blend_distance: 0.1,
        }; 32];
        for (i, p) in probes.iter_mut().enumerate() {
            p.min[0] = i as f32;
            p.max[0] = i as f32 + 1.;
            p.position[0] = i as f32 + 0.5;
        }
        let mut header = [0u8; ReflectionProbes::WIDTH * 16];
        build_candidate_grid(&probes, &mut header);
        let word = |texel: usize, lane: usize| {
            let at = texel * 16 + lane * 4;
            f32::from_le_bytes(header[at..at + 4].try_into().unwrap()) as u32
        };
        let mask = |cell: usize| word(5 + cell, 0) | word(5 + cell, 1) << 16;
        assert_eq!(mask(0) & 1, 1);
        assert_eq!(mask(0) & (1 << 31), 0);
        assert_ne!(mask(3) & (1 << 31), 0);
        assert_ne!(mask(0) & (1 << 8), 0);
        assert_ne!(mask(1) & (1 << 7), 0);
    }
}
