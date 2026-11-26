//! Incremental publication of an armor item per actor's equipment slot.
mod cooked;
mod resources;
use super::project::AssetGeneration;
use cooked::LoadedPart;
use resources::{Requirements, Reservations};
use sashiko_asset::{AssetId, Digest, MaterialSlot, ObjectBytes, ObjectStore, Primitive};
use sashiko_renderer::{
    AssetPrepareError,
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
use std::collections::VecDeque;
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
pub(super) const MAX_PARTS: usize = 4;
pub(super) const MAX_DRAWS: usize = 2 * (1 + MAX_PARTS);
const IMAGE_BUDGET_BYTES: usize = 4 * 1024 * 1024;
const IMAGE_NAME: &str = "yard armor texture";
fn can_retry(recovery: sashiko_renderer::CapacityRecovery) -> bool {
    matches!(
        recovery,
        sashiko_renderer::CapacityRecovery::Poll
            | sashiko_renderer::CapacityRecovery::ReleaseOwners
            | sashiko_renderer::CapacityRecovery::ReleaseResults
    )
}

pub(crate) fn reserve(prep: &mut Preparation) -> Result<(), GraphError> {
    for size in [
        resources::QUAD_VERTEX_BYTES,
        resources::QUAD_INDEX_BYTES,
        MaterialUniform::DESC.size,
    ] {
        prep.reserve_buffer(BufferDesc { size }, ITEMS * MAX_PARTS)?;
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

struct Part {
    object: RenderObject,
    recipe: Primitive,
    _sources: [Arc<ObjectBytes>; 2],
    _maps: [Option<(Arc<ObjectBytes>, Arc<ManagedImage>)>; 4],
}

type Armor = Vec<Part>;
struct Decoded {
    prepared: bool,
    remaining: VecDeque<LoadedPart>,
    parts: Armor,
    images: [Option<Arc<ManagedImage>>; 4],
}

#[derive(Default)]
enum State {
    #[default]
    Empty,
    Reading(JoinHandle<Result<Vec<LoadedPart>, String>>),
    Decoded(Decoded),
    Registered(Armor),
    Loading(Armor, AssetCandidate),
    Resident(Armor),
}
impl State {
    fn armor(&self) -> Option<&Armor> {
        match self {
            Self::Resident(a) | Self::Registered(a) | Self::Loading(a, _) => Some(a),
            Self::Decoded(d) => Some(&d.parts),
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
    #[cfg(all(windows, feature = "presentation"))]
    refresh: Option<(AssetId, bool)>,
}

pub(crate) struct Equipment {
    slots: Vec<Slot>,
    items: [State; ITEMS],
    keys: [Option<(Digest, AssetId)>; ITEMS],
    generation: Arc<AssetGeneration>,
    pub store: ObjectStore,
    images: HashMap<Digest, Weak<ManagedImage>>,
    requirements: [Option<Requirements>; ITEMS],
    reservations: Reservations,
    pending_image: Option<(FrameId, Arc<ManagedImage>)>,
    cursor: usize,
    collect: bool,
    pub error: Option<GraphError>,
    #[cfg(feature = "memory-profiling")]
    pub stats: EquipmentStats,
}

impl Equipment {
    pub fn new(root: &Path, generation: AssetGeneration, owners: usize) -> PipelineResult<Self> {
        if owners < 2 {
            return Err("training yard requires two equipment slots".into());
        }
        Ok(Self {
            slots: vec![Slot::default(); owners],
            items: Default::default(),
            keys: [None; ITEMS],
            generation: Arc::new(generation),
            store: ObjectStore::new(root),
            images: HashMap::new(),
            requirements: Default::default(),
            reservations: Default::default(),
            pending_image: None,
            cursor: 0,
            collect: false,
            error: None,
            #[cfg(feature = "memory-profiling")]
            stats: Default::default(),
        })
    }
    pub fn set_generation(&mut self, generation: AssetGeneration) {
        self.generation = Arc::new(generation);
        for slot in &mut self.slots {
            slot.wanted = None;
            slot.admitted = false;
            #[cfg(all(windows, feature = "presentation"))]
            {
                slot.refresh = None;
            }
        }
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub fn refresh(&mut self, generation: AssetGeneration) {
        self.generation = Arc::new(generation);
        for slot in &mut self.slots {
            slot.refresh = slot
                .wanted
                .map(|c| (c.id, slot.admitted))
                .or(slot.refresh)
                .or_else(|| slot.active.map(|c| (c.id, true)));
            slot.wanted = None;
            slot.admitted = false;
        }
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub fn retry_refresh(&mut self) -> PipelineResult<()> {
        for owner in 0..self.slots.len() {
            if let Some((id, admitted)) = self.slots[owner].refresh
                && self.try_request(owner, id, admitted)?
            {
                self.slots[owner].refresh = None;
            }
        }
        Ok(())
    }

    #[cfg(all(windows, feature = "presentation", feature = "memory-profiling"))]
    pub fn visible_generation(&self, owner: usize) -> Option<Digest> {
        let active = self.slots.get(owner)?.active?;
        self.keys[active.item].map(|(generation, _)| generation)
    }

    pub fn saved(&self, owner: usize) -> Option<AssetId> {
        self.slots.get(owner)?.active.map(|c| c.id)
    }
    #[cfg(feature = "memory-profiling")]
    pub fn lookups(&self) -> u64 {
        self.generation.catalog.lookups()
    }
    pub fn request(&mut self, owner: usize, id: AssetId, admitted: bool) -> PipelineResult<()> {
        if !self.try_request(owner, id, admitted)? {
            return Err("all prepared equipment entries are in use".into());
        }
        #[cfg(all(windows, feature = "presentation"))]
        {
            self.slots[owner].refresh = None;
        }
        Ok(())
    }

    fn try_request(&mut self, owner: usize, id: AssetId, admitted: bool) -> PipelineResult<bool> {
        let slot = self.slots.get_mut(owner).ok_or("unknown equipment slot")?;
        let key = (*self.generation.catalog.id(), id);
        let index = match self.keys.iter().position(|k| *k == Some(key)) {
            Some(index) => index,
            None => {
                let Some(index) = self.items.iter().position(|s| matches!(s, State::Empty)) else {
                    return Ok(false);
                };
                let generation = self.generation.clone();
                let store = self.store.clone();
                self.items[index] = State::Reading(
                    std::thread::Builder::new()
                        .spawn(move || cooked::read(&generation.catalog, &store, id))?,
                );
                self.keys[index] = Some(key);
                index
            }
        };
        slot.wanted = Some(Choice { id, item: index });
        slot.admitted = admitted;
        Ok(true)
    }
    pub fn deliver(&mut self, asset: EquipmentAsset) {
        for slot in &mut self.slots {
            #[cfg(all(windows, feature = "presentation"))]
            if let Some((id, admitted)) = &mut slot.refresh
                && *id == asset.id()
            {
                *admitted = true;
            }
            if slot.wanted.is_some_and(|choice| choice.id == asset.id()) {
                slot.admitted = true
            }
        }
    }
    pub fn objects(&self, owner: usize) -> impl ExactSizeIterator<Item = RenderObject> + '_ {
        self.slots[owner]
            .active
            .and_then(|c| self.items[c.item].armor())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|p| p.object)
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
        Ok(self.publish(None, next, renderer, meshes, materials)?)
    }
    fn publish(
        &mut self,
        adopt: Option<usize>,
        next: Vec<Slot>,
        renderer: &mut impl AssetPreparation,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> Result<(), AssetPrepareError> {
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
            sashiko_renderer::mesh::MeshHandle,
            sashiko_renderer::material::MaterialHandle,
        )>,
        Vec<sashiko_renderer::mesh::MeshHandle>,
        Vec<sashiko_renderer::material::MaterialHandle>,
    ) {
        let mut pairs = Vec::new();
        let mut meshes = Vec::new();
        let mut materials = Vec::new();
        let parts = || {
            self.items.iter().enumerate().flat_map(|(i, state)| {
                state
                    .armor()
                    .into_iter()
                    .flatten()
                    .map(move |p| (i, p.object))
            })
        };
        for (_, object) in parts().filter(|(i, _)| retiring[*i]) {
            let live = || parts().filter(|(j, _)| !retiring[*j]).map(|(_, o)| o);
            let mesh = object.mesh;
            let material = object.material;
            if !live().any(|o| o.mesh == mesh && o.material == material)
                && !pairs.contains(&(mesh, material))
            {
                pairs.push((mesh, material));
            }
            if !live().any(|o| o.mesh == mesh) && !meshes.contains(&mesh) {
                meshes.push(mesh);
            }
            if !live().any(|o| o.material == material) && !materials.contains(&material) {
                materials.push(material);
            }
        }
        (pairs, meshes, materials)
    }
    fn empty(&mut self, i: usize) {
        self.items[i] = State::Empty;
        self.keys[i] = None;
        self.requirements[i] = None;
        self.images.retain(|_, w| w.strong_count() > 0);
    }
    fn fail(&mut self, i: usize, error: String) {
        if let State::Decoded(data) = &mut self.items[i]
            && !data.parts.is_empty()
        {
            self.items[i] = State::Registered(std::mem::take(&mut data.parts));
        } else {
            self.empty(i);
        }
        self.revoke(i, error);
    }
    fn revoke(&mut self, i: usize, error: String) {
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
        upload: impl FnOnce(
            &mut R,
            &str,
            ImageDesc,
            &[u8],
        ) -> Result<(FrameId, ManagedImage), AssetPrepareError>,
    ) -> PipelineResult<()> {
        if self.collect {
            self.collect = renderer.collect_assets(meshes, materials)? != 0
        }
        if let Some((id, _)) = &self.pending_image {
            if !renderer.poll_texture_upload(*id)? {
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
            if matches!(&self.items[i], State::Decoded(d) if !d.prepared) && admitted {
                let reservations = Reservations::for_items(self.requirements.iter().flatten());
                if reservations != self.reservations {
                    match renderer.replace_asset_reservations(
                        IMAGE_NAME,
                        &reservations.buffers,
                        &reservations.images,
                        &reservations.samplers,
                    ) {
                        Ok(Poll::Ready(())) => self.reservations = reservations,
                        Ok(Poll::Pending) => return Ok(()),
                        Err(error) => return self.handle_upload_error(i, error),
                    }
                }
                let State::Decoded(decoded) = &mut self.items[i] else {
                    unreachable!()
                };
                decoded.prepared = true;
            }
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
                                match Requirements::for_parts(&loaded) {
                                    Ok(needed) => self.requirements[i] = Some(needed),
                                    Err(error) => {
                                        self.fail(i, error);
                                        return Ok(());
                                    }
                                }
                                self.items[i] = State::Decoded(Decoded {
                                    prepared: false,
                                    remaining: loaded.into(),
                                    parts: Vec::new(),
                                    images: Default::default(),
                                })
                            }
                            Err(error) => self.fail(i, error),
                        }
                    }
                }
                State::Decoded(data) if !wanted => {
                    if data.parts.is_empty() {
                        self.empty(i)
                    } else {
                        self.items[i] = State::Registered(std::mem::take(&mut data.parts));
                    }
                }
                State::Decoded(data) if admitted => {
                    let loaded = data.remaining.front().expect("one pending primitive");
                    let maps = &mut data.images;
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
                        let (id, image) = match upload(renderer, IMAGE_NAME, map.desc, bytes) {
                            Ok(uploaded) => uploaded,
                            Err(error) => return self.handle_upload_error(i, error),
                        };
                        self.images.retain(|_, image| image.strong_count() != 0);
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
                    let objects: Vec<_> = armor.iter().map(|p| p.object).collect();
                    let candidate = AssetCandidate::new(meshes, materials, &objects)?;
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
                    let progress = renderer.poll_candidate(
                        candidate,
                        meshes,
                        materials,
                        UploadBudget {
                            bytes: 128,
                            submissions: 1,
                        },
                    );
                    match progress {
                        Ok(Poll::Ready(())) => self.adopt(i, renderer, meshes, materials)?,
                        Ok(Poll::Pending) => {}
                        Err(error) if error.capacity().is_some_and(|c| can_retry(c.recovery())) => {
                            self.collect = true
                        }
                        Err(error) if error.capacity().is_some() => {
                            self.revoke(i, error.to_string());
                        }
                        Err(error) => return Err(error.into()),
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
    fn handle_upload_error(&mut self, i: usize, error: AssetPrepareError) -> PipelineResult<()> {
        use sashiko_renderer::core::runtime::ExecutionErrorKind;
        if error.capacity().is_some_and(|c| can_retry(c.recovery())) {
            self.collect = true;
        } else if error.capacity().is_some()
            || matches!(
                error.execution_kind(),
                Some(
                    ExecutionErrorKind::Unsupported
                        | ExecutionErrorKind::InvalidInput
                        | ExecutionErrorKind::CapacityExhausted
                )
            )
        {
            self.fail(i, error.to_string());
        } else {
            return Err(error.into());
        }
        Ok(())
    }
    fn register(
        &mut self,
        i: usize,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> PipelineResult<()> {
        let State::Decoded(data) = &mut self.items[i] else {
            unreachable!()
        };
        let maps = std::mem::take(&mut data.images);
        let LoadedPart {
            mesh,
            mut params,
            primitive,
            maps: src,
            sources,
        } = data.remaining.pop_front().expect("one pending primitive");
        let others = || self.items.iter().filter_map(State::armor).flatten();
        let reused_mesh = others()
            .find(|a| a.recipe.mesh == primitive.mesh)
            .map(|a| a.object.mesh);
        let reused_material = others()
            .find(|a| {
                a.recipe.material == primitive.material && a.recipe.images == primitive.images
            })
            .map(|a| a.object.material);
        let texture = |slot: MaterialSlot| {
            maps[slot.index()]
                .as_ref()
                .map(|m| m.snapshot().clone().into())
        };
        params.textures = MaterialTextures {
            base_color: texture(MaterialSlot::BaseColor),
            normal: texture(MaterialSlot::Normal),
            orm: texture(MaterialSlot::Orm),
            emissive: texture(MaterialSlot::Emissive),
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
        let State::Decoded(data) = &mut self.items[i] else {
            unreachable!()
        };
        data.parts.push(Part {
            object: RenderObject::new(mesh, material),
            recipe: primitive,
            _sources: sources,
            _maps: retained,
        });
        if data.remaining.is_empty() {
            self.items[i] = State::Registered(std::mem::take(&mut data.parts));
        }
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
        match self.publish(Some(i), next, renderer, meshes, materials) {
            Err(error) if error.capacity().is_some_and(|c| can_retry(c.recovery())) => {
                self.collect = true;
                Ok(())
            }
            result => Ok(result?),
        }
    }
    pub fn finish_upload(
        &mut self,
        runtime: &mut sashiko_renderer::core::runtime::Runtime,
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
