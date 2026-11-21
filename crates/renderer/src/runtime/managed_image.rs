use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use super::{ExecutionError, ExecutionErrorKind, FrameSubmission, Runtime, RuntimeInner};
use crate::{
    capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind},
    graph::{CompiledGraph, ImageHandle, ImportImage, ResourceDesc, ResourceKey},
};

/// Image snapshot returned with an accepted submission. Dropping its owner
/// queues a frame-created registration for release on the originating Runtime.
/// Imported output targets remain registered with their original owners.
/// GPU work retains separate native leases through completion.
pub struct ManagedImage {
    image: ImportImage,
    ticket: Option<ReleaseTicket>,
    #[cfg(all(windows, feature = "presentation"))]
    pub(super) runtime_id: u64,
}

impl ManagedImage {
    /// Current contents at submission. Cloning the snapshot does not extend
    /// registration ownership. Keep this owner until all consumers are scheduled.
    pub fn snapshot(&self) -> &ImportImage {
        &self.image
    }

    /// Hand registration lifetime back to the caller. A newly exported image
    /// must subsequently be released with Runtime::delete. An import keeps its
    /// existing owner.
    pub fn into_registered(mut self) -> ImportImage {
        if let Some(ticket) = &mut self.ticket {
            ticket.key = None;
        }
        self.image
    }
}

/// Reserve the release record *before* GPU acceptance. A dropped receipt can
/// always return its key without allocating, waiting on a fence or borrowing
/// the Runtime. This also bounds owners retained after explicit key deletion.
struct ReleaseTicket {
    releases: Arc<ImageReleases>,
    key: Option<ResourceKey>,
}

impl Drop for ReleaseTicket {
    fn drop(&mut self) {
        let mut state = self
            .releases
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(key) = self.key {
            state.pending.push(key);
            self.releases.ready.store(true, Ordering::Release);
        } else {
            state.active -= 1;
        }
    }
}

struct ReleaseState {
    active: usize,
    pending: Vec<ResourceKey>,
}

pub(super) struct ImageReleases {
    #[cfg(all(windows, feature = "presentation"))]
    pub(super) runtime_id: u64,
    limit: usize,
    ready: AtomicBool,
    state: Mutex<ReleaseState>,
}

impl ImageReleases {
    fn reserve(self: &Arc<Self>) -> Result<ReleaseTicket, ExecutionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.active == self.limit {
            return Err(ExecutionError::with_capacity(
                "release retained managed images before submitting another image",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Resources,
                    CapacityRecovery::ReleaseOwners,
                    Some(self.limit as u64),
                    (self.limit as u64).checked_add(1),
                ),
            ));
        }
        state.active += 1;
        Ok(ReleaseTicket {
            releases: Arc::clone(self),
            key: None,
        })
    }
}

impl Runtime {
    pub(super) fn with_inner(inner: RuntimeInner, limit: usize) -> Result<Self, ExecutionError> {
        let mut pending = Vec::new();
        pending.try_reserve_exact(limit).map_err(|_| {
            ExecutionError::with_capacity(
                "unable to reserve managed image release records",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::HostMemory,
                    CapacityRecovery::Reconfigure,
                    None,
                    Some(limit as u64),
                ),
            )
        })?;
        Ok(Self {
            inner,
            image_releases: Arc::new(ImageReleases {
                #[cfg(all(windows, feature = "presentation"))]
                runtime_id: crate::graph::next_id(),
                limit,
                ready: AtomicBool::new(false),
                state: Mutex::new(ReleaseState { active: 0, pending }),
            }),
        })
    }

    /// Submit a Graph with an exported image and capture its registration
    /// owner. Checks output identity and reserves an infallible Drop record
    /// before native submission. This operation never waits for GPU completion.
    pub fn submit_with_image(
        &mut self,
        graph: &CompiledGraph,
        output: ImageHandle,
    ) -> Result<(FrameSubmission, ManagedImage), ExecutionError> {
        if output.graph != graph.graph_id()
            || !graph.has_export(output.index)
            || !matches!(graph.resources[output.index].desc, ResourceDesc::Image(_))
            || graph.frame_image.is_some_and(|image| image != output)
        {
            return Err(ExecutionError::invalid(
                "expected this graph's exported frame image",
            ));
        }
        self.collect_released_images()?;
        let mut ticket = if graph.resources[output.index].import.is_none() {
            Some(self.image_releases.reserve()?)
        } else {
            None
        };
        let receipt = self.submit_native(graph)?;
        // The accepted backend receipt contains every export of the immutable
        // compiled Graph. ImageHandle is bound to its construction type/Graph.
        let image = receipt
            .export_image(output)
            .expect("prevalidated accepted image export");
        if let Some(ticket) = &mut ticket {
            ticket.key = Some(image.key());
        }
        Ok((
            receipt,
            ManagedImage {
                image,
                ticket,
                #[cfg(all(windows, feature = "presentation"))]
                runtime_id: self.image_releases.runtime_id,
            },
        ))
    }

    /// Unregister dropped frame images without waiting. Submission, frame
    /// polling and working-set preparation also collect automatically. Accepted
    /// work keeps each physical image until the GPU and presentation retire.
    pub fn collect_released_images(&mut self) -> Result<(), ExecutionError> {
        if !self.image_releases.ready.load(Ordering::Acquire) {
            return Ok(());
        }
        let releases = Arc::clone(&self.image_releases);
        loop {
            let key = {
                let mut state = releases.state.lock().unwrap_or_else(|e| e.into_inner());
                let Some(key) = state.pending.pop() else {
                    releases.ready.store(false, Ordering::Release);
                    return Ok(());
                };
                key
            };
            match self.delete(key) {
                Ok(()) => {}
                // The caller may have explicitly deleted this generation; the
                // registry always validates owner + generation before deletion.
                Err(e) if e.kind() == ExecutionErrorKind::StaleResource => {}
                Err(e) => {
                    releases
                        .state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .pending
                        .push(key);
                    return Err(e);
                }
            }
            releases
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active -= 1;
        }
    }
}
