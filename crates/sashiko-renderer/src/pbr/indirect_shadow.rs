//! View-local data for indirect diffuse shadows on static surface pages.
use super::{BakedLighting, PreparedView};
use sashiko_renderer_core::graph::{Graph, GraphError, ImageDesc, ImageView};

/// Shadow proxy with endpoints in a common coordinate space. Coincident endpoints define a sphere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowCapsule {
    a: [f32; 3],
    b: [f32; 3],
    radius: f32,
}

impl ShadowCapsule {
    pub fn new(a: [f32; 3], b: [f32; 3], radius: f32) -> Result<Self, GraphError> {
        if !radius.is_finite() || radius <= 0.0 || a.into_iter().chain(b).any(|v| !v.is_finite()) {
            return Err(GraphError::invalid_input("invalid indirect shadow capsule"));
        }
        Ok(Self { a, b, radius })
    }

    /// A sphere whose six-radius influence contains every proxy's influence.
    pub(crate) fn envelope(capsules: impl Iterator<Item = Self> + Clone) -> Option<Self> {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for c in capsules.clone() {
            for axis in 0..3 {
                lo[axis] = lo[axis].min(c.a[axis]).min(c.b[axis]);
                hi[axis] = hi[axis].max(c.a[axis]).max(c.b[axis]);
            }
        }
        let center = std::array::from_fn(|axis| lo[axis].midpoint(hi[axis]));
        let mut radius = 0.0f64;
        for c in capsules {
            for point in [c.a, c.b] {
                let length = (0..3)
                    .map(|i| (f64::from(point[i]) - f64::from(center[i])).powi(2))
                    .sum::<f64>()
                    .sqrt();
                radius = radius.max(length / 6.0 + f64::from(c.radius));
            }
        }
        // Unknown or unrepresentable bounds retain the whole instance.
        if radius == 0.0 {
            return None;
        }
        Self::new(center, center, (radius as f32).next_up()).ok()
    }

    pub(crate) fn may_affect(self, view: &PreparedView, transform: super::Transform) -> bool {
        self.transformed(transform)
            .map_or(true, |sphere| tile_bounds(sphere, view.matrix()).is_some())
    }

    pub(crate) fn transformed(self, transform: super::Transform) -> Result<Self, GraphError> {
        // sqrt(||A^T A||_infinity) bounds the largest singular value even for shear,
        // and preserves the radius under rotations and uniform scales.
        let rows = transform.model;
        let mut norm = 0.0f64;
        for i in 0..3 {
            let sum = (0..3)
                .map(|j| {
                    rows.iter()
                        .map(|r| f64::from(r[i]) * f64::from(r[j]))
                        .sum::<f64>()
                        .abs()
                })
                .sum();
            norm = norm.max(sum);
        }
        Self::new(
            transform.transform_point(self.a),
            transform.transform_point(self.b),
            (f64::from(self.radius) * norm.sqrt()) as f32,
        )
    }
}

/// A rigid mesh-space capsule, or a bind-pose capsule following one joint
/// of the mesh's final skinning palette.
#[derive(Clone, Copy, Debug)]
pub struct ShadowProxy {
    pub capsule: ShadowCapsule,
    pub joint: Option<u16>,
}

/// Per-view evaluation of diffuse indirect shadows on lightmapped receivers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IndirectShadowMode {
    Off,
    #[default]
    Full,
    Half,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShadowStage {
    Off,
    Full,
    Receiver,
    Resolve,
}

const GRID: usize = 16;
const TILES: usize = GRID * GRID;

#[cfg(feature = "memory-profiling")]
#[derive(Clone, Copy, Debug, Default)]
pub struct ShadowTileStats {
    pub active_capsules: usize,
    pub candidate_indices: usize,
    pub busiest_tile: usize,
}

/// One RGBA32F texel per tile stores the start and count of its index range.
/// Indices address three-texel capsule records. Integer texel addresses are
/// limited to the exactly representable f32 range at the preparation boundary.
pub(crate) struct Workspace {
    #[cfg(feature = "memory-profiling")]
    pub stats: ShadowTileStats,
    pub desc: ImageDesc,
    pub light_tail: Option<ImageDesc>,
    pub bytes: Vec<u8>,
    rectangles: Vec<[[usize; 2]; 2]>,
    capacity: usize,
}

impl Workspace {
    pub fn new(capacity: usize, light_tail: Option<ImageDesc>) -> Result<Self, GraphError> {
        let texels = capacity
            .checked_mul(3 + TILES / 4)
            .and_then(|n| n.checked_add(1 + TILES))
            .filter(|&n| n <= 1 << 22)
            .ok_or_else(|| GraphError::invalid_input("indirect shadow image address overflow"))?;
        let width = texels
            .min(1024)
            .max(light_tail.map_or(0, |d| d.width as usize));
        let height = (texels.div_ceil(width) as u32)
            .checked_add(light_tail.map_or(0, |d| d.height))
            .ok_or_else(|| GraphError::invalid_input("indirect shadow table height overflow"))?;
        let desc = ImageDesc::rgba32_float(width as u32, height);
        let size = desc
            .byte_len()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| GraphError::invalid_input("indirect shadow table size overflow"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| GraphError::invalid_input("indirect shadow upload allocation failed"))?;
        bytes.resize(size, 0);
        let mut rectangles = Vec::new();
        rectangles
            .try_reserve_exact(capacity)
            .map_err(|_| GraphError::invalid_input("indirect shadow bounds allocation failed"))?;
        let mut result = Self {
            #[cfg(feature = "memory-profiling")]
            stats: Default::default(),
            desc,
            bytes,
            rectangles,
            capacity,
            light_tail,
        };
        if let Some(lights) = light_tail {
            // Reserved components of tile zero locate the local-light records
            // for a combined Forward+ resolve. Tile updates write only x/y.
            result.word(6, lights.width as f32);
            result.word(7, lights.height as f32);
        }
        Ok(result)
    }

    fn word(&mut self, index: usize, value: f32) {
        self.bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
}

/// Sample the constant and linear luminance coefficients of the *irradiance*
/// polynomial. Visibility, relocation and scenario interpolation follow probe_volume.hlsli.
fn direction(volume: &BakedLighting, center: [f32; 3], blend: f32) -> Option<[f32; 4]> {
    let texels = volume.rgba32f().as_chunks::<16>().0;
    let texel = |at: usize| -> [f32; 4] {
        std::array::from_fn(|j| {
            f32::from_le_bytes(texels[at][4 * j..4 * j + 4].try_into().unwrap())
        })
    };
    let bounds = texel(0);
    let mut size = bounds[3];
    let mut f: [f32; 3] = std::array::from_fn(|j| (center[j] - bounds[j]) / size);
    if !f.iter().all(|x| (0.0..=1.0).contains(x)) {
        return None;
    }
    let count = volume.scenario_count();
    let header = 2 + count.max(2);
    let base = header + texel(1)[0] as usize * 3;
    let coeffs = volume.sh_order().coefficient_count();
    let stride = 2 + count * coeffs;
    let phase = blend * (count - 1) as f32;
    let key = phase as usize;
    let mut node = 0;
    loop {
        let at = header + node * 3;
        let first = texel(at)[0] as usize;
        if first == 0 {
            let mut sh = [0.0f64; 4];
            for i in 0..8 {
                let id = texel(at + 1 + i / 4)[i % 4] as usize;
                let p = base + id * stride;
                let minus = texel(p);
                let plus = texel(p + 1);
                let mut weight = minus[3];
                let mut visibility = 1.0f32;
                for j in 0..3 {
                    let side = ((i >> j) & 1) as f32;
                    weight *= if side == 0.0 { 1.0 - f[j] } else { f[j] };
                    let delta = (f[j] - side) * size - texel(p + 2 + j)[3];
                    let wall = if delta >= 0.0 { plus[j] } else { minus[j] };
                    visibility = visibility.min(
                        (1.0 + (wall - delta.abs()) / (size * 0.05).max(1e-6)).clamp(0.0, 1.0),
                    );
                }
                for (k, sum) in sh.iter_mut().enumerate() {
                    let luminance = |scenario| {
                        let c = texel(p + 2 + coeffs * scenario + k);
                        0.2126 * f64::from(c[0])
                            + 0.7152 * f64::from(c[1])
                            + 0.0722 * f64::from(c[2])
                    };
                    let a = luminance(key);
                    let b = luminance((key + 1).min(count - 1));
                    *sum +=
                        (a + (b - a) * f64::from(phase.fract())) * f64::from(weight * visibility);
                }
            }
            if sh[0] <= 0.0 {
                return None;
            }
            let length = sh[1].hypot(sh[2]).hypot(sh[3]);
            if length <= f64::EPSILON * sh[0] {
                return Some([0.; 4]);
            }
            return Some([
                (sh[1] / length) as f32,
                (sh[2] / length) as f32,
                (sh[3] / length) as f32,
                (length / sh[0]).min(1.0) as f32,
            ]);
        }
        node = first;
        for (j, coordinate) in f.iter_mut().enumerate() {
            let high = usize::from(*coordinate >= 0.5);
            node += high << j;
            *coordinate = (2.0 * *coordinate - high as f32).clamp(0.0, 1.0);
        }
        size *= 0.5;
    }
}

// Project a conservative world AABB. Crossing the eye plane covers all tiles.
fn tile_bounds(c: ShadowCapsule, matrix: &[[f32; 4]; 4]) -> Option<[[usize; 2]; 2]> {
    let mut lower = [f64::INFINITY; 2];
    let mut upper = [f64::NEG_INFINITY; 2];
    let mut max_w = f64::NEG_INFINITY;
    let mut before_near = true;
    let mut beyond_far = true;
    let mut crosses_projection_limit = false;
    for corner in 0..8 {
        let point: [f64; 3] = std::array::from_fn(|j| {
            let r = 6.0 * f64::from(c.radius);
            if corner & (1 << j) == 0 {
                f64::from(c.a[j].min(c.b[j])) - r
            } else {
                f64::from(c.a[j].max(c.b[j])) + r
            }
        });
        let clip = |row: usize| {
            f64::from(matrix[3][row])
                + (0..3)
                    .map(|j| point[j] * f64::from(matrix[j][row]))
                    .sum::<f64>()
        };
        let w = clip(3);
        let z = clip(2);
        let tolerance = 1e-6 * (1.0 + z.abs() + w.abs());
        before_near &= z < -tolerance;
        beyond_far &= z > w + tolerance;
        max_w = max_w.max(w);
        if w <= 1e-8 {
            crosses_projection_limit = true;
            continue;
        }
        for j in 0..2 {
            let v = (clip(j) / w + 1.0) * (GRID as f64 / 2.0);
            lower[j] = lower[j].min(v - 1e-4);
            upper[j] = upper[j].max(v + 1e-4);
        }
    }
    if max_w <= 0.0 || before_near || beyond_far {
        return None;
    }
    if crosses_projection_limit {
        return Some([[0, 0], [GRID - 1, GRID - 1]]);
    }
    if (0..2).any(|j| upper[j] < 0.0 || lower[j] >= GRID as f64) {
        return None;
    }
    Some([lower, upper].map(|xy| xy.map(|v| (v as usize).min(GRID - 1))))
}

impl Workspace {
    pub fn upload(
        &mut self,
        graph: &mut Graph,
        view: &PreparedView,
        volume: &BakedLighting,
        blend: f32,
        capsules: &[ShadowCapsule],
    ) -> Result<Option<ImageView>, GraphError> {
        if !self.encode(view.matrix(), view.extent, volume, blend, capsules)? {
            return Ok(None);
        }
        let image = graph.image("indirect shadow view", self.desc)?;
        graph.upload_image("indirect shadow upload", image, &self.bytes)?;
        Ok(Some(graph.image_view(image, 0, 0)?))
    }

    fn encode(
        &mut self,
        matrix: &[[f32; 4]; 4],
        extent: [u32; 2],
        volume: &BakedLighting,
        blend: f32,
        capsules: &[ShadowCapsule],
    ) -> Result<bool, GraphError> {
        self.rectangles.clear();
        #[cfg(feature = "memory-profiling")]
        {
            self.stats = Default::default();
        }
        let mut counts = [0usize; TILES];
        for &capsule in capsules {
            let Some([lo, hi]) = tile_bounds(capsule, matrix) else {
                continue;
            };
            let center = std::array::from_fn(|j| 0.5 * capsule.a[j] + 0.5 * capsule.b[j]);
            let Some(light) = direction(volume, center, blend) else {
                continue;
            };
            let index = self.rectangles.len();
            if index == self.capacity {
                return Err(GraphError::capacity_error(
                    sashiko_renderer_core::graph::GraphCapacityKind::InputRecords(
                        "visible indirect shadow capsules",
                    ),
                    self.capacity,
                    Some(index + 1),
                ));
            }
            self.rectangles.push([lo, hi]);
            let record = capsule
                .a
                .into_iter()
                .chain([capsule.radius])
                .chain(capsule.b)
                .chain([0.])
                .chain(light);
            for (j, value) in record.enumerate() {
                self.word(4 * (1 + TILES + 3 * index) + j, value);
            }
            for y in lo[1]..=hi[1] {
                for count in &mut counts[y * GRID + lo[0]..=y * GRID + hi[0]] {
                    *count += 1;
                }
            }
        }
        #[cfg(feature = "memory-profiling")]
        {
            self.stats = ShadowTileStats {
                active_capsules: self.rectangles.len(),
                candidate_indices: counts.iter().sum(),
                busiest_tile: counts.iter().copied().max().unwrap_or(0),
            };
        }
        if self.rectangles.is_empty() {
            return Ok(false);
        }
        self.word(0, GRID as f32 / extent[0] as f32);
        self.word(1, GRID as f32 / extent[1] as f32);
        self.word(2, extent[0] as f32);
        self.word(3, extent[1] as f32);
        let mut next = 4 * (1 + TILES + self.rectangles.len() * 3);
        let mut cursors = [0usize; TILES];
        for (tile, count) in counts.into_iter().enumerate() {
            cursors[tile] = next;
            self.word(4 * (tile + 1), next as f32);
            self.word(4 * (tile + 1) + 1, count as f32);
            next += count;
        }
        for i in 0..self.rectangles.len() {
            let [lo, hi] = self.rectangles[i];
            for y in lo[1]..=hi[1] {
                for cursor in &mut cursors[y * GRID + lo[0]..=y * GRID + hi[0]] {
                    self.word(*cursor, (1 + TILES + 3 * i) as f32);
                    *cursor += 1;
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opposing_scenario_directions_cancel_without_flipping_contact_shadows() {
        let mut bytes = BakedLighting::uniform([[1.; 3]; 2]).unwrap().into_rgba32f();
        for (texel, first) in [(10, 1.0f32), (14, -1.0)] {
            for channel in 0..3 {
                let at = texel * 16 + channel * 4;
                bytes[at..at + 4].copy_from_slice(&first.to_le_bytes());
            }
        }
        let volume = BakedLighting::from_texels(bytes).unwrap();
        assert_eq!(direction(&volume, [0.5; 3], 0.), Some([1., 0., 0., 1.]));
        assert_eq!(direction(&volume, [0.5; 3], 0.5), Some([0.; 4]));
        assert_eq!(direction(&volume, [0.5; 3], 1.), Some([-1., 0., 0., 1.]));
        assert_eq!(direction(&volume, [-0.01; 3], 0.), None);
    }

    #[test]
    fn offscreen_caster_is_retained_when_its_influence_reaches_the_view() {
        let matrix = crate::unlit_forward::IDENTITY;
        let caster = |x| ShadowCapsule::new([x, 0., 0.5], [x, 0.1, 0.5], 0.05).unwrap();
        assert_eq!(tile_bounds(caster(1.15), &matrix).unwrap()[1][0], GRID - 1);
        assert!(tile_bounds(caster(1.5), &matrix).is_none());
    }

    #[test]
    fn tile_lists_match_all_active_capsules_across_word_boundaries() {
        let volume = BakedLighting::uniform([[1.; 3]]).unwrap();
        let matrix = crate::unlit_forward::IDENTITY;
        for size in [0, 1, 16, 17, 33, 256] {
            let capsules: Vec<_> = (0..size)
                .map(|i| {
                    let point = [(i % 7) as f32 / 8.0, (i % 5) as f32 / 8.0, 0.5];
                    ShadowCapsule::new(point, point, if i % 8 == 0 { 1.0 } else { 0.03 }).unwrap()
                })
                .collect();
            let mut work = Workspace::new(size, None).unwrap();
            assert_eq!(
                work.encode(&matrix, [151, 93], &volume, 0.0, &capsules)
                    .unwrap(),
                size != 0
            );
            if size == 0 {
                continue;
            }
            let word = |i: usize| {
                f32::from_le_bytes(work.bytes[4 * i..4 * i + 4].try_into().unwrap()) as usize
            };
            for tile in 0..TILES {
                let start = word(4 * (1 + tile));
                let count = word(4 * (1 + tile) + 1);
                let actual: Vec<_> = (start..start + count)
                    .map(|i| (word(i) - 1 - TILES) / 3)
                    .collect();
                let expected: Vec<_> = capsules
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &c)| {
                        let [lo, hi] = tile_bounds(c, &matrix)?;
                        ((lo[0]..=hi[0]).contains(&(tile % GRID))
                            && (lo[1]..=hi[1]).contains(&(tile / GRID)))
                        .then_some(i)
                    })
                    .collect();
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn only_contributing_capsules_consume_capacity_and_a_failed_view_can_retry() {
        let volume = BakedLighting::uniform([[1.; 3]]).unwrap();
        let matrix = crate::unlit_forward::IDENTITY;
        let mut workspace = Workspace::new(1, None).unwrap();
        let sphere = |z| ShadowCapsule::new([0.5, 0.5, z], [0.5, 0.5, z], 0.01).unwrap();
        let mut encode = |capsules: &[ShadowCapsule]| {
            workspace.encode(&matrix, [101, 81], &volume, 0.0, capsules)
        };
        assert!(!encode(&[sphere(-1.0), sphere(2.0)]).unwrap());
        assert!(encode(&[sphere(0.5), sphere(0.5)]).is_err());
        assert!(encode(&[sphere(2.0), sphere(0.5)]).unwrap());
        assert!(!encode(&[]).unwrap());
    }

    #[test]
    fn perspective_culling_distinguishes_behind_eye_from_eye_plane_crossings() {
        let mut matrix = crate::unlit_forward::IDENTITY;
        matrix[2][3] = 1.0;
        matrix[3][3] = 0.0;
        let sphere = |z| ShadowCapsule::new([0.0, 0.0, z], [0.0, 0.0, z], 0.1).unwrap();
        assert_eq!(tile_bounds(sphere(-2.0), &matrix), None);
        assert_eq!(
            tile_bounds(sphere(-0.1), &matrix),
            Some([[0; 2], [GRID - 1; 2]])
        );
    }
}
