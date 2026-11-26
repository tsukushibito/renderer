//! Versioned, local-file distribution contract shared by the equipment cooker and game.
mod blob;
mod catalog;
#[cfg(feature = "renderer")]
mod renderer;
mod store;
#[cfg(feature = "renderer")]
pub use renderer::{MaterialBlob, Sampler};

pub use blob::{ImageBlob, MeshBlob, MeshVertex};
pub use catalog::{
    AssetId, BlobRef, Catalog, Digest, Equipment, Generation, MaterialSlot, Primitive,
};
#[cfg(feature = "metrics")]
pub use store::StoreStats;
pub use store::{ObjectBytes, ObjectStore};

use std::io;

fn invalid(message: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
