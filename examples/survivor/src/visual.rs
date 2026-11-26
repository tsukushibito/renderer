//! Research hall, animated subjects and gameplay feedback shared by capture and presentation.
use sashiko::jobs::{JobSystem, SharedJobs};
use sashiko::math::{LocalTransform, Rotation3, Vec2, Vec3};
use sashiko::view::{DrawList, on_screen, render_transform};
mod geometry;
mod hud;
mod instruments;
mod lab;
mod surfaces;

pub use crate::catalog_path;
use crate::character::{Cast, WALK_PHASES};
use crate::{Game, MAX_DROPS, MAX_ENEMIES, MAX_SHOTS};
use sashiko_renderer::{
    Renderer, WindowRenderer,
    canvas::Layer,
    core::runtime::Runtime,
    material::{AlphaMode, MaterialHandle, Materials, PbrParams},
    mesh::{MeshCapacity, MeshHandle, Meshes},
    pbr::{Bounds, LocalLight, Sun, Transform},
    pipeline::{CameraView, PipelineResult, RenderObject, SceneAssets, SceneInput},
};
use surfaces::Surfaces;

/// Lift the game's XZ bearing into a 3D placement; coincident targets face +Z.
fn placement(pos: Vec3, scale: Vec3, facing: Vec2) -> PipelineResult<Transform> {
    let rotation = if facing == Vec2::ZERO {
        Rotation3::IDENTITY
    } else {
        Rotation3::from_forward_up(Vec3::new(facing.x(), 0., facing.y()), Vec3::Y)?
    };
    render_transform(LocalTransform::new(pos, rotation, scale))
}

struct Palette {
    floor: MaterialHandle,
    steel: MaterialHandle,
    dark: MaterialHandle,
    ivory: MaterialHandle,
    lamp: MaterialHandle,
    lcd: MaterialHandle,
    safety: MaterialHandle,
    player: MaterialHandle,
    subject: MaterialHandle,
    shadow: MaterialHandle,
    pulse: MaterialHandle,
    drop: MaterialHandle,
    warning: MaterialHandle,
    shot: MaterialHandle,
}

fn surface(
    materials: &mut Materials,
    rgb: [f32; 3],
    metallic: f32,
    roughness: f32,
    emission: f32,
) -> PipelineResult<MaterialHandle> {
    let mut params = PbrParams::color([rgb[0], rgb[1], rgb[2], 1.]);
    params.factors.metallic = metallic;
    params.factors.roughness = roughness;
    params.factors.emissive_nits = rgb.map(|x| x * emission);
    Ok(materials.create_pbr(params)?)
}

impl Palette {
    fn new(m: &mut Materials) -> PipelineResult<Self> {
        let mut shadow = PbrParams::color([0., 0., 0., 0.38]);
        shadow.factors.double_sided = true;
        shadow.factors.alpha_mode = AlphaMode::Blend;
        Ok(Self {
            floor: surface(m, [0.34, 0.35, 0.35], 0., 0.72, 0.)?,
            steel: surface(m, [0.32, 0.38, 0.40], 0.58, 0.30, 0.)?,
            dark: surface(m, [0.018, 0.028, 0.032], 0.12, 0.55, 0.)?,
            ivory: surface(m, [0.52, 0.58, 0.56], 0.12, 0.28, 0.)?,
            lamp: surface(m, [0.69, 0.90, 1.0], 0.05, 0.26, 2.)?,
            lcd: surface(m, [0.05, 0.51, 0.58], 0.12, 0.20, 0.75)?,
            safety: surface(m, [0.68, 0.33, 0.048], 0., 0.5, 0.)?,
            player: surface(m, [0.67, 0.78, 0.80], 0.06, 0.42, 0.10)?,
            subject: surface(m, [0.35, 0.14, 0.078], 0.02, 0.56, 0.045)?,
            shadow: m.create_pbr(shadow)?,
            pulse: surface(m, [0.12, 0.85, 0.96], 0., 0.35, 1.8)?,
            drop: surface(m, [0.03, 0.84, 0.53], 0.05, 0.31, 1.35)?,
            warning: surface(m, [1., 0.15, 0.012], 0., 0.45, 1.20)?,
            shot: surface(m, [1., 0.74, 0.22], 0., 0.18, 2.)?,
        })
    }
}

#[derive(Clone, Copy, Default)]
pub struct CharacterRendering {
    pub compute_skin: bool,
    pub shadows: bool,
}

pub struct Visual {
    jobs: SharedJobs,
    meshes: Meshes,
    materials: Materials,
    palette: Palette,
    texture_owners: Option<Surfaces>,
    prepared: Vec<RenderObject>,
    objects: DrawList,
    hud: Vec<Layer>,
    cast: Cast,
    character_shadows: bool,
    block: MeshHandle,
    ring: MeshHandle,
    disk: MeshHandle,
    tracking: sashiko::view::TrackingCamera,
    camera_anchor: Vec2,
    lights: Vec<LocalLight>,
    sun: Sun,
    bounds: Bounds,
    visible: usize,
    drawn: usize,
    last_step: Option<u64>,
    player_facing: Vec2,
    previous_player: Vec2,
}

impl Visual {
    pub fn content(&self, character: &str) -> crate::RecordedVisual {
        crate::RecordedVisual {
            character: character.into(),
            character_shadows: self.character_shadows,
            catalog: self.cast.catalog_digest,
            models: self
                .cast
                .models
                .iter()
                .map(|model| model.content_digests())
                .collect(),
            environment: surfaces::content_digests(),
        }
    }

    pub fn new(
        game: &Game,
        catalog: &std::path::Path,
        character: &str,
        characters: CharacterRendering,
    ) -> PipelineResult<Self> {
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 20usize
                .checked_add(game.arena.subjects.len())
                .ok_or("subject mesh capacity overflow")?,
            retired_mesh_versions: 4,
        })?;
        let mut materials = Materials::default();
        let palette = Palette::new(&mut materials)?;
        let hall = lab::build(&game.arena, &mut meshes, &palette)?;
        let mut cube = geometry::Geometry::default();
        cube.slab([0., 0., 0.], [1.; 3])?;
        let block = meshes.create(cube.data()?)?;
        let ring = meshes.create(geometry::disk(0.91)?)?;
        let disk = meshes.create(geometry::disk(0.0001)?)?;
        let cast = Cast::load(
            catalog,
            character,
            characters.compute_skin,
            game.arena.subjects.iter().map(|subject| {
                (
                    subject.model.as_deref(),
                    subject.clip.as_str(),
                    subject.lunge.is_some(),
                )
            }),
            &mut meshes,
            &mut materials,
            palette.subject,
        )?;
        let body = cast.actors[0].mesh;
        let maximum = hall.len() + 7 + 3 * MAX_ENEMIES + MAX_DROPS + MAX_SHOTS;
        let mut prepared = Vec::with_capacity(maximum);
        prepared.extend_from_slice(&hall);
        for actor in &cast.actors {
            for pose in actor.poses {
                let mut obj = RenderObject::new(actor.mesh, actor.material);
                obj.pose = Some(pose);
                prepared.push(obj);
            }
        }
        let mut player = RenderObject::new(body, palette.player);
        player.pose = Some(cast.player_pose);
        prepared.push(player);
        for (mesh, material) in [
            (ring, palette.pulse),
            (ring, palette.drop),
            (disk, palette.shadow),
            (block, palette.safety),
            (block, palette.warning),
            (block, palette.shot),
            (block, palette.drop),
        ] {
            prepared.push(RenderObject::new(mesh, material));
        }
        // Reserving draws at startup covers spawn/kill/pickup changes. The repeated
        // entries bound transform and draw storage; uploads deduplicate shared meshes.
        prepared.resize(
            maximum.max(prepared.len()),
            RenderObject::new(block, palette.drop),
        );
        for (i, obj) in prepared[hall.len()..].iter_mut().enumerate() {
            obj.casts_shadow = characters.shadows && i <= MAX_ENEMIES;
        }
        let [w, d] = game.arena.half_extent.to_array();
        // A broad ceiling fixture shades the permanent hall. Moving subjects
        // retain their floor contacts without invalidating the static lamp atlas.
        let mut lights = vec![LocalLight::shadowed_rect(
            sashiko_renderer::pbr::RectLight::from_lumens(
                [-6., 6., -5.],
                [2.1, 0., 0.],
                [0., 0., 0.65],
                [0.94, 0.98, 1.],
                1800.,
                36.,
            )?,
        )];
        let view = game.arena.prepare_view()?;
        lights.extend(view.point_lights.iter().copied().map(LocalLight::Point));
        let participants = std::thread::available_parallelism()
            .unwrap_or(std::num::NonZeroUsize::MIN)
            .min(4.try_into().unwrap());
        Ok(Self {
            jobs: JobSystem::new(participants)?.into(),
            meshes,
            materials,
            palette,
            texture_owners: None,
            prepared,
            objects: DrawList::new(hall, 6 + 3 * MAX_ENEMIES + MAX_DROPS + MAX_SHOTS)?,
            hud: Vec::with_capacity(256),
            cast,
            character_shadows: characters.shadows,
            block,
            ring,
            disk,
            lights,
            tracking: view.camera,
            camera_anchor: view.anchor,
            sun: Sun::from_lux([-0.44, 0.84, 0.32], [0.87, 0.94, 1.], 2.8)?,
            bounds: Bounds::new([-w - 4., -1., -d - 4.], [w + 4., 6., d + 4.])?,
            visible: 0,
            drawn: 0,
            last_step: None,
            player_facing: Vec2::new(0., 1.),
            previous_player: game.player,
        })
    }

    pub fn assets(&mut self) -> SceneAssets<'_> {
        SceneAssets::new(&mut self.meshes, &mut self.materials, &self.prepared)
            .with_lights(&self.lights)
    }

    fn install(&mut self, maps: Surfaces) -> PipelineResult<()> {
        self.materials
            .update_pbr(self.palette.floor, maps.floor.clone())?;
        self.materials
            .update_pbr(self.palette.steel, maps.steel.clone())?;
        let mut player = maps.characters[0].clone();
        player.factors.base_color = [0.84, 0.96, 1., 1.];
        player.factors.emissive_nits = [0.025, 0.05, 0.06];
        self.materials.update_pbr(self.palette.player, player)?;
        for actor in &self.cast.actors {
            let mut subject = maps.characters[actor.texture].clone();
            subject.factors.base_color = [0.84, 0.43, 0.32, 1.];
            self.materials.update_pbr(actor.material, subject)?;
        }
        self.texture_owners = Some(maps);
        Ok(())
    }
    pub fn prepare_window(&mut self, r: &mut WindowRenderer) -> PipelineResult<()> {
        self.install(Surfaces::for_window(r, &self.cast.models)?)?;
        r.prepare_scene(self.assets())?;
        r.prepare_jobs(self.jobs.clone())
    }
    pub fn prepare_headless(&mut self, r: &mut Renderer) -> PipelineResult<()> {
        self.install(Surfaces::for_headless(r, &self.cast.models)?)?;
        r.prepare_scene(self.assets())?;
        r.prepare_jobs(self.jobs.clone())
    }

    pub fn camera(&self, game: &Game, extent: [u32; 2]) -> PipelineResult<CameraView> {
        self.tracking.follow_view(
            sashiko_renderer::pipeline::ViewId(1),
            self.camera_anchor,
            game.player,
            game.arena.half_extent,
            extent,
        )
    }

    fn instance(
        &mut self,
        mesh: MeshHandle,
        material: MaterialHandle,
        pos: Vec3,
        scale: Vec3,
        facing: Vec2,
    ) -> PipelineResult<()> {
        let mut object =
            RenderObject::new(mesh, material).with_transform(placement(pos, scale, facing)?);
        object.casts_shadow = false;
        self.objects.push(object)?;
        Ok(())
    }

    pub fn sync(&mut self, game: &Game, view: &CameraView) -> PipelineResult<()> {
        self.objects.begin();
        // Sparse animation phases share the same posed vertex stream across the crowd.
        // Fixed tick controls animation time even when a presentation gets repeated.
        if self.last_step != Some(game.tick) {
            self.cast.animate(game, &self.jobs, &mut self.meshes)?;
            let displacement = game.player - self.previous_player;
            let dist = displacement.length();
            if dist > 0.001 {
                self.player_facing = displacement / dist;
            }
            self.previous_player = game.player;
            self.last_step = Some(game.tick);
        }
        let aligned = Vec2::new(0., 1.);
        if let (Some(p), Some(b)) = (game.cargo_position(), game.arena.cargo) {
            self.instance(
                self.block,
                self.palette.safety,
                p,
                Vec3::new(2. * b.half_extent.x(), 1., 2. * b.half_extent.y()),
                aligned,
            )?;
        }
        let [px, pz] = game.player.to_array();
        let [mx, mz] = game.arena.marker.to_array();
        self.instance(
            self.ring,
            self.palette.drop,
            Vec3::new(mx, 0.035, mz),
            Vec3::new(0.70, 1., 0.70),
            aligned,
        )?;
        self.instance(
            self.ring,
            self.palette.pulse,
            Vec3::new(px, 0.045, pz),
            Vec3::new(0.63, 1., 0.63),
            aligned,
        )?;
        let mut player =
            RenderObject::new(self.cast.actors[0].mesh, self.palette.player).with_transform(
                placement(Vec3::new(px, 0., pz), Vec3::ONE, self.player_facing)?,
            );
        player.pose = Some(self.cast.player_pose);
        player.casts_shadow = self.character_shadows;
        self.objects.push(player)?;
        self.visible = 0;
        self.drawn = 0;
        for enemy in &game.enemies {
            let visible = on_screen(view, enemy.pos, 1.9, 0.65);
            self.visible += usize::from(visible);
            if visible || self.character_shadows {
                let [x, z] = enemy.pos.to_array();
                let actor = self.cast.subject_actor(enemy);
                let mut object = RenderObject::new(actor.mesh, actor.material)
                    .with_transform(placement(Vec3::new(x, 0., z), Vec3::ONE, enemy.facing)?);
                object.pose = Some(actor.poses[(enemy.id % WALK_PHASES as u64) as usize]);
                object.casts_shadow = self.character_shadows;
                self.objects.push(object)?;
                self.drawn += 1;
            }
            if visible {
                let [x, z] = enemy.pos.to_array();
                self.instance(
                    self.disk,
                    self.palette.shadow,
                    Vec3::new(x, 0.014, z),
                    Vec3::new(0.32, 1., 0.28),
                    aligned,
                )?;
            }
            // A lunge warning may enter the frame before its source becomes visible.
            if enemy.windup {
                let [dx, dz] = enemy.facing.to_array();
                self.instance(
                    self.block,
                    self.palette.warning,
                    Vec3::new(enemy.pos[0] + dx * 1.15, 0.04, enemy.pos[1] + dz * 1.15),
                    Vec3::new(0.14, 0.025, 2.25),
                    enemy.facing,
                )?;
            }
        }
        for drop in &game.drops {
            if on_screen(view, drop.pos, 1.9, 0.65) {
                self.instance(
                    self.block,
                    self.palette.drop,
                    Vec3::new(drop.pos[0], 0.22, drop.pos[1]),
                    Vec3::new(0.17, 0.31, 0.17),
                    Vec2::ONE * std::f32::consts::FRAC_1_SQRT_2,
                )?;
            }
        }
        for shot in &game.shots {
            self.instance(
                self.block,
                self.palette.shot,
                Vec3::new(shot.pos[0], 0.80, shot.pos[1]),
                Vec3::new(0.07, 0.07, 0.48),
                shot.dir,
            )?;
        }
        if game.pulse_radius > 0.01 {
            self.instance(
                self.ring,
                self.palette.pulse,
                Vec3::new(px, 0.07, pz),
                Vec3::new(game.pulse_radius, 1., game.pulse_radius),
                aligned,
            )?;
        }
        Ok(())
    }

    pub fn draw(
        &mut self,
        frame: &mut sashiko_renderer::Frame<'_>,
        game: &Game,
    ) -> PipelineResult<()> {
        let extent = frame.scene_view().extent;
        frame.draw_scene(self.scene(), 1.0)?;
        frame.draw_overlay(self.overlay(game, extent))?;
        Ok(())
    }

    pub fn scene(&self) -> SceneInput<'_> {
        SceneInput::new(&self.meshes, &self.materials, self.objects.objects())
            .with_sun(self.sun)
            .with_lights(&self.lights)
            .with_bounds(self.bounds)
    }
    pub fn overlay(&mut self, game: &Game, extent: [u32; 2]) -> &[Layer] {
        hud::paint(&mut self.hud, game, extent);
        &self.hud
    }
    #[cfg(feature = "memory-profiling")]
    pub fn take_pose_bytes(&self) -> u64 {
        self.meshes.take_recorded_pose_bytes()
    }
    pub fn visible_enemies(&self) -> usize {
        self.visible
    }
    pub fn drawn_enemies(&self) -> usize {
        self.drawn
    }
    pub fn release(mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        self.meshes.release_gpu(runtime)?;
        self.materials.clear(runtime)?;
        Ok(())
    }
}
