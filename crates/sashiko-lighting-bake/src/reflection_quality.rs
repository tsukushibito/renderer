//! Bounded, offline radiometric check of box-projected reflection placements.
use crate::{
    BakeError, BakeScene, Lighting, ReflectionProbeDesc, ReflectionReceiver, Vec3, dot,
    reflections::{direction, filter, parallel_fill, sample},
    sub, unit,
};
use sashiko_renderer::pbr::ReflectionProbes;
use std::f32::consts::TAU;

const WIDTH: usize = 32;
const ROUGHNESS: f32 = 0.5;
const RAYS: u32 = 64;
const SITES_PER_PROBE: usize = 24;

#[derive(Default, Debug, Clone, Copy)]
pub struct QualityMetrics {
    pub samples: usize,
    pub mean_rgb_error: f64,
    pub p90_rgb_error: f32,
    pub worst_position: Option<Vec3>,
    pub worst_rgb_error: f32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PlacementDecision {
    #[default]
    NoReceivers,
    NoTrainingGain,
    TooFewChangedReceivers,
    InsufficientGain,
    NeighborhoodRegression,
    TailRegression,
    Accepted,
}

#[derive(Default, Debug)]
pub struct PlacementQuality {
    /// Measurements on receivers reserved for acceptance.
    pub before: QualityMetrics,
    pub decision: PlacementDecision,
    pub after: QualityMetrics,
    /// Winning training proposal on the independent receivers, including rejected proposals.
    pub proposed: Option<QualityMetrics>,
    pub training_samples: usize,
    pub candidates_tested: usize,
    pub replacement_captures: usize,
    pub changed_probe: Option<usize>,
    pub moved_capture: bool,
}

#[derive(Clone, Copy)]
struct Site {
    owner: usize,
    position: Vec3,
    reflected: Vec3,
    reference: Vec3,
    sky: Vec3,
    response: Vec3,
}

fn receivers(
    scene: &BakeScene,
    probes: &[ReflectionProbeDesc],
    light: Lighting,
    spacing: f32,
    holdout: bool,
) -> Result<Vec<Site>, BakeError> {
    let mut sites = vec![Ok(None); probes.len() * SITES_PER_PROBE];
    parallel_fill(&mut sites, SITES_PER_PROBE, |index| {
        let owner = index / SITES_PER_PROBE;
        let probe = &probes[owner];
        let i = index % SITES_PER_PROBE;
        // Interleaved equal-solid-angle strata, with a separate azimuth rotation per set.
        let j = 2 * i + usize::from(holdout);
        let y = 1. - (j as f32 + 0.5) / SITES_PER_PROBE as f32;
        let phase = i as f32 * 0.618034 + owner as f32 * 0.414214 + if holdout { 0.37 } else { 0. };
        let (s, c) = (TAU * phase.fract()).sin_cos();
        let radial = (1. - y * y).sqrt();
        let ray = [c * radial, y, s * radial];
        let Some(hit) = scene.trace(probe.position, ray, spacing * 5.) else {
            return Ok(None);
        };
        if hit.backface {
            return Ok(None);
        }
        let position = std::array::from_fn(|k| probe.position[k] + hit.distance * ray[k]);
        let view = ray.map(|v| -v);
        let nv = dot(view, hit.normal);
        if nv <= 0. {
            return Ok(None);
        }
        let receiver = ReflectionReceiver {
            position,
            geometric_normal: hit.normal,
            shading_normal: hit.normal,
            view,
            f0: [0.04; 3],
            roughness: ROUGHNESS,
        };
        let reference = scene.reflection_reference(&receiver, light, RAYS)?;
        Ok(Some(Site {
            owner,
            position,
            reflected: std::array::from_fn(|k| 2. * nv * hit.normal[k] - view[k]),
            reference: std::array::from_fn(|k| reference.from_geometry[k] + reference.from_sky[k]),
            sky: reference.unobstructed,
            response: reference.unit_response,
        }))
    });
    sites.into_iter().filter_map(Result::transpose).collect()
}

fn preview(scene: &BakeScene, position: Vec3, light: Lighting) -> Vec<Vec3> {
    let mut sharp = vec![[0.; 3]; WIDTH * WIDTH / 2];
    parallel_fill(&mut sharp, 128, |i| {
        let ray = direction(i % WIDTH, i / WIDTH, WIDTH, WIDTH / 2);
        scene.incoming(position, ray, light, &mut (i as u32))
    });
    let mut filtered = vec![[0.; 3]; WIDTH * 8];
    parallel_fill(&mut filtered, 128, |i| {
        filter(
            &sharp,
            WIDTH,
            direction(i % WIDTH, i / WIDTH, WIDTH, 8),
            ROUGHNESS,
        )
    });
    filtered
}

fn error(site: &Site, probes: &[ReflectionProbeDesc], captures: &[Vec<Vec3>]) -> f32 {
    let mut weight = 0.;
    let mut sum = [0.; 3];
    for (probe, capture) in probes.iter().zip(captures) {
        let p = site.position;
        let r = site.reflected;
        let distance = (0..3)
            .map(|k| (p[k] - probe.min[k]).min(probe.max[k] - p[k]))
            .fold(f32::INFINITY, f32::min);
        if distance <= 0. {
            continue;
        }
        let w = (distance / probe.blend_distance).min(1.);
        let t = (0..3)
            .map(|k| {
                let extent = if r[k] >= 0. {
                    probe.max[k] - p[k]
                } else {
                    p[k] - probe.min[k]
                };
                extent / r[k].abs().max(1e-20)
            })
            .fold(f32::INFINITY, f32::min);
        let ray = unit(std::array::from_fn(|k| p[k] - probe.position[k] + t * r[k])).unwrap_or(r);
        let color = sample(capture, WIDTH, ray);
        for k in 0..3 {
            sum[k] += w * color[k];
        }
        weight += w;
    }
    (0..3)
        .map(|k| {
            let local = sum[k] / weight.max(1.) * site.response[k];
            (local + (1. - weight).max(0.) * site.sky[k] - site.reference[k]).abs()
        })
        .sum::<f32>()
        / 3.
}

fn metrics(sites: &[Site], errors: &[f32]) -> QualityMetrics {
    if errors.is_empty() {
        return QualityMetrics::default();
    }
    let mut ordered = errors.to_vec();
    ordered.sort_unstable_by(f32::total_cmp);
    let worst = errors
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap()
        .0;
    QualityMetrics {
        samples: errors.len(),
        mean_rgb_error: errors.iter().map(|&e| f64::from(e)).sum::<f64>() / errors.len() as f64,
        p90_rgb_error: ordered[(9 * errors.len()).div_ceil(10) - 1],
        worst_position: Some(sites[worst].position),
        worst_rgb_error: errors[worst],
    }
}

// Use paired differences so unaffected receivers cannot dilute a regression in
// the changed region. Also guard the neighborhood seen from every other capture.
fn improvement(sites: &[Site], before: &[f32], after: &[f32]) -> Result<f64, PlacementDecision> {
    let mut neighborhoods = [[0f64; 2]; ReflectionProbes::MAX_COUNT];
    let mut changed = 0;
    let mut old = 0.;
    let mut new = 0.;
    for ((site, &a), &b) in sites.iter().zip(before).zip(after) {
        if !a.is_finite() || !b.is_finite() {
            return Err(PlacementDecision::InsufficientGain);
        }
        if a == b {
            continue;
        }
        changed += 1;
        old += f64::from(a);
        new += f64::from(b);
        neighborhoods[site.owner][0] += f64::from(a);
        neighborhoods[site.owner][1] += f64::from(b);
    }
    if changed < 4 {
        return Err(PlacementDecision::TooFewChangedReceivers);
    }
    if new >= old * 0.99 {
        return Err(PlacementDecision::InsufficientGain);
    }
    if neighborhoods.iter().any(|[a, b]| *b > a * 1.05) {
        return Err(PlacementDecision::NeighborhoodRegression);
    }
    Ok(old - new)
}

impl BakeScene {
    /// Audit and conservatively repair one probe in a generated layout. A training
    /// sweep is followed by one independent acceptance test. Supply the lighting
    /// of the intended bake and the voxel spacing used to generate the layout.
    pub fn repair_reflection_placement(
        &self,
        probes: &mut [ReflectionProbeDesc],
        spacing: f32,
        light: Lighting,
    ) -> Result<PlacementQuality, BakeError> {
        light.validate(self)?;
        if probes.len() > ReflectionProbes::MAX_COUNT
            || !spacing.is_normal()
            || spacing <= 0.
            || !(spacing * 5.).is_finite()
        {
            return Err(BakeError("invalid reflection repair budget or spacing"));
        }
        for probe in probes.iter() {
            probe
                .validate()
                .map_err(|_| BakeError("invalid reflection repair box"))?;
        }
        let train = receivers(self, probes, light, spacing, false)?;
        let check = receivers(self, probes, light, spacing, true)?;
        let mut report = PlacementQuality {
            training_samples: train.len(),
            ..Default::default()
        };
        if check.is_empty() {
            return Ok(report);
        }
        let mut captures: Vec<_> = probes
            .iter()
            .map(|p| preview(self, p.position, light))
            .collect();
        let errors = |sites: &[Site], layout: &[ReflectionProbeDesc], maps: &[Vec<Vec3>]| {
            sites
                .iter()
                .map(|s| error(s, layout, maps))
                .collect::<Vec<_>>()
        };
        let baseline = errors(&train, probes, &captures);
        let check_before = errors(&check, probes, &captures);
        if baseline.iter().chain(&check_before).any(|e| !e.is_finite()) {
            return Err(BakeError("nonfinite reflection quality estimate"));
        }
        report.before = metrics(&check, &check_before);
        report.after = report.before;
        report.decision = PlacementDecision::NoTrainingGain;
        let mut trial = probes.to_vec();
        let mut best_gain = 0.;
        let mut best = None;
        // First vary each face independently. All alternatives reuse the captures.
        for (index, &original) in probes.iter().enumerate() {
            for axis in 0..3 {
                for upper in [false, true] {
                    for delta in [-0.5, 0.5] {
                        let mut candidate = original;
                        let face = if upper {
                            &mut candidate.max
                        } else {
                            &mut candidate.min
                        };
                        face[axis] += spacing * delta;
                        if candidate.validate().is_err() {
                            continue;
                        }
                        trial[index] = candidate;
                        report.candidates_tested += 1;
                        if let Ok(gain) =
                            improvement(&train, &baseline, &errors(&train, &trial, &captures))
                            && gain > best_gain
                        {
                            best_gain = gain;
                            best = Some((index, candidate, None));
                        }
                    }
                }
            }
            trial[index] = original;
        }
        // If box edits cannot improve the training set, move up to two existing
        // captures towards their worst visible receiver, along the known clear ray.
        if best.is_none() {
            let mut ranked: Vec<_> = (0..train.len()).collect();
            ranked.sort_by(|&a, &b| baseline[b].total_cmp(&baseline[a]));
            let mut tried = [false; ReflectionProbes::MAX_COUNT];
            for i in ranked {
                let site = &train[i];
                let index = site.owner;
                if tried[index] || baseline[i] == 0. {
                    continue;
                }
                let mut candidate = probes[index];
                let delta = sub(site.position, candidate.position);
                let fraction = (spacing * 0.5 / dot(delta, delta).sqrt()).min(0.5);
                candidate.position =
                    std::array::from_fn(|k| candidate.position[k] + fraction * delta[k]);
                if candidate.validate().is_err() {
                    continue;
                }
                tried[index] = true;
                trial[index] = candidate;
                let alternate = preview(self, candidate.position, light);
                let original = std::mem::replace(&mut captures[index], alternate);
                let result = errors(&train, &trial, &captures);
                let alternate = std::mem::replace(&mut captures[index], original);
                trial[index] = probes[index];
                report.candidates_tested += 1;
                report.replacement_captures += 1;
                if let Ok(gain) = improvement(&train, &baseline, &result)
                    && gain > best_gain
                {
                    best_gain = gain;
                    best = Some((index, candidate, Some(alternate)));
                }
                if report.replacement_captures == 2 {
                    break;
                }
            }
        }
        if let Some((index, candidate, alternate)) = best {
            trial[index] = candidate;
            let moved = alternate.is_some();
            if let Some(map) = alternate {
                captures[index] = map;
            }
            let proposed = errors(&check, &trial, &captures);
            if proposed.iter().any(|e| !e.is_finite()) {
                return Err(BakeError("nonfinite reflection quality estimate"));
            }
            let after = metrics(&check, &proposed);
            report.proposed = Some(after);
            report.decision = match improvement(&check, &check_before, &proposed) {
                Err(reason) => reason,
                Ok(_) if after.p90_rgb_error > report.before.p90_rgb_error => {
                    PlacementDecision::TailRegression
                }
                Ok(_) => PlacementDecision::Accepted,
            };
            if report.decision == PlacementDecision::Accepted {
                probes[index] = candidate;
                report.changed_probe = Some(index);
                report.moved_capture = moved;
                report.after = after;
            }
        }
        Ok(report)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Material, Triangle};

    fn room() -> BakeScene {
        let mut triangles = Vec::new();
        for axis in 0..3 {
            for side in [-1., 1.] {
                let corner = |u, v| {
                    let mut p = [0.; 3];
                    p[axis] = side * 2.;
                    p[(axis + 1) % 3] = u;
                    p[(axis + 2) % 3] = v;
                    p
                };
                let vertices = [
                    corner(-2., -2.),
                    corner(2., -2.),
                    corner(2., 2.),
                    corner(-2., 2.),
                ];
                for indices in [[0, 1, 2], [0, 2, 3]] {
                    let mut positions = indices.map(|i| vertices[i]);
                    if side > 0. {
                        positions.swap(1, 2);
                    }
                    triangles.push(Triangle {
                        positions,
                        uv: [[0.; 2]; 3],
                        material: 0,
                    });
                }
            }
        }
        BakeScene::new(
            triangles,
            vec![Material {
                diffuse: [0.; 3],
                ..Default::default()
            }],
            vec![],
        )
        .unwrap()
    }

    fn room_probe() -> ReflectionProbeDesc {
        ReflectionProbeDesc {
            position: [0.; 3],
            min: [-3.; 3],
            max: [3.; 3],
            blend_distance: 0.25,
        }
    }

    #[test]
    fn repairs_sky_leaking_through_one_face_and_preserves_a_correct_room() {
        let scene = room();
        let light = Lighting::uniform_sky([2.; 3]);
        let mut probes = [room_probe()];
        probes[0].max[1] = 1.9;
        let report = scene
            .repair_reflection_placement(&mut probes, 2., light)
            .unwrap();
        assert_eq!(report.changed_probe, Some(0));
        assert!(!report.moved_capture);
        assert_eq!(probes[0].position, [0.; 3]);
        assert!(report.before.mean_rgb_error > 0. && report.after.mean_rgb_error == 0.);
        assert_eq!(report.after.samples, SITES_PER_PROBE);
        let original = probes[0];
        let second = scene
            .repair_reflection_placement(&mut probes, 2., light)
            .unwrap();
        assert_eq!(second.changed_probe, None);
        assert_eq!(second.after.mean_rgb_error, 0.);
        assert_eq!(original.max, probes[0].max);
    }

    #[test]
    fn rejects_a_tradeoff_that_damages_another_neighborhood() {
        let sites: Vec<_> = (0..8)
            .map(|i| Site {
                owner: usize::from(i >= 4),
                position: [0.; 3],
                reflected: [1., 0., 0.],
                reference: [0.; 3],
                sky: [0.; 3],
                response: [0.; 3],
            })
            .collect();
        assert!(improvement(&sites, &[1.; 8], &[0., 0., 0., 0., 1.1, 1.1, 1.1, 1.1]).is_err());
        assert!(improvement(&sites, &[1.; 8], &[0., 0., 1., 1., 1., 1., 1., 1.]).is_err());
    }

    #[test]
    fn no_visible_surfaces_yields_no_measurement_and_no_edits() {
        let scene = BakeScene::new(vec![], vec![], vec![]).unwrap();
        let light = Lighting::uniform_sky([1.; 3]);
        let mut probes = [room_probe()];
        let quality = scene
            .repair_reflection_placement(&mut probes, 1., light)
            .unwrap();
        assert_eq!(quality.after.samples, 0);
        assert!(quality.after.worst_position.is_none());
        assert_eq!(quality.candidates_tested, 0);
        assert!(
            scene
                .repair_reflection_placement(&mut probes, f32::NAN, light)
                .is_err()
        );
    }
}
