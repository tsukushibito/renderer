//! Bistro applications for comparing explicit Graph composition and PBR recipes.

#[cfg(feature = "bake-tool")]
mod bake;
#[cfg(feature = "bake-tool")]
pub use bake::run as run_bake;
mod manual;
mod recipe;
mod shared;

pub use manual::main as run_manual;
pub use recipe::main as run_recipe;

type Error = Box<dyn std::error::Error>;

#[cfg(all(feature = "landscape", not(target_arch = "wasm32")))]
mod landscape;
#[cfg(all(feature = "landscape", not(target_arch = "wasm32")))]
pub use landscape::run as run_landscape;
