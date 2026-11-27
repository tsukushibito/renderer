//! Retain accepted directional shadow maps across Graphs.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::{error::Error, fmt};

use sashiko_renderer_core::graph::{Graph, GraphError, ImageDesc, ImageHandle, ImportImage};
use sashiko_renderer_core::{
    graph::CompiledGraph,
    runtime::{ExecutionError, FrameSubmission, Runtime},
};

use super::{CASCADE_COUNT, Draw, PreparedView, ShadowSource, raster::SHADOW_IMAGES};

/// Identity of the content a shadow cascade would produce.
///
/// The revision must change when any input that affects caster depth or
/// coverage changes without changing the supplied matrix. These inputs can
/// include geometry, transforms, the selected caster set, alpha-tested
/// materials and textures, and the shadow Program or raster settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShadowKey {
    matrix: [[u32; 4]; 4],
    caster_revision: u64,
}

impl ShadowKey {
    /// Key for an application-defined cascade using exact float bits. Include
    /// changes in the clear or draw policy in `caster_revision`.
    pub fn new(matrix: [[f32; 4]; 4], caster_revision: u64) -> Self {
        Self {
            matrix: matrix.map(|row| row.map(f32::to_bits)),
            caster_revision,
        }
    }
}

struct Entry {
    key: Option<ShadowKey>,
    image: ImportImage,
}

/// Optional per-view history for four independent D32 cascade images.
///
/// Construct the history during preparation. Build, submit and accept one
/// transaction per history before the next build begins. Pending GPU work
/// can overlap. The runtime keeps imported images alive and orders reuse and
/// overwrites with normal Graph dependencies.
///
/// Allow one retained set of images in the runtime resource limits for each
/// history. Call `clear` to release their registrations on reset,
/// resolution change or removal while the runtime continues to be used.
pub type ShadowHistory = MapHistory<CASCADE_COUNT>;
pub struct MapHistory<const N: usize> {
    entries: [Option<Entry>; N],
    in_progress: Arc<AtomicBool>,
}

impl<const N: usize> Default for MapHistory<N> {
    fn default() -> Self {
        Self {
            entries: [const { None }; N],
            in_progress: Arc::default(),
        }
    }
}

struct FrameLease(Arc<AtomicBool>);

impl Drop for FrameLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
struct Cascade {
    target: ImageHandle,
    key: ShadowKey,
    reused: bool,
}

/// Cascade choices and render targets for the current Graph.
pub type ShadowFrame = MapFrame<CASCADE_COUNT>;
#[must_use = "use the cascade targets and export the rendered maps"]
pub struct MapFrame<const N: usize> {
    lease: FrameLease,
    cascades: [Cascade; N],
}

impl<const N: usize> MapFrame<N> {
    /// Partial atlas updates republish the scheduled contents even when its
    /// projection key matches. Per-tile owners decide which depths survive.
    pub(crate) fn mark_written(&mut self, index: usize) {
        self.cascades[index].reused = false;
    }
    /// Return the selected cascade image. Use a reused target only for reads.
    ///
    /// Panics if `cascade` is outside `0..CASCADE_COUNT`.
    pub fn target(&self, cascade: usize) -> ImageHandle {
        self.cascades[cascade].target
    }

    /// Whether the existing map supplies this cascade without drawing.
    ///
    /// Panics if `cascade` is outside `0..CASCADE_COUNT`.
    pub fn is_reused(&self, cascade: usize) -> bool {
        self.cascades[cascade].reused
    }

    /// Export the rendered maps from this Graph. Rendering must establish each
    /// missed target's complete contents for the selected key. Graph compile
    /// checks that the exports have initialized contents. If declaration
    /// fails, discard the partially built Graph.
    pub fn export(self, graph: &mut Graph) -> Result<MapUpdates<N>, GraphError> {
        graph.image_desc(self.cascades[0].target)?;
        for cascade in self.cascades.iter().filter(|cascade| !cascade.reused) {
            graph.export_image(cascade.target)?;
        }
        Ok(MapUpdates {
            graph_id: graph.graph_id(),
            lease: Some(self.lease),
            cascades: self.cascades,
        })
    }
}

impl ShadowFrame {
    /// Select this cascade for the standard PBR Forward recipe.
    ///
    /// Panics if `cascade` is outside `0..CASCADE_COUNT`.
    pub fn source<'a>(&self, cascade: usize, casters: &'a [Draw]) -> ShadowSource<'a> {
        let Cascade { target, reused, .. } = self.cascades[cascade];
        if reused {
            ShadowSource::Reuse(target)
        } else {
            ShadowSource::Render {
                target: Some(target),
                casters,
            }
        }
    }
}

/// History transaction for one Graph. Use `ShadowHistory::submit` to publish
/// its updates on acceptance. When submitting through the runtime directly,
/// submit the plan once and call `ShadowHistory::accept` on success before
/// presentation or other fallible operations. Dropping an unsubmitted
/// transaction releases its CPU lease. To release registered images after
/// submission, first publish the accepted updates, then clear the history.
pub type ShadowUpdates = MapUpdates<CASCADE_COUNT>;
#[must_use = "publish the history updates after successful submission"]
pub struct MapUpdates<const N: usize> {
    graph_id: u64,
    lease: Option<FrameLease>,
    cascades: [Cascade; N],
}

impl<const N: usize> MapUpdates<N> {
    /// Reuse status by cascade, for diagnostics or application bookkeeping.
    pub fn reused(&self) -> [bool; N] {
        self.cascades.map(|cascade| cascade.reused)
    }
}

/// Failure to publish or clear a shadow history.
#[derive(Debug)]
#[non_exhaustive]
pub enum ShadowHistoryError {
    FrameInProgress,
    ForeignUpdates,
    ForeignGraph,
    AlreadyPublished,
    Execution(ExecutionError),
}

impl fmt::Display for ShadowHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameInProgress => {
                formatter.write_str("shadow history already has an active frame")
            }
            Self::ForeignUpdates => formatter.write_str("shadow updates belong to another history"),
            Self::ForeignGraph => formatter.write_str("shadow updates belong to a different Graph"),
            Self::AlreadyPublished => formatter.write_str("shadow updates were already published"),
            Self::Execution(error) => error.fmt(formatter),
        }
    }
}

impl Error for ShadowHistoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Execution(error) => Some(error),
            _ => None,
        }
    }
}

impl<const N: usize> MapHistory<N> {
    /// Force the next accepted frame to redraw into the existing targets.
    pub fn invalidate(&mut self) -> Result<(), ShadowHistoryError> {
        if self.in_progress.load(Ordering::Acquire) {
            return Err(ShadowHistoryError::FrameInProgress);
        }
        for entry in self.entries.iter_mut().flatten() {
            entry.key = None;
        }
        Ok(())
    }
}

impl ShadowHistory {
    /// Select cached images for a prepared standard PBR view.
    pub fn begin_view(
        &self,
        graph: &mut Graph,
        view: &PreparedView,
        caster_revisions: [u64; CASCADE_COUNT],
    ) -> Result<ShadowFrame, GraphError> {
        let (Some(settings), Some(keys)) = (view.shadows, view.shadow_keys(caster_revisions))
        else {
            return Err(GraphError::invalid_input(
                "a view without shadows has no cascade history",
            ));
        };
        self.begin_frame(graph, settings.resolution, keys)
    }

    /// Import prior maps or declare new targets. A matching key skips drawing
    /// its cascade. A changed key redraws into the existing registration.
    /// Clear the history before changing shadow resolution or runtime.
    pub fn begin_frame(
        &self,
        graph: &mut Graph,
        shadow_resolution: u32,
        keys: [ShadowKey; CASCADE_COUNT],
    ) -> Result<ShadowFrame, GraphError> {
        let desc = ImageDesc::depth32(shadow_resolution, shadow_resolution);
        self.begin_maps(graph, desc, SHADOW_IMAGES, keys)
    }
}
impl<const N: usize> MapHistory<N> {
    pub(crate) fn require_idle(&self) -> Result<(), GraphError> {
        if self.in_progress.load(Ordering::Acquire) {
            return Err(GraphError::invalid_input(
                "shadow history already has an active frame",
            ));
        }
        Ok(())
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.entries[0].is_none()
    }
    pub(crate) fn begin_maps(
        &self,
        graph: &mut Graph,
        desc: ImageDesc,
        names: [&str; N],
        keys: [ShadowKey; N],
    ) -> Result<MapFrame<N>, GraphError> {
        if self
            .entries
            .iter()
            .flatten()
            .next()
            .is_some_and(|entry| entry.image.desc() != desc)
        {
            return Err(GraphError::invalid_input(
                "clear shadow history before changing resolution",
            ));
        }
        if self.in_progress.swap(true, Ordering::Acquire) {
            return Err(GraphError::invalid_input(
                "shadow history already has an active frame",
            ));
        }
        let lease = FrameLease(Arc::clone(&self.in_progress));
        let mut cascades = [None; N];
        for (index, (name, key)) in names.into_iter().zip(keys).enumerate() {
            let (target, reused) = match &self.entries[index] {
                Some(entry) => (
                    graph.import_image(name, &entry.image)?,
                    entry.key == Some(key),
                ),
                None => (graph.image(name, desc)?, false),
            };
            cascades[index] = Some(Cascade {
                target,
                key,
                reused,
            });
        }
        Ok(MapFrame {
            lease,
            cascades: cascades.map(Option::unwrap),
        })
    }

    /// Submit this history's compiled Graph and publish the scheduled maps
    /// before returning. A failed submit leaves the history and updates
    /// available for retry or cancellation via the usual runtime rules.
    pub fn submit(
        &mut self,
        runtime: &mut Runtime,
        graph: &CompiledGraph,
        updates: &mut MapUpdates<N>,
    ) -> Result<FrameSubmission, ShadowHistoryError> {
        self.check_updates(graph.graph_id(), updates)?;
        let submission = runtime
            .submit(graph)
            .map_err(ShadowHistoryError::Execution)?;
        self.publish(&submission, updates)?;
        Ok(submission)
    }

    /// Publish rendered maps after a successful separate runtime submission.
    /// Call immediately after submit succeeds, before building the next Graph
    /// or dropping this transaction. The accepted exports are already registered
    /// in the runtime. A rejected receipt leaves `updates` available to retry
    /// with the matching submission. To abandon accepted history, publish it
    /// and call [`Self::clear`] to release the registrations.
    pub fn accept(
        &mut self,
        submission: &FrameSubmission,
        updates: &mut MapUpdates<N>,
    ) -> Result<(), ShadowHistoryError> {
        self.check_updates(submission.graph_id(), updates)?;
        self.publish(submission, updates)
    }

    pub(crate) fn check_updates(
        &self,
        graph_id: u64,
        updates: &MapUpdates<N>,
    ) -> Result<(), ShadowHistoryError> {
        let lease = updates
            .lease
            .as_ref()
            .ok_or(ShadowHistoryError::AlreadyPublished)?;
        if !Arc::ptr_eq(&self.in_progress, &lease.0) {
            return Err(ShadowHistoryError::ForeignUpdates);
        }
        if graph_id != updates.graph_id {
            return Err(ShadowHistoryError::ForeignGraph);
        }
        Ok(())
    }

    fn publish(
        &mut self,
        submission: &FrameSubmission,
        updates: &mut MapUpdates<N>,
    ) -> Result<(), ShadowHistoryError> {
        let mut incoming = [const { None }; N];
        for (entry, cascade) in incoming.iter_mut().zip(updates.cascades) {
            if !cascade.reused {
                *entry = Some(Entry {
                    key: Some(cascade.key),
                    image: submission
                        .export_image(cascade.target)
                        .map_err(ShadowHistoryError::Execution)?,
                });
            }
        }
        for (entry, update) in self.entries.iter_mut().zip(incoming) {
            if update.is_some() {
                *entry = update;
            }
        }
        drop(updates.lease.take());
        Ok(())
    }

    /// Release this history's registered images without waiting. The runtime
    /// keeps GPU work alive until retirement. If deletion fails, entries still
    /// registered retain their ownership and can be retried.
    pub fn clear(&mut self, runtime: &mut Runtime) -> Result<(), ShadowHistoryError> {
        if self.in_progress.load(Ordering::Acquire) {
            return Err(ShadowHistoryError::FrameInProgress);
        }
        for entry in &mut self.entries {
            if let Some(old) = entry {
                runtime
                    .delete(old.image.key())
                    .map_err(ShadowHistoryError::Execution)?;
                *entry = None;
            }
        }
        Ok(())
    }
}
