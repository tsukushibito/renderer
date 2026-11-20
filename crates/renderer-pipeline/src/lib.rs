//! Compose public Renderer Graphs with reusable drawing passes and recipes.
//!
//! Register their resource and Program requirements in
//! [`renderer::capacity::Preparation`].
//! Prepare resource and Program reservations, then append standard and
//! application-specific passes to the same Graph. Renderer owns native
//! preparation, scheduling and execution.

#![deny(unsafe_code)]

pub mod passes;
pub mod pbr;
pub mod pbr_deferred;
pub mod pbr_forward;
pub mod pbr_forward_plus;
#[path = "../shaders/packaged.rs"]
mod shader_binaries;
pub mod unlit_forward;
