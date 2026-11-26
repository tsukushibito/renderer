//! Double precision oracle from authored palettes, independent of runtime packing.
use super::*;
use crate::animation::CharacterCatalog;
use std::path::Path;

fn reference(rig: &Rig, source: &[Transform], target: &[Transform], t: f64) -> Vec<DMat4> {
    let worlds = |palette: &[Transform]| {
        palette
            .iter()
            .zip(&rig.bind)
            .map(|(&p, &b)| matrix(p) * b)
            .collect::<Vec<_>>()
    };
    let a = worlds(source);
    let b = worlds(target);
    if t == 0. {
        return a;
    }
    if t == 1. {
        return b;
    }
    let locals = (0..a.len())
        .map(|i| {
            let local = |world: &[DMat4]| {
                rig.parents[i]
                    .map_or(world[i], |p| world[p].inverse() * world[i])
                    .to_scale_rotation_translation()
            };
            let (sa, qa, pa) = local(&a);
            let (sb, qb, pb) = local(&b);
            DMat4::from_scale_rotation_translation(
                sa.lerp(sb, t),
                qa.normalize().slerp(qb.normalize(), t),
                pa.lerp(pb, t),
            )
        })
        .collect::<Vec<_>>();
    // Walk each ancestor chain independently; no reliance on runtime storage order.
    (0..a.len())
        .map(|i| {
            let mut parent = rig.parents[i];
            let mut model = locals[i];
            while let Some(p) = parent {
                model = locals[p] * model;
                parent = rig.parents[p];
            }
            model
        })
        .collect()
}

#[test]
fn compare_every_joint_origin_palette_and_representative_vertex_to_f64() -> PipelineResult<()> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/survivor/characters/catalog.json");
    let catalog = CharacterCatalog::open(&path)?;
    let rate = 60.try_into()?;
    for name in catalog.names() {
        let model = catalog.load(catalog.id(name).unwrap())?;
        let rig = model.model.skeleton.as_ref().unwrap();
        let mut max_origin = 0f64;
        let mut max_vertex = 0f64;
        let mut max_matrix = 0f64;
        for key in model.clip_names() {
            let next = if key == "walk" { "jog" } else { "walk" };
            let mut start = model.prepare_animation(key)?;
            let mut end = model.prepare_animation(next)?;
            let (from, out) = start.rigged().unwrap();
            let (to, _) = end.rigged().unwrap();
            let raw = model.clip(key).unwrap();
            let next_raw = model.clip(next).unwrap();
            let mut batch = PoseBatch::new(&out.workspace, 4);
            let mut p = Playback::new(from, rate);
            // All source frames, a full transition, then the first target-only tick.
            let fade_start = raw.frames() as u64;
            for tick in 0..=fade_start + 13 {
                p.advance_to(tick, &mut [0; 2])?;
                if tick == fade_start {
                    p.crossfade(to, 12.try_into()?)?;
                }
                let age = tick.saturating_sub(fade_start);
                let expected = reference(
                    rig,
                    raw.sample(tick, rate),
                    next_raw.sample(age, rate),
                    (age as f64 / 12.).min(1.),
                );
                let samples = [
                    from.sample_at(tick + 13, rate),
                    p.sample(),
                    to.sample_at(tick + 26, rate),
                    from.sample_at(tick + 39, rate),
                ];
                let packed = &batch.evaluate(&samples)?[1];
                p.evaluate_pose(&mut out.workspace)?;
                for pose in [&out.workspace, packed] {
                    write_palette(pose, &out.inverse_bind, &mut out.palette)?;
                    for (i, &world) in expected.iter().enumerate() {
                        let origin = glam::Vec4::from_array(pose.joint(i).unwrap()[3])
                            .truncate()
                            .as_dvec3();
                        max_origin = max_origin.max(origin.distance(world.w_axis.truncate()));
                        let skin = world * rig.bind[i].inverse();
                        let diff = matrix(out.palette[i]) - skin;
                        max_matrix = max_matrix.max(
                            diff.to_cols_array()
                                .iter()
                                .map(|x| x.abs())
                                .fold(0., f64::max),
                        );
                    }
                    for (vertex, weights) in model.model.vertices().step_by(71) {
                        let v = glam::Vec3::from_array(vertex.position()).as_dvec3();
                        let mut error = glam::DVec3::ZERO;
                        for (&j, &weight) in weights.joints.iter().zip(&weights.weights) {
                            let j = usize::from(j);
                            let reference =
                                (expected[j] * rig.bind[j].inverse()).transform_point3(v);
                            let actual = glam::Vec3::from_array(
                                out.palette[j].transform_point(vertex.position()),
                            )
                            .as_dvec3();
                            error += (actual - reference) * f64::from(weight);
                        }
                        max_vertex = max_vertex.max(error.length());
                    }
                }
            }
        }
        println!(
            "{name}: joint origin {max_origin:e} m, vertex {max_vertex:e} m, matrix {max_matrix:e}"
        );
        assert!(max_origin < 2e-4 && max_vertex < 2e-4 && max_matrix < 2e-4);
    }
    Ok(())
}
