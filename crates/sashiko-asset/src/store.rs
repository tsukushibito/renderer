use crate::{BlobRef, Digest, ImageBlob, MeshBlob, invalid};
#[cfg(feature = "metrics")]
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read},
    ops::Deref,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

type ReadObject = dyn Fn(&Path, u64) -> io::Result<Vec<u8>> + Send + Sync;

struct Shared {
    directory: PathBuf,
    reader: Box<ReadObject>,
    entries: Mutex<HashMap<Digest, Weak<ObjectBytes>>>,
    #[cfg(feature = "metrics")]
    metrics: Metrics,
}

/// Cheap to clone into independent I/O workers; shares live bytes and decoding
/// work by content across all catalog generations using the same object store.
#[derive(Clone)]
pub struct ObjectStore(Arc<Shared>);

fn read_file(path: &Path, size: u64) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if file.metadata()?.len() != size {
        return Err(invalid("file length differs from declared object size"));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size.try_into().map_err(invalid)?)
        .map_err(invalid)?;
    file.take(size.saturating_add(1)).read_to_end(&mut bytes)?;
    Ok(bytes)
}

impl ObjectStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self::with_reader(root, read_file)
    }

    pub fn with_reader(
        root: impl AsRef<Path>,
        reader: impl Fn(&Path, u64) -> io::Result<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(Shared {
            directory: root.as_ref().join("objects"),
            reader: Box::new(reader),
            entries: Mutex::default(),
            #[cfg(feature = "metrics")]
            metrics: Metrics::default(),
        }))
    }

    /// Synchronous worker operation. A concurrent request only waits for its own object.
    pub fn load(&self, reference: &BlobRef) -> io::Result<Arc<ObjectBytes>> {
        let owner = {
            let mut entries = self.0.entries.lock().unwrap();
            let entry = entries.entry(reference.hash).or_default();
            if let Some(owner) = entry.upgrade() {
                owner
            } else {
                let owner = Arc::new(ObjectBytes {
                    shared: Arc::downgrade(&self.0),
                    hash: reference.hash,
                    bytes: OnceLock::new(),
                    decoded: OnceLock::new(),
                });
                *entry = Arc::downgrade(&owner);
                owner
            }
        };
        let bytes = owner.bytes.get_or_init(|| {
            #[cfg(feature = "metrics")]
            let _read = self.0.metrics.begin(reference.size);
            let path = self.0.directory.join(reference.hash.to_string());
            let result = (self.0.reader)(&path, reference.size).and_then(|bytes| {
                if bytes.len() as u64 != reference.size || Digest::of(&bytes) != reference.hash {
                    return Err(invalid(format!(
                        "invalid object length or digest {}",
                        reference.hash
                    )));
                }
                #[cfg(feature = "metrics")]
                self.0.metrics.retain(bytes.len() as u64);
                Ok(bytes)
            });
            result.map_err(SharedError::from)
        });
        if bytes.as_ref().map_err(SharedError::to_io)?.len() as u64 != reference.size {
            return Err(invalid("conflicting size for shared object"));
        }
        Ok(owner)
    }

    #[cfg(feature = "metrics")]
    pub fn stats(&self) -> StoreStats {
        self.0.metrics.snapshot()
    }
}

struct SharedError(io::ErrorKind, String);
impl From<io::Error> for SharedError {
    fn from(value: io::Error) -> Self {
        Self(value.kind(), value.to_string())
    }
}
impl SharedError {
    fn to_io(&self) -> io::Error {
        io::Error::new(self.0, self.1.clone())
    }
}

enum Decoded {
    Mesh(MeshBlob),
    #[cfg(feature = "renderer")]
    Material(crate::MaterialBlob),
    Image(ImageBlob),
}
impl Decoded {
    #[cfg(feature = "metrics")]
    fn bytes(&self) -> u64 {
        match self {
            Self::Mesh(m) => (m.vertices.capacity() * 48 + m.indices.capacity() * 4) as u64,
            #[cfg(feature = "renderer")]
            Self::Material(m) => m.uniform.capacity() as u64,
            Self::Image(i) => i.pixels.capacity() as u64,
        }
    }
}

pub struct ObjectBytes {
    shared: Weak<Shared>,
    hash: Digest,
    bytes: OnceLock<Result<Vec<u8>, SharedError>>,
    decoded: OnceLock<Result<Decoded, SharedError>>,
}

impl ObjectBytes {
    fn decode(&self) -> io::Result<&Decoded> {
        self.decoded
            .get_or_init(|| {
                let result = if self.starts_with(crate::blob::MESH) {
                    MeshBlob::decode(self).map(Decoded::Mesh)
                } else if self.starts_with(crate::blob::IMAGE) {
                    ImageBlob::decode(self).map(Decoded::Image)
                } else {
                    #[cfg(feature = "renderer")]
                    {
                        crate::MaterialBlob::decode(self).map(Decoded::Material)
                    }
                    #[cfg(not(feature = "renderer"))]
                    {
                        Err(invalid("expected mesh or image object"))
                    }
                };
                #[cfg(feature = "metrics")]
                if let (Ok(value), Some(shared)) = (&result, self.shared.upgrade()) {
                    shared.metrics.retain(value.bytes());
                }
                result.map_err(SharedError::from)
            })
            .as_ref()
            .map_err(SharedError::to_io)
    }
    pub fn mesh(&self) -> io::Result<&MeshBlob> {
        match self.decode()? {
            Decoded::Mesh(m) => Ok(m),
            _ => Err(invalid("expected mesh object")),
        }
    }
    pub fn image(&self) -> io::Result<&ImageBlob> {
        match self.decode()? {
            Decoded::Image(i) => Ok(i),
            _ => Err(invalid("expected image object")),
        }
    }
    #[cfg(feature = "renderer")]
    pub fn material(&self) -> io::Result<&crate::MaterialBlob> {
        match self.decode()? {
            Decoded::Material(m) => Ok(m),
            _ => Err(invalid("expected material object")),
        }
    }
}

impl Deref for ObjectBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.bytes
            .get()
            .and_then(|result| result.as_ref().ok())
            .expect("published object bytes")
    }
}
impl AsRef<[u8]> for ObjectBytes {
    fn as_ref(&self) -> &[u8] {
        self
    }
}
impl Drop for ObjectBytes {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            #[cfg(feature = "metrics")]
            {
                let raw = self
                    .bytes
                    .get()
                    .and_then(|r| r.as_ref().ok())
                    .map_or(0, |b| b.len() as u64);
                let parsed = self
                    .decoded
                    .get()
                    .and_then(|r| r.as_ref().ok())
                    .map_or(0, Decoded::bytes);
                shared.metrics.live.fetch_sub(raw + parsed, Relaxed);
            }
            // A replacement may already have acquired the same key after this
            // owner's strong count reached zero. Never erase the replacement.
            let mut entries = shared.entries.lock().unwrap();
            if entries
                .get(&self.hash)
                .is_some_and(|v| std::ptr::eq(v.as_ptr(), self))
            {
                entries.remove(&self.hash);
            }
        }
    }
}

#[cfg(feature = "metrics")]
#[derive(Default)]
struct Metrics {
    reads: AtomicU64,
    bytes: AtomicU64,
    pending: AtomicU64,
    peak: AtomicU64,
    live: AtomicU64,
    peak_live: AtomicU64,
}
#[cfg(feature = "metrics")]
struct ReadScope<'a>(&'a Metrics, u64);
#[cfg(feature = "metrics")]
impl Drop for ReadScope<'_> {
    fn drop(&mut self) {
        self.0.pending.fetch_sub(self.1, Relaxed);
    }
}
#[cfg(feature = "metrics")]
impl Metrics {
    fn begin(&self, n: u64) -> ReadScope<'_> {
        self.reads.fetch_add(1, Relaxed);
        self.bytes.fetch_add(n, Relaxed);
        self.peak.fetch_max(
            self.pending.fetch_add(n, Relaxed).saturating_add(n),
            Relaxed,
        );
        ReadScope(self, n)
    }
    fn retain(&self, n: u64) {
        self.peak_live
            .fetch_max(self.live.fetch_add(n, Relaxed) + n, Relaxed);
    }
    fn snapshot(&self) -> StoreStats {
        StoreStats {
            reads: self.reads.load(Relaxed),
            bytes: self.bytes.load(Relaxed),
            peak_bytes: self.peak.load(Relaxed),
            live_bytes: self.live.load(Relaxed),
            peak_live_bytes: self.peak_live.load(Relaxed),
        }
    }
}
#[cfg(feature = "metrics")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreStats {
    pub reads: u64,
    /// Declared bytes of physical read requests, including failed reads.
    pub bytes: u64,
    /// Peak declared bytes of concurrent reads.
    pub peak_bytes: u64,
    /// Verified bytes and decoded heap payloads still owned by the application.
    pub live_bytes: u64,
    pub peak_live_bytes: u64,
}
