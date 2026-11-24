//! Locate the project-selected offline tool and retain its lease for every disk reader.
use renderer::pipeline::PipelineResult;
use renderer_asset::{Digest, Generation};
use serde::Deserialize;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

pub(super) struct AssetGeneration {
    pub catalog: Generation,
    _lease: Option<CookerLease>,
}

impl AssetGeneration {
    pub fn distribution(generation: Generation) -> Self {
        Self {
            catalog: generation,
            _lease: None,
        }
    }
}

struct CookerLease(Child);

impl Drop for CookerLease {
    fn drop(&mut self) {
        // EOF releases the OS-backed lease, including when the game exits abruptly.
        drop(self.0.stdin.take());
        // Windows discards the process record when its last handle is closed.
        // Unix retains child exit status until the parent collects it.
        #[cfg(unix)]
        let _ = self.0.wait();
    }
}

pub(super) fn packaged() -> PipelineResult<Option<PathBuf>> {
    if let Some(path) = std::env::var_os("TRAINING_YARD_ASSETS") {
        return Ok(Some(path.into()));
    }
    let path = std::env::current_exe()?
        .with_file_name("assets")
        .join("equipment/dist");
    Ok(path.is_dir().then_some(path))
}

pub(super) struct Project {
    pub source: PathBuf,
    pub tool: PathBuf,
    #[cfg(all(windows, feature = "presentation"))]
    pub index: PathBuf,
}

impl Project {
    pub fn discover() -> PipelineResult<Self> {
        let cwd = std::env::current_dir()?;
        let root = cwd.ancestors().find(|p| p.join(".git").exists())
            .ok_or("missing packaged assets/equipment/dist; for development start the game in its Git worktree")?;
        let source = root.join("crates/training-yard/assets/equipment/source");
        let pin = fs::read_to_string(root.join("crates/renderer-asset-cook/tool-version"))?;
        let pin = pin.trim();
        if pin.is_empty()
            || !pin
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err("invalid project cooker version".into());
        }
        let result = Command::new("git")
            .current_dir(root)
            .args([
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
                "--git-dir",
            ])
            .output()?;
        if !result.status.success() {
            return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
        }
        let paths = std::str::from_utf8(&result.stdout)?;
        let mut paths = paths.lines();
        let common = Path::new(paths.next().ok_or("missing Git common directory")?);
        let tool = std::env::var_os("TRAINING_YARD_COOKER")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                common.join("renderer-tools").join(pin).join(format!(
                    "renderer-asset-cook{}",
                    std::env::consts::EXE_SUFFIX
                ))
            });
        if !tool.is_file() {
            return Err(format!("missing project cooker {}. Run cargo run --locked --release -p renderer-asset-cook -- setup", tool.display()).into());
        }
        Ok(Self {
            source,
            tool,
            #[cfg(all(windows, feature = "presentation"))]
            index: Path::new(paths.next().ok_or("missing Git worktree directory")?).join("index"),
        })
    }

    pub fn open(&self) -> PipelineResult<(PathBuf, AssetGeneration)> {
        #[derive(Deserialize)]
        struct Selection {
            root: PathBuf,
            generation: Digest,
        }

        let mut lease = CookerLease(
            Command::new(&self.tool)
                .arg("dev")
                .arg(&self.source)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?,
        );
        let mut line = String::new();
        let stdout = lease.0.stdout.take().expect("piped cooker stdout");
        if BufReader::new(stdout).read_line(&mut line)? == 0 {
            return Err(format!(
                "project cooker returned no generation ({})",
                lease.0.wait()?
            )
            .into());
        }
        let selected: Selection = serde_json::from_str(&line)?;
        #[cfg(feature = "memory-profiling")]
        println!("asset-preview cook {}", line.trim_end());
        let generation = Generation::open_id(&selected.root, selected.generation)?;
        Ok((
            selected.root,
            AssetGeneration {
                catalog: generation,
                _lease: Some(lease),
            },
        ))
    }
}
