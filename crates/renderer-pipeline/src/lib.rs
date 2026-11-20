//! Compose public Renderer Graphs with reusable drawing passes and recipes.
//!
//! Register their resource and Program requirements in
//! [`renderer::capacity::Preparation`].
//! Prepare resource and Program reservations, then append standard and
//! application-specific passes to the same Graph. Renderer owns native
//! preparation, scheduling and execution.

#![forbid(unsafe_code)]

pub mod passes;
pub mod unlit_forward;
