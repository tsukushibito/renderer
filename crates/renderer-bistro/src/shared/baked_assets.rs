//! A Bistro generation pins its surface index. The index pins immutable page and probe bytes.
use crate::Error;
use renderer::pbr::{SurfaceLightmapIndex, SurfaceLightmaps};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

const SCHEMA: &str = "bistro-surface-lighting-v1";

fn objects(manifest: &Path) -> PathBuf {
    manifest.with_file_name("lighting-objects")
}

pub(crate) fn object_name(hash: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut name = String::with_capacity(64);
    for byte in hash {
        write!(name, "{byte:02x}").expect("writing to a string is infallible");
    }
    name
}

fn parse_hash(hex: &str) -> Result<[u8; 32], Error> {
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid SHA-256 in lighting manifest".into());
    }
    let mut hash = [0; 32];
    for (i, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)?;
    }
    Ok(hash)
}

pub(crate) struct LightingGeneration {
    dir: PathBuf,
    identity: String,
    index: [u8; 32],
}

impl LightingGeneration {
    pub(crate) fn open(path: &Path) -> Result<Self, Error> {
        let bytes = fs::read(path)?;
        let doc: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| "unrecognized lighting manifest; recook the scene lighting")?;
        if doc["schema"].as_str() != Some(SCHEMA) {
            return Err("unsupported lighting manifest; recook the scene lighting".into());
        }
        let identity = doc["identity"]
            .as_str()
            .ok_or("missing lighting identity")?;
        parse_hash(identity)?;
        let index = parse_hash(
            doc["index_sha256"]
                .as_str()
                .ok_or("missing surface index")?,
        )?;
        Ok(Self {
            dir: objects(path),
            identity: identity.to_owned(),
            index,
        })
    }

    pub(crate) fn identity(&self) -> &[u8] {
        self.identity.as_bytes()
    }

    pub(crate) fn index_sha256(&self) -> String {
        object_name(&self.index)
    }

    pub(crate) fn load(&self) -> Result<SurfaceLightmaps, Error> {
        let read = |hash: &[u8; 32]| -> Result<Vec<u8>, Error> {
            let object = self.dir.join(object_name(hash));
            fs::read(&object)
                .map_err(|error| format!("reading {}: {error}", object.display()).into())
        };
        let data = read(&self.index)?;
        if <[u8; 32]>::from(Sha256::digest(&data)) != self.index {
            return Err("lighting index SHA-256 mismatch".into());
        }
        let index = SurfaceLightmapIndex::from_bytes(&data)?;
        let probes = read(index.probe_sha256())?;
        index.load(0..index.page_count(), Some(&probes), read)
    }
}

#[cfg(feature = "bake-tool")]
pub(crate) struct LightingPublisher {
    manifest: PathBuf,
    dir: PathBuf,
}

#[cfg(feature = "bake-tool")]
impl LightingPublisher {
    pub(crate) fn new(path: &Path) -> Result<Self, Error> {
        if path.exists() {
            LightingGeneration::open(path)?;
        }
        let dir = objects(path);
        fs::create_dir_all(&dir)?;
        Ok(Self {
            manifest: path.to_owned(),
            dir,
        })
    }

    /// All competing writers of this name have computed the same digest.
    pub(crate) fn store(&self, bytes: &[u8]) -> Result<[u8; 32], Error> {
        let hash: [u8; 32] = Sha256::digest(bytes).into();
        let path = self.dir.join(object_name(&hash));
        match fs::File::open(&path) {
            Ok(mut file) => {
                let mut digest = Sha256::new();
                std::io::copy(&mut file, &mut digest)?;
                if <[u8; 32]>::from(digest.finalize()) == hash {
                    return Ok(hash);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        atomic_write(&path, bytes)?;
        Ok(hash)
    }

    pub(crate) fn publish(
        self,
        identity: &str,
        index_hash: &[u8; 32],
        build: serde_json::Value,
    ) -> Result<(), Error> {
        let doc = serde_json::json!({
            "schema": SCHEMA,
            "identity": identity,
            "index_sha256": object_name(index_hash),
            "build": build,
        });
        atomic_write(&self.manifest, &serde_json::to_vec_pretty(&doc)?)
    }
}

/// Rename after closing the staged file so readers see one complete generation.
#[cfg(feature = "bake-tool")]
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::{
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (tmp, mut file) = loop {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".{}-{id}.partial", std::process::id()));
        let tmp = PathBuf::from(name);
        match fs::File::create_new(&tmp) {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let result = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(result?)
}

#[cfg(all(test, feature = "bake-tool"))]
mod tests {
    use super::*;

    #[test]
    fn staging_preserves_previous_generation_and_commits_whole_manifest() -> Result<(), Error> {
        let dir = std::env::temp_dir().join(format!(
            "bistro-lighting-generations-{}",
            std::process::id()
        ));
        fs::create_dir(&dir)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let manifest = dir.join("scene.gi");
        let bake = |order: &str| {
            crate::bake::run(
                [
                    "--fixture",
                    "--day-only",
                    "--samples",
                    "8",
                    "--max-depth",
                    "1",
                    "--backend",
                    "cpu",
                    "--probe-sh",
                    order,
                    "--output",
                ]
                .into_iter()
                .map(str::to_owned)
                .chain(std::iter::once(manifest.to_str().unwrap().to_owned())),
            )
        };
        bake("l1")?;
        let old = LightingGeneration::open(&manifest)?;
        assert!(!old.load()?.pages().is_empty());
        let old_probe_order = old.load()?.into_parts().2.unwrap().sh_order();
        let staging = LightingPublisher::new(&manifest)?;
        staging.store(b"incomplete cook")?;
        drop(staging);
        assert_eq!(LightingGeneration::open(&manifest)?.index, old.index);
        bake("l1")?;
        assert_eq!(LightingGeneration::open(&manifest)?.index, old.index);
        bake("l2")?;
        let new = LightingGeneration::open(&manifest)?;
        assert_ne!(new.index, old.index);
        assert_ne!(
            new.load()?.into_parts().2.unwrap().sh_order(),
            old_probe_order
        );
        assert_eq!(
            old.load()?.into_parts().2.unwrap().sh_order(),
            old_probe_order
        );
        fs::write(old.dir.join(object_name(&old.index)), b"mismatch")?;
        assert!(
            old.load()
                .err()
                .unwrap()
                .to_string()
                .contains("SHA-256 mismatch")
        );
        Ok(())
    }
}
