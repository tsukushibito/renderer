//! A repository-local store uses OS locks for transactions, in-flight builds and
//! reader lifetimes. A crashed process releases its locks without a timeout.
use crate::store::FileLock;
use crate::{image_update::Manifest, store};
use sashiko_asset::{BlobRef, Catalog, Digest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

const FORMAT: &[u8] = b"renderer-asset-store 1\n";
const LOCK: &str = "transaction.lock";
// Bound the number of metadata deletions paid by one foreground request while
// draining history accumulated before cleanup, or after many processes exited.
const FOREGROUND_PRUNE_LIMIT: usize = 8;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.into())
}

fn gate(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
}

fn partial(name: &str) -> bool {
    name.strip_prefix(".asset-")
        .and_then(|s| s.strip_suffix(".partial"))
        .and_then(|s| s.split_once('-'))
        .is_some_and(|(pid, sequence)| {
            pid.parse::<u32>().is_ok() && sequence.parse::<u64>().is_ok()
        })
}

fn path_id(path: &Path) -> Digest {
    Digest::of(path.as_os_str().as_encoded_bytes())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    version: u32,
    selected: Digest,
    recovery: Option<Digest>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    git_dir: PathBuf,
    namespace: Digest,
}

impl Registration {
    fn id(&self) -> Digest {
        combine(path_id(&self.git_dir), self.namespace)
    }
}

fn combine(location: Digest, namespace: Digest) -> Digest {
    Digest::of(format!("{location}{namespace}").as_bytes())
}

/// Each registration identifies a Git administrative directory and a project asset
/// namespace (source path and profile). Its independent selection file
/// resides inside that directory, so removing/recreating a worktree, even with
/// the same name, never resurrects the old selection.
pub(super) struct StoreGuard {
    root: PathBuf,
    store_id: Digest,
    namespace: Digest,
    _transaction: FileLock,
}

impl StoreGuard {
    pub(super) fn lock(root: &Path, namespace: Digest) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        let transaction = FileLock::exclusive(gate(&root.join(LOCK))?)?;
        let format = root.join("format");
        match store::read_if_present(&format)? {
            Some(data) if data == FORMAT => (),
            Some(_) => {
                return Err(invalid(format!(
                    "unsupported asset store format at {}; use its matching cooker",
                    root.display()
                )));
            }
            None => {
                for entry in fs::read_dir(&root)? {
                    let name = entry?.file_name();
                    if name != LOCK && !name.to_str().is_some_and(partial) {
                        return Err(invalid(format!(
                            "unversioned non-empty asset store at {}",
                            root.display()
                        )));
                    }
                }
                store::publish(&format, FORMAT)?;
            }
        }
        let guard = Self {
            store_id: path_id(&root),
            namespace,
            root,
            _transaction: transaction,
        };
        guard.prune_metadata(FOREGROUND_PRUNE_LIMIT)?;
        Ok(guard)
    }

    /// Acquire while holding the store transaction, then release the transaction
    /// during conversion. GC never waits for a builder while holding a transaction.
    pub(super) fn build(root: &Path) -> io::Result<FileLock> {
        let file = gate(&root.join("build.lock"))?;
        FileLock::shared(file)
    }

    fn idle_builds(&self) -> io::Result<FileLock> {
        let file = gate(&self.root.join("build.lock"))?;
        FileLock::try_exclusive(file)?.ok_or_else(|| {
            io::Error::new(
                ErrorKind::WouldBlock,
                "asset cook in progress; retry collection after it finishes",
            )
        })
    }

    fn pin_path(&self, git_dir: &Path, namespace: Digest) -> PathBuf {
        git_dir.join(format!(
            "renderer-asset-{}-v1.json",
            combine(self.store_id, namespace)
        ))
    }

    fn pin(&self, git_dir: &Path, namespace: Digest) -> io::Result<Option<Pin>> {
        let path = self.pin_path(git_dir, namespace);
        let Some(bytes) = store::read_if_present(&path)? else {
            return Ok(None);
        };
        let pin: Pin = serde_json::from_slice(&bytes).map_err(|e| {
            invalid(format!(
                "unrecognized retention record {}: {e}",
                path.display()
            ))
        })?;
        if pin.version != 1 {
            return Err(invalid(format!(
                "unsupported retention version {} at {}; use its matching cooker",
                pin.version,
                path.display()
            )));
        }
        Ok(Some(pin))
    }

    fn registrations(&self) -> io::Result<Vec<(Registration, Option<Pin>)>> {
        let mut registrations = Vec::new();
        for (id, path) in entries(&self.root.join("registrations"))? {
            let record: Registration = serde_json::from_slice(&fs::read(&path)?).map_err(|e| {
                invalid(format!(
                    "invalid worktree registration {}: {e}",
                    path.display()
                ))
            })?;
            if !record.git_dir.is_absolute() || record.id() != id {
                return Err(invalid(format!(
                    "invalid worktree path in {}",
                    path.display()
                )));
            }
            let pin = self.pin(&record.git_dir, record.namespace)?;
            registrations.push((record, pin));
        }
        Ok(registrations)
    }

    fn lease_files(&self) -> io::Result<Vec<(Digest, PathBuf)>> {
        let files = entries(&self.root.join("leases"))?;
        for (_, path) in &files {
            if fs::metadata(path)?.len() != 0 {
                return Err(invalid(format!(
                    "unknown lease format in {}; use its matching cooker",
                    path.display()
                )));
            }
        }
        Ok(files)
    }

    pub(super) fn selected(&self, git_dir: &Path) -> io::Result<Option<Digest>> {
        Ok(self
            .pin(&git_dir.canonicalize()?, self.namespace)?
            .map(|pin| pin.selected))
    }

    pub(super) fn select(&self, git_dir: &Path, generation: Digest) -> io::Result<()> {
        let git_dir = git_dir.canonicalize()?;
        if !self
            .root
            .join("catalogs")
            .join(generation.to_string())
            .try_exists()?
        {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                format!("missing asset catalog {generation}"),
            ));
        }
        let previous = self.pin(&git_dir, self.namespace)?;
        let record = Registration {
            git_dir,
            namespace: self.namespace,
        };
        let registration = self
            .root
            .join("registrations")
            .join(record.id().to_string());
        if !registration.try_exists()? {
            store::publish(&registration, &serde_json::to_vec(&record)?)?;
        }
        if previous
            .as_ref()
            .is_some_and(|pin| pin.selected == generation)
        {
            return Ok(());
        }
        let pin = Pin {
            version: 1,
            selected: generation,
            recovery: previous.map(|pin| pin.selected),
        };
        store::publish(
            &self.pin_path(&record.git_dir, record.namespace),
            &serde_json::to_vec(&pin)?,
        )
    }

    /// Keep the returned guard alive for all consumers of this catalog generation,
    /// including asynchronous reads after a worktree moves to a newer selection.
    pub(super) fn lease(&self, generation: Digest) -> io::Result<FileLock> {
        if !self
            .root
            .join("catalogs")
            .join(generation.to_string())
            .try_exists()?
        {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                format!("missing asset catalog {generation}"),
            ));
        }
        let directory = self.root.join("leases");
        fs::create_dir_all(&directory)?;
        let file = gate(&directory.join(generation.to_string()))?;
        FileLock::shared(file)
    }

    fn protected(&self, skip: Option<&Path>) -> io::Result<BTreeSet<Digest>> {
        let mut live = BTreeSet::new();
        for (record, pin) in self.registrations()? {
            if (Some(record.git_dir.as_path()) != skip || record.namespace != self.namespace)
                && let Some(pin) = pin
            {
                live.insert(pin.selected);
                live.extend(pin.recovery);
            }
        }
        for (generation, path) in self.lease_files()? {
            if FileLock::try_exclusive(gate(&path)?)?.is_none() {
                live.insert(generation);
            }
        }
        Ok(live)
    }

    fn objects(&self, catalogs: &BTreeSet<Digest>) -> io::Result<BTreeSet<Digest>> {
        let mut objects = BTreeSet::new();
        for id in catalogs {
            let bytes = fs::read(self.root.join("catalogs").join(id.to_string()))?;
            let catalog = Catalog::decode(&bytes, *id)
                .map_err(|error| invalid(format!("protected catalog {id}: {error}")))?;
            objects.extend(catalog.objects().map(|reference| reference.hash));
        }
        Ok(objects)
    }

    /// Logical file lengths in the same unit used by GC. Excludes garbage which
    /// could already be collected before this worktree released its selection.
    pub(super) fn estimate_release(&self, git_dir: &Path) -> io::Result<u64> {
        let _idle = self.idle_builds()?;
        self.release_bytes(&git_dir.canonicalize()?)
    }

    fn release_bytes(&self, git_dir: &Path) -> io::Result<u64> {
        let Some(pin) = self.pin(git_dir, self.namespace)? else {
            return Ok(0);
        };
        let other = self.protected(Some(git_dir))?;
        let candidate: BTreeSet<_> = [Some(pin.selected), pin.recovery]
            .into_iter()
            .flatten()
            .filter(|id| !other.contains(id))
            .collect();
        if candidate.is_empty() {
            return Ok(0);
        }
        let objects = self.objects(&candidate)?;
        let shared = self.objects(&other)?;
        let mut bytes = 0;
        for (directory, ids) in [
            ("objects", objects.difference(&shared).copied().collect()),
            ("catalogs", candidate),
        ] {
            for id in ids {
                bytes += fs::metadata(self.root.join(directory).join(id.to_string()))?.len();
            }
        }
        Ok(bytes)
    }

    pub(super) fn release(&self, git_dir: &Path) -> io::Result<u64> {
        let _idle = self.idle_builds()?;
        let git_dir = git_dir.canonicalize()?;
        let bytes = self.release_bytes(&git_dir)?;
        if self.pin(&git_dir, self.namespace)?.is_some() {
            fs::remove_file(self.pin_path(&git_dir, self.namespace))?;
        }
        Ok(bytes)
    }

    pub(super) fn gc(&self) -> io::Result<u64> {
        let _idle = self.idle_builds()?;
        let catalogs = self.protected(None)?;
        let objects = self.objects(&catalogs)?;
        let mut unused = Vec::new();
        for (directory, live) in [("catalogs", &catalogs), ("objects", &objects)] {
            for (id, path) in entries(&self.root.join(directory))? {
                if !live.contains(&id) {
                    unused.push(path);
                }
            }
        }
        self.clean_interrupted_builds()?;
        let mut bytes = 0;
        for path in unused {
            let size = fs::metadata(&path)?.len();
            fs::remove_file(path)?;
            bytes += size;
        }
        self.prune_weak_indexes(&catalogs, &objects)?;
        self.prune_metadata(usize::MAX)?;
        Ok(bytes)
    }

    // The caller has completed the payload sweep and exclusively owns the build
    // gate. A weak record surviving interruption is reconsidered on the next GC.
    fn prune_weak_indexes(
        &self,
        catalogs: &BTreeSet<Digest>,
        objects: &BTreeSet<Digest>,
    ) -> io::Result<()> {
        let snapshots = self.root.join("snapshots");
        for (_, path) in weak_entries(&snapshots)? {
            let bytes = fs::read(&path)?;
            if let Some(id) = std::str::from_utf8(&bytes)
                .ok()
                .and_then(|s| s.parse().ok())
                && !catalogs.contains(&id)
            {
                fs::remove_file(path)?;
            }
        }
        for (key, path) in weak_entries(&self.root.join("dependencies"))? {
            if snapshots.join(key.to_string()).try_exists()? {
                continue;
            }
            let bytes = fs::read(&path)?;
            if let Ok((hash, manifest)) = serde_json::from_slice::<(Digest, Manifest)>(&bytes)
                // No snapshot can select this index. Require the canonical v1
                // encoding so a future record with additional fields survives.
                && bytes == serde_json::to_vec(&(hash, manifest))?
            {
                fs::remove_file(path)?;
            }
        }
        for (_, path) in weak_entries(&self.root.join("recipes"))? {
            let bytes = fs::read(&path)?;
            if let Ok(reference) = serde_json::from_slice::<BlobRef>(&bytes)
                && !objects.contains(&reference.hash)
                // v1 stores the canonical BlobRef bytes. Keep any unknown fields.
                && bytes == serde_json::to_vec(&reference)?
            {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    /// A missing pin and an unlocked lease represent no holder. Validate both
    /// registries completely before the first unlink, so an unknown record
    /// prevents all changes. The transaction excludes new lease acquisitions.
    /// No catalog or object is swept until an explicit GC.
    fn prune_metadata(&self, limit: usize) -> io::Result<()> {
        let registrations = self.registrations()?;
        let leases = self.lease_files()?;
        for (record, _) in registrations
            .into_iter()
            .filter(|(_, pin)| pin.is_none())
            .take(limit)
        {
            fs::remove_file(
                self.root
                    .join("registrations")
                    .join(record.id().to_string()),
            )?;
        }
        let mut removed = 0;
        for (_, path) in leases {
            if removed == limit {
                break;
            }
            if FileLock::try_exclusive(gate(&path)?)?.is_some() {
                fs::remove_file(path)?;
                removed += 1;
            }
        }
        Ok(())
    }

    // Every writer creating a scratch tree or staging payload holds the build
    // gate. Once it is exclusively held, only abandoned intermediates remain.
    fn clean_interrupted_builds(&self) -> io::Result<()> {
        let scratch = self.root.join("scratch");
        for entry in read_directory(&scratch)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let known = name.to_str().is_some_and(|s| {
                s.len() == 32
                    && s.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            });
            if !known
                || !entry.file_type()?.is_dir()
                || path.canonicalize()?.parent() != Some(scratch.as_path())
            {
                return Err(invalid(format!(
                    "unexpected scratch directory {}",
                    path.display()
                )));
            }
            // remove_dir_all does not descend through symbolic links; check the
            // direct child and the scratch root separately before recursion.
            fs::remove_dir_all(path)?;
        }
        for dir in [
            "objects",
            "catalogs",
            "recipes",
            "snapshots",
            "dependencies",
            "registrations",
        ] {
            for entry in read_directory(&self.root.join(dir))? {
                let entry = entry?;
                if entry.file_type()?.is_file() && entry.file_name().to_str().is_some_and(partial) {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        Ok(())
    }
}

fn read_directory(directory: &Path) -> io::Result<impl Iterator<Item = io::Result<fs::DirEntry>>> {
    let contents = match fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if !metadata.is_dir() || directory.canonicalize()? != directory {
                return Err(invalid(format!(
                    "expected a local store directory at {}",
                    directory.display()
                )));
            }
            Some(fs::read_dir(directory)?)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    Ok(contents.into_iter().flatten())
}

// Unrecognized weak hints do not own data. Preserve them for their writer.
fn weak_entries(directory: &Path) -> io::Result<Vec<(Digest, PathBuf)>> {
    let mut files = Vec::new();
    for entry in read_directory(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && let Some(id) = entry.file_name().to_str().and_then(|s| s.parse().ok())
        {
            files.push((id, entry.path()));
        }
    }
    Ok(files)
}

/// Reject unfamiliar retention records before deletion. The publisher's private
/// staging files are never live references and are not part of object accounting.
fn entries(directory: &Path) -> io::Result<Vec<(Digest, PathBuf)>> {
    let mut files = Vec::new();
    for entry in read_directory(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 asset store record"))?;
        if !entry.file_type()?.is_file() {
            return Err(invalid(format!(
                "unexpected store entry {}",
                entry.path().display()
            )));
        }
        if partial(name) {
            continue;
        }
        let id = name.parse().map_err(|_| {
            invalid(format!(
                "unrecognized store record {}; use its matching cooker",
                entry.path().display()
            ))
        })?;
        files.push((id, entry.path()));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sashiko_asset::{AssetId, Equipment, Primitive};
    use std::collections::BTreeMap;

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> io::Result<Self> {
            let mut bytes = [0; 16];
            getrandom::fill(&mut bytes).map_err(io::Error::other)?;
            let dir = std::env::temp_dir().join(format!(
                "renderer-retention-{:032x}",
                u128::from_le_bytes(bytes)
            ));
            fs::create_dir(&dir)?;
            Ok(Self(dir))
        }
        fn git_dir(&self, name: &str) -> io::Result<PathBuf> {
            let path = self.0.join(name);
            fs::create_dir_all(&path)?;
            Ok(path)
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn publish(root: &Path, color: &[u8]) -> io::Result<(Digest, u64)> {
        let shared = BlobRef::of(b"common");
        let image = BlobRef::of(color);
        store::publish(
            &root.join("objects").join(shared.hash.to_string()),
            b"common",
        )?;
        store::publish(&root.join("objects").join(image.hash.to_string()), color)?;
        let primitive = Primitive {
            mesh: shared,
            material: shared,
            images: [Some(image), None, None, None],
        };
        let catalog = Catalog {
            version: 1,
            assets: BTreeMap::from([(
                AssetId::from_u128(1),
                Equipment {
                    primitives: vec![primitive],
                },
            )]),
        };
        let bytes = serde_json::to_vec(&catalog)?;
        let id = Digest::of(&bytes);
        store::publish(&root.join("catalogs").join(id.to_string()), &bytes)?;
        Ok((id, bytes.len() as u64 + color.len() as u64))
    }

    #[test]
    fn release_estimate_excludes_shared_and_reading_generations() -> io::Result<()> {
        let tmp = Sandbox::new()?;
        let root = tmp.0.join("store");
        let a = tmp.git_dir("administration-a")?;
        let b = tmp.git_dir("administration-b")?;
        let store = StoreGuard::lock(&root, Digest::of(b"equipment"))?;
        let (red, _) = publish(&root, b"red")?;
        let (blue, blue_bytes) = publish(&root, b"blue")?;
        store.select(&a, red)?;
        store.select(&b, red)?;
        store.select(&b, blue)?;
        assert_eq!(store.estimate_release(&b)?, blue_bytes);
        let reader = store.lease(blue)?;
        assert_eq!(store.release(&b)?, 0);
        assert_eq!(store.gc()?, 0);
        drop(reader);
        assert_eq!(store.gc()?, blue_bytes);
        assert_eq!(store.selected(&a)?, Some(red));
        assert_eq!(store.selected(&b)?, None);
        assert!(
            root.join("objects")
                .join(BlobRef::of(b"common").hash.to_string())
                .exists()
        );
        Ok(())
    }

    #[test]
    fn unrelated_source_roots_in_one_worktree_keep_independent_selections() -> io::Result<()> {
        let tmp = Sandbox::new()?;
        let root = tmp.0.join("store");
        let git_dir = tmp.git_dir("administration")?;
        let armor = Digest::of(b"source/armor#profile");
        let weapons = Digest::of(b"source/weapons#profile");
        let store = StoreGuard::lock(&root, armor)?;
        let (red, red_bytes) = publish(&root, b"red")?;
        store.select(&git_dir, red)?;
        drop(store);

        let store = StoreGuard::lock(&root, weapons)?;
        assert_eq!(store.selected(&git_dir)?, None);
        assert_eq!(store.release(&git_dir)?, 0);
        let (blue, _) = publish(&root, b"blue")?;
        store.select(&git_dir, blue)?;
        drop(store);

        let store = StoreGuard::lock(&root, armor)?;
        assert_eq!(store.selected(&git_dir)?, Some(red));
        assert_eq!(store.release(&git_dir)?, red_bytes);
        assert_eq!(store.gc()?, red_bytes);
        drop(store);

        let store = StoreGuard::lock(&root, weapons)?;
        assert_eq!(store.selected(&git_dir)?, Some(blue));
        let still_live = store.estimate_release(&git_dir)?;
        fs::remove_dir_all(&git_dir)?;
        assert_eq!(store.gc()?, still_live);
        assert!(fs::read_dir(root.join("registrations"))?.next().is_none());
        Ok(())
    }

    #[test]
    fn ordinary_transactions_retire_history_but_preserve_live_readers_and_payloads()
    -> io::Result<()> {
        let tmp = Sandbox::new()?;
        let root = tmp.0.join("store");
        let namespace = Digest::of(b"equipment");
        let store = StoreGuard::lock(&root, namespace)?;
        let resident_tree = tmp.git_dir("resident")?;
        let (resident, _) = publish(&root, b"resident")?;
        store.select(&resident_tree, resident)?;
        let _reader = store.lease(resident)?;
        drop(store);
        for n in 0..20_u8 {
            let store = StoreGuard::lock(&root, namespace)?;
            assert_eq!(store.registrations()?.len(), 1);
            assert_eq!(store.lease_files()?.len(), 1);
            let tree = tmp.git_dir(&format!("abandoned-{n}"))?;
            let (generation, _) = publish(&root, &[n])?;
            store.select(&tree, generation)?;
            drop(store.lease(generation)?);
            drop(store);
            fs::remove_dir_all(tree)?;
        }
        let store = StoreGuard::lock(&root, namespace)?;
        assert_eq!(store.selected(&resident_tree)?, Some(resident));
        assert_eq!(fs::read_dir(root.join("catalogs"))?.count(), 21);
        assert_eq!(store.lease_files()?.len(), 1);
        assert_eq!(store.registrations()?.len(), 1);
        assert!(store.gc()? > 0);
        assert_eq!(fs::read_dir(root.join("catalogs"))?.count(), 1);
        Ok(())
    }

    #[test]
    fn unknown_retention_record_prevents_cleaning_known_abandoned_metadata() -> io::Result<()> {
        let tmp = Sandbox::new()?;
        let root = tmp.0.join("store");
        let namespace = Digest::of(b"equipment");
        let store = StoreGuard::lock(&root, namespace)?;
        let tree = tmp.git_dir("abandoned")?;
        let (id, _) = publish(&root, b"unused")?;
        store.select(&tree, id)?;
        drop(store.lease(id)?);
        drop(store);
        fs::remove_dir_all(tree)?;
        fs::write(root.join("leases").join("unknown"), b"unknown")?;
        assert!(StoreGuard::lock(&root, namespace).is_err());
        assert_eq!(fs::read_dir(root.join("registrations"))?.count(), 1);
        assert!(root.join("leases").join(id.to_string()).is_file());
        Ok(())
    }

    #[test]
    fn gc_sweeps_only_known_unreachable_weak_indexes_and_can_rebuild() -> io::Result<()> {
        let tmp = Sandbox::new()?;
        let root = tmp.0.join("store");
        let store = StoreGuard::lock(&root, Digest::of(b"equipment"))?;
        let a = tmp.git_dir("a")?;
        let b = tmp.git_dir("b")?;
        let (red, red_bytes) = publish(&root, b"red")?;
        let (blue, _) = publish(&root, b"blue")?;
        store.select(&a, red)?;
        store.select(&b, blue)?;
        let write_indexes = |color: &[u8], id: Digest| -> io::Result<_> {
            let key = Digest::of(color);
            let paths = ["snapshots", "dependencies", "recipes"]
                .map(|dir| root.join(dir).join(key.to_string()));
            let manifest = Manifest::default();
            let hash = Digest::of(&serde_json::to_vec(&(key, id, &manifest))?);
            store::publish(&paths[0], id.to_string().as_bytes())?;
            store::publish(&paths[1], &serde_json::to_vec(&(hash, manifest))?)?;
            store::publish(&paths[2], &serde_json::to_vec(&BlobRef::of(color))?)?;
            Ok(paths)
        };
        let retired = write_indexes(b"red", red)?;
        let retained = write_indexes(b"blue", blue)?;
        let unknown = ["snapshots", "dependencies", "recipes"]
            .map(|dir| root.join(dir).join(Digest::of(dir.as_bytes()).to_string()));
        for path in &unknown {
            store::publish(path, b"future format")?;
        }
        let staged = root.join("dependencies").join(".asset-99-1.partial");
        fs::write(&staged, b"partial map")?;
        assert_eq!(store.release(&a)?, red_bytes);
        assert_eq!(store.gc()?, red_bytes);
        assert!(!staged.exists());
        assert!(retired.iter().all(|p| !p.exists()));
        assert!(retained.iter().chain(&unknown).all(|p| p.exists()));
        assert_eq!(publish(&root, b"red")?.0, red);
        write_indexes(b"red", red)?;
        store.select(&a, red)?;
        assert_eq!(store.gc()?, 0);
        assert!(retired.iter().all(|p| p.exists()));
        Ok(())
    }

    #[test]
    fn recovery_and_recreated_worktrees_have_distinct_lifetimes() -> io::Result<()> {
        let tmp = Sandbox::new()?;
        let root = tmp.0.join("store");
        let a = tmp.git_dir("administration-a")?;
        let store = StoreGuard::lock(&root, Digest::of(b"equipment"))?;
        let (old, _) = publish(&root, b"old")?;
        let (new, _) = publish(&root, b"new")?;
        store.select(&a, old)?;
        store.select(&a, new)?;
        assert_eq!(store.gc()?, 0); // The previous generation is held for recovery.
        let predicted = store.estimate_release(&a)?;
        fs::remove_dir_all(&a)?;
        fs::create_dir(&a)?;
        assert_eq!(store.selected(&a)?, None);
        assert_eq!(store.gc()?, predicted);
        Ok(())
    }

    #[test]
    fn builders_protect_unpublished_outputs_and_unknown_records_block_mutation() -> io::Result<()> {
        let tmp = Sandbox::new()?;
        let root = tmp.0.join("store");
        let store = StoreGuard::lock(&root, Digest::of(b"equipment"))?;
        let builder = StoreGuard::build(&root)?;
        let scratch = root
            .join("scratch")
            .join("123456789012345678901234567890ab");
        fs::create_dir_all(&scratch)?;
        fs::write(scratch.join("input"), b"snapshot")?;
        let (new, _) = publish(&root, b"new")?;
        assert_eq!(store.gc().unwrap_err().kind(), ErrorKind::WouldBlock);
        assert!(scratch.exists());
        drop(builder);
        let a = tmp.git_dir("a")?;
        store.select(&a, new)?;
        assert_eq!(store.gc()?, 0);
        assert!(!scratch.exists());
        let pin_path = store.pin_path(&a, store.namespace);
        fs::write(
            &pin_path,
            format!(r#"{{"version":2,"selected":"{new}","recovery":null}}"#),
        )?;
        drop(store);
        assert!(StoreGuard::lock(&root, Digest::of(b"equipment")).is_err());
        fs::remove_file(pin_path)?;
        fs::create_dir_all(root.join("leases"))?;
        fs::write(root.join("leases").join("future-reader-set"), b"")?;
        assert!(StoreGuard::lock(&root, Digest::of(b"equipment")).is_err());
        assert!(root.join("catalogs").join(new.to_string()).exists());
        Ok(())
    }
}
