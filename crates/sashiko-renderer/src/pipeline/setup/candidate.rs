//! Incremental preparation for one engine-owned asset change.
use super::*;
use crate::AssetPrepareError;
use sashiko_renderer_core::capacity::{
    CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind,
};
use sashiko_renderer_core::runtime::ManagedImage;
use std::{sync::Arc, task::Poll};

/// Upper bounds per maintenance tick. A material uniform requires 64 bytes in one upload.
#[derive(Clone, Copy, Debug)]
pub struct UploadBudget {
    pub bytes: u64,
    pub submissions: usize,
}

/// Handles whose last engine consumer will disappear when this change is published.
/// Each resource or pair must occur once. Omit resources shared with another object.
#[derive(Default, Clone, Copy)]
pub struct RetireAssets<'a> {
    pub pairs: &'a [(MeshHandle, MaterialHandle)],
    pub meshes: &'a [MeshHandle],
    pub materials: &'a [MaterialHandle],
}

impl RetireAssets<'_> {
    fn check(&self, meshes: &Meshes, materials: &Materials) -> PipelineResult<()> {
        let unique_meshes: BTreeSet<_> = self.meshes.iter().collect();
        let unique_materials: BTreeSet<_> = self.materials.iter().collect();
        if unique_meshes.len() != self.meshes.len()
            || unique_materials.len() != self.materials.len()
        {
            return Err("duplicate asset retirement".into());
        }
        for &mesh in self.meshes {
            meshes.source_revision(mesh)?;
        }
        for &material in self.materials {
            materials.revision(material)?;
        }
        let (occupied, limit) = meshes.retirement_usage();
        if self.meshes.len() > limit - occupied {
            return Err(RuntimeCapacityError::new(
                RuntimeCapacityKind::RetiredMeshVersions,
                if self.meshes.len() > limit {
                    CapacityRecovery::Reconfigure
                } else {
                    CapacityRecovery::ReleaseOwners
                },
                Some(limit as u64),
                (occupied as u64).checked_add(self.meshes.len() as u64),
            )
            .into());
        }
        Ok(())
    }

    /// check() preflights every registry access and all remaining retirement slots
    /// under the same exclusive borrows. No fallible GPU work follows publication.
    fn apply(self, driver: &mut ViewRenderer, meshes: &mut Meshes, materials: &mut Materials) {
        driver.remove_pairs(self.pairs);
        for &handle in self.meshes {
            meshes.remove(handle).expect("preflighted mesh retirement");
        }
        for &handle in self.materials {
            materials
                .remove(handle)
                .expect("preflighted material retirement");
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Meshes,
    Materials,
    Ready,
    Finished,
}

/// A delta to the prepared drawing set. Stage replacement data under fresh
/// handles. Live unchanged resources can be shared. The engine retains owners
/// of referenced images and both registries while the request is outstanding.
/// Concurrent candidates may share both fully prepared revisions and uploads in
/// progress on this renderer. All dependants observe the producer's completion
/// before becoming ready. Externally submitted uploads remain the host's responsibility.
#[must_use]
pub struct AssetCandidate {
    objects: Vec<RenderObject>,
    meshes: Vec<MeshHandle>,
    mesh_revisions: Vec<u64>,
    materials: BTreeSet<MaterialHandle>,
    material_revisions: Vec<u64>,
    lights: Vec<LocalLight>,
    stage: Stage,
    queue: Option<Arc<()>>,
    pending: Option<FrameId>,
    allow_in_place: bool,
}

impl AssetCandidate {
    pub fn new(
        meshes: &Meshes,
        materials: &Materials,
        changed: &[RenderObject],
    ) -> PipelineResult<Self> {
        Self::from_selection(meshes, materials, changed, &[], &[], &[], false)
    }

    pub(super) fn from_scene(assets: &SceneAssets<'_>) -> PipelineResult<Self> {
        Self::from_selection(
            assets.meshes,
            assets.materials,
            &assets.objects,
            &assets.extra_meshes,
            &assets.extra_materials,
            assets.lights,
            true,
        )
    }

    fn from_selection(
        registry: &Meshes,
        material_registry: &Materials,
        objects: &[RenderObject],
        extra_meshes: &[MeshHandle],
        extra_materials: &[MaterialHandle],
        lights: &[LocalLight],
        allow_in_place: bool,
    ) -> PipelineResult<Self> {
        let mut seen = std::collections::HashSet::new();
        let meshes: Vec<_> = objects
            .iter()
            .map(|o| o.mesh)
            .chain(extra_meshes.iter().copied())
            .filter(|h| seen.insert(*h))
            .collect();
        let materials: BTreeSet<_> = objects
            .iter()
            .map(|o| o.material)
            .chain(extra_materials.iter().copied())
            .collect();
        let mesh_revisions = meshes
            .iter()
            .map(|&h| registry.source_revision(h))
            .collect::<Result<_, _>>()?;
        let material_revisions = materials
            .iter()
            .map(|&h| material_registry.revision(h))
            .collect::<Result<_, _>>()?;
        let stage = if meshes.is_empty() && materials.is_empty() {
            Stage::Ready
        } else {
            Stage::Meshes
        };
        let candidate = Self {
            objects: objects.to_vec(),
            meshes,
            materials,
            mesh_revisions,
            material_revisions,
            lights: lights.to_vec(),
            stage,
            queue: None,
            pending: None,
            allow_in_place,
        };
        candidate.check_versions(registry, material_registry)?;
        Ok(candidate)
    }

    fn check_versions(&self, meshes: &Meshes, materials: &Materials) -> PipelineResult<()> {
        for (&handle, &rev) in self.meshes.iter().zip(&self.mesh_revisions) {
            if meshes.source_revision(handle)? != rev
                || (!self.allow_in_place && meshes.revision(handle)? != rev)
            {
                return Err(
                    "candidate mesh changed; stage a replacement under a new handle".into(),
                );
            }
        }
        for (&handle, &rev) in self.materials.iter().zip(&self.material_revisions) {
            if materials.revision(handle)? != rev {
                return Err("candidate material revision changed".into());
            }
        }
        Ok(())
    }

    fn check_queue(&self, transfers: &Transfers) -> PipelineResult<()> {
        if self.stage == Stage::Finished {
            return Err("asset candidate has already finished".into());
        }
        if self
            .queue
            .as_ref()
            .is_some_and(|id| !Arc::ptr_eq(id, &transfers.id))
        {
            return Err("asset candidate belongs to another renderer".into());
        }
        Ok(())
    }

    /// A shared native revision can become importable at queue acceptance,
    /// including through a duplicate material handle. Adoption also requires
    /// completion of the asset transfer that produced that exact revision.
    fn pending_upload(
        &self,
        meshes: &Meshes,
        materials: &Materials,
        transfers: &Transfers,
    ) -> Result<Option<FrameId>, GraphError> {
        if transfers.pending.is_empty() {
            return Ok(None);
        }
        for &mesh in &self.meshes {
            if let Some(id) = meshes.resident_submission(mesh)
                && transfers.pending.contains(&id)
            {
                return Ok(Some(id));
            }
        }
        for &handle in &self.materials {
            if let MaterialInstance::Pbr(material) = materials.get(handle)?
                && let Some(id) = material.uploaded_by()
                && transfers.pending.contains(&id)
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }
}

/// Accepted submissions belong to the renderer, including after a candidate is
/// cancelled or abandoned. Completion collection never consumes an engine frame.
pub(super) struct Transfers {
    id: Arc<()>,
    pending: Vec<FrameId>,
}

impl Transfers {
    pub(super) fn new(max_in_flight: usize) -> Self {
        Self {
            id: Arc::new(()),
            pending: Vec::with_capacity(max_in_flight),
        }
    }
    fn collect(&mut self, runtime: &mut Runtime, timeout: Duration) -> PipelineResult<()> {
        let mut i = 0;
        while i < self.pending.len() {
            if runtime.wait_for_frame(self.pending[i], timeout)?.is_some() {
                self.pending.swap_remove(i);
            } else {
                i += 1;
            }
        }
        Ok(())
    }

    pub(super) fn drain(&mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        while !self.pending.is_empty() {
            self.collect(runtime, Duration::from_secs(1))?;
        }
        Ok(())
    }
}

pub(crate) trait AssetTarget {
    type Device: BorrowMut<Runtime>;
    fn asset_renderer(&mut self) -> &mut SceneRenderer<Self::Device>;
    fn poll_presentation_for_asset_change(&mut self) -> PipelineResult<bool> {
        Ok(true)
    }
}

impl<R: BorrowMut<Runtime>> AssetTarget for SceneRenderer<R> {
    type Device = R;
    fn asset_renderer(&mut self) -> &mut SceneRenderer<R> {
        self
    }
}

/// Change-boundary operations on standard, window and host renderers.
/// Prepare mesh, uniform, texture, draw and transfer capacities up front.
/// The engine must submit or cancel recorded graphs before retiring their assets.
///
/// ```compile_fail
/// use sashiko_renderer::pipeline::AssetPreparation;
/// fn access_scene_renderer(assets: &mut impl AssetPreparation) {
///     let _ = assets.asset_renderer();
/// }
/// ```
pub trait AssetPreparation {
    /// Check this device's sampled/transfer support (and linear filtering when required).
    fn check_texture_support(
        &mut self,
        desc: ImageDesc,
        linear_filtering: bool,
    ) -> Result<(), AssetPrepareError>;

    /// Upload selected mips into a fresh immutable version. Other levels are undefined.
    fn upload_texture_mips(
        &mut self,
        name: &str,
        desc: ImageDesc,
        levels: std::ops::Range<u32>,
        bytes: &[u8],
    ) -> Result<(FrameId, ManagedImage), AssetPrepareError>;

    /// Publish a new version, copying other initialized levels on the GPU. The old
    /// registration and its previously recorded and future reads remain valid.
    fn promote_texture(
        &mut self,
        name: &str,
        previous: &ManagedImage,
        levels: std::ops::Range<u32>,
        bytes: &[u8],
    ) -> Result<(FrameId, ManagedImage), AssetPrepareError>;

    /// Consume this upload completion when available. Poll it only until true.
    fn poll_texture_upload(&mut self, id: FrameId) -> Result<bool, AssetPrepareError>;

    /// Upload one complete texture. Keep its owner until the last use is submitted.
    fn upload_texture(
        &mut self,
        name: &str,
        desc: ImageDesc,
        bytes: &[u8],
    ) -> Result<(FrameId, ManagedImage), AssetPrepareError>;

    /// Install one reservation per simultaneously needed buffer or image without
    /// recreating cameras. Texture transfers are serialized. At a change boundary,
    /// return external result/export owners before calling. A window may yield a
    /// draw to retire its own presentation; keep pumping events and drawing until ready.
    fn replace_asset_reservations(
        &mut self,
        texture_name: &str,
        buffers: &[sashiko_renderer_core::graph::BufferDesc],
        images: &[ImageDesc],
        samplers: &[sashiko_renderer_core::program::SamplerDescriptor],
    ) -> Result<Poll<()>, AssetPrepareError>;

    /// Progress a candidate without waiting. Already accepted work survives
    /// backpressure; retry the same candidate after recovering the reported capacity.
    fn poll_candidate(
        &mut self,
        candidate: &mut AssetCandidate,
        meshes: &mut Meshes,
        materials: &mut Materials,
        budget: UploadBudget,
    ) -> Result<Poll<()>, AssetPrepareError>;

    /// Publish across all current view pipelines, then invalidate the last-use
    /// handles in `retire`. A failed call preserves the old registrations.
    fn commit_candidate(
        &mut self,
        candidate: &mut AssetCandidate,
        meshes: &mut Meshes,
        materials: &mut Materials,
        retire: RetireAssets<'_>,
    ) -> Result<(), AssetPrepareError>;

    /// Discard the candidate. Supply only candidate-owned resources for retirement.
    /// Accepted transfer completions remain with the renderer for explicit collection.
    fn cancel_candidate(
        &mut self,
        candidate: &mut AssetCandidate,
        meshes: &mut Meshes,
        materials: &mut Materials,
        retire: RetireAssets<'_>,
    ) -> Result<(), AssetPrepareError>;

    /// Release completed transfer results and obsolete GPU registrations at a
    /// maintenance boundary. Returns the number of still-pending asset submissions.
    fn collect_assets(
        &mut self,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> Result<usize, AssetPrepareError>;
}

impl<T: AssetTarget> AssetPreparation for T {
    fn check_texture_support(
        &mut self,
        desc: ImageDesc,
        linear_filtering: bool,
    ) -> Result<(), AssetPrepareError> {
        Ok(self
            .asset_renderer()
            .runtime()
            .check_texture_support(desc, linear_filtering)?)
    }

    fn upload_texture_mips(
        &mut self,
        name: &str,
        desc: ImageDesc,
        levels: std::ops::Range<u32>,
        bytes: &[u8],
    ) -> Result<(FrameId, ManagedImage), AssetPrepareError> {
        let (_, pool, runtime) = self.asset_renderer().parts_mut();
        Ok(runtime.upload_texture_mips(pool, name, desc, None, levels, bytes)?)
    }

    fn promote_texture(
        &mut self,
        name: &str,
        previous: &ManagedImage,
        levels: std::ops::Range<u32>,
        bytes: &[u8],
    ) -> Result<(FrameId, ManagedImage), AssetPrepareError> {
        let (_, pool, runtime) = self.asset_renderer().parts_mut();
        Ok(runtime.upload_texture_mips(
            pool,
            name,
            previous.snapshot().desc(),
            Some(previous),
            levels,
            bytes,
        )?)
    }

    fn poll_texture_upload(&mut self, id: FrameId) -> Result<bool, AssetPrepareError> {
        Ok(self
            .asset_renderer()
            .runtime()
            .wait_for_frame(id, Duration::ZERO)?
            .is_some())
    }

    fn upload_texture(
        &mut self,
        name: &str,
        desc: ImageDesc,
        bytes: &[u8],
    ) -> Result<(FrameId, ManagedImage), AssetPrepareError> {
        let (_, pool, runtime) = self.asset_renderer().parts_mut();
        Ok(runtime.upload_texture(pool, name, desc, bytes)?)
    }

    fn replace_asset_reservations(
        &mut self,
        texture_name: &str,
        buffers: &[sashiko_renderer_core::graph::BufferDesc],
        images: &[ImageDesc],
        samplers: &[sashiko_renderer_core::program::SamplerDescriptor],
    ) -> Result<Poll<()>, AssetPrepareError> {
        if !self.poll_presentation_for_asset_change()? {
            return Ok(Poll::Pending);
        }
        self.asset_renderer().install_asset_reservations(
            texture_name,
            buffers,
            images,
            samplers,
        )?;
        Ok(Poll::Ready(()))
    }

    fn poll_candidate(
        &mut self,
        candidate: &mut AssetCandidate,
        meshes: &mut Meshes,
        materials: &mut Materials,
        budget: UploadBudget,
    ) -> Result<Poll<()>, AssetPrepareError> {
        self.asset_renderer()
            .advance_candidate(candidate, meshes, materials, budget, Duration::ZERO)
            .map_err(Into::into)
    }

    fn commit_candidate(
        &mut self,
        candidate: &mut AssetCandidate,
        meshes: &mut Meshes,
        materials: &mut Materials,
        retire: RetireAssets<'_>,
    ) -> Result<(), AssetPrepareError> {
        self.asset_renderer()
            .end_candidate(candidate, meshes, materials, retire, true)
            .map_err(Into::into)
    }

    fn cancel_candidate(
        &mut self,
        candidate: &mut AssetCandidate,
        meshes: &mut Meshes,
        materials: &mut Materials,
        retire: RetireAssets<'_>,
    ) -> Result<(), AssetPrepareError> {
        self.asset_renderer()
            .end_candidate(candidate, meshes, materials, retire, false)
            .map_err(Into::into)
    }

    fn collect_assets(
        &mut self,
        meshes: &mut Meshes,
        materials: &mut Materials,
    ) -> Result<usize, AssetPrepareError> {
        let drawing = self.asset_renderer();
        let runtime = drawing
            .runtime
            .as_mut()
            .expect("live renderer")
            .borrow_mut();
        drawing.transfers.collect(runtime, Duration::ZERO)?;
        meshes.collect_gpu(runtime)?;
        materials.collect_gpu(runtime)?;
        Ok(drawing.transfers.pending.len())
    }
}

impl<R: BorrowMut<Runtime>> SceneRenderer<R> {
    pub(super) fn advance_candidate(
        &mut self,
        c: &mut AssetCandidate,
        meshes: &mut Meshes,
        materials: &mut Materials,
        mut budget: UploadBudget,
        timeout: Duration,
    ) -> PipelineResult<Poll<()>> {
        c.check_queue(&self.transfers)?;
        c.check_versions(meshes, materials)?;
        let runtime = self.runtime.as_mut().expect("live renderer").borrow_mut();
        self.transfers.collect(runtime, timeout)?;
        if c.pending
            .is_some_and(|id| self.transfers.pending.contains(&id))
        {
            return Ok(Poll::Pending);
        }
        c.pending = None;
        meshes.check_runtime(&c.meshes, runtime)?;
        if c.queue.is_none() {
            MaterialCheckpoint::begin(materials, &c.materials, runtime)?;
            c.queue = Some(self.transfers.id.clone());
        }
        // Each issued frame is acknowledged before the candidate becomes ready.
        // Different candidates can use the remaining prepared execution slots.
        loop {
            if c.stage == Stage::Ready || budget.submissions == 0 {
                if c.stage == Stage::Ready && c.pending.is_none() {
                    c.pending = c.pending_upload(meshes, materials, &self.transfers)?;
                }
                return Ok(if c.stage == Stage::Ready && c.pending.is_none() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                });
            }
            let sent = match c.stage {
                Stage::Meshes => {
                    let progress = meshes.stream_gpu(
                        runtime,
                        &mut self.pool,
                        &c.meshes,
                        budget.bytes.min(self.mesh_budget),
                    )?;
                    if progress.remaining_bytes == 0 {
                        c.stage = Stage::Materials;
                    } else if progress.submission.is_none() {
                        return Ok(Poll::Pending);
                    }
                    budget.bytes -= progress.submitted_bytes;
                    progress.submission
                }
                Stage::Materials => {
                    let mut batch = BTreeSet::new();
                    let mut deferred = false;
                    for &handle in &c.materials {
                        if matches!(materials.get(handle)?, MaterialInstance::Pbr(p) if !p.is_resident())
                        {
                            let cost = crate::passes::pbr::MATERIAL_BYTES as u64;
                            if cost > budget.bytes {
                                deferred = true;
                                break;
                            }
                            budget.bytes -= cost;
                            batch.insert(handle);
                        }
                    }
                    let checkpoint = MaterialCheckpoint::begin(materials, &batch, runtime)?;
                    let id = checkpoint.upload(materials, runtime)?;
                    if !deferred {
                        c.stage = Stage::Ready;
                    } else if id.is_none() {
                        return Ok(Poll::Pending);
                    }
                    id
                }
                Stage::Ready | Stage::Finished => unreachable!(),
            };
            if let Some(id) = sent {
                self.transfers.pending.push(id);
                c.pending = Some(id);
                budget.submissions -= 1;
            }
        }
    }

    pub(super) fn end_candidate(
        &mut self,
        c: &mut AssetCandidate,
        meshes: &mut Meshes,
        materials: &mut Materials,
        retire: RetireAssets<'_>,
        publish: bool,
    ) -> PipelineResult<()> {
        c.check_queue(&self.transfers)?;
        self.driver.check_idle()?;
        retire.check(meshes, materials)?;
        if publish {
            if c.stage != Stage::Ready
                || c.pending.is_some()
                || c.pending_upload(meshes, materials, &self.transfers)?
                    .is_some()
            {
                return Err("asset candidate has not completed preparation".into());
            }
            if retire.meshes.iter().any(|h| c.meshes.contains(h))
                || retire.materials.iter().any(|h| c.materials.contains(h))
                || retire
                    .pairs
                    .iter()
                    .any(|&(m, a)| c.objects.iter().any(|o| o.mesh == m && o.material == a))
            {
                return Err("retired asset is used by the new selection".into());
            }
            c.check_versions(meshes, materials)?;
            if c.meshes
                .iter()
                .zip(&c.mesh_revisions)
                .any(|(&handle, &revision)| meshes.resident_revision(handle) != Some(revision))
            {
                return Err("candidate mesh residency changed before publication".into());
            }
            let runtime = self.runtime.as_mut().expect("live renderer").borrow_mut();
            meshes.check_runtime(&c.meshes, runtime)?;
            for &handle in &c.materials {
                if matches!(materials.get(handle)?, MaterialInstance::Pbr(p) if !p.is_resident()) {
                    return Err("candidate material residency changed before publication".into());
                }
            }
            MaterialCheckpoint::begin(materials, &c.materials, runtime)?;
            self.driver.prepare_all(
                SceneInput::new(meshes, materials, &c.objects).with_lights(&c.lights),
            )?;
        }
        retire.apply(&mut self.driver, meshes, materials);
        c.stage = Stage::Finished;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        material::UnlitParams,
        mesh::{MeshCapacity, MeshData},
        unlit_forward::Vertex,
    };

    fn triangle() -> Result<MeshData, GraphError> {
        MeshData::unlit(
            &[Vertex {
                position: [0.; 3],
                color: [1.; 4],
            }; 3],
            &[0, 1, 2],
        )
    }

    #[test]
    fn candidate_rejects_changed_mesh_and_capacity_before_retiring_anything() -> PipelineResult<()>
    {
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 3,
            retired_mesh_versions: 1,
        })?;
        let mut materials = Materials::default();
        let material = materials.create_unlit(UnlitParams::default())?;
        let old = meshes.create(triangle()?)?;
        let next = meshes.create(triangle()?)?;
        let candidate =
            AssetCandidate::new(&meshes, &materials, &[RenderObject::new(next, material)])?;
        meshes.update(next, triangle()?)?;
        assert!(candidate.check_versions(&meshes, &materials).is_err());
        let retire = RetireAssets {
            meshes: &[old],
            materials: &[material],
            pairs: &[(old, material)],
        };
        assert!(retire.check(&meshes, &materials).is_err());
        assert!(meshes.geometry(old).is_ok() && materials.get(material).is_ok());
        Ok(())
    }

    #[test]
    fn duplicate_retirement_is_rejected_before_deleting_the_first_handle() -> PipelineResult<()> {
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 2,
            retired_mesh_versions: 2,
        })?;
        let materials = Materials::default();
        let old = meshes.create(triangle()?)?;
        assert!(
            RetireAssets {
                meshes: &[old, old],
                ..Default::default()
            }
            .check(&meshes, &materials)
            .is_err()
        );
        assert!(meshes.geometry(old).is_ok());
        Ok(())
    }
}
