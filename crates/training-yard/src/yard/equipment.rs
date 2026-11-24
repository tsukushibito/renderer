//! Incremental publication of one armor primitive per actor's equipment slot.
mod cooked;
use cooked::LoadedArmor;
use renderer::{
    core::{
        capacity::{GraphRequirements, Preparation},
        graph::{BufferDesc, GraphError, ImageDesc},
        runtime::{FrameId, ManagedImage},
    },
    material::{MaterialTextures, Materials},
    mesh::Meshes,
    pbr::MaterialUniform,
    pipeline::{
        AssetCandidate, AssetPreparation, PipelineResult, RenderObject, RetireAssets, UploadBudget,
    },
};
use renderer_asset::{AssetId, Digest, Generation, ObjectBytes, ObjectStore, Primitive};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Weak},
    task::Poll,
    thread::JoinHandle,
};

// Two visible pieces plus two replacements. Requests from additional owners can
// share any of these entries; a fifth distinct revision reports backpressure.
const ITEMS: usize = 4;
const IMAGE_NAME: &str = "yard armor texture";
pub(crate) fn reserve(prep: &mut Preparation) -> Result<(), GraphError> {
    for size in [
        4 * renderer::passes::pbr::VERTEX_BYTES as u64,
        6 * 4,
        MaterialUniform::DESC.size,
    ] {
        prep.reserve_buffer(BufferDesc { size }, ITEMS)?;
    }
    prep.reserve_image(ImageDesc::rgba8_srgb(1, 1), ITEMS)?;
    prep.add_graph_requirements(GraphRequirements {
        upload_bytes: 256,
        resources: 4,
        accesses: 4,
        passes: 4,
        name_bytes: 128,
        ..Default::default()
    })
}

/// Named persistent IDs used in the deterministic training-yard replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EquipmentAsset {
    Narrow,
    Wide,
    Spare,
    Broken,
}
impl EquipmentAsset {
    pub const fn id(self) -> AssetId {
        AssetId::from_u128(match self {
            Self::Narrow => 1,
            Self::Wide => 2,
            Self::Spare => 3,
            Self::Broken => 4,
        })
    }
}

#[cfg(feature = "memory-profiling")]
#[derive(Default, Clone, Copy, Debug)]
pub struct EquipmentStats {
    pub image_uploads: u64,
    pub image_bytes: u64,
    pub mesh_registrations: u64,
    pub material_registrations: u64,
}

struct Armor {
    object: RenderObject,
    recipe: Primitive,
    _sources: [Arc<ObjectBytes>; 2],
    _maps: [Option<(Arc<ObjectBytes>, Arc<ManagedImage>)>; 4],
}

#[derive(Default)]
enum State {
    #[default]
    Empty,
    Reading(JoinHandle<Result<LoadedArmor, String>>),
    Decoded(Box<LoadedArmor>, [Option<Arc<ManagedImage>>; 4]),
    Registered(Armor),
    Loading(Armor, AssetCandidate),
    Resident(Armor),
}
impl State {
    fn armor(&self) -> Option<&Armor> {
        match self {
            Self::Resident(a) | Self::Registered(a) | Self::Loading(a, _) => Some(a),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Choice {
    id: AssetId,
    item: usize,
}
#[derive(Default, Clone, Copy)]
struct Slot {
    active: Option<Choice>,
    wanted: Option<Choice>,
    admitted: bool,
}

pub(crate) struct Equipment {
    slots: Vec<Slot>,
    items: [State; ITEMS],
    keys: [Option<(Digest, AssetId)>; ITEMS],
    generation: Arc<Generation>,
    pub store: ObjectStore,
    images: HashMap<Digest, Weak<ManagedImage>>,
    pending_image: Option<(FrameId, Arc<ManagedImage>)>,
    cursor: usize,
    collect: bool,
    pub error: Option<GraphError>,
    #[cfg(feature = "memory-profiling")]
    pub stats: EquipmentStats,
}

impl Equipment {
    pub fn new(root: &Path, owners: usize) -> PipelineResult<Self> {
        if owners < 2 {
            return Err("training yard requires two equipment slots".into());
        }
        Ok(Self {
            slots: vec![Slot::default(); owners],
            items: Default::default(),
            keys: [None; ITEMS],
            generation: Arc::new(Generation::open(root)?),
            store: ObjectStore::new(root),
            images: HashMap::new(),
            pending_image: None,
            cursor: 0,
            collect: false,
            error: None,
            #[cfg(feature = "memory-profiling")]
            stats: Default::default(),
        })
    }
    pub fn set_generation(&mut self, generation: Generation) {
        self.generation = Arc::new(generation);
        for slot in &mut self.slots {
            slot.wanted = None;
            slot.admitted = false;
        }
    }
    pub fn saved(&self, owner: usize) -> Option<AssetId> {
        self.slots.get(owner)?.active.map(|c| c.id)
    }
    #[cfg(feature = "memory-profiling")]
    pub fn lookups(&self) -> u64 {
        self.generation.lookups()
    }
    pub fn request(&mut self, owner: usize, id: AssetId, admitted: bool) -> PipelineResult<()> {
        let slot = self.slots.get_mut(owner).ok_or("unknown equipment slot")?;
        let key = (*self.generation.id(), id);
        let index = match self.keys.iter().position(|k| *k == Some(key)) {
            Some(index) => index,
            None => {
                let index = self
                    .items
                    .iter()
                    .position(|s| matches!(s, State::Empty))
                    .ok_or("all prepared equipment entries are in use")?;
                let generation = self.generation.clone();
                let store = self.store.clone();
                self.items[index] = State::Reading(
                    std::thread::Builder::new()
                        .spawn(move || cooked::read(&generation, &store, id))?,
                );
                self.keys[index] = Some(key);
                index
            }
        };
        slot.wanted = Some(Choice { id, item: index });
        slot.admitted = admitted;
        Ok(())
    }
    pub fn deliver(&mut self, asset: EquipmentAsset) {
        for slot in &mut self.slots {
            if slot.wanted.is_some_and(|choice| choice.id == asset.id()) {
                slot.admitted = true
            }
        }
    }
    pub fn object(&self, owner: usize) -> Option<RenderObject> {
        let choice = self.slots[owner].active?;
        self.items[choice.item].armor().map(|a| a.object)
    }
    pub fn pending_delivery(&self) -> bool {
        self.slots.iter().any(|s| s.admitted && s.wanted.is_some())
    }
    fn wanted(&self, index: usize) -> bool {
        self.slots
            .iter()
            .any(|s| s.wanted.is_some_and(|c| c.item == index))
    }
    fn admitted(&self, index: usize) -> bool {
        self.slots
            .iter()
            .any(|s| s.admitted && s.wanted.is_some_and(|c| c.item == index))
    }

    pub fn detach(
        &mut self,
        owner: usize,
        renderer: &mut impl AssetPreparation,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> PipelineResult<()> {
        let mut next = self.slots.clone();
        *next.get_mut(owner).ok_or("unknown equipment slot")? = Slot::default();
        self.publish(None, next, renderer, meshes, materials)
    }
    fn publish(
        &mut self,
        adopt: Option<usize>,
        next: Vec<Slot>,
        renderer: &mut impl AssetPreparation,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> PipelineResult<()> {
        let retiring = std::array::from_fn(|i| {
            matches!(self.items[i], State::Resident(_))
                && !next.iter().any(|s| {
                    [s.active, s.wanted]
                        .into_iter()
                        .flatten()
                        .any(|c| c.item == i)
                })
        });
        let (pairs, mesh_handles, material_handles) = self.retire_handles(&retiring);
        let retire = RetireAssets {
            pairs: &pairs,
            meshes: &mesh_handles,
            materials: &material_handles,
        };
        if let Some(State::Loading(_, candidate)) = adopt.map(|i| &mut self.items[i]) {
            renderer.commit_candidate(candidate, meshes, materials, retire)?;
        } else {
            renderer.commit_candidate(
                &mut AssetCandidate::new(meshes, materials, &[])?,
                meshes,
                materials,
                retire,
            )?;
        }
        for (i, retired) in retiring.into_iter().enumerate() {
            if retired {
                self.empty(i);
                self.collect = true;
            }
        }
        if let Some(i) = adopt
            && matches!(self.items[i], State::Loading(..))
        {
            let State::Loading(armor, _) = std::mem::take(&mut self.items[i]) else {
                unreachable!()
            };
            self.items[i] = State::Resident(armor);
        }
        self.slots = next;
        Ok(())
    }
    fn retire_handles(
        &self,
        retiring: &[bool; ITEMS],
    ) -> (
        Vec<(
            renderer::mesh::MeshHandle,
            renderer::material::MaterialHandle,
        )>,
        Vec<renderer::mesh::MeshHandle>,
        Vec<renderer::material::MaterialHandle>,
    ) {
        let mut pairs = Vec::new();
        let mut meshes = Vec::new();
        let mut materials = Vec::new();
        for (i, state) in self.items.iter().enumerate().filter(|(i, _)| retiring[*i]) {
            let Some(armor) = state.armor() else { continue };
            let live = || {
                self.items.iter().enumerate().filter_map(|(j, s)| {
                    if j != i && !retiring[j] {
                        s.armor().map(|a| a.object)
                    } else {
                        None
                    }
                })
            };
            let m = armor.object.mesh;
            let a = armor.object.material;
            if !live().any(|o| o.mesh == m && o.material == a) && !pairs.contains(&(m, a)) {
                pairs.push((m, a))
            }
            if !live().any(|o| o.mesh == m) && !meshes.contains(&m) {
                meshes.push(m)
            }
            if !live().any(|o| o.material == a) && !materials.contains(&a) {
                materials.push(a)
            }
        }
        (pairs, meshes, materials)
    }
    fn empty(&mut self, i: usize) {
        self.items[i] = State::Empty;
        self.keys[i] = None;
        self.images.retain(|_, w| w.strong_count() > 0);
    }
    fn fail(&mut self, i: usize, error: String) {
        self.empty(i);
        for slot in &mut self.slots {
            if slot.wanted.is_some_and(|c| c.item == i) {
                slot.wanted = None;
                slot.admitted = false;
            }
        }
        self.error = Some(GraphError::custom(error))
    }
    pub fn advance<R: AssetPreparation>(
        &mut self,
        renderer: &mut R,
        meshes: &mut Meshes,
        materials: &mut Materials,
        upload: impl FnOnce(&mut R, &str, ImageDesc, &[u8]) -> PipelineResult<(FrameId, ManagedImage)>,
    ) -> PipelineResult<()> {
        if self.collect {
            self.collect = renderer.collect_assets(meshes, materials)? != 0
        }
        if let Some((id, _)) = &self.pending_image {
            if !renderer.asset_renderer().poll_texture_upload(*id)? {
                return Ok(());
            }
            self.pending_image = None;
        }
        for delta in 0..ITEMS {
            let i = (self.cursor + delta) % ITEMS;
            if matches!(self.items[i], State::Empty) {
                continue;
            }
            let wanted = self.wanted(i);
            let admitted = self.admitted(i);
            match &mut self.items[i] {
                State::Reading(worker) if worker.is_finished() && (admitted || !wanted) => {
                    let State::Reading(worker) = std::mem::take(&mut self.items[i]) else {
                        unreachable!()
                    };
                    if !wanted {
                        self.empty(i)
                    } else {
                        match worker
                            .join()
                            .unwrap_or_else(|_| Err("equipment reader panicked".into()))
                        {
                            Ok(loaded) => {
                                self.items[i] = State::Decoded(Box::new(loaded), Default::default())
                            }
                            Err(error) => self.fail(i, error),
                        }
                    }
                }
                State::Decoded(..) if !wanted => self.empty(i),
                State::Decoded(loaded, maps) if admitted => {
                    for (slot, map) in maps.iter_mut().zip(&loaded.maps) {
                        if slot.is_some() {
                            continue;
                        }
                        let Some(map) = map else { continue };
                        if let Some(image) = self.images.get(&map.hash).and_then(Weak::upgrade) {
                            *slot = Some(image);
                            continue;
                        }
                        let bytes = &map.object.image()?.pixels;
                        let (id, image) = upload(renderer, IMAGE_NAME, map.desc, bytes)?;
                        let image = Arc::new(image);
                        self.images.insert(map.hash, Arc::downgrade(&image));
                        *slot = Some(image.clone());
                        self.pending_image = Some((id, image));
                        #[cfg(feature = "memory-profiling")]
                        {
                            self.stats.image_uploads += 1;
                            self.stats.image_bytes += bytes.len() as u64;
                        }
                        return Ok(());
                    }
                    self.register(i, meshes, materials)?;
                }
                State::Registered(armor) => {
                    let candidate = AssetCandidate::new(meshes, materials, &[armor.object])?;
                    let State::Registered(armor) = std::mem::take(&mut self.items[i]) else {
                        unreachable!()
                    };
                    self.items[i] = State::Loading(armor, candidate);
                }
                State::Loading(..) if !wanted => {
                    let (pairs, m, a) = self.retire_handles(&std::array::from_fn(|j| j == i));
                    let State::Loading(_, candidate) = &mut self.items[i] else {
                        unreachable!()
                    };
                    renderer.cancel_candidate(
                        candidate,
                        meshes,
                        materials,
                        RetireAssets {
                            pairs: &pairs,
                            meshes: &m,
                            materials: &a,
                        },
                    )?;
                    self.empty(i);
                    self.collect = true;
                }
                State::Loading(_, candidate) if admitted => {
                    if let Poll::Ready(()) = renderer.poll_candidate(
                        candidate,
                        meshes,
                        materials,
                        UploadBudget {
                            bytes: 128,
                            submissions: 1,
                        },
                    )? {
                        self.adopt(i, renderer, meshes, materials)?
                    }
                }
                State::Resident(_) if admitted => self.adopt(i, renderer, meshes, materials)?,
                State::Resident(_)
                    if !wanted
                        && !self
                            .slots
                            .iter()
                            .any(|s| s.active.is_some_and(|c| c.item == i)) =>
                {
                    self.publish(None, self.slots.clone(), renderer, meshes, materials)?
                }
                _ => continue,
            }
            self.cursor = (i + 1) % ITEMS;
            return Ok(());
        }
        Ok(())
    }
    fn register(
        &mut self,
        i: usize,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> PipelineResult<()> {
        let State::Decoded(data, maps) = std::mem::take(&mut self.items[i]) else {
            unreachable!()
        };
        let LoadedArmor {
            mesh,
            mut params,
            primitive,
            maps: src,
            sources,
        } = *data;
        let others = || self.items.iter().filter_map(State::armor);
        let reused_mesh = others()
            .find(|a| a.recipe.mesh == primitive.mesh)
            .map(|a| a.object.mesh);
        let reused_material = others()
            .find(|a| {
                a.recipe.material == primitive.material && a.recipe.images == primitive.images
            })
            .map(|a| a.object.material);
        let [base_color, normal, orm, emissive] = maps
            .each_ref()
            .map(|m| m.as_ref().map(|m| m.snapshot().clone().into()));
        params.textures = MaterialTextures {
            base_color,
            normal,
            orm,
            emissive,
        };
        let material = match reused_material {
            Some(handle) => handle,
            None => match materials.create_pbr(params) {
                Ok(handle) => handle,
                Err(e) => {
                    self.fail(i, e.to_string());
                    return Ok(());
                }
            },
        };
        let mesh = match reused_mesh {
            Some(handle) => handle,
            None => match meshes.create(mesh) {
                Ok(handle) => handle,
                Err(e) => {
                    if reused_material.is_none() {
                        materials.remove(material)?;
                    }
                    self.fail(i, e.to_string());
                    return Ok(());
                }
            },
        };
        #[cfg(feature = "memory-profiling")]
        {
            self.stats.mesh_registrations += u64::from(reused_mesh.is_none());
            self.stats.material_registrations += u64::from(reused_material.is_none());
        }
        let mut src = src.into_iter();
        let retained = maps.map(|map| {
            src.next()
                .unwrap()
                .zip(map)
                .map(|(m, owner)| (m.object, owner))
        });
        self.items[i] = State::Registered(Armor {
            object: RenderObject::new(mesh, material),
            recipe: primitive,
            _sources: sources,
            _maps: retained,
        });
        Ok(())
    }
    fn adopt(
        &mut self,
        i: usize,
        renderer: &mut impl AssetPreparation,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> PipelineResult<()> {
        let mut next = self.slots.clone();
        for s in &mut next {
            if s.admitted && s.wanted.is_some_and(|c| c.item == i) {
                s.active = s.wanted.take();
                s.admitted = false;
            }
        }
        self.publish(Some(i), next, renderer, meshes, materials)
    }
    pub fn finish_upload(
        &mut self,
        runtime: &mut renderer::core::runtime::Runtime,
    ) -> PipelineResult<()> {
        if let Some((id, _)) = &self.pending_image {
            runtime
                .wait_for_frame(*id, std::time::Duration::from_secs(15))?
                .ok_or("equipment upload shutdown timed out")?;
            self.pending_image = None;
        }
        Ok(())
    }
}
