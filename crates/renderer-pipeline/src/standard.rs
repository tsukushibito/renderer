//! Optional drawing parts and recipes built with the ordinary public Graph API.
//!
//! Parts only declare work. The caller chooses exports, readbacks, submission and
//! presentation, and can compose these functions with its own Graph builders.

pub mod graphs;
pub mod passes;
pub mod snapshot;
