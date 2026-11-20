//! Scene data, shading inputs, and diagnostics shared by both Bistro applications.

#![cfg_attr(
    not(any(feature = "vulkan", all(windows, feature = "d3d12"))),
    allow(dead_code)
)]

use crate::Error;

pub(crate) mod asset;
pub(crate) mod camera;
pub(crate) mod deferred;
pub(crate) mod ibl;
pub(crate) mod instancing;
pub(crate) mod memory;
pub(crate) mod program;
pub(crate) mod shadow;
#[cfg(feature = "tracy")]
pub(crate) mod trace;
