#[path = "support/project.rs"]
mod project;

use project::{Project, Result};
use renderer_asset::{AssetId, Generation, ObjectStore};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
};

const COOKER: &str = env!("CARGO_BIN_EXE_renderer-asset-cook");

fn git(repo: &Path, args: &[&str]) -> Result {
    let out = Command::new("git").current_dir(repo).args(args).output()?;
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

fn workspace() -> Result<Project> {
    let tmp = Project::new()?;
    let a = &tmp.0;
    git(a, &["init", "-q"])?;
    git(a, &["config", "user.email", "test@example.invalid"])?;
    git(a, &["config", "user.name", "Test"])?;
    git(a, &["config", "core.autocrlf", "false"])?;
    git(a, &["add", "source"])?;
    git(
        a,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"],
    )?;
    git(a, &["worktree", "add", "-q", "-b", "edit", "b"])?;
    Ok(tmp)
}

struct Preview {
    child: Child,
    data: Value,
}

impl Preview {
    fn start(repo: &Path) -> Result<Child> {
        Ok(Command::new(COOKER)
            .current_dir(repo)
            .args(["dev", "source"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?)
    }

    fn read(mut child: Child) -> Result<Self> {
        let mut line = String::new();
        let count = BufReader::new(child.stdout.take().unwrap()).read_line(&mut line)?;
        assert_ne!(count, 0, "preview exited without returning its generation");
        Ok(Self {
            child,
            data: serde_json::from_str(&line)?,
        })
    }

    fn paint(&self) -> Result<Vec<u8>> {
        let root = self.data["root"].as_str().unwrap();
        let generation =
            Generation::open_id(root, self.data["generation"].as_str().unwrap().parse()?)?;
        let equipment = generation.equipment(&AssetId::from_u128(1))?;
        let paint = equipment.primitives[0].images[3].unwrap();
        Ok(ObjectStore::new(root)
            .load(&paint)?
            .image()?
            .pixels
            .to_vec())
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn management(repo: &Path, args: &[&str]) -> Result<Value> {
    let out = Command::new(COOKER).current_dir(repo).args(args).output()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(serde_json::from_slice(&out.stdout)?)
}

#[test]
fn linked_worktrees_share_cooks_keep_independent_readers_and_resume_local_edits() -> Result {
    let tmp = workspace()?;
    let (a, b) = (tmp.0.clone(), tmp.0.join("b"));
    let first = Preview::start(&a)?;
    let second = Preview::start(&b)?;
    let (first, second) = (Preview::read(first)?, Preview::read(second)?);
    assert_eq!(first.data["generation"], second.data["generation"]);
    assert_eq!(first.data["generation"], tmp.report()?["generation"]);
    assert_ne!(first.data["snapshot_hit"], second.data["snapshot_hit"]);
    let warm = if first.data["snapshot_hit"] == true {
        &first
    } else {
        &second
    };
    assert_eq!(warm.data["conversions"], 0);
    assert_eq!(warm.data["object_bytes_written"], 0);
    drop(second);

    fs::copy(b.join("source/normal.png"), b.join("source/paint.png"))?;
    let edit = Preview::read(Preview::start(&b)?)?;
    assert_eq!(edit.data["conversions"], 1);
    assert_ne!(edit.paint()?, first.paint()?);
    let new_pixels = edit.paint()?;
    assert_eq!(
        management(&b, &["release", "source"])?["reclaimable_bytes"],
        0
    );
    drop(edit);
    let estimate = management(&b, &["release", "source"])?["reclaimable_bytes"]
        .as_u64()
        .unwrap();
    assert!(estimate > 0);
    assert_eq!(
        management(&b, &["release", "source", "--apply"])?["reclaimable_bytes"],
        estimate
    );
    assert_eq!(
        management(&a, &["gc", "source"])?["reclaimed_bytes"],
        estimate
    );
    assert_eq!(first.paint()?, [255, 255, 0, 255]);
    let resumed = Preview::read(Preview::start(&b)?)?;
    assert_eq!(resumed.paint()?, new_pixels);

    fs::write(b.join("source/paint.png"), b"invalid")?;
    let failed = Preview::read(Preview::start(&b)?)?;
    assert!(failed.data["diagnostic"].is_string());
    assert_eq!(resumed.data["generation"], failed.data["generation"]);
    drop((resumed, failed));
    git(&a, &["worktree", "remove", "--force", "b"])?;
    assert!(
        management(&a, &["gc", "source"])?["reclaimed_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(first.paint()?, [255, 255, 0, 255]);
    Ok(())
}
