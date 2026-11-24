use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

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
