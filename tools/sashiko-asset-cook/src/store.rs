use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A single logical owner of an OS file lock. An unrelated child spawned by
/// another thread can inherit the open file description between fork and exec.
/// Closing only our descriptor would let that child extend the lock lifetime.
pub(super) struct FileLock(File);

impl FileLock {
    pub(super) fn exclusive(file: File) -> io::Result<Self> {
        file.lock()?;
        Ok(Self(file))
    }

    pub(super) fn shared(file: File) -> io::Result<Self> {
        file.lock_shared()?;
        Ok(Self(file))
    }

    pub(super) fn try_exclusive(file: File) -> io::Result<Option<Self>> {
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A persistent lock file whose operating-system lock is released on process exit.
pub(super) fn lock(path: &Path) -> io::Result<FileLock> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| io::Error::other("missing lock directory"))?,
    )?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    FileLock::exclusive(file)
}

/// All writes happen on the same filesystem as their destination. Closing before
/// publication also permits replacement on Windows. A reader sees complete bytes.
struct Staged(PathBuf);

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn stage(path: &Path, bytes: &[u8]) -> io::Result<Staged> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("missing output directory"))?;
    fs::create_dir_all(directory)?;
    loop {
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!(".asset-{}-{sequence}.partial", std::process::id());
        let temp = directory.join(name);
        let mut file = match File::create_new(&temp) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let staged = Staged(temp);
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        return Ok(staged);
    }
}

pub(super) fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let staged = stage(path, bytes)?;
    fs::rename(&staged.0, path)
}

/// Publish a new authoring ID once even when two importers start together.
pub(super) fn create_sidecar(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let staged = stage(path, bytes)?;
    match fs::hard_link(&staged.0, path) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        result => result,
    }
}

pub(super) fn read_if_present(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn unlocking_is_independent_of_inherited_descriptors() -> io::Result<()> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(io::Error::other)?;
        let path =
            std::env::temp_dir().join(format!("cook-lock-{:032x}", u128::from_le_bytes(bytes)));
        let first = File::create_new(&path)?;
        let second = File::open(&path)?;
        let writer = File::open(&path)?;
        fs::remove_file(path)?;
        // dup shares the same open file description as inheritance across fork.
        let _inherited = first.try_clone()?;
        let first = FileLock::shared(first)?;
        let second = FileLock::shared(second)?;
        drop(first);
        assert!(matches!(
            writer.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(second);
        assert!(FileLock::try_exclusive(writer)?.is_some());
        Ok(())
    }
}
