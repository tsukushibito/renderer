//! Explicit, resumable uploads into prepared device storage.
use super::{Asset, Meshes, Resident};
use crate::pbr::DefaultResourceError;
use renderer::{
    geometry::{GeometryData, GeometryStream, MeshError, MeshHandle, MeshRegistry},
    graph::{BufferDesc, BufferHandle, Graph, GraphError, GraphPool},
    runtime::{ExecutionError, FrameId, Runtime},
};
use std::collections::HashMap;

/// Progress for the meshes selected by this call. A completed upload is usable
/// by subsequent draws on the same queue before its GPU fence signals.
#[derive(Clone, Copy, Debug)]
pub struct MeshUploadProgress {
    pub submission: Option<FrameId>,
    pub submitted_bytes: u64,
    pub remaining_bytes: u64,
    pub published_meshes: usize,
}

struct Transfer {
    mesh: MeshHandle,
    end: u64,
    exports: Option<[BufferHandle; 2]>,
}

pub(super) struct UploadBatch {
    transfers: Vec<Transfer>,
    epoch: u64,
}

impl UploadBatch {
    pub(super) fn new(capacity: usize) -> Result<Self, MeshError> {
        let mut transfers = Vec::new();
        transfers
            .try_reserve_exact(capacity)
            .map_err(|_| MeshError::capacity_exhausted("unable to reserve mesh transfer plans"))?;
        Ok(Self {
            transfers,
            epoch: 0,
        })
    }

    fn select(
        &mut self,
        registry: &MeshRegistry,
        assets: &mut HashMap<MeshHandle, Asset>,
        selection: impl Iterator<Item = MeshHandle>,
        byte_budget: u64,
    ) -> Result<MeshUploadProgress, GraphError> {
        self.transfers.clear();
        if self.epoch == u64::MAX {
            for asset in assets.values_mut() {
                asset.selected_epoch = 0;
            }
            self.epoch = 0;
        }
        self.epoch += 1;
        let mut available = byte_budget / 4 * 4;
        let mut progress = MeshUploadProgress {
            submission: None,
            submitted_bytes: 0,
            remaining_bytes: 0,
            published_meshes: 0,
        };
        for mesh in selection {
            let asset = assets
                .get_mut(&mesh)
                .ok_or_else(|| GraphError::invalid_input("unknown mesh handle"))?;
            if asset.selected_epoch == self.epoch {
                continue;
            }
            asset.selected_epoch = self.epoch;
            if asset
                .active
                .as_ref()
                .is_some_and(|ready| ready.revision == asset.revision)
            {
                continue;
            }
            let data = registry.geometry(mesh).expect("registered mesh");
            let remaining = data.byte_len() - asset.upload.accepted_bytes;
            let bytes = available.min(remaining);
            progress.remaining_bytes = progress
                .remaining_bytes
                .checked_add(remaining)
                .ok_or_else(|| GraphError::invalid_input("selected mesh upload size overflow"))?;
            if bytes != 0 {
                self.transfers.push(Transfer {
                    mesh,
                    end: asset.upload.accepted_bytes + bytes,
                    exports: None,
                });
                available -= bytes;
                progress.submitted_bytes += bytes;
            }
        }
        progress.remaining_bytes -= progress.submitted_bytes;
        Ok(progress)
    }
}

impl Meshes {
    /// Upload every pending version in a single startup/asset submission.
    /// Use `stream_gpu` for a bounded frame or a mesh larger than staging.
    pub fn prepare_gpu(
        &mut self,
        runtime: &mut Runtime,
        pool: &mut GraphPool,
    ) -> Result<Option<FrameId>, DefaultResourceError> {
        let progress = self.batch.select(
            &self.registry,
            &mut self.assets,
            self.registry.live_mesh_handles(),
            u64::MAX,
        )?;
        Ok(self.submit_uploads(runtime, pool, progress)?.submission)
    }

    /// Prefer meshes in caller order, uploading at most `byte_budget` bytes in
    /// one submission. Unlisted meshes keep their current versions. The budget
    /// is rounded down to four-byte stream alignment. Zero requests a progress
    /// report. Partial buffers remain private; a superseding edit discards the
    /// partial version. On capacity failure collect completed work or adjust the
    /// selection/budget and retry. The accepted offset only advances on submit.
    pub fn stream_gpu(
        &mut self,
        runtime: &mut Runtime,
        pool: &mut GraphPool,
        selection: &[MeshHandle],
        byte_budget: u64,
    ) -> Result<MeshUploadProgress, DefaultResourceError> {
        let progress = self.batch.select(
            &self.registry,
            &mut self.assets,
            selection.iter().copied(),
            byte_budget,
        )?;
        self.submit_uploads(runtime, pool, progress)
    }

    fn submit_uploads(
        &mut self,
        runtime: &mut Runtime,
        pool: &mut GraphPool,
        mut progress: MeshUploadProgress,
    ) -> Result<MeshUploadProgress, DefaultResourceError> {
        if self.batch.transfers.is_empty() {
            return Ok(progress);
        }
        let mut graph = pool.begin()?;
        for transfer in &mut self.batch.transfers {
            let geometry = self
                .registry
                .geometry(transfer.mesh)
                .expect("registered mesh");
            let asset = self
                .assets
                .get_mut(&transfer.mesh)
                .expect("registered mesh");
            transfer.exports = asset
                .upload
                .append(&mut graph, runtime, geometry, transfer.end)?;
        }
        let sent = runtime.submit(&graph.compile()?)?;
        for transfer in &self.batch.transfers {
            let asset = self
                .assets
                .get_mut(&transfer.mesh)
                .expect("registered mesh");
            if let Some(handles) = transfer.exports {
                let buffers = handles.map(|h| sent.export_buffer(h).expect("accepted mesh export"));
                asset.retire_active(&mut self.retired);
                asset.active = Some(Resident {
                    layout: asset.layout.clone(),
                    revision: asset.revision,
                    buffers,
                });
                asset.upload = Default::default();
                progress.published_meshes += 1;
            } else {
                asset.upload.accepted_bytes = transfer.end;
            }
        }
        progress.submission = Some(sent.id());
        Ok(progress)
    }

    /// Unregister superseded or removed versions after submitting or discarding
    /// their CPU graphs. Accepted GPU work retains its own leases until completion.
    pub fn collect_gpu(&mut self, runtime: &mut Runtime) -> Result<(), ExecutionError> {
        self.delete_retired(runtime)?;
        self.registry.collect_retired_meshes();
        Ok(())
    }

    fn delete_retired(&mut self, runtime: &mut Runtime) -> Result<(), ExecutionError> {
        while let Some(version) = self.retired.last_mut() {
            for key in &mut version.keys {
                if let Some(key) = *key {
                    runtime.delete(key)?;
                }
                *key = None;
            }
            self.retired.pop();
        }
        Ok(())
    }

    /// Release device registrations at a maintenance boundary. Mesh handles and
    /// CPU assets survive, so they can be uploaded again, including on a new runtime.
    pub fn release_gpu(&mut self, runtime: &mut Runtime) -> Result<(), ExecutionError> {
        // Preserve the bound across retries even when deletion on an incorrect
        // runtime failed: dispose the previous batch before detaching this one.
        self.delete_retired(runtime)?;
        for asset in self.assets.values_mut() {
            asset.retire_upload(&mut self.retired);
            asset.retire_active(&mut self.retired);
        }
        self.collect_gpu(runtime)
    }
}

impl super::Upload {
    fn append(
        &mut self,
        graph: &mut Graph,
        runtime: &mut Runtime,
        geometry: &GeometryData,
        end: u64,
    ) -> Result<Option<[BufferHandle; 2]>, DefaultResourceError> {
        let sizes = [geometry.vertex_byte_len(), geometry.index_byte_len()];
        let mut exports = [None; 2];
        let complete = end == geometry.byte_len();
        let mut base = 0;
        for (n, ((name, pass, stream), size)) in [
            (
                "mesh vertices",
                "mesh vertex upload",
                GeometryStream::Vertices,
            ),
            ("mesh indices", "mesh index upload", GeometryStream::Indices),
        ]
        .into_iter()
        .zip(sizes)
        .enumerate()
        {
            let from = self.accepted_bytes.saturating_sub(base).min(size);
            let to = end.saturating_sub(base).min(size);
            base += size;
            if from == to && !complete {
                continue;
            }
            let key = match self.keys[n] {
                Some(key) => key,
                None => {
                    let key = runtime.register_buffer(name, BufferDesc { size })?;
                    self.keys[n] = Some(key);
                    key
                }
            };
            let target = graph.import_buffer(name, &runtime.refresh_buffer(key)?)?;
            if from < to {
                // These byte offsets are bounded by GeometryData's owned slices.
                geometry.upload_range(graph, pass, target, stream, from as usize..to as usize)?;
            }
            if complete {
                graph.export_buffer(target)?;
                exports[n] = Some(target);
            }
        }
        Ok(complete.then(|| exports.map(|h| h.expect("complete upload exports both streams"))))
    }
}
