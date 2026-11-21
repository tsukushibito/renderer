use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use super::{ExecutionError, ExecutionErrorKind, FrameSubmission, Runtime};
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
    #[cfg(all(
        windows,
        feature = "presentation",
        any(feature = "vulkan", feature = "d3d12")
    ))]
    pub(super) runtime_id: u64,
}

impl ManagedImage {
    /// Current contents at submission. Cloning the snapshot does not extend
    /// registration ownership. Keep this owner until all consumers are scheduled.
    pub fn snapshot(&self) -> &ImportImage {
        &self.image
    }

    /// Update the snapshot after a submission writes this registered image.
    pub fn refresh(&mut self, runtime: &Runtime) -> Result<(), ExecutionError> {
        self.image = runtime.refresh_image(self.image.key())?;
        Ok(())
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
    #[cfg(all(
        windows,
        feature = "presentation",
        any(feature = "vulkan", feature = "d3d12")
    ))]
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
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub(super) fn with_inner(
        inner: super::RuntimeInner,
        limits: super::RuntimeLimits,
        config: super::RuntimeConfig,
    ) -> Result<Self, ExecutionError> {
        let limit = limits.resource_capacity;
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
            limits,
            config,
            image_releases: Arc::new(ImageReleases {
                #[cfg(all(
                    windows,
                    feature = "presentation",
                    any(feature = "vulkan", feature = "d3d12")
                ))]
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
        let (submission, [image]) = self.submit_with_images(graph, [output])?;
        Ok((submission, image))
    }

    /// Submit once and acquire owners for distinct exported images in the given
    /// order. For example, batch albedo and normal maps declared with
    /// [`crate::graph::Graph::upload_texture_for_export`]. Each newly created
    /// registration is released independently when its owner is dropped.
    /// If the Graph selected a frame image, include it among these outputs.
    pub fn submit_with_images<const N: usize>(
        &mut self,
        graph: &CompiledGraph,
        outputs: [ImageHandle; N],
    ) -> Result<(FrameSubmission, [ManagedImage; N]), ExecutionError> {
        if graph
            .frame_image
            .is_some_and(|image| !outputs.contains(&image))
        {
            return Err(ExecutionError::invalid(
                "expected the selected frame image among the exported images",
            ));
        }
        self.collect_released_images()?;
        let mut tickets = [const { None }; N];
        for (index, output) in outputs.iter().enumerate() {
            if output.graph != graph.graph_id()
                || !graph.has_export(output.index)
                || !matches!(graph.resources[output.index].desc, ResourceDesc::Image(_))
                || outputs[..index].contains(output)
            {
                return Err(ExecutionError::invalid(
                    "expected distinct exported images from this graph",
                ));
            }
            if graph.resources[output.index].import.is_none() {
                tickets[index] = Some(self.image_releases.reserve()?);
            }
        }
        let receipt = self.submit_native(graph)?;
        // Native acceptance publishes all prevalidated exports together. Each
        // reserved ticket is returned automatically if any earlier step fails.
        let images = std::array::from_fn(|index| {
            let image = receipt
                .export_image(outputs[index])
                .expect("prevalidated accepted image export");
            let mut ticket = tickets[index].take();
            if let Some(ticket) = &mut ticket {
                ticket.key = Some(image.key());
            }
            ManagedImage {
                image,
                ticket,
                #[cfg(all(
                    windows,
                    feature = "presentation",
                    any(feature = "vulkan", feature = "d3d12")
                ))]
                runtime_id: self.image_releases.runtime_id,
            }
        });
        Ok((receipt, images))
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
