use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

/// A private writable package for tests that change a catalog or an object.
pub struct Distribution {
    directory: PathBuf,
    pub root: PathBuf,
}

impl Distribution {
    pub fn cook() -> Result<Self, Box<dyn std::error::Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "renderer-yard-package-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        let root = directory.join("dist");
        let package = Self { directory, root };
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut command = Command::new(env!("CARGO"));
        command.current_dir(manifest).args([
            "run",
            "--quiet",
            "--locked",
            "-p",
            "renderer-asset-cook",
        ]);
        if !cfg!(debug_assertions) {
            command.arg("--release");
        }
        let output = command
            .args(["--", "cook"])
            .arg(manifest.join("assets/equipment/source"))
            .arg(&package.root)
            .arg("--cache")
            .arg(package.directory.join("cache"))
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(String::from_utf8_lossy(&output.stderr)).into());
        }
        Ok(package)
    }
}

impl Drop for Distribution {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
