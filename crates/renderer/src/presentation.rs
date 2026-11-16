//! Backend-independent Windows presentation requests and outcomes.
//!
//! Enable `presentation` together with `vulkan` or `d3d12`. The application owns
//! its event loop; each native presenter retains the window it uses. Readiness
//! never implies a wait, and accepted presentation does not prove display time.

/// Explicit presentation capacity, independent of Graph execution capacity.
#[derive(Clone, Copy, Debug)]
pub struct PresentationConfig {
    /// Maximum simultaneously retained presentation submissions.
    pub max_frames: usize,
    /// Bound on retired swapchain generations where the backend retains them.
    /// A backend which resizes its current chain waits for readiness by returning
    /// `NotReady`, without creating an additional chain for the same window.
    pub max_retired_swapchains: usize,
    /// Desired image count, between 2 and 16. Surface limits can raise or cap the
    /// actual count; presentation slots and image indices remain independent.
    pub desired_images: u32,
    /// Require synchronized presentation when true, unsynchronized presentation
    /// when false. Unsupported requests fail without changing the active mode.
    /// VSync off may tear; compositor/driver behavior is outside this request.
    pub vsync: bool,
}

impl Default for PresentationConfig {
    fn default() -> Self {
        Self {
            max_frames: 2,
            max_retired_swapchains: 2,
            desired_images: 3,
            vsync: true,
        }
    }
}

impl PresentationConfig {
    pub(crate) fn has_valid_capacities(self) -> bool {
        self.max_frames != 0
            && self.max_frames <= u32::MAX as usize
            && self.max_retired_swapchains != 0
            && self.max_retired_swapchains <= u32::MAX as usize
            && (2..=16).contains(&self.desired_images)
    }
}

/// Outcome shared by both native presenters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentOutcome {
    /// The presentation request was accepted, without proving that its image
    /// reached the display or that GPU/presentation resources have retired.
    Presented { suboptimal: bool },
    /// An image, execution slot or presentation queue is temporarily unavailable.
    /// No implicit CPU/GPU wait was added. A previously accepted rendering submit
    /// may remain pending even when its presentation request was not accepted.
    NotReady,
    /// The swapchain was recreated/resized. Rebuild for `Presenter::extent()`.
    Recreated,
    /// Presentation needs a new swapchain extent. Call `prepare` before rebuilding;
    /// previously accepted GPU and presentation work may still remain pending.
    RecreateRequired,
    /// The requested window extent is zero; no acquisition is attempted.
    Suspended,
}

#[cfg(test)]
mod tests {
    use super::PresentationConfig;

    #[test]
    fn presentation_capacity_contract_is_backend_independent() {
        let config = PresentationConfig::default();
        assert!(config.has_valid_capacities());
        for desired_images in [0, 1, 17, u32::MAX] {
            assert!(
                !PresentationConfig {
                    desired_images,
                    ..config
                }
                .has_valid_capacities()
            );
        }
        for desired_images in [2, 16] {
            assert!(
                PresentationConfig {
                    desired_images,
                    ..config
                }
                .has_valid_capacities()
            );
        }
        assert!(
            !PresentationConfig {
                max_frames: 0,
                ..config
            }
            .has_valid_capacities()
        );
        assert!(
            !PresentationConfig {
                max_retired_swapchains: 0,
                ..config
            }
            .has_valid_capacities()
        );
    }
}
