//! Game runtime and authoring interfaces shared by Sashiko applications.
pub mod app;
pub use app::App;
#[cfg(all(windows, feature = "presentation"))]
pub mod window;
pub use renderer_asset as asset;
