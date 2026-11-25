//! Window-independent camera controls and profiling boundary clocks.

#[cfg(windows)]
pub mod clock;
pub mod window_camera;
pub mod window_input;
