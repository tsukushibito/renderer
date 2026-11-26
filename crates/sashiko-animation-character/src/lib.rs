//! Fixed-tick character control independent of mesh data and rendering.
mod locomotion;
pub use locomotion::{Gaits, Locomotion, Movement};
mod action;
pub use action::{
    Action, ActionClip, ActionSet, Character, Handle, Notification, Rejection, Slot, TickResult,
};
