use crate::Error;
use std::{fs, io, path::PathBuf, process::Command};

const VERSION: &str = include_str!("../tool-version");

fn git_dir(option: &str) -> Result<PathBuf, Error> {
    let result = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", option])
        .output()?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
    }
    Ok(PathBuf::from(
        std::str::from_utf8(&result.stdout)?.trim_end(),
    ))
}

/// Install one immutable executable per project tool version and repository.
pub(super) fn install() -> Result<(), Error> {
    let repo = git_dir("--show-toplevel")?;
    let version = VERSION.trim();
    if version.is_empty()
        || !version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return Err("invalid project cooker version".into());
    }
    let requested = fs::read_to_string(repo.join("tools/sashiko-asset-cook/tool-version"))?;
    if version != requested.trim() {
        return Err(format!(
            "this cooker is {version}, but this checkout requests {}",
            requested.trim()
        )
        .into());
    }
    crate::inputs::setup(&repo)?;
    let directory = git_dir("--git-common-dir")?
        .join("renderer-tools")
        .join(version);
    fs::create_dir_all(&directory)?;
    let target = directory.join(format!(
        "sashiko-asset-cook{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !target.try_exists()? {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).map_err(|e| format!("generating install path: {e}"))?;
        let temp = directory.join(format!(".install-{:016x}", u64::from_ne_bytes(nonce)));
        // Stage a private copy: future Cargo builds at the origin cannot change the pin.
        let result = (|| -> io::Result<()> {
            fs::copy(std::env::current_exe()?, &temp)?;
            fs::OpenOptions::new().write(true).open(&temp)?.sync_all()?;
            match fs::hard_link(&temp, &target) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                other => other,
            }
        })();
        let _ = fs::remove_file(temp);
        result?;
    }
    println!("Installed project cooker at {}", target.display());
    Ok(())
}
