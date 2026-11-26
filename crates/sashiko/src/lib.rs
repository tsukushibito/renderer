//! Game runtime and authoring interfaces shared by Sashiko applications.
pub mod app;
#[cfg(feature = "rendering")]
pub mod capture;
#[cfg(feature = "cli")]
pub mod cli;
mod config;
pub use config::EngineConfig;
pub mod input;
#[cfg(feature = "live-inspector")]
pub use sashiko_inspect as inspect;
#[cfg(feature = "animation")]
pub use sashiko_jobs as jobs;
pub use sashiko_math as math;
#[cfg(feature = "physics")]
pub use sashiko_physics as physics;
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
pub mod profiling;
pub use app::App;
#[cfg(all(windows, feature = "presentation"))]
pub mod window;
#[cfg(feature = "asset")]
pub use sashiko_asset as asset;

#[cfg(feature = "animation")]
pub mod animation;
#[cfg(feature = "audio")]
pub mod audio;
pub mod authoring;
pub mod effects;
pub mod save;
pub mod scene;
pub mod space;
#[cfg(feature = "rendering")]
pub mod ui;
#[cfg(feature = "rendering")]
pub mod view;
