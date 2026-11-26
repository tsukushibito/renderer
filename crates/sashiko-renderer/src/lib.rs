//! # Draw a frame
//!
//! Start with [`Renderer::headless`] for capture or the Windows window renderer
//! for display. Supply extracted objects to [`Frame::draw_scene`] and HUD
//! layers to [`Frame::draw_overlay`]. See training-yard's standalone loop.
//! [`scene::Scene`] is optional. Use [`scene::Scene::instance_mut`] for one
//! object and [`scene::Scene::update_transforms`] for atomic batch motion.
//!
//! # Integrate with a host graph
//!
//! [`pipeline::RendererSetup`] prepares shared assets and multiple cameras.
//! Use [`pipeline::SceneAssets::add_model`] at load or replacement time, then
//! [`model::ModelInstance::draws`] to extract each camera's selected LOD.
//! [`pipeline::SceneRenderer::begin_host_frame`] records into a host graph.
//! Publish its receipt with [`pipeline::SceneRenderer::finish_host_submission`].
//! See training-yard's host replay and the `two_views` and `mixed_post` examples.
//!
//! # Implement a drawing technique
//!
//! Implement [`pipeline::ViewPipeline`] to plug into the shared view driver.
//! [`pipeline::GeometryDrawInput`] describes mixed-material geometry and
//! [`pbr::PbrDrawInput`] supplies classified draws to individual PBR recipes.
//! [`passes`] and [`core`] expose graph operands for custom passes.
//!
//! # Choosing a PBR recipe
//!
//! Select the standard technique through [`pipeline::GeometryPipeline::new`] and reuse
//! the same frame API. Indexed draws are supported by each recipe. The table
//! shows the additional draw forms, MSAA and Program options.
//!
//! | Recipe | Samples | Opaque and masked | Shadow casters | Transparent | Programs |
//! | --- | --- | --- | --- | --- | --- |
//! | [`pbr_forward::prepare`] | 1×, 4× | Indexed, indirect, instanced | Indexed, instanced | Indexed, instanced | Built in |
//! | [`pbr_forward::prepare_with_programs`] | 1× | Indexed | Indexed | Indexed | Custom Forward ABI |
//! | [`pbr_forward_plus::prepare`] | 1×, 4× | Indexed, indirect, instanced | Indexed, instanced | Indexed, instanced | Built in |
//! | [`pbr_deferred::prepare`] | 1× | Indexed, indirect, instanced | Indexed, instanced | Indexed, instanced | Built in |
//!
//! Set [`pbr::SceneLimits::max_transforms`] to the required transform capacity.
//! Instanced draws may span multiple transform pages, and an opaque indirect group
//! can share one transform. Indexed draws
//! without transform operands keep their world-space vertex contract.
//! Transparent draws are supplied in back-to-front order. A 4× scene resolves
//! its color to a single-sample HDR image and has no scene depth output.
//! Set [`pbr::SceneLimits::max_local_lights`] for Forward, Forward+ or Deferred.
//! Forward+ culls lights by screen tile. Deferred shades its G-buffer before
//! compositing transparent draws.
//!
//! For HDR composition, add the scene and application passes to the same Graph
//! before adding the standard output stage. Drop or reset the frame Graph after
//! a recipe declaration fails, since a recipe can append more than one pass.

#![deny(unsafe_code)]

/// Shared graph, runtime and native interoperation APIs used by drawing passes.
pub use sashiko_renderer_core as core;

pub mod canvas;
mod frame;
pub use frame::*;
pub mod material;
pub mod mesh;
pub mod model;
pub mod passes;
pub mod pbr;
pub mod pbr_deferred;
pub mod pbr_forward;
pub mod pbr_forward_plus;
pub mod picking;
pub mod pipeline;
pub mod scene;
mod shader_binaries;
pub mod unlit_forward;

/// Recovery actions for capacity failures during asset preparation.
pub use sashiko_renderer_core::capacity::{CapacityRecovery, RuntimeCapacityError};

pub use sashiko_renderer_core::graph::{GraphCapacityError, GraphCapacityKind};

/// The capacity contract that prevented an asset change.
#[derive(Debug, Clone, Copy)]
pub enum AssetCapacity {
    Runtime(RuntimeCapacityError),
    Graph(GraphCapacityError),
}
impl AssetCapacity {
    pub fn recovery(self) -> CapacityRecovery {
        match self {
            Self::Runtime(details) => details.recovery,
            Self::Graph(details) => match details.kind {
                GraphCapacityKind::Slots
                | GraphCapacityKind::ImportRangeSlots
                | GraphCapacityKind::SnapshotInputs
                    if details.limit > 0
                        && details
                            .required
                            .is_some_and(|needed| needed > details.limit) =>
                {
                    CapacityRecovery::ReleaseOwners
                }
                _ => CapacityRecovery::Prepare,
            },
        }
    }
}

/// Asset-change failure, retaining backend diagnostics for error reporting.
#[derive(Debug)]
pub struct AssetPrepareError(Box<dyn std::error::Error>);
impl From<Box<dyn std::error::Error>> for AssetPrepareError {
    fn from(error: Box<dyn std::error::Error>) -> Self {
        Self(error)
    }
}
impl From<core::runtime::ExecutionError> for AssetPrepareError {
    fn from(error: core::runtime::ExecutionError) -> Self {
        Self(Box::new(error))
    }
}
impl From<core::runtime::TextureUploadError> for AssetPrepareError {
    fn from(error: core::runtime::TextureUploadError) -> Self {
        Self(Box::new(error))
    }
}
impl AssetPrepareError {
    fn causes(&self) -> impl Iterator<Item = &(dyn std::error::Error + 'static)> {
        std::iter::successors(Some(self.0.as_ref()), |error| error.source())
    }

    /// Native-independent cause when a runtime operation failed during the change.
    pub fn execution_kind(&self) -> Option<core::runtime::ExecutionErrorKind> {
        self.causes()
            .find_map(|error| error.downcast_ref::<core::runtime::ExecutionError>())
            .map(|error| error.kind())
    }

    pub fn capacity(&self) -> Option<AssetCapacity> {
        for error in self.causes() {
            if let Some(details) = error.downcast_ref::<RuntimeCapacityError>() {
                return Some(AssetCapacity::Runtime(*details));
            }
            if let Some(details) = error
                .downcast_ref::<sashiko_renderer_core::runtime::ExecutionError>()
                .and_then(|e| e.capacity())
            {
                return Some(AssetCapacity::Runtime(details));
            }
            if let Some(details) = error
                .downcast_ref::<core::graph::GraphError>()
                .and_then(|e| e.capacity())
            {
                return Some(AssetCapacity::Graph(details));
            }
        }
        None
    }
}
impl std::fmt::Display for AssetPrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for AssetPrepareError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_transfer_keeps_graph_capacity_recovery() {
        let graph_error =
            core::graph::GraphError::capacity_error(GraphCapacityKind::Slots, 1, Some(2));
        let error = AssetPrepareError::from(core::runtime::TextureUploadError::Graph(graph_error));
        assert_eq!(
            error.capacity().unwrap().recovery(),
            CapacityRecovery::ReleaseOwners
        );
        assert_eq!(error.execution_kind(), None);
    }

    #[test]
    fn pending_window_frame_preserves_poll_recovery() {
        let details = RuntimeCapacityError::new(
            core::capacity::RuntimeCapacityKind::PresentationSlots,
            CapacityRecovery::Poll,
            Some(1),
            Some(2),
        );
        let error = AssetPrepareError(Box::new(details));
        assert!(
            matches!(error.capacity(), Some(AssetCapacity::Runtime(found)) if found == details)
        );
    }
}
