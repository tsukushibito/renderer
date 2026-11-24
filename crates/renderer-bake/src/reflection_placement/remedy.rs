//! Geometry-only, budgeted coverage of visible surface patches.
use super::ReflectionPlacementSettings;
use crate::{BakeError, BakeScene, ReflectionProbeDesc, Vec3, dot, sub};
use std::collections::HashMap;

type Cell = [i32; 3];
type Point = [f64; 3];
const EMPTY: u32 = u32::MAX;
const RADIUS: i32 = 3;

struct Grid {
    origin: Point,
    spacing: f64,
    dims: Cell,
    end: Point,
    // Split opposite sides and differently oriented surfaces in a voxel.
    cells: HashMap<Cell, [u32; 6]>,
    patches: Vec<Patch>,
}

#[derive(Default)]
struct Patch {
    area: f64,
    centroid: Point,
    blocked: u8,
    face: usize,
}

impl Grid {
    fn new(settings: ReflectionPlacementSettings, lo: Vec3, hi: Vec3) -> Result<Self, BakeError> {
        let spacing = f64::from(settings.spacing);
        let (min, max) = settings.bounds.map_or_else(
            || {
                (
                    lo.map(|v| f64::from(v) - spacing),
                    hi.map(|v| f64::from(v) + spacing),
                )
            },
            |(min, max)| (min.map(f64::from), max.map(f64::from)),
        );
        let counts: Point = std::array::from_fn(|i| ((max[i] - min[i]) / spacing).ceil());
        if counts.iter().any(|n| *n < 1.) || counts.iter().product::<f64>() > 262_144. {
            return Err(BakeError(
                "Remedy placement grid exceeds its range; increase spacing or restrict bounds",
            ));
        }
        Ok(Self {
            origin: min,
            spacing,
            dims: counts.map(|n| n as i32),
            end: std::array::from_fn(|i| (max[i] - min[i]) / spacing),
            cells: HashMap::new(),
            patches: Vec::new(),
        })
    }

    fn contains(&self, cell: Cell) -> bool {
        (0..3).all(|i| (0..self.dims[i]).contains(&cell[i]))
    }

    fn world(&self, p: Point) -> Vec3 {
        std::array::from_fn(|i| (self.origin[i] + p[i] * self.spacing) as f32)
    }

    fn add(&mut self, cell: Cell, face: usize, vertices: &[Point]) {
        // Triangle fan area and first moment, in voxel units. Geometric area
        // is independent of how an asset split a flat surface into triangles.
        for edge in vertices[1..].windows(2) {
            let [a, b, c] = [vertices[0], edge[0], edge[1]];
            let du: Point = std::array::from_fn(|i| b[i] - a[i]);
            let dv: Point = std::array::from_fn(|i| c[i] - a[i]);
            let cross: Point = std::array::from_fn(|i| {
                du[(i + 1) % 3] * dv[(i + 2) % 3] - du[(i + 2) % 3] * dv[(i + 1) % 3]
            });
            let surface_area = 0.5 * cross[0].hypot(cross[1]).hypot(cross[2]);
            if surface_area == 0. {
                continue;
            }
            let id = &mut self.cells.entry(cell).or_insert([EMPTY; 6])[face];
            if *id == EMPTY {
                *id = self.patches.len() as u32;
                self.patches.push(Patch {
                    face,
                    ..Patch::default()
                });
            }
            let patch = &mut self.patches[*id as usize];
            patch.area += surface_area;
            for i in 0..3 {
                patch.centroid[i] += surface_area * ((a[i] + b[i] + c[i]) / 3.);
            }
        }
    }

    fn voxelize(&mut self, scene: &BakeScene) {
        let mut polygon = Vec::with_capacity(10);
        let mut scratch = Vec::with_capacity(10);
        for t in &scene.triangles {
            let mat = scene.materials[t.material];
            if mat
                .alpha_cutoff
                .is_some_and(|cutoff| mat.base_texture.is_none() && mat.opacity < cutoff)
            {
                continue;
            }
            let normal = t.normal();
            let axis = (0..3)
                .max_by(|&a, &b| normal[a].abs().total_cmp(&normal[b].abs()))
                .unwrap();
            let face = axis * 2 + usize::from(normal[axis] > 0.);
            let points = t.positions.map(|p| {
                std::array::from_fn(|i| (f64::from(p[i]) - self.origin[i]) / self.spacing)
            });
            let lo: Cell = std::array::from_fn(|i| {
                (points
                    .iter()
                    .map(|p| p[i])
                    .fold(f64::INFINITY, f64::min)
                    .floor() as i32)
                    .clamp(0, self.dims[i] - 1)
            });
            let hi: Cell = std::array::from_fn(|i| {
                (points
                    .iter()
                    .map(|p| p[i])
                    .fold(f64::NEG_INFINITY, f64::max)
                    .floor() as i32)
                    .min(self.dims[i] - 1)
            });
            for z in lo[2]..=hi[2] {
                for y in lo[1]..=hi[1] {
                    for x in lo[0]..=hi[0] {
                        let cell = [x, y, z];
                        polygon.clear();
                        polygon.extend_from_slice(&points);
                        for (axis, &index) in cell.iter().enumerate() {
                            let lower = f64::from(index);
                            let upper = (lower + 1.).min(self.end[axis]);
                            for (plane, sign) in [(lower, 1.), (upper, -1.)] {
                                if polygon.iter().any(|p| (p[axis] - plane) * sign < 0.) {
                                    clip(&polygon, &mut scratch, axis, plane, sign);
                                    std::mem::swap(&mut polygon, &mut scratch);
                                }
                            }
                            if polygon.len() < 3 {
                                break;
                            }
                        }
                        if polygon.len() >= 3 {
                            self.add(cell, face, &polygon);
                        }
                    }
                }
            }
        }
        for p in &mut self.patches {
            p.centroid = p.centroid.map(|x| x / p.area);
        }
    }
}

fn clip(vertices: &[Point], out: &mut Vec<Point>, axis: usize, plane: f64, sign: f64) {
    out.clear();
    let mut a = vertices[vertices.len() - 1];
    for &b in vertices {
        let da = sign * (a[axis] - plane);
        let db = sign * (b[axis] - plane);
        if (da < 0.) != (db < 0.) {
            let t = da / (da - db);
            let mut p: Point = std::array::from_fn(|i| a[i] + t * (b[i] - a[i]));
            p[axis] = plane;
            out.push(p);
        }
        if db >= 0. {
            out.push(b);
        }
        a = b;
    }
}

struct Candidate {
    probe: ReflectionProbeDesc,
    blocked: u8,
    coverage: Vec<(u32, f64)>,
}

fn candidate(
    scene: &BakeScene,
    grid: &Grid,
    cell: Cell,
    settings: ReflectionPlacementSettings,
    occupied: bool,
) -> Result<Option<Candidate>, BakeError> {
    let position = grid.world(cell.map(|i| f64::from(i) + 0.5));
    if settings
        .bounds
        .is_some_and(|(lo, hi)| (0..3).any(|i| position[i] < lo[i] || position[i] > hi[i]))
    {
        return Ok(None);
    }
    let reach = settings.spacing * RADIUS as f32;
    // Include a struck surface in the shader's inward fade, limiting the
    // extension behind the first blocker to a fraction of a voxel.
    let pad = settings.blend_distance.min(settings.spacing * 0.125);
    let mut probe = ReflectionProbeDesc {
        position,
        min: position,
        max: position,
        blend_distance: settings.blend_distance,
    };
    let mut blocked = 0u8;
    for axis in 0..3 {
        for sign in [-1., 1.] {
            let mut dir = [0.; 3];
            dir[axis] = sign;
            let hit = scene.trace(position, dir, reach);
            if occupied
                && hit
                    .as_ref()
                    .is_some_and(|h| h.backface || h.distance < settings.spacing * 0.1)
            {
                return Ok(None);
            }
            if hit.is_some() {
                blocked |= 1 << (axis * 2 + usize::from(sign > 0.));
            }
            let extent = hit.map_or(reach, |h| h.distance) + pad;
            if sign < 0. {
                probe.min[axis] -= extent;
            } else {
                probe.max[axis] += extent;
            }
        }
    }
    // Occupied voxels can still contain empty space inside a narrow passage.
    // Only keep their centers where opposed surfaces bound a clear span.
    let paired = blocked & (blocked >> 1) & 0b01_01_01;
    if occupied && paired.count_ones() < 2 {
        return Ok(None);
    }
    if occupied {
        probe.blend_distance = pad;
    }
    probe
        .validate()
        .map_err(|_| BakeError("unrepresentable Remedy reflection box"))?;
    let mut coverage = Vec::new();
    for z in cell[2] - RADIUS..=cell[2] + RADIUS {
        for y in cell[1] - RADIUS..=cell[1] + RADIUS {
            for x in cell[0] - RADIUS..=cell[0] + RADIUS {
                let Some(faces) = grid.cells.get(&[x, y, z]) else {
                    continue;
                };
                for (face, &id) in faces.iter().enumerate().filter(|(_, id)| **id != EMPTY) {
                    let patch = &grid.patches[id as usize];
                    let target = grid.world(patch.centroid);
                    let inside = (0..3)
                        .map(|i| (target[i] - probe.min[i]).min(probe.max[i] - target[i]))
                        .fold(f32::INFINITY, f32::min);
                    if inside <= 0. {
                        continue;
                    }
                    let delta = sub(target, position);
                    let distance = dot(delta, delta).sqrt();
                    if distance <= 0. || !distance.is_finite() {
                        continue;
                    }
                    let dir = delta.map(|v| v / distance);
                    let tolerance = settings.spacing * 0.05;
                    let Some(hit) = scene.trace(position, dir, distance + tolerance) else {
                        continue;
                    };
                    let axis = face / 2;
                    if hit.backface
                        || (hit.distance - distance).abs() > tolerance
                        || (hit.normal[axis] > 0.) != (face % 2 == 1)
                    {
                        continue;
                    }
                    let weight = f64::from((inside / probe.blend_distance).min(1.));
                    let d = f64::from(distance) / grid.spacing;
                    coverage.push((id, patch.area * weight / (1. + d * d)));
                }
            }
        }
    }
    Ok((!coverage.is_empty()).then_some(Candidate {
        probe,
        blocked,
        coverage,
    }))
}

// Trace the five outward/tangential axial directions once per patch. All
// candidate comparisons reuse this signature and the existing box-fit rays.
fn classify_patches(scene: &BakeScene, grid: &mut Grid, spacing: f32) {
    let reach = spacing * RADIUS as f32;
    for index in 0..grid.patches.len() {
        let patch = &grid.patches[index];
        let face = patch.face;
        let mut origin = grid.world(patch.centroid);
        origin[face / 2] += if face % 2 == 1 { 1. } else { -1. } * spacing * 0.02;
        let mut blocked = 0;
        for direction in 0..6 {
            if direction == (face ^ 1) {
                continue;
            }
            let mut ray = [0.; 3];
            ray[direction / 2] = if direction % 2 == 1 { 1. } else { -1. };
            if scene.trace(origin, ray, reach).is_some() {
                blocked |= 1 << direction;
            }
        }
        grid.patches[index].blocked = blocked;
    }
}

fn append_candidates(
    scene: &BakeScene,
    grid: &Grid,
    settings: ReflectionPlacementSettings,
    cells: Vec<Cell>,
    occupied: bool,
    candidates: &mut Vec<Candidate>,
) -> Result<(), BakeError> {
    let mut edges: usize = candidates.iter().map(|c| c.coverage.len()).sum();
    for cell in cells {
        if let Some(c) = candidate(scene, grid, cell, settings, occupied)? {
            edges += c.coverage.len();
            if edges > 4_194_304 {
                return Err(BakeError(
                    "Remedy visibility budget exceeded; increase spacing or restrict bounds",
                ));
            }
            candidates.push(c);
        }
    }
    Ok(())
}

fn mark_covered(covered: &mut [f64], candidate: &Candidate) {
    for &(id, score) in &candidate.coverage {
        covered[id as usize] = covered[id as usize].max(score);
    }
}

pub(super) fn place(
    scene: &BakeScene,
    settings: ReflectionPlacementSettings,
    lo: Vec3,
    hi: Vec3,
) -> Result<Vec<ReflectionProbeDesc>, BakeError> {
    let mut grid = Grid::new(settings, lo, hi)?;
    grid.voxelize(scene);
    let mut neighbors = Vec::new();
    for &cell in grid.cells.keys() {
        for axis in 0..3 {
            for offset in [-1, 1] {
                let mut adjacent = cell;
                adjacent[axis] += offset;
                if grid.contains(adjacent) && !grid.cells.contains_key(&adjacent) {
                    neighbors.push(adjacent);
                }
            }
        }
    }
    neighbors.sort_unstable();
    neighbors.dedup();
    let mut candidates = Vec::new();
    append_candidates(scene, &grid, settings, neighbors, false, &mut candidates)?;
    let mut covered = vec![0f64; grid.patches.len()];
    let mut selected: Vec<usize> = Vec::new();
    let mut first_gain = 0.;
    let mut refined = false;
    while selected.len() < settings.max_probes {
        let mut best = None;
        let mut gain = 0.;
        for (index, c) in candidates.iter().enumerate() {
            let extra = c
                .coverage
                .iter()
                .map(|&(id, score)| (score - covered[id as usize]).max(0.))
                .sum();
            if extra > gain {
                best = Some(index);
                gain = extra;
            }
        }
        if selected.is_empty() {
            first_gain = gain;
        }
        // Broad, distinct regions retain priority while the next capture
        // delivers at least half the first capture's area-weighted gain.
        if !refined && (best.is_none() || gain < first_gain * 0.5) {
            let mut occupied: Vec<_> = grid.cells.keys().copied().collect();
            occupied.sort_unstable();
            append_candidates(scene, &grid, settings, occupied, true, &mut candidates)?;
            classify_patches(scene, &mut grid, settings.spacing);
            for c in &mut candidates {
                for (id, score) in &mut c.coverage {
                    let patch = &grid.patches[*id as usize];
                    let outward = 0b11_11_11 ^ (1 << (patch.face ^ 1));
                    let mismatch = ((patch.blocked ^ c.blocked) & outward).count_ones();
                    *score *= (1. + f64::from(patch.blocked.count_ones()))
                        / (1. + f64::from(mismatch)).powi(2);
                }
            }
            covered.fill(0.);
            for &index in &selected {
                mark_covered(&mut covered, &candidates[index]);
            }
            refined = true;
            continue;
        }
        let Some(index) = best else { break };
        mark_covered(&mut covered, &candidates[index]);
        selected.push(index);
    }
    Ok(selected.into_iter().map(|i| candidates[i].probe).collect())
}
