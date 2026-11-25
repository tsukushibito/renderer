use crate::{BakeError, BakeScene, ReflectionProbeDesc, Vec3};
use sashiko_renderer::pbr::ReflectionProbes;

mod remedy;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReflectionPlacementMethod {
    /// Find horizontal supports in vertical columns.
    #[default]
    Floor,
    /// Rank empty voxels by newly covered, visible surface area and distance.
    Remedy,
}

/// Offline placement on a Y-up scene in metres.
#[derive(Clone, Copy, Debug)]
pub struct ReflectionPlacementSettings {
    pub method: ReflectionPlacementMethod,
    /// Atlas budget. Floor reports overflow; Remedy selects up to this many.
    pub max_probes: usize,
    /// Floor column spacing or Remedy voxel width in metres.
    pub spacing: f32,
    /// Height of the capture above an upward-facing support surface.
    pub capture_height: f32,
    /// Fade width and overlap into adjacent unobstructed cells.
    pub blend_distance: f32,
    /// Optional world-space search box for support surfaces and capture points.
    /// Influence volumes may extend outside it. By default all geometry is searched.
    pub bounds: Option<(Vec3, Vec3)>,
}

impl Default for ReflectionPlacementSettings {
    fn default() -> Self {
        Self {
            method: ReflectionPlacementMethod::Floor,
            max_probes: ReflectionProbes::MAX_COUNT,
            spacing: 8.,
            capture_height: 1.5,
            blend_distance: 0.5,
            bounds: None,
        }
    }
}

impl BakeScene {
    /// Suggest editable box-projected probes using static scene geometry.
    /// Uses the bake BVH and material alpha tests. Empty or unsupported areas yield
    /// an empty list. All placement and visibility work happens offline.
    pub fn place_reflection_probes(
        &self,
        settings: ReflectionPlacementSettings,
    ) -> Result<Vec<ReflectionProbeDesc>, BakeError> {
        let ReflectionPlacementSettings {
            spacing,
            capture_height,
            blend_distance,
            bounds,
            method,
            max_probes,
        } = settings;
        if !(1..=ReflectionProbes::MAX_COUNT).contains(&max_probes) {
            return Err(BakeError(
                "reflection probe budget must be between 1 and 32",
            ));
        }
        if ![spacing, blend_distance]
            .into_iter()
            .all(|v| v.is_normal() && v > 0.)
            || (method == ReflectionPlacementMethod::Floor
                && !(capture_height.is_normal() && capture_height > 0.))
        {
            return Err(BakeError(
                "reflection placement distances must be positive and finite",
            ));
        }
        if bounds.is_some_and(|(lo, hi)| {
            !(0..3).all(|i| lo[i].is_finite() && hi[i].is_finite() && lo[i] < hi[i])
        }) {
            return Err(BakeError("invalid reflection placement bounds"));
        }
        let Some(root) = self.bvh.0.first() else {
            return Ok(Vec::new());
        };
        if method == ReflectionPlacementMethod::Remedy {
            return remedy::place(self, settings, root.lo, root.hi);
        }
        let (lo, hi) = bounds.unwrap_or((root.lo, root.hi));
        // Grid arithmetic uses f64 so dimensions are checked before integer conversion.
        let spans = [0, 2].map(|i| f64::from(hi[i]) - f64::from(lo[i]));
        if spans.contains(&0.) {
            return Ok(Vec::new());
        }
        let counts = spans.map(|s| (s / f64::from(spacing)).ceil());
        if counts[0] * counts[1] > 65_536. {
            return Err(BakeError(
                "reflection placement exceeds 65536 columns; increase spacing or restrict bounds",
            ));
        }
        let step = [spans[0] / counts[0], spans[1] / counts[1]];
        // Start above all geometry, even for a cropped region, to account for its ceiling.
        let top = (root.hi[1] + capture_height).next_up();
        if !top.is_finite() {
            return Err(BakeError("unrepresentable reflection placement height"));
        }
        let mut probes = Vec::new();
        for z in 0..counts[1] as usize {
            for x in 0..counts[0] as usize {
                let origin = |dx, dz| {
                    [
                        (f64::from(lo[0]) + (x as f64 + dx) * step[0]) as f32,
                        top,
                        (f64::from(lo[2]) + (z as f64 + dz) * step[1]) as f32,
                    ]
                };
                let previous = probes.len();
                self.place_column(origin(0.5, 0.5), step, settings, &mut probes)?;
                if probes.len() == previous {
                    // A central wall or a gap between floors can hide the entire
                    // cell. Sample the four quarter-cell centres once in that case.
                    for dz in [0.25, 0.75] {
                        for dx in [0.25, 0.75] {
                            self.place_column(
                                origin(dx, dz),
                                step.map(|v| v * 0.5),
                                settings,
                                &mut probes,
                            )?;
                        }
                    }
                }
            }
        }
        Ok(probes)
    }

    fn place_column(
        &self,
        mut origin: Vec3,
        step: [f64; 2],
        settings: ReflectionPlacementSettings,
        probes: &mut Vec<ReflectionProbeDesc>,
    ) -> Result<(), BakeError> {
        let mut ceiling = f32::INFINITY;
        while let Some(hit) = self.trace(origin, [0., -1., 0.], f32::INFINITY) {
            let floor = origin[1] - hit.distance;
            if !floor.is_finite() {
                return Err(BakeError(
                    "unrepresentable reflection placement intersection",
                ));
            }
            if settings.bounds.is_some_and(|(min, _)| floor < min[1]) {
                break;
            }
            let y = floor + settings.capture_height;
            // Reject undersides, steep walls, and shelves without headroom.
            if !hit.backface
                && hit.normal[1] >= std::f32::consts::FRAC_1_SQRT_2
                && y < ceiling
                && settings.bounds.is_none_or(|(_, max)| y <= max[1])
            {
                let position = [origin[0], y, origin[2]];
                if let Some(probe) =
                    self.fit_reflection_box(position, floor, ceiling, step, settings)?
                {
                    if probes.len() == settings.max_probes {
                        return Err(BakeError(
                            "reflection probe atlas capacity exceeded; increase spacing or restrict bounds",
                        ));
                    }
                    probes.push(probe);
                }
            }
            ceiling = floor;
            // Step below the hit, including coincident triangles, and guarantee progress.
            origin[1] = floor.next_down();
            if !origin[1].is_finite() {
                break;
            }
        }
        Ok(())
    }

    fn fit_reflection_box(
        &self,
        position: Vec3,
        floor: f32,
        ceiling: f32,
        step: [f64; 2],
        settings: ReflectionPlacementSettings,
    ) -> Result<Option<ReflectionProbeDesc>, BakeError> {
        let fade = settings.blend_distance;
        let mut min = position;
        let mut max = position;
        // The supporting floor needs nonzero influence at the surface itself.
        min[1] = floor - fade;
        max[1] = ceiling.min(position[1] + settings.spacing * 0.5 + fade);
        for (axis, width) in [0, 2].into_iter().zip(step) {
            let reach = (width * 0.5) as f32 + fade;
            let mut direction = [0.; 3];
            direction[axis] = 1.;
            // A ray starting exactly on a wall cannot hit that wall. Test a short
            // segment across the capture before using it as a projection origin.
            let clearance = 0.01_f32.min(reach * 0.25);
            let mut start = position;
            start[axis] -= clearance;
            if self.trace(start, direction, 2. * clearance).is_some() {
                return Ok(None);
            }
            max[axis] += self
                .trace(position, direction, reach)
                .map_or(reach, |h| h.distance);
            direction[axis] = -1.;
            min[axis] -= self
                .trace(position, direction, reach)
                .map_or(reach, |h| h.distance);
        }
        let probe = ReflectionProbeDesc {
            position,
            min,
            max,
            blend_distance: fade,
        };
        probe
            .validate()
            .map_err(|_| BakeError("unrepresentable reflection probe box"))?;
        Ok(Some(probe))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Lighting, Material, Triangle};

    fn quad(corners: [Vec3; 4], material: usize) -> [Triangle; 2] {
        [[0, 1, 2], [0, 2, 3]].map(|indices| Triangle {
            positions: indices.map(|i| corners[i]),
            uv: [[0.; 2]; 3],
            material,
        })
    }

    fn floor(y: f32) -> [Triangle; 2] {
        quad([[-4., y, -2.], [-4., y, 2.], [4., y, 2.], [4., y, -2.]], 0)
    }

    #[test]
    fn adjacent_rooms_and_stacked_floors_have_separate_boxes() {
        let mut triangles = floor(0.).to_vec();
        triangles.extend(floor(3.));
        triangles.extend(quad(
            [[0., 0., -2.], [0., 6., -2.], [0., 6., 2.], [0., 0., 2.]],
            0,
        ));
        let scene = BakeScene::new(triangles, vec![Material::default()], vec![]).unwrap();
        let settings = ReflectionPlacementSettings {
            spacing: 4.,
            ..Default::default()
        };
        let probes = scene.place_reflection_probes(settings).unwrap();
        assert_eq!(probes, scene.place_reflection_probes(settings).unwrap());
        assert_eq!(probes.len(), 4);
        for x in [-2., 2.] {
            for y in [1.5, 4.5] {
                let p = probes.iter().find(|p| p.position == [x, y, 0.]).unwrap();
                assert_eq!(if x < 0. { p.max[0] } else { p.min[0] }, 0.);
                assert!(p.min[1] < y - settings.capture_height);
                if y < 3. {
                    assert_eq!(p.max[1], 3.);
                }
            }
        }
        // A centre on the wall recovers captures on both sides.
        let fallback = scene.place_reflection_probes(Default::default()).unwrap();
        assert!(
            fallback
                .iter()
                .any(|p| p.position[0] < 0. && p.max[0] == 0.)
        );
        assert!(
            fallback
                .iter()
                .any(|p| p.position[0] > 0. && p.min[0] == 0.)
        );
        let mut region = scene
            .place_reflection_probes(ReflectionPlacementSettings {
                bounds: Some(([-4., -1., -2.], [0., 2., 2.])),
                ..settings
            })
            .unwrap();
        assert_eq!(region.len(), 1);
        region[0].position[2] += 0.25;
        scene
            .bake_reflection_probes(&region, Lighting::uniform_sky([1.; 3]))
            .unwrap();
    }

    #[test]
    fn masked_obstacles_and_low_headroom_affect_placement() {
        let mut triangles = floor(0.).to_vec();
        let mut shelf = floor(1.);
        for tri in &mut shelf {
            tri.positions.swap(1, 2);
        }
        triangles.extend(shelf);
        let bake = |triangles| {
            BakeScene::new(
                triangles,
                vec![
                    Material::default(),
                    Material {
                        opacity: 0.,
                        alpha_cutoff: Some(0.5),
                        ..Default::default()
                    },
                ],
                vec![],
            )
            .unwrap()
        };
        assert!(
            bake(triangles.clone())
                .place_reflection_probes(Default::default())
                .unwrap()
                .is_empty()
        );
        for tri in &mut triangles[2..] {
            tri.material = 1;
        }
        let probes = bake(triangles)
            .place_reflection_probes(Default::default())
            .unwrap();
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].position, [0., 1.5, 0.]);
    }

    #[test]
    fn remedy_can_capture_inside_a_partially_occupied_passage_cell() {
        let mut triangles = Vec::new();
        for axis in [0, 2] {
            for (distance, inward) in [(0.2, 1.), (0.8, -1.)] {
                let mut corners = [[0.; 3]; 4];
                for (point, [u, v]) in
                    corners
                        .iter_mut()
                        .zip([[0., 0.], [1., 0.], [1., 1.], [0., 1.]])
                {
                    point[axis] = distance;
                    point[(axis + 1) % 3] = u;
                    point[(axis + 2) % 3] = v;
                }
                if inward < 0. {
                    corners.reverse();
                }
                triangles.extend(quad(corners, 0));
            }
        }
        let scene = BakeScene::new(triangles, vec![Material::default()], vec![]).unwrap();
        let settings = ReflectionPlacementSettings {
            method: ReflectionPlacementMethod::Remedy,
            spacing: 1.,
            max_probes: 1,
            bounds: Some(([0.; 3], [1.; 3])),
            ..Default::default()
        };
        let probes = scene.place_reflection_probes(settings).unwrap();
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].position, [0.5; 3]);
        assert!(probes[0].position[0] - probes[0].min[0] >= probes[0].blend_distance);
    }

    #[test]
    fn remedy_covers_both_levels_of_a_free_standing_shelf_within_budget() {
        let mut triangles = floor(0.).to_vec();
        triangles.extend(floor(4.));
        let scene = BakeScene::new(triangles, vec![Material::default()], vec![]).unwrap();
        let settings = ReflectionPlacementSettings {
            spacing: 1.,
            method: ReflectionPlacementMethod::Remedy,
            max_probes: 2,
            ..Default::default()
        };
        let probes = scene.place_reflection_probes(settings).unwrap();
        assert_eq!(probes.len(), 2);
        assert!(probes.iter().any(|p| p.position[1] > 4. && p.min[1] < 4.));
        assert!(
            probes
                .iter()
                .any(|p| (0. ..4.).contains(&p.position[1]) && p.min[1] < 0.)
        );
        assert_eq!(probes, scene.place_reflection_probes(settings).unwrap());
        let with_budget = |max_probes| {
            scene.place_reflection_probes(ReflectionPlacementSettings {
                max_probes,
                ..settings
            })
        };
        assert_eq!(with_budget(1).unwrap(), probes[..1]);
        assert!(with_budget(0).is_err());
        assert!(with_budget(ReflectionProbes::MAX_COUNT + 1).is_err());
        let empty = BakeScene::new(vec![], vec![], vec![]).unwrap();
        assert!(empty.place_reflection_probes(settings).unwrap().is_empty());
    }

    #[test]
    fn remedy_fits_boxes_against_a_freestanding_thin_wall_without_floor_support() {
        let corners = [[0., 0., -3.], [0., 6., -3.], [0., 6., 3.], [0., 0., 3.]];
        let mut tris = quad(corners, 0).to_vec();
        let mut back = corners.map(|[_, y, z]| [-0.05, y, z]);
        back.reverse();
        tris.extend(quad(back, 0));
        let scene = BakeScene::new(tris, vec![Material::default()], vec![]).unwrap();
        let probes = scene
            .place_reflection_probes(ReflectionPlacementSettings {
                method: ReflectionPlacementMethod::Remedy,
                spacing: 1.,
                max_probes: 4,
                bounds: Some(([-3., 1., -2.], [3., 5., 2.])),
                ..Default::default()
            })
            .unwrap();
        for sign in [-1., 1.] {
            let p = probes.iter().find(|p| p.position[0] * sign > 0.).unwrap();
            let beyond_wall = if sign < 0. {
                p.max[0] + 0.05
            } else {
                -p.min[0]
            };
            assert!(beyond_wall > 0. && beyond_wall <= 0.126);
        }
    }

    #[test]
    fn limits_are_reported_without_partial_layouts() {
        let scene = BakeScene::new(floor(0.).to_vec(), vec![Material::default()], vec![]).unwrap();
        let place = |spacing| {
            scene.place_reflection_probes(ReflectionPlacementSettings {
                spacing,
                ..Default::default()
            })
        };
        assert_eq!(place(1.).unwrap().len(), ReflectionProbes::MAX_COUNT);
        for spacing in [0.9, 0., f32::NAN, f32::MIN_POSITIVE] {
            assert!(place(spacing).is_err());
        }
        let empty = BakeScene::new(vec![], vec![], vec![]).unwrap();
        assert!(
            empty
                .place_reflection_probes(Default::default())
                .unwrap()
                .is_empty()
        );
    }
}
