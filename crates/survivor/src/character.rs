//! Game asset selection and shared crowd pose slots.
use renderer::{
    material::{MaterialHandle, Materials},
    mesh::{MeshHandle, Meshes, SkinPoseId},
    pipeline::PipelineResult,
};
use sashiko::animation::{AnimatedModel, CharacterCatalog};
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
    clip: String,
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
                    let mesh = meshes.create(model.model.mesh_data()?)?;
                    let texture = models.len();
                    models.push(model);
                    loaded.insert(id, (texture, mesh));
                    (texture, mesh)
                };
                if models[texture].clip(clip).is_none() {
                    return Err(format!("unknown animation {name}/{clip}").into());
                }
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
                    clip: clip.to_owned(),
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

    pub fn animate(&self, tick: u64, meshes: &mut Meshes) -> PipelineResult<()> {
        for actor in &self.actors {
            let clip = self.models[actor.texture]
                .clip(&actor.clip)
                .expect("validated at load");
            for (phase, pose) in actor.poses.iter().enumerate() {
                let time = tick.wrapping_add(phase as u64 * 13);
                meshes.set_joint_transforms(
                    *pose,
                    clip.sample(
                        time,
                        crate::TICK_HZ.try_into().expect("positive update rate"),
                    ),
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::mesh::{MeshCapacity, Meshes};

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
        let material = materials.create_pbr(renderer::material::PbrParams::default())?;
        let cast = Cast::load(
            &catalog_path()?,
            "primary",
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
    fn both_cooked_rigs_crossfade_walk_to_jog() -> PipelineResult<()> {
        let catalog = CharacterCatalog::open(&catalog_path()?)?;
        let rate = crate::TICK_HZ.try_into().unwrap();
        let duration = std::num::NonZeroU64::new(12).unwrap();
        for name in catalog.names() {
            let model = catalog.load(catalog.id(name).unwrap())?;
            let mut fade = model.prepare_crossfade("walk", "jog")?;
            assert_eq!(
                fade.sample(36, 0, duration, rate)?,
                model.clip("walk").unwrap().sample(36, rate)
            );
            for tick in 1..12 {
                let pose = fade.sample(36, tick, duration, rate)?;
                assert_eq!(pose.len(), 62);
                assert_ne!(pose, model.clip("jog").unwrap().sample(tick, rate));
            }
            assert_eq!(
                fade.sample(36, 12, duration, rate)?,
                model.clip("jog").unwrap().sample(12, rate)
            );
        }
        Ok(())
    }
}
