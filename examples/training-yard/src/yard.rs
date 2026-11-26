//! The training yard owns the actors, equipment choices and animation inputs.
use sashiko_asset::{AssetId, Generation, ObjectStore};
use sashiko_renderer::{
    material::{MaterialHandle, Materials, PbrParams},
    mesh::{JointWeights, MeshCapacity, MeshData, MeshHandle, Meshes, SkinPoseId},
    model::{Model, ModelInstance, ModelLod, ModelPart},
    pbr::{Bounds, PbrVertex, Projection, Sun, Transform, ViewInput},
    pipeline::{
        AssetPreparation, CameraView, MeshIndexRange, PipelineResult, RenderObject, SceneAssets,
        SceneInput, ViewId,
    },
    unlit_forward::IDENTITY,
};
use std::path::Path;
use std::sync::Arc;
mod equipment;
#[cfg(all(windows, feature = "presentation"))]
pub(crate) mod preview;
mod project;
use equipment::Equipment;
pub use equipment::EquipmentAsset;
#[cfg(feature = "memory-profiling")]
pub use equipment::EquipmentStats;
use project::{AssetGeneration, Project};

pub(crate) const EQUIPMENT_DRAWS: usize = equipment::MAX_DRAWS;
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
        if let Some(root) = project::packaged()? {
            return Self::with_asset_catalog(root, 2);
        }
        let (root, generation) = Project::discover()?.open()?;
        Self::with_generation(&root, generation, 2)
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub(crate) fn for_interactive_preview() -> PipelineResult<(Self, Option<preview::Preview>)> {
        if let Some(root) = project::packaged()? {
            return Ok((Self::with_asset_catalog(root, 2)?, None));
        }
        let (preview, (root, generation)) = preview::Preview::start(Project::discover()?)?;
        let mut yard = Self::with_generation(&root, generation, 2)?;
        yard.request_equipment(0, EquipmentAsset::Narrow.id())?;
        Ok((yard, Some(preview)))
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub(crate) fn poll_preview(&mut self, preview: &preview::Preview) -> PipelineResult<()> {
        if let Some(generation) = preview.update() {
            self.equipment.refresh(generation);
        }
        self.equipment.retry_refresh()
    }

    #[cfg(all(windows, feature = "presentation", feature = "memory-profiling"))]
    pub(crate) fn visible_preview_generation(&self) -> Option<sashiko_asset::Digest> {
        self.equipment.visible_generation(0)
    }

    /// Open a local distribution. Extra slots can retain equipment for off-screen actors.
    pub fn with_asset_catalog(
        root: impl AsRef<Path>,
        equipment_slots: usize,
    ) -> PipelineResult<Self> {
        let root = root.as_ref();
        Self::with_generation(
            root,
            AssetGeneration::distribution(Generation::open(root)?),
            equipment_slots,
        )
    }

    fn with_generation(
        root: &Path,
        generation: AssetGeneration,
        equipment_slots: usize,
    ) -> PipelineResult<Self> {
        let equipment = Equipment::new(root, generation, equipment_slots)?;
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 1 + 4 * equipment::MAX_PARTS,
            retired_mesh_versions: 4 * equipment::MAX_PARTS,
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
            equipment,
            poses,
            body,
            glow,
            bounds: world_bounds,
            sun,
        })
    }

    pub fn assets(&mut self) -> SceneAssets<'_> {
        let equipment = (0..2).flat_map(|i| self.equipment.objects(i));
        let mut assets = SceneAssets::new(&mut self.meshes, &mut self.materials, &[]);
        for actor in self.actors.iter().flatten() {
            assets.add_model(actor);
        }
        for object in equipment {
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
                let armor = self.equipment.objects(index);
                if part.material_slot == 1 && armor.len() > 0 {
                    out.extend(armor.map(|o| o.with_transform(actor.transform)));
                } else {
                    out.push(object);
                }
            }
        }
    }

    /// The GPU sees frame-local IDs. Keep this small lookup with an outstanding
    /// mouse request so reusing an actor slot never redirects the answer.
    pub(crate) fn pick_targets(
        &self,
        camera: &CameraView,
    ) -> [Option<(usize, u64, usize)>; equipment::MAX_DRAWS] {
        let mut targets = [None; equipment::MAX_DRAWS];
        let mut entries = targets.iter_mut();
        for (i, actor) in self.actors.iter().enumerate() {
            if let Some(actor) = actor {
                for (part, _) in actor.draw_parts(camera) {
                    let count = if part.material_slot == 1 {
                        self.equipment.objects(i).len().max(1)
                    } else {
                        1
                    };
                    for _ in 0..count {
                        *entries.next().expect("prepared draw capacity") = Some((
                            i,
                            if i == 0 { self.occupant } else { 1 },
                            part.material_slot,
                        ));
                    }
                }
            }
        }
        targets
    }

    pub fn reserve_equipment(
        preparation: &mut sashiko_renderer::core::capacity::Preparation,
    ) -> Result<(), sashiko_renderer::core::graph::GraphError> {
        equipment::reserve(preparation)
    }

    pub fn pending_equipment(&self) -> bool {
        self.equipment.pending_delivery()
    }

    /// Request the saved persistent ID for one owner. Its old armor remains visible until adoption.
    pub fn request_equipment(&mut self, slot: usize, id: AssetId) -> PipelineResult<()> {
        self.equipment.request(slot, id, true)
    }
    pub fn saved_equipment(&self, slot: usize) -> Option<AssetId> {
        self.equipment.saved(slot)
    }
    /// Pin subsequent requests to a verified generation in the same object store.
    pub fn set_asset_generation(&mut self, generation: Generation) {
        self.equipment
            .set_generation(AssetGeneration::distribution(generation));
    }
    pub fn equipment_store(&self) -> &ObjectStore {
        &self.equipment.store
    }
    #[cfg(feature = "memory-profiling")]
    pub fn equipment_stats(&self) -> EquipmentStats {
        self.equipment.stats
    }
    #[cfg(feature = "memory-profiling")]
    pub fn equipment_lookups(&self) -> u64 {
        self.equipment.lookups()
    }

    pub fn load_error(&self) -> Option<&sashiko_renderer::core::graph::GraphError> {
        self.equipment.error.as_ref()
    }

    /// Poll the latest requests, keeping each actor's current draw until publication succeeds.
    pub fn advance_equipment(
        &mut self,
        renderer: &mut impl AssetPreparation,
    ) -> PipelineResult<()> {
        self.equipment.advance(
            renderer,
            &mut self.meshes,
            &mut self.materials,
            AssetPreparation::upload_texture,
        )
    }

    pub fn apply<R: AssetPreparation>(
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
                    self.equipment.request(0, asset.id(), false)?;
                }
            }
            Action::RequestFor(slot, asset) => {
                if self.actors.get(slot).ok_or("unknown actor")?.is_some() {
                    self.equipment.request(slot, asset.id(), false)?;
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

    pub fn release(
        self,
        runtime: &mut sashiko_renderer::core::runtime::Runtime,
    ) -> PipelineResult<()> {
        let mut yard = self;
        yard.equipment.finish_upload(runtime)?;
        yard.meshes.release_gpu(runtime)?;
        yard.materials.clear(runtime)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/support/distribution.rs"]
mod distribution;

#[cfg(test)]
mod tests {
    use super::PipelineResult;
    use crate::validation;
    use sashiko_renderer::core::runtime::RuntimeConfig;
    use std::error::Error;
    fn exercise_shared_equipment(config: RuntimeConfig) -> PipelineResult<()> {
        use super::{Action, EquipmentAsset, Yard};
        use sashiko_renderer::{Renderer, pipeline::AssetPreparation};
        let fixture = super::distribution::Distribution::cook()?;
        let mut yard = Yard::with_asset_catalog(&fixture.root, 2)?;
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
                        return Err(Box::<dyn Error>::from("injected texture failure").into());
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
                |_, _, _, _| Err(Box::<dyn Error>::from("paint was uploaded twice").into()),
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

    fn two_parts(config: RuntimeConfig) -> PipelineResult<()> {
        use super::{EquipmentAsset, Yard};
        use sashiko_asset::{Catalog, Digest, Generation};
        let fixture = super::distribution::Distribution::cook()?;
        let root = fixture.root.as_path();
        let old = Generation::open(root)?;
        let mut yard = Yard::with_asset_catalog(root, 2)?;
        let mut catalog: Catalog = serde_json::from_slice(&std::fs::read(
            root.join("catalogs").join(old.id().to_string()),
        )?)?;
        let id = EquipmentAsset::Wide.id();
        let second = catalog.assets[&EquipmentAsset::Narrow.id()].primitives[0].clone();
        catalog.assets.get_mut(&id).unwrap().primitives.push(second);
        let triangle = sashiko_asset::AssetId::from_u128(6);
        let mut alternate = catalog.assets[&EquipmentAsset::Narrow.id()].clone();
        let mesh = &mut alternate.primitives[0].mesh;
        let mut data = sashiko_asset::MeshBlob::decode(&std::fs::read(
            root.join("objects").join(mesh.hash.to_string()),
        )?)?;
        data.vertices.truncate(3);
        data.indices = vec![0, 1, 2];
        let bytes = data.encode()?;
        mesh.hash = Digest::of(&bytes);
        mesh.size = bytes.len() as u64;
        std::fs::write(root.join("objects").join(mesh.hash.to_string()), bytes)?;
        catalog.assets.insert(triangle, alternate);
        let publish = |c: &Catalog| -> PipelineResult<Generation> {
            let bytes = serde_json::to_vec(c)?;
            let hash = Digest::of(&bytes);
            std::fs::write(root.join("catalogs").join(hash.to_string()), bytes)?;
            Ok(Generation::open_id(root, hash)?)
        };
        let camera = super::camera(1, [48, 24])?;
        let mut renderer = sashiko_renderer::Renderer::headless_with(
            config,
            camera,
            yard.assets(),
            std::num::NonZeroUsize::new(2).unwrap(),
            Yard::reserve_equipment,
        )?;
        yard.request_equipment(0, id)?;
        let poll = |yard: &mut Yard,
                    renderer: &mut sashiko_renderer::Renderer,
                    previous: Vec<sashiko_renderer::pipeline::RenderObject>|
         -> PipelineResult<()> {
            let start = std::time::Instant::now();
            while yard.pending_equipment() {
                if start.elapsed() > std::time::Duration::from_secs(30) {
                    return Err("equipment timeout".into());
                }
                yard.advance_equipment(renderer)?;
                if yard.pending_equipment() {
                    assert!(
                        yard.equipment
                            .objects(0)
                            .map(|o| o.mesh)
                            .eq(previous.iter().map(|o| o.mesh))
                    );
                }
                std::thread::yield_now();
            }
            Ok(())
        };
        poll(&mut yard, &mut renderer, vec![])?;
        let saved = serde_json::to_vec(&yard.saved_equipment(0).unwrap())?;
        let old_parts = yard.equipment.objects(0).collect::<Vec<_>>();
        assert_eq!(old_parts.len(), 1);
        yard.set_asset_generation(publish(&catalog)?);
        yard.request_equipment(0, serde_json::from_slice(&saved)?)?;
        poll(&mut yard, &mut renderer, old_parts.clone())?;
        let parts = yard.equipment.objects(0).collect::<Vec<_>>();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].mesh, old_parts[0].mesh);
        assert_ne!(parts[1].mesh, old_parts[0].mesh);
        let mut objects = Vec::new();
        let scene = yard.extract(&camera, &mut objects);
        assert_eq!(scene.objects.len(), 5);
        let mut frame = renderer.begin_frame()?;
        frame.draw_scene(scene, 1.)?;
        let capture = frame.capture()?;
        let mut pixels = [0; 48 * 24 * 4];
        assert!(renderer.read_capture(
            &capture,
            &mut pixels,
            std::time::Duration::from_secs(15)
        )?);
        // Force a failure after the comparison material and all of its images have
        // been registered. Retrying after slots are freed must make forward progress.
        let comparison = sashiko_asset::AssetId::from_u128(5);
        let positions = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]];
        let vertices = positions.map(|position| {
            super::PbrVertex::new(position, [0., 0., 1.], [1., 0., 0., 1.], [0., 0.]).unwrap()
        });
        let mut fillers = Vec::new();
        while let Ok(handle) = yard
            .meshes
            .create(super::MeshData::pbr(&vertices, &[0, 1, 2])?)
        {
            fillers.push(handle);
        }
        for _ in 0..3 {
            yard.request_equipment(0, comparison)?;
            poll(&mut yard, &mut renderer, parts.clone())?;
            assert!(yard.load_error().is_some());
            assert_eq!(yard.saved_equipment(0), Some(id));
        }
        for handle in fillers {
            yard.meshes.remove(handle)?;
        }
        // An unrelated baked-lighting upload must keep the last prepared streaming
        // working set usable when the same item is retried with no capacity change.
        use sashiko_renderer::pbr::{ReflectionProbeDesc, ReflectionProbes};
        let probes = ReflectionProbes::new(
            vec![ReflectionProbeDesc {
                position: [0.; 3],
                min: [-1.; 3],
                max: [1.; 3],
                blend_distance: 1.,
            }],
            1,
            sashiko_renderer::core::ImageFormat::Rgba32Float,
            vec![0; 16 * ReflectionProbes::WIDTH * ReflectionProbes::SCENARIO_ROWS],
        )?;
        let lighting = renderer.load_probe_lighting(sashiko_renderer::pipeline::ProbeLighting {
            diffuse: None,
            reflections: Some(&probes),
        })?;
        let mut previous = parts;
        for next in [comparison, triangle, id, comparison, triangle, id] {
            yard.request_equipment(0, next)?;
            poll(&mut yard, &mut renderer, previous)?;
            assert_eq!(
                yard.saved_equipment(0),
                Some(next),
                "{:?}",
                yard.load_error()
            );
            previous = yard.equipment.objects(0).collect();
            let mut frame = renderer.begin_frame()?;
            frame.draw_scene(yard.extract(&camera, &mut objects), 1.)?;
            let capture = frame.capture()?;
            assert!(renderer.read_capture(
                &capture,
                &mut pixels,
                std::time::Duration::from_secs(15)
            )?);
        }
        let parts = previous;
        catalog.assets.get_mut(&id).unwrap().primitives[1].mesh.hash =
            Digest::of(b"missing second primitive");
        yard.set_asset_generation(publish(&catalog)?);
        yard.request_equipment(0, id)?;
        poll(&mut yard, &mut renderer, parts.clone())?;
        assert!(yard.load_error().is_some());
        assert!(
            yard.equipment
                .objects(0)
                .map(|o| o.mesh)
                .eq(parts.iter().map(|o| o.mesh))
        );
        drop(lighting);
        let mut runtime = renderer.into_runtime()?;
        yard.release(&mut runtime)?;
        validation::check_validation(runtime)?;
        Ok(())
    }

    #[test]
    #[ignore = "requires real devices for each enabled backend"]
    fn multipart_generation_reuses_unchanged_parts_and_publishes_atomically() -> PipelineResult<()>
    {
        #[cfg(feature = "vulkan")]
        two_parts(RuntimeConfig::Vulkan(Default::default()))?;
        #[cfg(all(windows, feature = "d3d12"))]
        two_parts(RuntimeConfig::D3D12(Default::default()))?;
        Ok(())
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
