//! Game asset selection and shared crowd pose slots.
use sashiko::animation::{AnimatedModel, CharacterCatalog, PhaseAnimation};
use sashiko_renderer::{
    material::{MaterialHandle, Materials},
    mesh::{MeshHandle, Meshes, SkinPoseId},
    pipeline::PipelineResult,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub fn catalog_path() -> PipelineResult<PathBuf> {
    let exe = std::env::current_exe()?;
    let relative = Path::new("assets/survivor/characters/catalog.json");
    let packaged = exe
        .parent()
        .ok_or("executable directory unavailable")?
        .join(relative);
    if exe.with_file_name("sashiko-package.json").try_exists()? || packaged.try_exists()? {
        return Ok(packaged);
    }
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative))
}

pub(super) const WALK_PHASES: usize = 4;

pub(super) struct Actor {
    pub mesh: MeshHandle,
    pub poses: [SkinPoseId; WALK_PHASES],
    pub texture: usize,
    pub material: MaterialHandle,
    animation: PhaseAnimation,
}

pub(super) struct Cast {
    pub catalog_digest: sashiko::asset::Digest,
    pub models: Vec<AnimatedModel>,
    pub actors: Vec<Actor>,
    pub subjects: Vec<usize>,
}
impl Cast {
    pub fn load<'a>(
        path: &Path,
        player: &'a str,
        compute_skin: bool,
        subjects: impl IntoIterator<Item = (Option<&'a str>, &'a str)>,
        meshes: &mut Meshes,
        materials: &mut Materials,
        default_subject: MaterialHandle,
    ) -> PipelineResult<Self> {
        let catalog = CharacterCatalog::open(path)?;
        let mut models = Vec::<AnimatedModel>::new();
        let mut actors = Vec::new();
        let mut loaded = HashMap::new();
        let mut actors_by_clip = HashMap::new();
        let mut subject_indices = Vec::new();
        for (name, clip) in std::iter::once((Some(player), "walk")).chain(subjects) {
            let name = name.unwrap_or(player);
            let id = catalog
                .id(name)
                .ok_or_else(|| format!("{}: unknown character {name}", path.display()))?;
            let actor = if let Some(&index) = actors_by_clip.get(&(id, clip)) {
                index
            } else {
                let (texture, mesh) = if let Some(&loaded) = loaded.get(&id) {
                    loaded
                } else {
                    let model = catalog.load(id)?;
                    let data = model.model.mesh_data()?;
                    let mesh = meshes.create(if compute_skin {
                        data.with_compute_skin()?
                    } else {
                        data
                    })?;
                    let texture = models.len();
                    models.push(model);
                    loaded.insert(id, (texture, mesh));
                    (texture, mesh)
                };
                let animation = models[texture].prepare_animation(clip)?.into_four_phases();
                let poses = [
                    meshes.create_pose(mesh)?,
                    meshes.create_pose(mesh)?,
                    meshes.create_pose(mesh)?,
                    meshes.create_pose(mesh)?,
                ];
                let material = if actors.is_empty() {
                    default_subject
                } else {
                    materials.duplicate(default_subject)?
                };
                let index = actors.len();
                actors.push(Actor {
                    mesh,
                    poses,
                    texture,
                    material,
                    animation,
                });
                actors_by_clip.insert((id, clip), index);
                index
            };
            subject_indices.push(actor);
        }
        // The first entry is the player's animation, fixed at actor slot zero.
        subject_indices.remove(0);
        Ok(Self {
            catalog_digest: catalog.definition_digest(),
            models,
            actors,
            subjects: subject_indices,
        })
    }

    pub fn animate(&mut self, tick: u64, meshes: &mut Meshes) -> PipelineResult<()> {
        for actor in &mut self.actors {
            let ticks = std::array::from_fn(|phase| tick.wrapping_add(phase as u64 * 13));
            let palettes = actor.animation.sample(
                ticks,
                crate::TICK_HZ.try_into().expect("positive update rate"),
            )?;
            for (pose, palette) in actor.poses.iter().zip(palettes) {
                meshes.set_joint_transforms(*pose, palette)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sashiko_renderer::mesh::{MeshCapacity, Meshes};

    #[test]
    fn both_subject_appearances_play_the_walk_cycle_in_renderer() -> PipelineResult<()> {
        let catalog = CharacterCatalog::open(&catalog_path()?)?;
        for variant in catalog.names() {
            let subject = catalog.load(catalog.id(variant).unwrap())?;
            let clip = subject.clip("walk").unwrap();
            let palette_at = |tick| clip.sample(tick, crate::TICK_HZ.try_into().unwrap());
            let mut meshes = Meshes::new(MeshCapacity {
                meshes: 1,
                retired_mesh_versions: 0,
            })?;
            let mesh = meshes.create(subject.model.mesh_data()?)?;
            let pose = meshes.create_pose(mesh)?;
            let frames = clip.frames() as u64;
            for tick in [0, frames / 4, frames / 2, frames - 1] {
                meshes.set_joint_transforms(pose, palette_at(tick))?;
            }
            // The simulation always puts a subject's origin on the floor.
            // A replacement skin must remain planted throughout the walk.
            for tick in 0..frames {
                let palette = palette_at(tick);
                let sole = subject
                    .model
                    .vertices()
                    .map(|(v, weights)| {
                        weights
                            .joints
                            .iter()
                            .zip(weights.weights)
                            .map(|(&j, w)| {
                                w * palette[usize::from(j)].transform_point(v.position())[1]
                            })
                            .sum::<f32>()
                    })
                    .fold(f32::INFINITY, f32::min);
                assert!(sole.abs() < 0.025, "{variant:?} at tick {tick}: {sole} m");
            }
            assert_ne!(palette_at(0), palette_at(frames / 2));
            assert_eq!(palette_at(0), palette_at(frames));
            assert_eq!(subject.color().0.len(), 256 * 256 * 4);
        }
        Ok(())
    }
    #[test]
    fn subject_prefabs_share_pose_slots_and_can_override_the_player_model() -> PipelineResult<()> {
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 2,
            retired_mesh_versions: 0,
        })?;
        let mut materials = Materials::default();
        let material = materials.create_pbr(sashiko_renderer::material::PbrParams::default())?;
        let mut cast = Cast::load(
            &catalog_path()?,
            "primary",
            true,
            [
                (None, "walk"),
                (Some("alternate"), "jog"),
                (Some("alternate"), "jog"),
            ],
            &mut meshes,
            &mut materials,
            material,
        )?;
        assert_eq!(cast.subjects, [0, 1, 1]);
        assert_ne!(cast.actors[0].mesh, cast.actors[1].mesh);
        cast.animate(20, &mut meshes)
    }
    #[test]
    fn transitions_use_the_shared_rig_and_advance_both_clips() -> PipelineResult<()> {
        let catalog = CharacterCatalog::open(&catalog_path()?)?;
        let rate = crate::TICK_HZ.try_into().unwrap();
        for name in catalog.names() {
            let model = catalog.load(catalog.id(name).unwrap())?;
            let mut walk = model.prepare_animation("walk")?;
            let mut jog = model.prepare_animation("jog")?;
            let (source, out) = walk.rigged().unwrap();
            let (target, _) = jog.rigged().unwrap();
            let mut player = sashiko::animation::Playback::new(source, rate);
            player.advance_to(36, &mut [])?;
            let before = out.evaluate(&player)?.to_vec();
            player.crossfade(target, 12.try_into().unwrap())?;
            assert_eq!(out.evaluate(&player)?, before);
            player.advance_to(42, &mut [])?;
            assert_ne!(out.evaluate(&player)?, before);
            player.advance_to(48, &mut [])?;
            assert_eq!(out.evaluate(&player)?, jog.sample(12, rate)?);
        }
        Ok(())
    }
    #[test]
    fn evaluated_palettes_match_both_cooked_rigs() -> PipelineResult<()> {
        let catalog = CharacterCatalog::open(&catalog_path()?)?;
        let rate = crate::TICK_HZ.try_into().unwrap();
        for name in catalog.names() {
            let model = catalog.load(catalog.id(name).unwrap())?;
            for clip_name in model.clip_names() {
                let mut animation = model.prepare_animation(clip_name)?;
                let reference = model.clip(clip_name).unwrap();
                for tick in 0..=reference.frames() as u64 {
                    let actual = animation.sample(tick, rate)?;
                    let expected = reference.sample(tick, rate);
                    for (a, b) in actual.iter().zip(expected) {
                        for (x, y) in a
                            .columns()
                            .into_iter()
                            .flatten()
                            .zip(b.columns().into_iter().flatten())
                        {
                            assert!(
                                (x - y).abs() < 2e-4,
                                "{name}/{clip_name} tick {tick}: {x} vs {y}"
                            );
                        }
                    }
                    for (v, w) in model.model.vertices().step_by(71) {
                        let position = |palette: &[sashiko_renderer::pbr::Transform]| {
                            let mut p = [0.; 3];
                            for (&j, weight) in w.joints.iter().zip(w.weights) {
                                let q = palette[j as usize].transform_point(v.position());
                                for i in 0..3 {
                                    p[i] += weight * q[i];
                                }
                            }
                            p
                        };
                        assert!(
                            position(actual)
                                .into_iter()
                                .zip(position(expected))
                                .all(|(a, b)| (a - b).abs() < 2e-4)
                        );
                    }
                }
            }
        }
        Ok(())
    }
}
