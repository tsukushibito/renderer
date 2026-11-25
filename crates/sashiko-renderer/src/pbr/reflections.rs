//! Linear HDR, single- or paired-scenario specular reflection atlas consumed by standard PBR.
use sashiko_renderer_core::graph::{GraphError, ImageView};

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

/// A radiance atlas with block-aligned roughness levels and exact probe records.
pub struct ReflectionProbes {
    pixels: Vec<u8>,
    probes: Vec<ReflectionProbeDesc>,
    scenarios: usize,
    format: sashiko_renderer_core::ImageFormat,
}

impl ReflectionProbes {
    pub const WIDTH: usize = 128;
    pub const HEIGHT: usize = 64;
    pub const LEVELS: usize = 7;
    pub const MAX_COUNT: usize = 32;
    /// One scenario occupies heights 64, 32, 16, 8, 4, 4, 4.
    pub const SCENARIO_ROWS: usize = 132;
    /// Two grid vectors, 64 masks and 32 triples of float4 box records.
    pub const METADATA_BYTES: usize = 32 + 64 * 4 + Self::MAX_COUNT * 48;

    pub fn new(
        probes: Vec<ReflectionProbeDesc>,
        scenarios: usize,
        format: sashiko_renderer_core::ImageFormat,
        pixels: Vec<u8>,
    ) -> Result<Self, GraphError> {
        use sashiko_renderer_core::ImageFormat::*;
        if !(1..=Self::MAX_COUNT).contains(&probes.len())
            || !(1..=2).contains(&scenarios)
            || !matches!(format, Rgba32Float | Bc6hRgbUfloat)
        {
            return Err(GraphError::invalid_input(
                "invalid reflection atlas descriptor",
            ));
        }
        for p in &probes {
            p.validate()?;
        }
        let height = Self::SCENARIO_ROWS * scenarios * probes.len();
        if format.packed_byte_len(Self::WIDTH as u32, height as u32) != Some(pixels.len() as u64) {
            return Err(GraphError::invalid_input("invalid reflection atlas length"));
        }
        if format == Rgba32Float
            && !pixels.as_chunks::<4>().0.iter().all(|v| {
                let v = f32::from_le_bytes(*v);
                v.is_finite() && v >= 0.
            })
        {
            return Err(GraphError::invalid_input("invalid reflection radiance"));
        }
        Ok(Self {
            probes,
            scenarios,
            format,
            pixels,
        })
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, GraphError> {
        let invalid = || GraphError::invalid_input("invalid reflection asset");
        let (magic, data) = bytes.split_at_checked(8).ok_or_else(invalid)?;
        if magic == b"RSPROB02" {
            let (header, data) = data.split_at_checked(4).ok_or_else(invalid)?;
            let format = match header[0] {
                0 => sashiko_renderer_core::ImageFormat::Rgba32Float,
                1 => sashiko_renderer_core::ImageFormat::Bc6hRgbUfloat,
                _ => return Err(invalid()),
            };
            let count = header[2] as usize;
            if header[3] != 0
                || !(1..=Self::MAX_COUNT).contains(&count)
                || !(1..=2).contains(&header[1])
            {
                return Err(invalid());
            }
            let (records, pixels) = data.split_at_checked(count * 48).ok_or_else(invalid)?;
            let expected = format.packed_byte_len(
                Self::WIDTH as u32,
                (count * header[1] as usize * Self::SCENARIO_ROWS) as u32,
            );
            if expected != Some(pixels.len() as u64) {
                return Err(invalid());
            }
            return Self::new(
                records
                    .as_chunks::<48>()
                    .0
                    .iter()
                    .map(decode_record)
                    .collect(),
                header[1] as usize,
                format,
                pixels.to_vec(),
            );
        }
        let scenarios = match magic {
            b"RSPRST01" => 1,
            b"RSPROB01" => 2,
            _ => return Err(invalid()),
        };
        let row = Self::WIDTH * 16;
        let stride = row * (1 + 127 * scenarios);
        if !data.len().is_multiple_of(stride)
            || !(1..=Self::MAX_COUNT).contains(&(data.len() / stride))
        {
            return Err(invalid());
        }
        let mut probes = Vec::new();
        let mut pixels = Vec::new();
        for old in data.chunks_exact(stride) {
            probes.push(decode_record(
                old.first_chunk().expect("validated record stride"),
            ));
            let mut rows = &old[row..];
            for _ in 0..scenarios {
                for level in 0..Self::LEVELS {
                    let height = Self::HEIGHT >> level;
                    let (plane, rest) = rows.split_at(row * height);
                    rows = rest;
                    pixels.extend_from_slice(plane);
                    for _ in height..height.next_multiple_of(4) {
                        pixels.extend_from_slice(&plane[plane.len() - row..]);
                    }
                }
            }
        }
        Self::new(
            probes,
            scenarios,
            sashiko_renderer_core::ImageFormat::Rgba32Float,
            pixels,
        )
    }
    pub fn write(&self, mut writer: impl std::io::Write) -> std::io::Result<()> {
        writer.write_all(b"RSPROB02")?;
        writer.write_all(&[
            u8::from(self.format == sashiko_renderer_core::ImageFormat::Bc6hRgbUfloat),
            self.scenarios as u8,
            self.probes.len() as u8,
            0,
        ])?;
        for p in &self.probes {
            writer.write_all(&encode_record(p))?;
        }
        writer.write_all(&self.pixels)
    }
    pub fn probes(&self) -> &[ReflectionProbeDesc] {
        &self.probes
    }
    pub fn scenario_count(&self) -> usize {
        self.scenarios
    }
    pub fn extent(&self) -> [u32; 2] {
        [
            Self::WIDTH as u32,
            (self.probes.len() * self.scenarios * Self::SCENARIO_ROWS) as u32,
        ]
    }
    pub fn image_desc(&self) -> sashiko_renderer_core::graph::ImageDesc {
        let [w, h] = self.extent();
        sashiko_renderer_core::graph::ImageDesc::texture_2d(w, h, self.format)
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
    pub const fn metadata_desc() -> sashiko_renderer_core::graph::BufferDesc {
        sashiko_renderer_core::graph::BufferDesc {
            size: Self::METADATA_BYTES as u64,
        }
    }
    pub fn bind_metadata(
        buffer: sashiko_renderer_core::graph::BufferHandle,
    ) -> sashiko_renderer_core::graph::Binding {
        sashiko_renderer_core::graph::Binding {
            group: 2,
            binding: 5,
            resource: sashiko_renderer_core::graph::BindingResource::Buffer {
                buffer,
                offset: 0,
                size: Self::METADATA_BYTES as u64,
            },
        }
    }
    pub fn upload_metadata(
        graph: &mut sashiko_renderer_core::graph::Graph,
        bytes: &[u8; Self::METADATA_BYTES],
    ) -> Result<sashiko_renderer_core::graph::BufferHandle, GraphError> {
        let buffer = graph.buffer("reflection metadata", Self::metadata_desc())?;
        graph.upload_buffer("reflection metadata", buffer, 0, bytes)?;
        Ok(buffer)
    }
    pub fn metadata(&self) -> [u8; Self::METADATA_BYTES] {
        let mut bytes = [0; Self::METADATA_BYTES];
        bytes[12..16].copy_from_slice(&(self.probes.len() as u32).to_le_bytes());
        bytes[28..32].copy_from_slice(&(self.scenarios as u32).to_le_bytes());
        if self.probes.len() > 4 {
            build_candidate_grid(&self.probes, &mut bytes);
        }
        for (dest, p) in bytes[288..]
            .as_chunks_mut::<48>()
            .0
            .iter_mut()
            .zip(&self.probes)
        {
            dest.copy_from_slice(&encode_record(p));
        }
        bytes
    }
}

fn decode_record(bytes: &[u8; 48]) -> ReflectionProbeDesc {
    let f = |i: usize| f32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().unwrap());
    let xyz = |start: usize| std::array::from_fn(|i| f(start + i));
    ReflectionProbeDesc {
        position: xyz(0),
        blend_distance: f(3),
        min: xyz(4),
        max: xyz(8),
    }
}
fn encode_record(p: &ReflectionProbeDesc) -> [u8; 48] {
    let mut bytes = [0; 48];
    for (i, v) in p
        .position
        .into_iter()
        .chain([p.blend_distance])
        .chain(p.min)
        .chain([0.])
        .chain(p.max)
        .chain([0.])
        .enumerate()
    {
        bytes[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes
}

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
    if !scale.iter().all(|s| s.is_normal()) {
        return;
    }
    for axis in 0..3 {
        header[4 * axis..4 * axis + 4].copy_from_slice(&lo[axis].to_le_bytes());
        header[16 + 4 * axis..20 + 4 * axis].copy_from_slice(&scale[axis].to_le_bytes());
    }
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
        header[32 + 4 * i..36 + 4 * i].copy_from_slice(&m.to_le_bytes());
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
    fn legacy_levels_are_repacked_without_crossing_block_boundaries() {
        let desc = ReflectionProbeDesc {
            position: [-2.; 3],
            min: [-4.; 3],
            max: [1.; 3],
            blend_distance: 0.5,
        };
        let row = ReflectionProbes::WIDTH * 16;
        for (magic, scenarios) in [(b"RSPRST01", 1), (b"RSPROB01", 2)] {
            let mut file = magic.to_vec();
            file.extend(encode_record(&desc));
            file.resize(8 + row, 0);
            for scenario in 0..scenarios {
                for level in 0..7 {
                    let value = ((scenario * 7 + level + 1) as f32).to_le_bytes();
                    file.extend(value.repeat(row * (64 >> level) / 4));
                }
            }
            let probes = ReflectionProbes::from_bytes(&file).unwrap();
            assert_eq!(probes.probes(), &[desc]);
            for (index, plane) in probes.pixels().chunks_exact(row * 132).enumerate() {
                assert_eq!(
                    plane[124 * row..128 * row],
                    ((index * 7 + 6) as f32).to_le_bytes().repeat(row)
                );
                assert_eq!(
                    plane[128 * row..],
                    ((index * 7 + 7) as f32).to_le_bytes().repeat(row)
                );
            }
        }
    }

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
        let mask = |cell: usize| {
            u32::from_le_bytes(header[32 + 4 * cell..36 + 4 * cell].try_into().unwrap())
        };
        assert_eq!(mask(0) & 1, 1);
        assert_eq!(mask(0) & (1 << 31), 0);
        assert_ne!(mask(3) & (1 << 31), 0);
        assert_ne!(mask(0) & (1 << 8), 0);
        assert_ne!(mask(1) & (1 << 7), 0);
    }
}
