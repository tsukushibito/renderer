//! Versioned, local-file distribution contract shared by the equipment cooker and game.
mod blob;
mod catalog;
mod store;

pub use blob::{ImageBlob, MaterialBlob, MeshBlob, Sampler};
pub use catalog::{AssetId, BlobRef, Catalog, Digest, Equipment, Generation, Primitive};
#[cfg(feature = "metrics")]
pub use store::StoreStats;
pub use store::{ObjectBytes, ObjectStore};

use std::io;

fn invalid(message: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
