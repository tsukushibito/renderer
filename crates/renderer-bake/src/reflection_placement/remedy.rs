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

    fn nearby_patches(&self, [x, y, z]: Cell, radius: i32) -> impl Iterator<Item = u32> + '_ {
        (z - radius..=z + radius)
            .flat_map(move |k| (y - radius..=y + radius).map(move |j| (j, k)))
            .flat_map(move |(j, k)| (x - radius..=x + radius).map(move |i| [i, j, k]))
            .filter_map(|cell| self.cells.get(&cell))
            .flatten()
            .copied()
            .filter(|&id| id != EMPTY)
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

// Preserve the distance used by the broad pass so refinement only rescales
// existing edges; it needs no second visibility trace or world-space lookup.
struct Coverage {
    patch: u32,
    distance_squared: f32,
    score: f64,
}

struct Candidate {
    cell: Cell,
    probe: ReflectionProbeDesc,
    blocked: u8,
    coverage: Vec<Coverage>,
}

fn bounds_multiple_axes(blocked: u8) -> bool {
    (blocked & (blocked >> 1) & 0b01_01_01).count_ones() >= 2
}

// The centroid can lie off a curved or clipped surface. Only accept a first
// hit near the centroid with the expected front-facing orientation.
fn visible_patch(
    scene: &BakeScene,
    origin: Vec3,
    target: Vec3,
    face: usize,
    tolerance: f32,
) -> Option<(f32, Vec3)> {
    let delta = sub(target, origin);
    let distance = dot(delta, delta).sqrt();
    if distance <= 0. || !distance.is_finite() {
        return None;
    }
    let direction = delta.map(|x| x / distance);
    let hit = scene.trace(origin, direction, distance + tolerance)?;
    if hit.backface
        || (hit.distance - distance).abs() > tolerance
        || (hit.normal[face / 2] > 0.) != (face % 2 == 1)
    {
        return None;
    }
    Some((
        distance,
        std::array::from_fn(|k| origin[k] + direction[k] * hit.distance),
    ))
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
    if occupied && !bounds_multiple_axes(blocked) {
        return Ok(None);
    }
    if occupied {
        probe.blend_distance = pad;
    }
    probe
        .validate()
        .map_err(|_| BakeError("unrepresentable Remedy reflection box"))?;
    let mut coverage = Vec::new();
    for id in grid.nearby_patches(cell, RADIUS) {
        let patch = &grid.patches[id as usize];
        let target = grid.world(patch.centroid);
        let inside = (0..3)
            .map(|i| (target[i] - probe.min[i]).min(probe.max[i] - target[i]))
            .fold(f32::INFINITY, f32::min);
        if inside <= 0. {
            continue;
        }
        let Some((distance, _)) =
            visible_patch(scene, position, target, patch.face, settings.spacing * 0.05)
        else {
            continue;
        };
        let weight = f64::from((inside / probe.blend_distance).min(1.));
        let d = f64::from(distance) / grid.spacing;
        let distance_squared = d * d;
        coverage.push(Coverage {
            patch: id,
            distance_squared: distance_squared as f32,
            score: patch.area * weight / (1. + distance_squared),
        });
    }
    Ok((!coverage.is_empty()).then_some(Candidate {
        cell,
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
    for edge in &candidate.coverage {
        let best = &mut covered[edge.patch as usize];
        *best = best.max(edge.score);
    }
}

// An axial ray can stop at a pillar even though the same capture sees the
// wall beside it. Fit only to confirmed first hits near the original box.
fn fit_visible_faces(
    scene: &BakeScene,
    grid: &Grid,
    candidate: &Candidate,
) -> Result<ReflectionProbeDesc, BakeError> {
    let mut probe = candidate.probe;
    let spacing = grid.spacing as f32;
    let margin = spacing.min(probe.blend_distance);
    // The axial reach is RADIUS cells plus padding. One extra cell of
    // expansion and the partial end cells fit within this neighborhood.
    for id in grid.nearby_patches(candidate.cell, RADIUS + 2) {
        let patch = &grid.patches[id as usize];
        let target = grid.world(patch.centroid);
        let gap = (0..3)
            .map(|k| (candidate.probe.min[k] - target[k]).max(target[k] - candidate.probe.max[k]))
            .fold(f32::NEG_INFINITY, f32::max);
        if gap <= -margin || gap > spacing {
            continue;
        }
        let Some((_, point)) =
            visible_patch(scene, probe.position, target, patch.face, spacing * 0.05)
        else {
            continue;
        };
        for (k, coordinate) in point.into_iter().enumerate() {
            probe.min[k] = probe.min[k].min(coordinate - margin);
            probe.max[k] = probe.max[k].max(coordinate + margin);
        }
    }
    probe
        .validate()
        .map_err(|_| BakeError("unrepresentable Remedy reflection box"))?;
    Ok(probe)
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
                .map(|edge| (edge.score - covered[edge.patch as usize]).max(0.))
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
                for edge in &mut c.coverage {
                    let patch = &grid.patches[edge.patch as usize];
                    let d2 = f64::from(edge.distance_squared);
                    let outward = 0b11_11_11 ^ (1 << (patch.face ^ 1));
                    let mismatch = ((patch.blocked ^ c.blocked) & outward).count_ones();
                    // Score distance over the full search radius in the refined pass,
                    // so a visible distant wall can compete with another nearby face.
                    edge.score *= (1. + d2) / (1. + d2 / f64::from(RADIUS * RADIUS))
                        * (1. + f64::from(patch.blocked.count_ones()))
                        / (1. + f64::from(mismatch)).powi(2);
                }
            }
            covered.fill(0.);
            // All captures must compete under the same objective, including
            // broad captures whose old scores assumed an open environment.
            selected.clear();
            refined = true;
            continue;
        }
        let Some(index) = best else { break };
        mark_covered(&mut covered, &candidates[index]);
        selected.push(index);
    }
    selected
        .into_iter()
        .map(|i| {
            let c = &candidates[i];
            if refined && bounds_multiple_axes(c.blocked) {
                fit_visible_faces(scene, &grid, c)
            } else {
                Ok(c.probe)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Material, Triangle};

    #[test]
    fn enclosed_box_covers_the_visible_recess_but_stops_at_its_back_wall() {
        let mut triangles = Vec::new();
        // A narrow pillar blocks the axial ray, while off-axis rays can see
        // the first wall. The second wall is fully hidden behind the first.
        for (x, half_width) in [(0.99, 0.1), (1.96, 1.), (2.10, 1.)] {
            let corners = [
                [x, -1., -half_width],
                [x, -1., half_width],
                [x, 1., half_width],
                [x, 1., -half_width],
            ];
            for indices in [[0, 1, 2], [0, 2, 3]] {
                triangles.push(Triangle {
                    positions: indices.map(|i| corners[i]),
                    uv: [[0.; 2]; 3],
                    material: 0,
                });
            }
        }
        let scene = BakeScene::new(triangles, vec![Material::default()], vec![]).unwrap();
        let settings = ReflectionPlacementSettings {
            spacing: 1.,
            bounds: Some(([-3.; 3], [3.; 3])),
            ..Default::default()
        };
        let mut grid = Grid::new(settings, [-3.; 3], [3.; 3]).unwrap();
        grid.voxelize(&scene);
        let candidate = Candidate {
            cell: [3; 3],
            probe: ReflectionProbeDesc {
                position: [0.; 3],
                min: [-1.5; 3],
                max: [1.115, 1.5, 1.5],
                blend_distance: 0.125,
            },
            blocked: 0b11_11_11,
            coverage: vec![],
        };
        let fitted = fit_visible_faces(&scene, &grid, &candidate).unwrap();
        assert!((fitted.max[0] - fitted.blend_distance - 1.96).abs() < 1e-5);
        assert!(fitted.max[0] < 2.10);
    }
}
