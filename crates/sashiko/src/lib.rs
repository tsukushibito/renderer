//! Game runtime and authoring interfaces shared by Sashiko applications.
pub mod app;
pub mod capture;
pub mod input;
pub use app::App;
#[cfg(all(windows, feature = "presentation"))]
pub mod window;
pub use renderer_asset as asset;

pub mod animation;
pub mod audio;
pub mod authoring;
pub mod effects;
pub mod save;
pub mod scene;
pub mod space;
pub mod ui;
pub mod view;
