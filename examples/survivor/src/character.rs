//! Game asset selection and shared crowd pose slots.
use sashiko::animation::{
    AnimatedModel, CharacterCatalog, LocalBuffer, PaletteBatch, PaletteRequest, PoseClip,
};
use sashiko_renderer::{
    core::jobs::RendererJobs,
    material::{MaterialHandle, Materials},
    mesh::{MeshHandle, Meshes, SkinPoseId},
    pipeline::PipelineResult,
};
use std::{collections::HashMap, path::Path};

pub(super) const WALK_PHASES: usize = 4;

pub(super) struct Actor {
    pub mesh: MeshHandle,
    pub poses: [SkinPoseId; WALK_PHASES],
    pub texture: usize,
    pub material: MaterialHandle,
    clip: usize,
    requested: u8,
}

struct ModelPoses {
    batch: PaletteBatch,
    requests: Vec<PaletteRequest>,
    targets: Vec<SkinPoseId>,
}

impl ModelPoses {
    fn request(&mut self, request: PaletteRequest, pose: SkinPoseId) {
        self.requests.push(request);
        self.targets.push(pose);
    }
}

pub(super) struct Cast {
    pub catalog_digest: sashiko::asset::Digest,
    pub models: Vec<AnimatedModel>,
    pub actors: Vec<Actor>,
    subjects: Vec<[usize; 2]>,
    pub player_pose: SkinPoseId,
    last_player_sample: Option<(f64, f64, f32)>,
    player_gaits: Option<(PoseClip, Option<PoseClip>, LocalBuffer)>,
    poses: Vec<ModelPoses>,
    player_clips: [usize; 2],
}
impl Cast {
    pub fn load<'a>(
        path: &Path,
        player: &'a str,
        compute_skin: bool,
        subjects: impl IntoIterator<Item = (Option<&'a str>, &'a str, bool)>,
        meshes: &mut Meshes,
        materials: &mut Materials,
        default_subject: MaterialHandle,
    ) -> PipelineResult<Self> {
        let catalog = CharacterCatalog::open(path)?;
        let mut models = Vec::<AnimatedModel>::new();
        let mut actors = Vec::new();
        let mut clip_names = Vec::<Vec<&str>>::new();
        let mut loaded = HashMap::new();
        let mut actors_by_clip = HashMap::new();
        let mut subject_indices = Vec::new();
        let mut actor_for = |name: &'a str, clip: &'a str| -> PipelineResult<usize> {
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
                    let data = model.model().mesh_data()?;
                    let mesh = meshes.create(if compute_skin {
                        data.with_compute_skin()?
                    } else {
                        data
                    })?;
                    let texture = models.len();
                    models.push(model);
                    clip_names.push(Vec::new());
                    loaded.insert(id, (texture, mesh));
                    (texture, mesh)
                };
                let clip_index = clip_names[texture].len();
                clip_names[texture].push(clip);
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
                    clip: clip_index,
                    requested: 0,
                });
                actors_by_clip.insert((id, clip), index);
                index
            };
            Ok(actor)
        };
        actor_for(player, "walk")?;
        for (name, clip, lunges) in subjects {
            let name = name.unwrap_or(player);
            let normal = actor_for(name, clip)?;
            let dash = if lunges && catalog.has_clip(name, "jog") {
                actor_for(name, "jog")?
            } else {
                normal
            };
            subject_indices.push([normal, dash]);
        }
        let player_pose = meshes.create_pose(actors[0].mesh)?;
        let player_model = actors[0].texture;
        let walk = actors[0].clip;
        let names = &mut clip_names[player_model];
        let jog = if models[player_model].clip("jog").is_some() {
            names
                .iter()
                .position(|&name| name == "jog")
                .unwrap_or_else(|| {
                    names.push("jog");
                    names.len() - 1
                })
        } else {
            walk
        };
        let model = &models[player_model];
        let player_gaits = if model.has_skeleton() {
            let walk_index = walk;
            let walk = model.prepare_pose_clip("walk")?;
            let jog = if jog != walk_index {
                Some(model.prepare_pose_clip("jog")?)
            } else {
                None
            };
            let mixer = LocalBuffer::new(walk.skeleton());
            Some((walk, jog, mixer))
        } else {
            None
        };
        let poses = models
            .iter()
            .enumerate()
            .zip(clip_names)
            .map(|((index, model), names)| {
                let capacity = actors.iter().filter(|a| a.texture == index).count() * WALK_PHASES
                    + usize::from(index == player_model);
                Ok(ModelPoses {
                    batch: model.prepare_palette_batch(
                        &names,
                        capacity,
                        usize::from(index == player_model && player_gaits.is_some()),
                    )?,
                    requests: Vec::with_capacity(capacity),
                    targets: Vec::with_capacity(capacity),
                })
            })
            .collect::<PipelineResult<_>>()?;
        Ok(Self {
            player_pose,
            player_gaits,
            last_player_sample: None,
            player_clips: [walk, jog],
            poses,
            catalog_digest: catalog.definition_digest(),
            models,
            actors,
            subjects: subject_indices,
        })
    }

    pub fn subject_actor(&self, enemy: &crate::Enemy) -> &Actor {
        &self.actors[self.subjects[enemy.archetype][usize::from(enemy.dashing)]]
    }

    pub fn animate(
        &mut self,
        game: &crate::Game,
        jobs: &RendererJobs,
        meshes: &mut Meshes,
    ) -> PipelineResult<()> {
        for model in &mut self.poses {
            model.requests.clear();
            model.targets.clear();
        }
        for enemy in &game.enemies {
            let index = self.subjects[enemy.archetype][usize::from(enemy.dashing)];
            self.actors[index].requested |= 1 << (enemy.id % WALK_PHASES as u64);
        }
        for actor in &mut self.actors {
            let requested = std::mem::take(&mut actor.requested);
            for (phase, pose) in actor.poses.into_iter().enumerate() {
                if requested & (1 << phase) == 0 {
                    continue;
                }
                self.poses[actor.texture].request(
                    PaletteRequest::Frame {
                        clip: actor.clip,
                        tick: game.tick.wrapping_add(phase as u64 * 13),
                    },
                    pose,
                );
            }
        }
        let player = game.locomotion.sample();
        let player_model = self.actors[0].texture;
        let mut resolved = None;
        if self.last_player_sample != Some(player) {
            let (walk_phase, jog_phase, weight) = player;
            let dominant = usize::from(weight >= 0.5);
            let phase = [walk_phase, jog_phase][dominant];
            let mut request = PaletteRequest::Phase {
                clip: self.player_clips[dominant],
                phase,
            };
            if let Some((walk, jog, mixer)) = &mut self.player_gaits {
                let run = jog.as_ref().unwrap_or(walk);
                if weight > 0. && weight < 1. {
                    mixer.mix(&[
                        (walk.sample_phase(walk_phase)?, 1. - weight),
                        (run.sample_phase(jog_phase)?, weight),
                    ])?;
                    resolved = Some(mixer.sample());
                } else {
                    let clip = if dominant == 0 { &*walk } else { run };
                    if (phase * clip.timeline().frames() as f64).fract() != 0. {
                        resolved = Some(clip.sample_phase(phase)?);
                    }
                }
                if resolved.is_some() {
                    request = PaletteRequest::Sample(0);
                }
            }
            self.poses[player_model].request(request, self.player_pose);
        }
        let rate = crate::TICK_HZ.try_into().expect("positive update rate");
        for (i, model) in self.poses.iter_mut().enumerate() {
            let samples = if i == player_model {
                resolved.as_slice()
            } else {
                &[]
            };
            let palettes = model.batch.evaluate(&model.requests, samples, rate, jobs)?;
            for (&pose, palette) in model.targets.iter().zip(palettes) {
                meshes.set_joint_transforms(pose, palette)?;
            }
        }
        self.last_player_sample = Some(player);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_path;
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
            let mesh = meshes.create(subject.model().mesh_data()?)?;
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
                    .model()
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
                (None, "walk", true),
                (Some("alternate"), "jog", false),
                (Some("alternate"), "jog", false),
            ],
            &mut meshes,
            &mut materials,
            material,
        )?;
        assert_eq!(cast.subjects, [[0, 1], [2, 2], [2, 2]]);
        assert_ne!(cast.actors[0].mesh, cast.actors[2].mesh);
        let game = crate::Game::new(Default::default(), crate::Scenario::Early, 1);
        cast.animate(
            &game,
            &RendererJobs::new(sashiko_renderer::core::jobs::JobSystem::inline()),
            &mut meshes,
        )
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
            player.advance_to(36, &mut [0; 2])?;
            let before = out.evaluate(&player)?.to_vec();
            player.crossfade(target, 12.try_into().unwrap())?;
            assert_eq!(out.evaluate(&player)?, before);
            player.advance_to(42, &mut [0; 2])?;
            assert_ne!(out.evaluate(&player)?, before);
            player.advance_to(48, &mut [0; 2])?;
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
                    for (v, w) in model.model().vertices().step_by(71) {
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
