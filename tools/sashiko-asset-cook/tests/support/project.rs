use serde_json::Value;
use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

pub(super) type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

pub(super) struct Project(pub(super) PathBuf);

impl Project {
    pub(super) fn new() -> Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("renderer-armor-{}-{id}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => break root,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        let project = Self(root);
        copy_directory(&fixture().join("source"), &project.0.join("source"))?;
        Ok(project)
    }

    pub(super) fn cook(&self) -> io::Result<Output> {
        Command::new(env!("CARGO_BIN_EXE_sashiko-asset-cook"))
            .args(["cook", "source", "dist", "--cache", "cache"])
            .current_dir(&self.0)
            .output()
    }

    pub(super) fn report(&self) -> Result<Value> {
        let output = self.cook()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/training-yard/assets/equipment")
}

fn copy_directory(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}
