//! Embed the shader formats selected for this application build.

#[path = "../../../renderer-pipeline/shaders/packaged.rs"]
mod packaging;
pub(super) use packaging::packaged_shaders;
