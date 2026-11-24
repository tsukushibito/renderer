//! Research hall, animated subjects and gameplay feedback shared by capture and presentation.
mod camera;
mod geometry;
mod hud;
mod lab;
mod surfaces;

use crate::character::Character;
pub(super) use crate::character::CharacterVariant;
use renderer::{
    Renderer, WindowRenderer,
    canvas::Layer,
    core::runtime::Runtime,
    material::{AlphaMode, MaterialHandle, Materials, PbrParams},
    mesh::{MeshCapacity, MeshHandle, Meshes, SkinPoseId},
    pbr::{Bounds, LocalLight, PointLight, Sun, Transform},
    pipeline::{CameraView, PipelineResult, RenderObject, SceneAssets, SceneInput},
};
use surfaces::Surfaces;
use survivor::{Game, MAX_DROPS, MAX_ENEMIES, MAX_SHOTS};

const WALK_PHASES: usize = 4;

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

pub struct Visual {
    meshes: Meshes,
    materials: Materials,
    palette: Palette,
    texture_owners: Option<Surfaces>,
    hall: Vec<RenderObject>,
    prepared: Vec<RenderObject>,
    objects: Vec<RenderObject>,
    hud: Vec<Layer>,
    character: Character,
    base_rgba: &'static [u8],
    body: MeshHandle,
    poses: [SkinPoseId; WALK_PHASES],
    block: MeshHandle,
    ring: MeshHandle,
    disk: MeshHandle,
    lights: Vec<LocalLight>,
    sun: Sun,
    bounds: Bounds,
    visible: usize,
    drawn: usize,
    last_step: Option<u64>,
    player_facing: [f32; 2],
    previous_player: [f32; 2],
}

fn transform(
    [x, y, z]: [f32; 3],
    [w, h, d]: [f32; 3],
    [sin, cos]: [f32; 2],
) -> PipelineResult<Transform> {
    // A stationary actor coincident with its target has no movement bearing.
    let (sin, cos) = if sin == 0. && cos == 0. {
        (0., 1.)
    } else {
        (sin, cos)
    };
    Ok(Transform::new([
        [cos * w, 0., sin * d, x],
        [0., h, 0., y],
        [-sin * w, 0., cos * d, z],
    ])?)
}

impl Visual {
    pub fn new(game: &Game, variant: CharacterVariant) -> PipelineResult<Self> {
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 20,
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
        let character = Character::load(variant)?;
        let body = meshes.create(character.mesh_data()?)?;
        let poses = [
            meshes.create_pose(body)?,
            meshes.create_pose(body)?,
            meshes.create_pose(body)?,
            meshes.create_pose(body)?,
        ];
        let maximum = hall.len() + 6 + 3 * MAX_ENEMIES + MAX_DROPS + MAX_SHOTS;
        let mut prepared = Vec::with_capacity(maximum);
        prepared.extend_from_slice(&hall);
        for pose in poses {
            let mut obj = RenderObject::new(body, palette.subject);
            obj.pose = Some(pose);
            prepared.push(obj);
        }
        for (mesh, material) in [
            (body, palette.player),
            (ring, palette.pulse),
            (ring, palette.drop),
            (disk, palette.shadow),
            (block, palette.warning),
            (block, palette.shot),
            (block, palette.drop),
        ] {
            prepared.push(RenderObject::new(mesh, material));
        }
        // Reserving draws at startup covers spawn/kill/pickup changes. The repeated
        // entries bound transform and draw storage; uploads deduplicate shared meshes.
        prepared.resize(maximum, RenderObject::new(block, palette.drop));
        let [w, d] = game.arena.half_extent;
        let mut lights = Vec::new();
        for (x, z, tint, power) in [
            (-0.55, -0.52, [0.69, 0.84, 1.], 72.),
            (0.52, -0.45, [1., 0.72, 0.48], 67.),
            (0.0, 0.18, [0.67, 0.90, 1.], 80.),
            (-0.53, 0.65, [0.52, 0.79, 1.], 52.),
        ] {
            lights.push(LocalLight::Point(PointLight::from_candela(
                [x * w, 4.5, z * d],
                14.,
                tint,
                power,
            )?));
        }
        Ok(Self {
            meshes,
            materials,
            palette,
            texture_owners: None,
            hall,
            prepared,
            objects: Vec::with_capacity(maximum),
            hud: Vec::with_capacity(256),
            character,
            base_rgba: variant.base_rgba(),
            body,
            poses,
            block,
            ring,
            disk,
            lights,
            sun: Sun::from_lux([-0.44, 0.84, 0.32], [0.87, 0.94, 1.], 3.6)?,
            bounds: Bounds::new([-w - 4., -1., -d - 4.], [w + 4., 6., d + 4.])?,
            visible: 0,
            drawn: 0,
            last_step: None,
            player_facing: [0., 1.],
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
        let mut player = maps.character.clone();
        player.factors.base_color = [0.84, 0.96, 1., 1.];
        player.factors.emissive_nits = [0.025, 0.05, 0.06];
        let mut subject = maps.character.clone();
        subject.factors.base_color = [0.84, 0.43, 0.32, 1.];
        self.materials.update_pbr(self.palette.player, player)?;
        self.materials.update_pbr(self.palette.subject, subject)?;
        self.texture_owners = Some(maps);
        Ok(())
    }
    pub fn prepare_window(&mut self, r: &mut WindowRenderer) -> PipelineResult<()> {
        self.install(Surfaces::for_window(r, self.base_rgba)?)?;
        r.prepare_scene(self.assets())?;
        Ok(())
    }
    pub fn prepare_headless(&mut self, r: &mut Renderer) -> PipelineResult<()> {
        self.install(Surfaces::for_headless(r, self.base_rgba)?)?;
        r.prepare_scene(self.assets())?;
        Ok(())
    }

    pub fn camera(&self, game: &Game, extent: [u32; 2]) -> PipelineResult<CameraView> {
        camera::tracking(game.player, extent)
    }

    fn instance(
        &mut self,
        mesh: MeshHandle,
        material: MaterialHandle,
        pos: [f32; 3],
        scale: [f32; 3],
        facing: [f32; 2],
    ) -> PipelineResult<()> {
        self.objects
            .push(RenderObject::new(mesh, material).with_transform(transform(pos, scale, facing)?));
        Ok(())
    }

    pub fn sync(&mut self, game: &Game, view: &CameraView) -> PipelineResult<()> {
        self.objects.clear();
        self.objects.extend_from_slice(&self.hall);
        // Sparse animation phases share the same posed vertex stream across the crowd.
        // Fixed tick controls animation time even when a presentation gets repeated.
        if self.last_step != Some(game.tick) {
            for (phase, pose) in self.poses.iter().enumerate() {
                self.meshes.set_joint_transforms(
                    *pose,
                    self.character.palette(game.tick + phase as u64 * 13),
                )?;
            }
            let dx = game.player[0] - self.previous_player[0];
            let dz = game.player[1] - self.previous_player[1];
            let dist = dx.hypot(dz);
            if dist > 0.001 {
                self.player_facing = [dx / dist, dz / dist]
            }
            self.previous_player = game.player;
            self.last_step = Some(game.tick);
        }
        let aligned = [0., 1.];
        let [px, pz] = game.player;
        let [mx, mz] = game.arena.marker;
        self.instance(
            self.ring,
            self.palette.drop,
            [mx, 0.035, mz],
            [0.70, 1., 0.70],
            aligned,
        )?;
        self.instance(
            self.ring,
            self.palette.pulse,
            [px, 0.045, pz],
            [0.63, 1., 0.63],
            aligned,
        )?;
        let mut player = RenderObject::new(self.body, self.palette.player)
            .with_transform(transform([px, 0., pz], [1.; 3], self.player_facing)?);
        player.pose = Some(self.poses[0]);
        self.objects.push(player);
        self.visible = 0;
        self.drawn = 0;
        for enemy in &game.enemies {
            if camera::on_screen(view, enemy.pos) {
                self.visible += 1;
                let [x, z] = enemy.pos;
                let mut object = RenderObject::new(self.body, self.palette.subject)
                    .with_transform(transform([x, 0., z], [1.; 3], enemy.facing)?);
                object.pose = Some(self.poses[(enemy.id % WALK_PHASES as u64) as usize]);
                self.objects.push(object);
                self.drawn += 1;
                self.instance(
                    self.disk,
                    self.palette.shadow,
                    [x, 0.014, z],
                    [0.32, 1., 0.28],
                    aligned,
                )?;
            }
            // A lunge warning may enter the frame before its source becomes visible.
            if enemy.windup {
                let [dx, dz] = enemy.facing;
                self.instance(
                    self.block,
                    self.palette.warning,
                    [enemy.pos[0] + dx * 1.15, 0.04, enemy.pos[1] + dz * 1.15],
                    [0.14, 0.025, 2.25],
                    enemy.facing,
                )?;
            }
        }
        for drop in &game.drops {
            if camera::on_screen(view, drop.pos) {
                self.instance(
                    self.block,
                    self.palette.drop,
                    [drop.pos[0], 0.22, drop.pos[1]],
                    [0.17, 0.31, 0.17],
                    [std::f32::consts::FRAC_1_SQRT_2; 2],
                )?;
            }
        }
        for shot in &game.shots {
            self.instance(
                self.block,
                self.palette.shot,
                [shot.pos[0], 0.80, shot.pos[1]],
                [0.07, 0.07, 0.48],
                shot.dir,
            )?;
        }
        if game.pulse_radius > 0.01 {
            self.instance(
                self.ring,
                self.palette.pulse,
                [px, 0.07, pz],
                [game.pulse_radius, 1., game.pulse_radius],
                aligned,
            )?;
        }
        Ok(())
    }

    pub fn scene(&self) -> SceneInput<'_> {
        SceneInput::new(&self.meshes, &self.materials, &self.objects)
            .with_sun(self.sun)
            .with_lights(&self.lights)
            .with_bounds(self.bounds)
    }
    pub fn overlay(&mut self, game: &Game, extent: [u32; 2]) -> &[Layer] {
        hud::paint(&mut self.hud, game, extent);
        &self.hud
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
