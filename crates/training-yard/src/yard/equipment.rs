//! Shared equipment assets, with one independent request per actor's armor slot.
use super::emission;
use renderer::{
    core::{
        capacity::{GraphRequirements, Preparation},
        graph::{BufferDesc, GraphError, ImageDesc},
        runtime::{FrameId, ManagedImage},
    },
    material::{MaterialHandle, Materials},
    mesh::{MeshData, MeshHandle, Meshes},
    pbr::{MaterialUniform, PbrVertex},
    pipeline::{
        AssetCandidate, AssetPreparation, PipelineResult, RenderObject, RetireAssets, UploadBudget,
    },
};
use std::{
    sync::{Arc, Weak},
    task::Poll,
    thread::JoinHandle,
};

const SLOTS: usize = 2;
const KINDS: [EquipmentAsset; 4] = [
    EquipmentAsset::Narrow,
    EquipmentAsset::Wide,
    EquipmentAsset::Broken,
    EquipmentAsset::Spare,
];

const PAINT_NAME: &str = "yard armor paint";
const PAINT_DESC: ImageDesc = ImageDesc::rgba8(1, 1);
const PAINT_PIXELS: [u8; 4] = [255, 255, 0, 255];

pub(crate) fn reserve(prep: &mut Preparation) -> Result<(), GraphError> {
    // Both actors can retain their old kit while the replacements are uploading.
    for size in [
        4 * renderer::passes::pbr::VERTEX_BYTES as u64,
        6 * 4,
        MaterialUniform::DESC.size,
    ] {
        prep.reserve_buffer(BufferDesc { size }, KINDS.len())?;
    }
    prep.reserve_image(PAINT_DESC, 1)?;
    prep.add_graph_requirements(GraphRequirements {
        upload_bytes: 256,
        resources: 4,
        accesses: 4,
        passes: 4,
        name_bytes: 128,
        ..Default::default()
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EquipmentAsset {
    Narrow,
    Wide,
    Broken,
    Spare,
}

struct LoadedArmor {
    mesh: MeshData,
    color: [f32; 3],
}

impl EquipmentAsset {
    fn load(self) -> Result<LoadedArmor, GraphError> {
        let (width, color) = match self {
            Self::Wide => (0.35, [1.; 3]),
            Self::Narrow | Self::Broken | Self::Spare => (0.17, [1., 0., 1.]),
        };
        let vertices = [
            (-width, 0.),
            (width, 0.),
            (-width * 0.5, 0.75),
            (width * 0.5, 0.75),
        ]
        .map(|(x, y)| {
            PbrVertex::new([x, y, 0.5], [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap()
        });
        let last = if matches!(self, Self::Broken) { 4 } else { 2 };
        Ok(LoadedArmor {
            mesh: MeshData::pbr(&vertices, &[0, 1, 2, 1, 3, last])?,
            color,
        })
    }
}

struct Armor {
    object: RenderObject,
    _paint: Arc<ManagedImage>,
}

#[derive(Default)]
enum State {
    #[default]
    Empty,
    Reading(JoinHandle<Result<LoadedArmor, GraphError>>),
    Decoded(LoadedArmor),
    Registered(Armor),
    LoadingGpu {
        armor: Armor,
        candidate: AssetCandidate,
    },
    Resident(Armor),
}

#[derive(Clone, Copy, Default)]
struct Slot {
    active: Option<EquipmentAsset>,
    wanted: Option<EquipmentAsset>,
    sequence: u64,
    delivered: Option<u64>,
}
impl Slot {
    fn can_adopt(&self, asset: EquipmentAsset) -> bool {
        self.wanted == Some(asset) && self.delivered == Some(self.sequence)
    }
    fn revoke(&mut self) {
        self.wanted = None;
        self.delivered = None;
    }
}

struct Retirement {
    ids: Vec<EquipmentAsset>,
    pairs: Vec<(MeshHandle, MaterialHandle)>,
    meshes: Vec<MeshHandle>,
    materials: Vec<MaterialHandle>,
}

impl Default for Retirement {
    fn default() -> Self {
        Self {
            ids: Vec::with_capacity(KINDS.len()),
            pairs: Vec::with_capacity(KINDS.len()),
            meshes: Vec::with_capacity(KINDS.len()),
            materials: Vec::with_capacity(KINDS.len()),
        }
    }
}

#[derive(Default)]
pub(crate) struct Equipment {
    retiring: Retirement,
    slots: [Slot; SLOTS],
    items: [State; KINDS.len()],
    paint: Weak<ManagedImage>,
    pending_paint: Option<(FrameId, Arc<ManagedImage>)>,
    cursor: usize,
    collect_retired: bool,
    pub error: Option<GraphError>,
}

impl Equipment {
    pub fn request(&mut self, slot: usize, asset: EquipmentAsset) -> PipelineResult<()> {
        let target = self.slots.get_mut(slot).ok_or("unknown equipment slot")?;
        let sequence = target
            .sequence
            .checked_add(1)
            .ok_or("equipment sequence exhausted")?;
        let item = &mut self.items[asset as usize];
        if matches!(item, State::Empty) {
            *item = State::Reading(std::thread::Builder::new().spawn(move || asset.load())?);
        }
        target.sequence = sequence;
        target.wanted = Some(asset);
        target.delivered = None;
        Ok(())
    }

    // Delivery is an input to the replay so reversed completions are reproducible.
    pub fn deliver(&mut self, asset: EquipmentAsset) {
        for slot in &mut self.slots {
            if slot.wanted == Some(asset) {
                slot.delivered = Some(slot.sequence);
            }
        }
    }

    pub fn object(&self, slot: usize) -> Option<RenderObject> {
        let id = self.slots[slot].active?;
        let State::Resident(armor) = &self.items[id as usize] else {
            unreachable!("only committed equipment is visible")
        };
        Some(armor.object)
    }

    pub fn detach(
        &mut self,
        slot: usize,
        device: &mut impl AssetPreparation,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> PipelineResult<()> {
        let mut next = self.slots;
        next[slot].active = None;
        next[slot].revoke();
        self.publish(None, next, device, meshes, materials)?;
        Ok(())
    }

    fn publish(
        &mut self,
        new: Option<EquipmentAsset>,
        next: [Slot; SLOTS],
        device: &mut impl AssetPreparation,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> PipelineResult<()> {
        let retiring = &mut self.retiring;
        retiring.ids.clear();
        retiring.pairs.clear();
        retiring.meshes.clear();
        retiring.materials.clear();
        for id in KINDS {
            if next
                .iter()
                .any(|s| s.active == Some(id) || s.wanted == Some(id))
            {
                continue;
            }
            let State::Resident(armor) = &self.items[id as usize] else {
                continue;
            };
            retiring
                .pairs
                .push((armor.object.mesh, armor.object.material));
            retiring.meshes.push(armor.object.mesh);
            retiring.materials.push(armor.object.material);
            retiring.ids.push(id);
        }
        let mut empty;
        let candidate = match new.and_then(|id| self.items.get_mut(id as usize)) {
            Some(State::LoadingGpu { candidate, .. }) => candidate,
            _ => {
                empty = AssetCandidate::new(meshes, materials, &[])?;
                &mut empty
            }
        };
        device.commit_candidate(
            candidate,
            meshes,
            materials,
            RetireAssets {
                pairs: &retiring.pairs,
                meshes: &retiring.meshes,
                materials: &retiring.materials,
            },
        )?;
        self.collect_retired |= !retiring.ids.is_empty();
        for id in retiring.ids.drain(..) {
            self.items[id as usize] = State::Empty;
        }
        if let Some(id) = new {
            let item = &mut self.items[id as usize];
            if matches!(item, State::LoadingGpu { .. }) {
                let State::LoadingGpu { armor, .. } = std::mem::take(item) else {
                    unreachable!()
                };
                *item = State::Resident(armor);
            }
        }
        self.slots = next;
        Ok(())
    }

    /// Advance at most one asset per tick. Readers and GPU transfers never wait on the frame thread.
    pub fn advance<R: AssetPreparation>(
        &mut self,
        renderer: &mut R,
        meshes: &mut Meshes,
        materials: &mut Materials,
        upload: impl FnOnce(&mut R, &str, ImageDesc, &[u8]) -> PipelineResult<(FrameId, ManagedImage)>,
    ) -> PipelineResult<()> {
        if self.collect_retired {
            self.collect_retired = renderer.collect_assets(meshes, materials)? != 0;
        }
        // Only consume this asset upload's result; a host may have a readback outstanding.
        if let Some((frame, _)) = &self.pending_paint {
            if !renderer.asset_renderer().poll_texture_upload(*frame)? {
                return Ok(());
            }
            self.pending_paint = None;
        }
        for delta in 0..KINDS.len() {
            let index = (self.cursor + delta) % KINDS.len();
            let asset = KINDS[index];
            let wanted = self.slots.iter().any(|s| s.wanted == Some(asset));
            let admitted = self.slots.iter().any(|s| s.can_adopt(asset));
            let item = &mut self.items[index];
            match item {
                State::Reading(worker) if worker.is_finished() && (admitted || !wanted) => {
                    let State::Reading(worker) = std::mem::take(item) else {
                        unreachable!()
                    };
                    self.cursor = (index + 1) % KINDS.len();
                    if !wanted {
                        return Ok(());
                    }
                    let loaded = worker
                        .join()
                        .unwrap_or_else(|_| Err(GraphError::custom("equipment reader panicked")));
                    let loaded = match loaded {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            for slot in &mut self.slots {
                                if slot.wanted == Some(asset) {
                                    slot.revoke();
                                }
                            }
                            self.error = Some(error);
                            return Ok(());
                        }
                    };
                    *item = State::Decoded(loaded);
                }
                State::Decoded(_) if !wanted => *item = State::Empty,
                State::Decoded(_) if admitted => {
                    let paint = match self.paint.upgrade() {
                        Some(paint) => paint,
                        None => {
                            let (frame, image) =
                                upload(renderer, PAINT_NAME, PAINT_DESC, &PAINT_PIXELS)?;
                            let paint = Arc::new(image);
                            self.paint = Arc::downgrade(&paint);
                            self.pending_paint = Some((frame, paint.clone()));
                            paint
                        }
                    };
                    let State::Decoded(loaded) = item else {
                        unreachable!()
                    };
                    let mut params = emission(loaded.color);
                    params.textures.emissive = Some(paint.snapshot().clone().into());
                    let material = materials.create_pbr(params)?;
                    let State::Decoded(loaded) = std::mem::take(item) else {
                        unreachable!()
                    };
                    let mesh = match meshes.create(loaded.mesh) {
                        Ok(mesh) => mesh,
                        Err(error) => {
                            materials.remove(material)?;
                            for s in &mut self.slots {
                                if s.wanted == Some(asset) {
                                    s.revoke();
                                }
                            }
                            return Err(error.into());
                        }
                    };
                    let object = RenderObject::new(mesh, material);
                    *item = State::Registered(Armor {
                        object,
                        _paint: paint,
                    });
                }
                State::Registered(armor) => {
                    let candidate = AssetCandidate::new(meshes, materials, &[armor.object])?;
                    let State::Registered(armor) = std::mem::take(item) else {
                        unreachable!()
                    };
                    *item = State::LoadingGpu { armor, candidate };
                }
                State::LoadingGpu { armor, candidate } if !wanted => {
                    let object = armor.object;
                    renderer.cancel_candidate(
                        candidate,
                        meshes,
                        materials,
                        RetireAssets {
                            pairs: &[(object.mesh, object.material)],
                            meshes: &[object.mesh],
                            materials: &[object.material],
                        },
                    )?;
                    *item = State::Empty;
                    self.collect_retired = true;
                }
                State::LoadingGpu { candidate, .. } if admitted => {
                    if let Poll::Ready(()) = renderer.poll_candidate(
                        candidate,
                        meshes,
                        materials,
                        UploadBudget {
                            bytes: 128,
                            submissions: 1,
                        },
                    )? {
                        self.adopt(asset, renderer, meshes, materials)?;
                    }
                }
                State::Resident(_)
                    if !wanted && !self.slots.iter().any(|s| s.active == Some(asset)) =>
                {
                    self.publish(None, self.slots, renderer, meshes, materials)?;
                }
                State::Resident(_) if admitted => self.adopt(asset, renderer, meshes, materials)?,
                _ => continue,
            }
            self.cursor = (index + 1) % KINDS.len();
            return Ok(());
        }
        Ok(())
    }

    fn adopt(
        &mut self,
        asset: EquipmentAsset,
        renderer: &mut impl AssetPreparation,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> PipelineResult<()> {
        let mut next = self.slots;
        for slot in &mut next {
            if slot.can_adopt(asset) {
                slot.active = Some(asset);
                slot.revoke();
            }
        }
        self.publish(Some(asset), next, renderer, meshes, materials)
    }

    /// End the lifetime of an outstanding texture transfer when the application closes mid-load.
    pub fn finish_upload(
        &mut self,
        runtime: &mut renderer::core::runtime::Runtime,
    ) -> PipelineResult<()> {
        if let Some((id, _)) = &self.pending_paint {
            runtime
                .wait_for_frame(*id, std::time::Duration::from_secs(15))?
                .ok_or("equipment upload shutdown timed out")?;
            self.pending_paint = None;
        }
        Ok(())
    }
    pub fn pending_delivery(&self) -> bool {
        self.slots
            .iter()
            .any(|s| s.wanted.is_some() && s.delivered == Some(s.sequence))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_targets_each_actors_current_request() -> PipelineResult<()> {
        let mut equipment = Equipment::default();
        equipment.request(0, EquipmentAsset::Narrow)?;
        equipment.request(1, EquipmentAsset::Narrow)?;
        equipment.request(0, EquipmentAsset::Wide)?;
        equipment.deliver(EquipmentAsset::Narrow);
        assert!(!equipment.slots[0].can_adopt(EquipmentAsset::Narrow));
        assert!(equipment.slots[1].can_adopt(EquipmentAsset::Narrow));
        equipment.deliver(EquipmentAsset::Wide);
        assert!(equipment.slots[0].can_adopt(EquipmentAsset::Wide));
        // Clearing an actor slot revokes its old ticket, independently of its reader.
        equipment.slots[0].revoke();
        equipment.deliver(EquipmentAsset::Wide);
        assert!(!equipment.slots[0].can_adopt(EquipmentAsset::Wide));
        assert!(equipment.slots[1].can_adopt(EquipmentAsset::Narrow));
        Ok(())
    }
}
