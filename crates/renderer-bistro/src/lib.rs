//! Bistro applications for comparing explicit Graph composition and PBR recipes.

mod manual;
mod recipe;
mod shared;

pub use manual::main as run_manual;
pub use recipe::main as run_recipe;

type Error = Box<dyn std::error::Error>;
