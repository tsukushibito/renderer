//! The training yard owns the actors, equipment choices and animation inputs.
use renderer::{
    material::{MaterialHandle, Materials, PbrParams},
    mesh::{JointWeights, MeshCapacity, MeshData, Meshes, SkinPoseId},
    model::{Model, ModelInstance, ModelLod, ModelPart},
    pbr::{Bounds, PbrVertex, Projection, Sun, Transform, ViewInput},
    pipeline::{
        CameraView, MeshIndexRange, PipelineResult, RenderObject, SceneAssets, SceneInput, ViewId,
    },
    unlit_forward::IDENTITY,
};

pub struct Yard {
    pub(crate) meshes: Meshes,
    pub(crate) materials: Materials,
    actors: [ModelInstance; 2],
    poses: [SkinPoseId; 2],
    assets: Vec<RenderObject>,
    body: MaterialHandle,
    glow: MaterialHandle,
    bounds: Bounds,
    sun: Sun,
}

/// Actions from either keyboard input or the deterministic replay.
#[derive(Clone, Copy)]
pub enum Action {
    Armor(bool),
    BodyColor([f32; 3]),
    Pose([f32; 2]),
}

pub fn replay_action(step: usize) -> Option<Action> {
    match step {
        1 => Some(Action::Armor(true)),
        2 => Some(Action::BodyColor([0., 1., 1.])),
        3 => Some(Action::Armor(false)),
        4 => Some(Action::Pose([-0.4, 0.])),
        5 => Some(Action::Pose([0., 0.2])),
        6 => Some(Action::Pose([0.; 2])),
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
            meshes: 1,
            retired_mesh_versions: 1,
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
        let mut assets = actors
            .iter()
            .flat_map(ModelInstance::asset_draws)
            .collect::<Vec<_>>();
        // Load the optional emissive material along with the equipment.
        assets.push(RenderObject::new(mesh, glow));
        let world_bounds = Bounds::new([-1., -1., 0.], [1., 1., 1.])?;
        let sun = Sun::new([0., 0., -1.], [0.01; 3])?;

        Ok(Self {
            meshes,
            materials,
            actors,
            poses,
            assets,
            body,
            glow,
            bounds: world_bounds,
            sun,
        })
    }

    pub fn assets(&mut self) -> SceneAssets<'_> {
        SceneAssets::new(&mut self.meshes, &mut self.materials, &self.assets)
    }

    pub fn extract<'a>(
        &'a self,
        camera: &CameraView,
        out: &'a mut Vec<RenderObject>,
    ) -> SceneInput<'a> {
        out.clear();
        out.extend(self.actors.iter().flat_map(|actor| actor.draws(camera)));
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
        out.extend(self.actors[actor].draws(camera));
        SceneInput::new(&self.meshes, &self.materials, out)
    }

    /// Returns true when a shared GPU asset needs to be prepared at a change boundary.
    pub fn apply(&mut self, action: Action) -> PipelineResult<bool> {
        match action {
            Action::Armor(enabled) => {
                self.actors[0].set_material(1, enabled.then_some(self.glow))?
            }
            Action::BodyColor(color) => {
                self.materials.update_pbr(self.body, emission(color))?;
                return Ok(true);
            }
            Action::Pose(heights) => {
                for (&pose, dy) in self.poses.iter().zip(heights) {
                    let joint =
                        Transform::new([[1., 0., 0., 0.], [0., 1., 0., dy], [0., 0., 1., 0.]])?;
                    self.meshes
                        .set_joint_transforms(pose, &[Transform::IDENTITY, joint])?;
                }
            }
        }
        Ok(false)
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
