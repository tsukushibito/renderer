//! Linear HDR, paired-scenario specular reflection atlas consumed by standard PBR.
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

/// Each probe occupies a metadata row and two equal stacks of GGX-filtered
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
    const MAGIC: &[u8; 8] = b"RSPROB01";

    /// Validate the native atlas at the import or offline creation boundary.
    pub fn from_rgba32f(pixels: Vec<u8>) -> Result<Self, GraphError> {
        let stride = Self::WIDTH * Self::ROWS * 16;
        if !pixels.len().is_multiple_of(stride)
            || !(1..=Self::MAX_COUNT).contains(&(pixels.len() / stride))
        {
            return Err(GraphError::invalid_input("invalid reflection atlas extent"));
        }
        for probe in pixels.chunks_exact(stride) {
            let f = |i: usize| f32::from_le_bytes(probe[i * 4..i * 4 + 4].try_into().unwrap());
            let xyz = |start: usize| std::array::from_fn(|i| f(start + i));
            ReflectionProbeDesc {
                position: xyz(0),
                blend_distance: f(3),
                min: xyz(4),
                max: xyz(8),
            }
            .validate()?;
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
        Ok(Self { pixels })
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, GraphError> {
        let payload = bytes
            .strip_prefix(Self::MAGIC)
            .ok_or_else(|| GraphError::invalid_input("unknown reflection probe asset version"))?;
        if payload.len() > Self::MAX_COUNT * Self::WIDTH * Self::ROWS * 16 {
            return Err(GraphError::invalid_input(
                "oversized reflection probe asset",
            ));
        }
        Self::from_rgba32f(payload.to_vec())
    }
    pub fn write(&self, mut writer: impl std::io::Write) -> std::io::Result<()> {
        writer.write_all(Self::MAGIC)?;
        writer.write_all(&self.pixels)
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
    pub fn rgba32f(&self) -> &[u8] {
        &self.pixels
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
/// Probe atlas resident in this graph. Both endpoints stay resident during blending.
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
