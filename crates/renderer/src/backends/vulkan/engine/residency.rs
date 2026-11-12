//! Bounded logical mesh residency, prepared pins and explicit chunk submission.
use super::super::graph::RuntimeErrorKind;
use super::*;
use crate::graph::BufferDesc;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct MeshVersionKey {
    pub(super) mesh: MeshHandle,
    pub(super) revision: u64,
}
impl MeshVersionKey {
    pub(super) fn new(mesh: MeshHandle, revision: u64) -> Self {
        Self { mesh, revision }
    }
}
pub(super) struct ResidentBuffers {
    pub(super) vertices: ResourceKey,
    pub(super) indices: ResourceKey,
}
pub(super) struct ResidentMesh {
    pub(super) buffers: Option<Arc<ResidentBuffers>>,
    data: Option<Arc<MeshData>>,
    bytes: u64,
    uploaded: u64,
    pub(super) last_use: AtomicU64,
    pub(super) retiring: bool,
}
impl ResidentMesh {
    pub(super) fn ready(&self) -> bool {
        self.uploaded == self.bytes
    }
    fn pinned(&self) -> bool {
        self.buffers
            .as_ref()
            .is_some_and(|buffers| Arc::strong_count(buffers) > 1)
    }
}

enum UploadObservation<E> {
    Pending,
    Complete,
    Failed { error: E, pending: bool },
}

fn observe_queue<T, E>(
    queue: &mut Vec<T>,
    timeout: Duration,
    mut probe: impl FnMut(&mut T, Duration) -> UploadObservation<E>,
) -> Result<(), E> {
    let start = Instant::now();
    let mut index = 0;
    while index < queue.len() {
        match probe(&mut queue[index], timeout.saturating_sub(start.elapsed())) {
            UploadObservation::Pending => index += 1,
            UploadObservation::Complete => {
                queue.remove(index);
            }
            UploadObservation::Failed { error, pending } => {
                if !pending {
                    queue.remove(index);
                }
                return Err(error);
            }
        }
    }
    Ok(())
}

impl VulkanEngine {
    pub(super) fn next_use(&self) -> Result<u64, EngineRenderError> {
        self.clock
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map(|old| old + 1)
            .map_err(|_| EngineError::new("mesh use counter exhausted").into())
    }
    /// Admit one immutable working set and submit at most one upload chunk.
    /// Oversized sets fail before eviction; a different active set requires explicit cancellation.
    pub fn prepare_snapshot(
        &mut self,
        snapshot: &FrameSnapshot,
    ) -> Result<MeshUploadProgress, EngineRenderError> {
        let required = snapshot
            .draws
            .iter()
            .map(|draw| {
                (
                    MeshVersionKey::new(draw.mesh, draw.revision),
                    Arc::clone(&draw.data),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let required_bytes = required.values().try_fold(0u64, |total, data| {
            total
                .checked_add(data.vertices.len() as u64)
                .and_then(|v| v.checked_add(data.indices.len() as u64))
                .ok_or_else(|| EngineError::new("mesh working set byte size overflows"))
        })?;
        if required.len() > self.config.max_meshes || required_bytes > self.config.max_bytes {
            return Err(MeshPreparationError::WorkingSetTooLarge {
                required_meshes: required.len(),
                required_bytes,
                max_meshes: self.config.max_meshes,
                max_bytes: self.config.max_bytes,
            }
            .into());
        }
        let required_resources = required.len() * 2 + 2;
        if required_resources > self.runtime.resource_capacity() {
            return Err(MeshPreparationError::ResourceCapacityTooSmall {
                required_resources,
                capacity: self.runtime.resource_capacity(),
            }
            .into());
        }
        let keys = required.keys().copied().collect::<Vec<_>>();
        if self.remaining_bytes() != 0
            && self
                .active_upload
                .as_ref()
                .is_some_and(|active| *active != keys)
        {
            return Err(MeshPreparationError::UploadBusy.into());
        }
        let pinned = self
            .residents
            .iter()
            .filter(|(key, entry)| !required.contains_key(key) && entry.pinned());
        let (pinned_count, pinned_bytes) = pinned
            .fold((0usize, 0u64), |(count, bytes), (_, entry)| {
                (count + 1, bytes.saturating_add(entry.bytes))
            });
        // Mesh registrations must leave room for the standard color/depth
        // targets, even when logical cache limits are larger than the registry.
        let physical_mesh_limit = self.runtime.resource_capacity().saturating_sub(2) / 2;
        if pinned_count.saturating_add(required.len())
            > self.config.max_meshes.min(physical_mesh_limit)
            || pinned_bytes.saturating_add(required_bytes) > self.config.max_bytes
        {
            return Err(MeshPreparationError::ResidencyPinned.into());
        }
        let missing_count = required
            .keys()
            .filter(|key| !self.residents.contains_key(key))
            .count();
        let missing_bytes = required
            .iter()
            .filter(|(key, _)| !self.residents.contains_key(key))
            .map(|(_, data)| data.vertices.len() as u64 + data.indices.len() as u64)
            .sum::<u64>();
        let mut total_count = self.residents.len() + missing_count;
        let mut total_bytes = self
            .residents
            .values()
            .map(|entry| u128::from(entry.bytes))
            .sum::<u128>()
            + u128::from(missing_bytes);
        let mut candidates = self
            .residents
            .iter()
            .filter(|(key, entry)| !required.contains_key(key) && !entry.pinned())
            .map(|(key, entry)| (entry.last_use.load(Ordering::Relaxed), *key))
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        for (_, key) in candidates {
            let retiring = self.residents[&key].retiring;
            if !retiring
                && total_count <= self.config.max_meshes.min(physical_mesh_limit)
                && total_bytes <= u128::from(self.config.max_bytes)
            {
                continue;
            }
            let bytes = self.residents[&key].bytes;
            self.remove_resident(key)?;
            total_count -= 1;
            total_bytes -= u128::from(bytes);
            if !retiring {
                self.evictions += 1;
            }
        }
        let serial = self.next_use()?;
        for (key, data) in required {
            let bytes = data.vertices.len() as u64 + data.indices.len() as u64;
            let entry = self.residents.entry(key).or_insert_with(|| ResidentMesh {
                buffers: None,
                data: Some(data),
                bytes,
                uploaded: 0,
                last_use: AtomicU64::new(serial),
                retiring: false,
            });
            entry.retiring = false;
            entry.last_use.store(serial, Ordering::Relaxed);
        }
        self.active_upload = Some(keys);
        self.poll_uploads()
    }
    /// Poll only this loader's bounded submissions and advance at most one chunk.
    /// Application frame completions/readback results remain in the runtime queue.
    pub fn poll_uploads(&mut self) -> Result<MeshUploadProgress, EngineRenderError> {
        self.observe_uploads(Duration::ZERO)?;
        self.pump_chunk()
    }
    /// Explicit native wait using a shared timeout budget, then advance one chunk.
    pub fn wait_uploads(
        &mut self,
        timeout: Duration,
    ) -> Result<MeshUploadProgress, EngineRenderError> {
        self.observe_uploads(timeout)?;
        self.pump_chunk()
    }
    fn observe_uploads(&mut self, timeout: Duration) -> Result<(), EngineRenderError> {
        observe_queue(
            &mut self.pending_uploads,
            timeout,
            |submission, budget| match submission.wait(budget) {
                Ok(Some(_)) => UploadObservation::Complete,
                Ok(None) => UploadObservation::Pending,
                Err(error) => UploadObservation::Failed {
                    error,
                    pending: submission.is_pending(),
                },
            },
        )?;
        self.reap_retiring()
    }
    fn remaining_bytes(&self) -> u64 {
        self.active_upload
            .as_ref()
            .into_iter()
            .flatten()
            .filter_map(|key| self.residents.get(key))
            .filter(|entry| !entry.retiring)
            .map(|entry| entry.bytes - entry.uploaded)
            .sum()
    }
    fn progress(&self, submitted_bytes: u64) -> MeshUploadProgress {
        let remaining_bytes = self.remaining_bytes();
        MeshUploadProgress {
            state: if remaining_bytes == 0 {
                MeshUploadState::Ready
            } else if submitted_bytes == 0 {
                MeshUploadState::Backpressure
            } else {
                MeshUploadState::Submitted
            },
            submitted_bytes,
            remaining_bytes,
            pending_submissions: self.pending_uploads.len(),
        }
    }
    fn pump_chunk(&mut self) -> Result<MeshUploadProgress, EngineRenderError> {
        let Some(key) = self
            .active_upload
            .as_ref()
            .into_iter()
            .flatten()
            .copied()
            .find(|key| {
                self.residents
                    .get(key)
                    .is_some_and(|entry| !entry.ready() && !entry.retiring)
            })
        else {
            self.active_upload = None;
            return Ok(self.progress(0));
        };
        if self.pending_uploads.len() >= self.config.max_upload_submissions {
            return Ok(self.progress(0));
        }
        if self.residents[&key].buffers.is_none() {
            let data = self.residents[&key]
                .data
                .as_ref()
                .expect("pending upload data");
            let vertex_size = data.vertices.len() as u64;
            let index_size = data.indices.len() as u64;
            let vertices = match self
                .runtime
                .create_buffer("resident mesh vertices", BufferDesc { size: vertex_size })
            {
                Ok(buffer) => buffer,
                Err(error) if error.kind() == RuntimeErrorKind::CapacityExhausted => {
                    return Ok(self.progress(0));
                }
                Err(error) => return Err(error.into()),
            };
            let indices = match self
                .runtime
                .create_buffer("resident mesh indices", BufferDesc { size: index_size })
            {
                Ok(buffer) => buffer,
                Err(error) => {
                    self.runtime.delete(vertices.key())?;
                    if error.kind() == RuntimeErrorKind::CapacityExhausted {
                        return Ok(self.progress(0));
                    }
                    return Err(error.into());
                }
            };
            self.residents.get_mut(&key).unwrap().buffers = Some(Arc::new(ResidentBuffers {
                vertices: vertices.key(),
                indices: indices.key(),
            }));
        }
        let entry = &self.residents[&key];
        let data = entry.data.as_ref().expect("pending upload data");
        let buffers = entry.buffers.as_ref().unwrap();
        let vertex_bytes = data.vertices.len() as u64;
        let (buffer_key, offset, input) = if entry.uploaded < vertex_bytes {
            (buffers.vertices, entry.uploaded, &data.vertices)
        } else {
            (
                buffers.indices,
                entry.uploaded - vertex_bytes,
                &data.indices,
            )
        };
        let available = self.runtime.available_upload_bytes();
        let alignment = self.runtime.upload_alignment();
        let budget = available / alignment * alignment;
        let remaining = input.len() as u64 - offset;
        let mut size = remaining.min(budget);
        if size == 0 {
            return Ok(self.progress(0));
        }
        let submission = loop {
            let bytes = Arc::clone(input);
            let mut graph = Graph::new();
            let target = graph.import_buffer(
                "mesh upload target",
                &self.runtime.refresh_buffer(buffer_key)?,
            )?;
            graph.upload_buffer_slice(
                "mesh upload chunk",
                target,
                offset,
                bytes,
                offset as usize..(offset + size) as usize,
            )?;
            let plan = graph.compile()?;
            match self.runtime.submit(&plan) {
                Ok(submission) => break submission,
                Err(error) if error.kind() == RuntimeErrorKind::CapacityExhausted => {
                    if !error.is_staging_exhausted() {
                        return Ok(self.progress(0));
                    }
                    // Free bytes are advisory: other upload leases may fragment
                    // the arena. Retry smaller four-byte chunks, bounded by the
                    // bit width; at most one successful submission per call.
                    size = size / 2 / 4 * 4;
                    if size == 0 {
                        return Ok(self.progress(0));
                    }
                }
                Err(error) => return Err(error.into()),
            }
        };
        self.pending_uploads.push(submission);
        let entry = self.residents.get_mut(&key).unwrap();
        entry.uploaded += size;
        if entry.ready() {
            entry.data = None;
        }
        if self.remaining_bytes() == 0 {
            self.active_upload = None;
        }
        Ok(self.progress(size))
    }
    fn remove_resident(&mut self, key: MeshVersionKey) -> Result<(), EngineRenderError> {
        if let Some(entry) = self.residents.remove(&key)
            && let Some(buffers) = entry.buffers
        {
            self.runtime.delete(buffers.vertices)?;
            self.runtime.delete(buffers.indices)?;
        }
        Ok(())
    }
    pub(super) fn reap_retiring(&mut self) -> Result<(), EngineRenderError> {
        let keys = self
            .residents
            .iter()
            .filter(|(_, entry)| entry.retiring && !entry.pinned())
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        for key in keys {
            self.remove_resident(key)?;
        }
        Ok(())
    }
    /// Stop incomplete work; accepted GPU chunks retain native leases until retired.
    pub fn cancel_uploads(&mut self) -> Result<(), EngineRenderError> {
        if let Some(keys) = self.active_upload.take() {
            for key in keys {
                if let Some(entry) = self.residents.get_mut(&key)
                    && !entry.ready()
                {
                    entry.retiring = true;
                }
            }
        }
        self.reap_retiring()
    }
    /// Unregister GPU versions without deleting the CPU asset or invalidating prepared pins.
    pub fn unload_mesh(&mut self, mesh: MeshHandle) -> Result<usize, EngineRenderError> {
        let mut count = 0;
        for (key, entry) in &mut self.residents {
            if key.mesh == mesh {
                entry.retiring = true;
                count += 1;
            }
        }
        if let Some(keys) = &mut self.active_upload {
            keys.retain(|key| key.mesh != mesh);
        }
        self.reap_retiring()?;
        Ok(count)
    }
    pub fn unload_all_meshes(&mut self) -> Result<(), EngineRenderError> {
        self.active_upload = None;
        for entry in self.residents.values_mut() {
            entry.retiring = true;
        }
        self.reap_retiring()
    }
    pub fn mesh_cache_stats(&self) -> MeshCacheStats {
        MeshCacheStats {
            resident_meshes: self.residents.len(),
            resident_logical_bytes: self.residents.values().map(|entry| entry.bytes).sum(),
            ready_meshes: self
                .residents
                .values()
                .filter(|entry| entry.ready() && !entry.retiring)
                .count(),
            queued_meshes: self
                .residents
                .values()
                .filter(|entry| !entry.ready() && !entry.retiring)
                .count(),
            prepared_retained_meshes: self
                .residents
                .values()
                .filter(|entry| entry.retiring && entry.pinned())
                .count(),
            pending_upload_submissions: self.pending_uploads.len(),
            evictions: self.evictions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_retry_distinguishes_staging_from_execution_backpressure() {
        use crate::backends::vulkan::transfer::{TransferError, staging::StagingError};
        let staging = RuntimeError::from(StagingError::CapacityExhausted {
            capacity: 1024,
            requested: 512,
            reserved: 512,
        });
        let execution = RuntimeError::from(TransferError::CapacityExhausted { capacity: 1 });
        assert_eq!(staging.kind(), execution.kind());
        assert!(staging.is_staging_exhausted());
        assert!(!execution.is_staging_exhausted());
    }

    #[test]
    fn retired_upload_error_is_returned_once_without_blocking_remaining_work() {
        let mut queue = vec!["retired query failure", "next upload"];
        let error = observe_queue(&mut queue, Duration::ZERO, |work, _| {
            if *work == "retired query failure" {
                UploadObservation::Failed {
                    error: "query result OOM",
                    pending: false,
                }
            } else {
                UploadObservation::Complete
            }
        })
        .unwrap_err();
        assert_eq!(error, "query result OOM");
        assert_eq!(queue, ["next upload"]);
        observe_queue::<_, ()>(&mut queue, Duration::ZERO, |_, _| {
            UploadObservation::Complete
        })
        .unwrap();
        assert!(queue.is_empty());
    }

    #[test]
    fn unresolved_upload_failure_and_timeout_retain_ownership() {
        let mut queue = vec!["unproven upload"];
        assert_eq!(
            observe_queue(&mut queue, Duration::ZERO, |_, _| {
                UploadObservation::Failed {
                    error: "Fence error",
                    pending: true,
                }
            }),
            Err("Fence error")
        );
        assert_eq!(queue, ["unproven upload"]);
        observe_queue::<_, ()>(&mut queue, Duration::ZERO, |_, _| {
            UploadObservation::Pending
        })
        .unwrap();
        assert_eq!(queue, ["unproven upload"]);
    }
}
