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
pub use equipment::EquipmentAsset;
use equipment::{EquipmentLoads, LoadedArmor};

pub const REPLAY_FRAMES: usize = 24;

pub struct Yard {
    pub(crate) meshes: Meshes,
    pub(crate) materials: Materials,
    actors: [Option<ModelInstance>; 2],
    model: Arc<Model>,
    mesh: MeshHandle,
    occupant: u64,
    loads: EquipmentLoads,
    equipped: Option<RenderObject>,
    pub(crate) load_error: Option<renderer::core::graph::GraphError>,
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
    params.factors.emissive = rgb;
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
            meshes: 3,
            retired_mesh_versions: 2,
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
        let sun = Sun::new([0., 0., -1.], [0.01; 3])?;

        Ok(Self {
            meshes,
            materials,
            actors: actors.map(Some),
            model,
            mesh,
            occupant: 1,
            loads: EquipmentLoads::default(),
            equipped: None,
            load_error: None,
            poses,
            body,
            glow,
            bounds: world_bounds,
            sun,
        })
    }

    pub fn assets(&mut self) -> SceneAssets<'_> {
        self.assets_with(self.equipped)
    }

    fn assets_with(&mut self, equipment: Option<RenderObject>) -> SceneAssets<'_> {
        let mut assets = SceneAssets::new(&mut self.meshes, &mut self.materials, &[]);
        for actor in self.actors.iter().flatten() {
            assets.add_model(actor);
        }
        if let Some(object) = equipment {
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
                    .equipped
                    .filter(|_| index == 0 && part.material_slot == 1);
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

    fn release_armor(&mut self, armor: RenderObject) -> PipelineResult<()> {
        self.meshes.remove(armor.mesh)?;
        self.materials.remove(armor.material)?;
        Ok(())
    }

    fn equip(
        &mut self,
        loaded: LoadedArmor,
        prepare: impl FnOnce(SceneAssets<'_>) -> PipelineResult<()>,
    ) -> PipelineResult<()> {
        let material = self.materials.create_pbr(emission(loaded.color))?;
        let mesh = match self.meshes.create(loaded.mesh) {
            Ok(mesh) => mesh,
            Err(error) => {
                self.materials.remove(material)?;
                return Err(error.into());
            }
        };
        let armor = RenderObject::new(mesh, material);
        if let Err(error) = prepare(self.assets_with(Some(armor))) {
            self.release_armor(armor)?;
            return Err(error);
        }
        if let Some(old) = self.equipped.replace(armor) {
            self.release_armor(old)?;
        }
        Ok(())
    }

    pub fn apply(
        &mut self,
        action: Action,
        prepare: impl FnOnce(SceneAssets<'_>) -> PipelineResult<()>,
    ) -> PipelineResult<()> {
        match action {
            Action::Armor(enabled) => {
                if let Some(actor) = &mut self.actors[0] {
                    actor.set_material(1, enabled.then_some(self.glow))?;
                }
            }
            Action::BodyColor(color) => {
                self.materials.update_pbr(self.body, emission(color))?;
                prepare(self.assets())?;
            }
            Action::Pose(heights) => {
                for (&pose, dy) in self.poses.iter().zip(heights) {
                    let joint =
                        Transform::new([[1., 0., 0., 0.], [0., 1., 0., dy], [0., 0., 1., 0.]])?;
                    self.meshes
                        .set_joint_transforms(pose, &[Transform::IDENTITY, joint])?;
                }
            }
            Action::Request(asset) => {
                if self.actors[0].is_some() {
                    self.loads.request(self.occupant, asset)?;
                }
            }
            Action::Deliver(asset) => {
                let occupant = self.actors[0].as_ref().map(|_| self.occupant);
                match self.loads.deliver(occupant, asset)? {
                    Some(Ok(payload)) => self.equip(payload, prepare)?,
                    Some(Err(error)) => self.load_error = Some(error),
                    None => {}
                }
            }
            Action::Despawn => {
                self.actors[0] = None;
                if let Some(armor) = self.equipped.take() {
                    self.release_armor(armor)?;
                }
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
        yard.meshes.release_gpu(runtime)?;
        yard.materials.clear(runtime)?;
        Ok(())
    }
}
