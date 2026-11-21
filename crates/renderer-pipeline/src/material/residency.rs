//! GPU registrations owned by the material registry.
use super::*;

impl Materials {
    /// Upload new or edited inline PBR factors together at an asset-change boundary.
    /// Reserve one 64-byte buffer per distinct new revision, including overlap
    /// with old versions. The preparation pool must accommodate these uploads.
    /// Already resident revisions and non-PBR materials need no submission.
    pub fn prepare_gpu(
        &mut self,
        runtime: &mut renderer::runtime::Runtime,
        pool: &mut renderer::graph::GraphPool,
    ) -> Result<Option<renderer::runtime::FrameId>, pbr::DefaultResourceError> {
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
            material.source = PbrSource::Imported(snapshot);
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
        runtime: &mut renderer::runtime::Runtime,
    ) -> Result<(), renderer::runtime::ExecutionError> {
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
        runtime: &mut renderer::runtime::Runtime,
    ) -> Result<(), renderer::runtime::ExecutionError> {
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
