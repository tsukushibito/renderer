//! GPU registrations owned by the material registry.
use super::*;
use renderer_core::runtime::{ExecutionError, Runtime};
use std::collections::BTreeSet;

pub(crate) struct MaterialCheckpoint {
    original: Vec<(MaterialHandle, Arc<MaterialInstance>)>,
    uniform_start: usize,
}

impl MaterialCheckpoint {
    pub(crate) fn begin(
        materials: &Materials,
        selected: &BTreeSet<MaterialHandle>,
        runtime: &mut Runtime,
    ) -> Result<Self, pbr::DefaultResourceError> {
        materials.prepare_samplers(runtime, selected.iter().copied())?;
        let mut original = Vec::new();
        for &handle in selected {
            let entry = &materials.entries[&handle];
            match entry.as_ref() {
                MaterialInstance::Pbr(p) => {
                    if let PbrSource::Imported { buffer, .. } = &p.source {
                        runtime.refresh_buffer(buffer.key())?;
                    }
                    for image in [
                        &p.textures.base_color,
                        &p.textures.normal,
                        &p.textures.orm,
                        &p.textures.emissive,
                    ]
                    .into_iter()
                    .flatten()
                    {
                        runtime.refresh_image(image.image.key())?;
                    }
                }
                MaterialInstance::Custom(m) => {
                    for p in m.definition().parameters() {
                        if let Some(Value::Texture(image)) = m.value(p.name()) {
                            runtime.refresh_image(image.key())?;
                        }
                    }
                }
                MaterialInstance::Unlit(_) => {}
            }
            if materials.dirty.contains(&handle) {
                original.push((handle, entry.clone()));
            }
        }
        Ok(Self {
            original,
            uniform_start: materials.owned_uniforms.len(),
        })
    }

    pub(crate) fn upload(
        &self,
        materials: &mut Materials,
        runtime: &mut Runtime,
    ) -> Result<Option<renderer_core::runtime::FrameId>, pbr::DefaultResourceError> {
        let count = self.original.len();
        if count == 0 {
            return Ok(None);
        }
        // Exported buffer provenance may keep its upload Graph alive after fence
        // completion. Give this revision its own storage outside the frame pool.
        let size = |per: usize| {
            count
                .checked_mul(per)
                .ok_or_else(|| GraphError::invalid_input("material upload capacity overflow"))
        };
        let mut pool = renderer_core::graph::GraphPool::new(
            renderer_core::graph::GraphCapacity {
                resources: count,
                passes: count,
                accesses: count,
                draws: 0,
                raster_constant_words: Some(0),
                bindings: 0,
                attachments: 0,
                dependencies: 0,
                initial_ranges: 0,
                name_bytes: size(2 * "material factors".len())?,
                upload_bytes: size(crate::passes::pbr::MATERIAL_BYTES)?,
                edges: size(2)?,
                segments: size(2)?,
                readers: count,
            },
            1,
        )?;
        let mut other = std::mem::take(&mut materials.dirty);
        for (handle, _) in &self.original {
            if other.remove(handle) {
                materials.dirty.insert(*handle);
            }
        }
        let result = materials.prepare_gpu(runtime, &mut pool);
        materials.dirty.append(&mut other);
        result
    }

    pub(crate) fn rollback(
        &mut self,
        materials: &mut Materials,
        runtime: &mut Runtime,
    ) -> Result<(), ExecutionError> {
        for (handle, entry) in self.original.drain(..) {
            materials.entries.insert(handle, entry);
            materials.dirty.insert(handle);
        }
        while materials.owned_uniforms.len() > self.uniform_start {
            materials
                .owned_uniforms
                .last_mut()
                .expect("staged uniform")
                .1
                .release(runtime)?;
            materials.owned_uniforms.pop();
        }
        Ok(())
    }
}

impl Materials {
    /// Prepare the immutable sampler combinations used by these material revisions.
    /// Call at creation or update when using a host-owned runtime.
    pub fn prepare_samplers(
        &self,
        runtime: &mut Runtime,
        selected: impl IntoIterator<Item = MaterialHandle>,
    ) -> Result<(), pbr::DefaultResourceError> {
        let mut seen = [false; SamplerDescriptor::COUNT];
        let mut samplers = [SamplerDescriptor::LinearRepeat; SamplerDescriptor::COUNT];
        let mut count = 0;
        for handle in selected {
            self.get(handle)?.visit_samplers(|sampler| {
                if !std::mem::replace(&mut seen[sampler.index()], true) {
                    samplers[count] = sampler;
                    count += 1;
                }
            });
        }
        Ok(runtime.prepare_samplers(&samplers[..count])?)
    }

    /// Upload new or edited inline PBR factors together at an asset-change boundary.
    /// Reserve one 64-byte buffer per distinct new revision, including overlap
    /// with old versions. The preparation pool must accommodate these uploads.
    /// Already resident revisions and non-PBR materials need no submission.
    pub fn prepare_gpu(
        &mut self,
        runtime: &mut renderer_core::runtime::Runtime,
        pool: &mut renderer_core::graph::GraphPool,
    ) -> Result<Option<renderer_core::runtime::FrameId>, pbr::DefaultResourceError> {
        if self.dirty.is_empty() {
            return Ok(None);
        }
        let mut graph = pool.begin()?;
        let mut pending = HashMap::new();
        for handle in &self.dirty {
            let entry = &self.entries[handle];
            let MaterialInstance::Pbr(material) = entry.as_ref() else {
                unreachable!("dirty handles refer to inline PBR revisions");
            };
            let PbrSource::Inline(bytes) = &material.source else {
                unreachable!("dirty handles refer to inline PBR revisions");
            };
            if let std::collections::hash_map::Entry::Vacant(slot) =
                pending.entry(Arc::as_ptr(entry))
            {
                let uniform =
                    MaterialUniform::append(&mut graph, "material factors", bytes, material.alpha)?;
                uniform.export(&mut graph)?;
                slot.insert((entry.clone(), uniform));
            }
        }
        let submission = runtime.submit(&graph.compile()?)?;
        let mut replacements = HashMap::with_capacity(pending.len());
        for (key, (original, uniform)) in pending {
            let resident = uniform
                .accept(&submission)
                .expect("material exported by this submission");
            let MaterialInstance::Pbr(old) = original.as_ref() else {
                unreachable!()
            };
            let mut material = old.clone();
            let snapshot = Arc::new(resident.snapshot()?.clone());
            let lease = Arc::downgrade(&snapshot);
            material.source = PbrSource::Imported {
                buffer: snapshot,
                uploaded_by: Some(submission.id()),
            };
            let revision = Arc::new(MaterialInstance::Pbr(material));
            self.owned_uniforms.push((lease, resident));
            replacements.insert(key, (original, revision));
        }
        for handle in &self.dirty {
            let slot = self
                .entries
                .get_mut(handle)
                .expect("dirty material is registered");
            *slot = replacements[&Arc::as_ptr(slot)].1.clone();
        }
        self.dirty.clear();
        Ok(Some(submission.id()))
    }

    /// At a maintenance boundary after submitting or discarding previously
    /// built Graphs, unregister obsolete uniform versions. Accepted GPU work
    /// retains its own leases. Removed handles with CPU snapshots stay live.
    pub fn collect_gpu(
        &mut self,
        runtime: &mut renderer_core::runtime::Runtime,
    ) -> Result<(), renderer_core::runtime::ExecutionError> {
        let mut i = 0;
        while i < self.owned_uniforms.len() {
            if self.owned_uniforms[i].0.strong_count() == 0 {
                self.owned_uniforms[i].1.release(runtime)?;
                drop(self.owned_uniforms.swap_remove(i));
            } else {
                i += 1;
            }
        }
        Ok(())
    }

    /// End the registry's lifetime after all unsubmitted consumers are returned.
    pub fn clear(
        &mut self,
        runtime: &mut renderer_core::runtime::Runtime,
    ) -> Result<(), renderer_core::runtime::ExecutionError> {
        self.dirty.clear();
        self.entries.clear();
        self.revisions.clear();
        while let Some((_, uniform)) = self.owned_uniforms.last_mut() {
            uniform.release(runtime)?;
            self.owned_uniforms.pop();
        }
        Ok(())
    }
}
