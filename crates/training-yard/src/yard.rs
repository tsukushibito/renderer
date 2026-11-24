//! The training yard owns the actors, equipment choices and animation inputs.
use renderer::{
    material::{MaterialHandle, Materials, PbrParams},
    mesh::{JointWeights, MeshCapacity, MeshData, MeshHandle, Meshes, SkinPoseId},
    model::{Model, ModelInstance, ModelLod, ModelPart},
    pbr::{Bounds, PbrVertex, Projection, Sun, Transform, ViewInput},
    pipeline::{
        CameraView, MeshIndexRange, PipelineResult, RenderObject, SceneAssets, SceneInput, ViewId,
    },
    unlit_forward::IDENTITY,
};
use std::sync::Arc;
mod equipment;
use equipment::Equipment;
pub use equipment::EquipmentAsset;

pub const REPLAY_FRAMES: usize = 24;

pub struct Yard {
    pub(crate) meshes: Meshes,
    pub(crate) materials: Materials,
    actors: [Option<ModelInstance>; 2],
    model: Arc<Model>,
    mesh: MeshHandle,
    occupant: u64,
    equipment: Equipment,
    poses: [SkinPoseId; 2],
    body: MaterialHandle,
    glow: MaterialHandle,
    bounds: Bounds,
    sun: Sun,
}

/// Actions from either keyboard input or the deterministic replay.
#[derive(Clone, Copy, Debug)]
pub enum Action {
    Armor(bool),
    BodyColor([f32; 3]),
    Pose([f32; 2]),
    Request(EquipmentAsset),
    Deliver(EquipmentAsset),
    RequestFor(usize, EquipmentAsset),
    Despawn,
    Respawn,
}

pub fn replay_action(step: usize) -> Option<Action> {
    match step {
        1 => Some(Action::Armor(true)),
        2 => Some(Action::BodyColor([0., 1., 1.])),
        3 => Some(Action::Armor(false)),
        4 => Some(Action::Pose([-0.4, 0.])),
        5 => Some(Action::Pose([0., 0.2])),
        6 => Some(Action::Pose([0.; 2])),
        14 => Some(Action::Request(EquipmentAsset::Narrow)),
        15 => Some(Action::Request(EquipmentAsset::Wide)),
        16 => Some(Action::Deliver(EquipmentAsset::Wide)),
        17 => Some(Action::Deliver(EquipmentAsset::Narrow)),
        18 => Some(Action::Request(EquipmentAsset::Broken)),
        19 => Some(Action::Deliver(EquipmentAsset::Broken)),
        20 => Some(Action::Request(EquipmentAsset::Spare)),
        21 => Some(Action::Despawn),
        22 => Some(Action::Respawn),
        23 => Some(Action::Deliver(EquipmentAsset::Spare)),
        _ => None,
    }
}

fn emission(rgb: [f32; 3]) -> PbrParams {
    let mut params = PbrParams::color([0., 0., 0., 1.]);
    params.factors.double_sided = true;
    params.factors.emissive_nits = rgb;
    params
}

pub fn camera(id: u64, extent: [u32; 2]) -> PipelineResult<CameraView> {
    let projection = ViewInput::new(
        IDENTITY,
        [0., 0., -1.],
        [0., 0., 1.],
        [1., 2.],
        Projection::Orthographic,
    )?;

    Ok(CameraView {
        id: ViewId(id),
        extent,
        camera: projection,
    })
}
impl Yard {
    pub fn new() -> PipelineResult<Self> {
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 5,
            retired_mesh_versions: 4,
        })?;
        let vertices: Vec<_> = [-0.75, 0., 0.75]
            .into_iter()
            .flat_map(|y| {
                [-0.35, 0.35].map(|x| {
                    PbrVertex::new([x, y, 0.5], [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap()
                })
            })
            .collect();
        let weights = vertices
            .iter()
            .map(|v| {
                let w = (v.position()[1] + 0.75) / 1.5;
                JointWeights {
                    joints: [0, 1, 0, 0],
                    weights: [1. - w, w, 0., 0.],
                }
            })
            .collect::<Vec<_>>();
        let mesh = meshes.create(MeshData::skinned_pbr(
            &vertices,
            &[0, 1, 2, 1, 3, 2, 2, 3, 4, 3, 5, 4],
            2,
            &weights,
        )?)?;
        let mut materials = Materials::default();
        let body = materials.create_pbr(emission([1., 0., 0.]))?;
        let armor = materials.create_pbr(emission([0., 0., 1.]))?;
        let glow = materials.create_pbr(emission([0., 1., 0.]))?;
        let body_part = ModelPart {
            mesh,
            indices: Some(MeshIndexRange::new(0..6)?),
            material_slot: 0,
        };
        let armor_part = ModelPart {
            mesh,
            indices: Some(MeshIndexRange::new(6..12)?),
            material_slot: 1,
        };
        let model = Model::new(
            Bounds::new([-0.35, -0.75, 0.5], [0.35, 0.75, 0.5])?,
            vec![body, armor],
            vec![
                ModelLod {
                    min_pixels: 10.,
                    parts: vec![body_part, armor_part],
                },
                ModelLod {
                    min_pixels: 0.,
                    parts: vec![body_part],
                },
            ],
        )?;
        let mut actors = [-0.5, 0.5].map(|x| {
            let mut actor = model.instantiate();
            actor.transform =
                Transform::new([[1., 0., 0., x], [0., 1., 0., 0.], [0., 0., 1., 0.]]).unwrap();
            actor
        });
        let poses = [meshes.create_pose(mesh)?, meshes.create_pose(mesh)?];
        for (actor, pose) in actors.iter_mut().zip(poses) {
            actor.set_pose(mesh, Some(pose))?;
        }
        let world_bounds = Bounds::new([-1., -1., 0.], [1., 1., 1.])?;
        let sun = Sun::from_lux([0., 0., -1.], [1.; 3], 0.01)?;

        Ok(Self {
            meshes,
            materials,
            actors: actors.map(Some),
            model,
            mesh,
            occupant: 1,
            equipment: Equipment::default(),
            poses,
            body,
            glow,
            bounds: world_bounds,
            sun,
        })
    }

    pub fn assets(&mut self) -> SceneAssets<'_> {
        let equipment = [self.equipment.object(0), self.equipment.object(1)];
        let mut assets = SceneAssets::new(&mut self.meshes, &mut self.materials, &[]);
        for actor in self.actors.iter().flatten() {
            assets.add_model(actor);
        }
        for object in equipment.into_iter().flatten() {
            assets.add_object(object);
        }
        assets.preload_pair(self.mesh, self.glow);
        assets
    }

    pub fn extract<'a>(
        &'a self,
        camera: &CameraView,
        out: &'a mut Vec<RenderObject>,
    ) -> SceneInput<'a> {
        out.clear();
        for actor in 0..self.actors.len() {
            self.extract_parts(actor, camera, out);
        }
        SceneInput::new(&self.meshes, &self.materials, out)
            .with_sun(self.sun)
            .with_bounds(self.bounds)
    }

    pub(crate) fn inspect_actor<'a>(
        &'a self,
        actor: usize,
        camera: &CameraView,
        out: &'a mut Vec<RenderObject>,
    ) -> SceneInput<'a> {
        out.clear();
        self.extract_parts(actor, camera, out);
        SceneInput::new(&self.meshes, &self.materials, out).with_bounds(self.bounds)
    }

    fn extract_parts(&self, index: usize, camera: &CameraView, out: &mut Vec<RenderObject>) {
        if let Some(actor) = &self.actors[index] {
            for (part, object) in actor.draw_parts(camera) {
                let replacement = self
                    .equipment
                    .object(index)
                    .filter(|_| part.material_slot == 1);
                out.push(replacement.map_or(object, |armor| armor.with_transform(actor.transform)));
            }
        }
    }

    /// The GPU sees frame-local IDs. Keep this small lookup with an outstanding
    /// mouse request so reusing an actor slot never redirects the answer.
    pub(crate) fn pick_targets(&self, camera: &CameraView) -> [Option<(usize, u64, usize)>; 4] {
        let mut targets = [None; 4];
        let mut entries = targets.iter_mut();
        for (i, actor) in self.actors.iter().enumerate() {
            if let Some(actor) = actor {
                for (part, _) in actor.draw_parts(camera) {
                    *entries.next().expect("two parts per training actor") = Some((
                        i,
                        if i == 0 { self.occupant } else { 1 },
                        part.material_slot,
                    ));
                }
            }
        }
        targets
    }

    pub fn reserve_equipment(
        preparation: &mut renderer::core::capacity::Preparation,
    ) -> Result<(), renderer::core::graph::GraphError> {
        equipment::reserve(preparation)
    }

    pub fn pending_equipment(&self) -> bool {
        self.equipment.pending_delivery()
    }

    pub fn load_error(&self) -> Option<&renderer::core::graph::GraphError> {
        self.equipment.error.as_ref()
    }

    /// Poll the latest requests, keeping each actor's current draw until publication succeeds.
    pub fn advance_equipment(
        &mut self,
        renderer: &mut impl renderer::pipeline::AssetPreparation,
    ) -> PipelineResult<()> {
        self.equipment.advance(
            renderer,
            &mut self.meshes,
            &mut self.materials,
            |r, name, desc, bytes| r.asset_renderer().upload_texture(name, desc, bytes),
        )
    }

    pub fn apply<R: renderer::pipeline::AssetPreparation>(
        &mut self,
        action: Action,
        renderer: &mut R,
        prepare: impl FnOnce(&mut R, SceneAssets<'_>) -> PipelineResult<()>,
    ) -> PipelineResult<()> {
        match action {
            Action::Armor(enabled) => {
                if let Some(actor) = &mut self.actors[0] {
                    actor.set_material(1, enabled.then_some(self.glow))?;
                }
            }
            Action::BodyColor(color) => {
                self.materials.update_pbr(self.body, emission(color))?;
                prepare(renderer, self.assets())?;
            }
            Action::Pose(heights) => {
                for (&pose, dy) in self.poses.iter().zip(heights) {
                    let joint =
                        Transform::new([[1., 0., 0., 0.], [0., 1., 0., dy], [0., 0., 1., 0.]])?;
                    self.meshes
                        .set_joint_transforms(pose, &[Transform::IDENTITY, joint])?;
                }
            }
            Action::Request(asset) | Action::RequestFor(0, asset) => {
                if self.actors[0].is_some() {
                    self.equipment.request(0, asset)?;
                }
            }
            Action::RequestFor(slot, asset) => {
                if self.actors.get(slot).ok_or("unknown actor")?.is_some() {
                    self.equipment.request(slot, asset)?;
                }
            }
            Action::Deliver(asset) => self.equipment.deliver(asset),
            Action::Despawn => {
                self.equipment
                    .detach(0, renderer, &mut self.meshes, &mut self.materials)?;
                self.actors[0] = None;
            }
            Action::Respawn if self.actors[0].is_none() => {
                self.occupant = self
                    .occupant
                    .checked_add(1)
                    .ok_or("actor generation exhausted")?;
                let mut actor = self.model.instantiate();
                actor.set_pose(self.mesh, Some(self.poses[0]))?;
                actor.transform =
                    Transform::new([[1., 0., 0., -0.5], [0., 1., 0., 0.], [0., 0., 1., 0.]])?;
                self.actors[0] = Some(actor);
            }
            Action::Respawn => {}
        }
        Ok(())
    }

    pub(crate) fn release(
        self,
        runtime: &mut renderer::core::runtime::Runtime,
    ) -> PipelineResult<()> {
        let mut yard = self;
        yard.equipment.finish_upload(runtime)?;
        yard.meshes.release_gpu(runtime)?;
        yard.materials.clear(runtime)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PipelineResult;
    use crate::validation;
    use renderer::core::runtime::RuntimeConfig;
    fn exercise_shared_equipment(config: RuntimeConfig) -> PipelineResult<()> {
        use super::{Action, EquipmentAsset, Yard};
        use renderer::Renderer;
        let mut yard = Yard::new()?;
        let camera = super::camera(1, [48, 24])?;
        let mut renderer = Renderer::headless_with(
            config,
            camera,
            yard.assets(),
            std::num::NonZeroUsize::new(2).unwrap(),
            Yard::reserve_equipment,
        )?;
        let mut objects = Vec::new();
        let armor = |yard: &Yard, objects: &mut Vec<_>| {
            let scene = yard.extract(&camera, objects);
            [scene.objects[1].mesh, scene.objects[3].mesh]
        };
        let baseline = armor(&yard, &mut objects);
        let apply = |yard: &mut Yard, renderer: &mut Renderer, action| {
            yard.apply(action, renderer, |r, assets| Ok(r.prepare_scene(assets)?))
        };
        let mut uploads = 0;
        for who in [0, 1] {
            apply(
                &mut yard,
                &mut renderer,
                Action::RequestFor(who, EquipmentAsset::Wide),
            )?;
        }
        apply(
            &mut yard,
            &mut renderer,
            Action::Deliver(EquipmentAsset::Wide),
        )?;
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(15);
        let mut injected = false;
        while yard.pending_equipment() {
            if start.elapsed() > std::time::Duration::from_secs(60) {
                return Err("equipment did not become ready".into());
            }
            let result = yard.equipment.advance(
                &mut renderer,
                &mut yard.meshes,
                &mut yard.materials,
                |r, name, desc, bytes| {
                    if !injected {
                        injected = true;
                        return Err("injected texture failure".into());
                    }
                    uploads += 1;
                    r.upload_texture(name, desc, bytes)
                },
            );
            if let Err(error) = result {
                assert_eq!(error.to_string(), "injected texture failure");
            }
            if yard.pending_equipment() {
                assert_eq!(armor(&yard, &mut objects), baseline);
                let mut frame = renderer.begin_frame()?;
                frame.draw_scene(yard.extract(&camera, &mut objects), 1.)?;
                let capture = frame.capture()?;
                let mut pixels = [0; 48 * 24 * 4];
                assert!(renderer.read_capture(&capture, &mut pixels, timeout)?);
            }
        }
        assert!(injected);
        assert_eq!(uploads, 1);
        let shared = armor(&yard, &mut objects);
        assert_ne!(baseline[0], shared[0]);
        assert_eq!(shared[0], shared[1]);

        // A different mesh and material reuse the same paint while actor 1 keeps the first kit.
        apply(
            &mut yard,
            &mut renderer,
            Action::RequestFor(0, EquipmentAsset::Narrow),
        )?;
        apply(
            &mut yard,
            &mut renderer,
            Action::Deliver(EquipmentAsset::Narrow),
        )?;
        let start = std::time::Instant::now();
        while yard.pending_equipment() {
            assert!(start.elapsed() < std::time::Duration::from_secs(60));
            yard.equipment.advance(
                &mut renderer,
                &mut yard.meshes,
                &mut yard.materials,
                |_, _, _, _| Err("paint was uploaded twice".into()),
            )?;
            std::thread::yield_now();
        }
        let split = armor(&yard, &mut objects);
        assert_ne!(split[0], shared[0]);
        assert_eq!(split[1], shared[1]);
        apply(&mut yard, &mut renderer, Action::Despawn)?;
        let scene = yard.extract(&camera, &mut objects);
        assert_eq!(scene.objects.len(), 2);
        assert_eq!(scene.objects[1].mesh, shared[1]);
        let mut frame = renderer.begin_frame()?;
        frame.draw_scene(scene, 1.)?;
        let capture = frame.capture()?;
        let mut pixels = [0; 48 * 24 * 4];
        assert!(renderer.read_capture(&capture, &mut pixels, timeout)?);
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] > 100 && p[1] > 100 && p[2] < 100)
        );
        let mut runtime = renderer.into_runtime()?;
        yard.release(&mut runtime)?;
        validation::check_validation(runtime)
    }

    #[test]
    #[ignore = "requires real devices for each enabled backend"]
    fn two_actors_share_one_upload_and_one_can_leave() -> PipelineResult<()> {
        #[cfg(feature = "vulkan")]
        exercise_shared_equipment(RuntimeConfig::Vulkan(Default::default()))?;
        #[cfg(all(windows, feature = "d3d12"))]
        exercise_shared_equipment(RuntimeConfig::D3D12(Default::default()))?;
        Ok(())
    }
}
