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
    git(a, &["init", "-q", "-b", "main"])?;
    git(a, &["config", "core.hooksPath", ".unused-hooks"])?;
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
fn repair_restores_only_corrupt_bytes_while_both_generations_are_leased() -> Result {
    let tmp = workspace()?;
    let a = &tmp.0;
    let b = a.join("b");
    let first = Preview::read(Preview::start(a)?)?;
    fs::copy(b.join("source/normal.png"), b.join("source/paint.png"))?;
    let edited = Preview::read(Preview::start(&b)?)?;
    let edited_pixels = edited.paint()?;
    let root = Path::new(first.data["root"].as_str().unwrap());
    let id = first.data["generation"].as_str().unwrap();
    let generation = Generation::open_id(root, id.parse()?)?;
    let image = generation.equipment(&AssetId::from_u128(1))?.primitives[0].images[3].unwrap();
    let path = root.join("objects").join(image.hash.to_string());
    let mut damaged = fs::read(&path)?;
    damaged[0] ^= 1;
    fs::write(path, damaged)?;
    assert!(first.paint().is_err());
    let report = management(a, &["repair", "source"])?;
    assert_eq!(report["generation"], id);
    assert_eq!(report["published_bytes"], image.size);
    assert_eq!(
        report["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["action"] == "built")
            .count(),
        1
    );
    assert_eq!(first.paint()?, [255, 255, 0, 255]);
    assert_eq!(edited.paint()?, edited_pixels);
    assert_eq!(management(a, &["repair", "source"])?["published_bytes"], 0);
    assert_eq!(management(a, &["gc", "source"])?["reclaimed_bytes"], 0);
    Ok(())
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

#[test]
fn merging_assets_keeps_the_last_selection_until_git_records_the_resolution() -> Result {
    let tmp = workspace()?;
    let a = &tmp.0;
    let b = a.join("b");
    for (worktree, image, new_id) in [(a, "cutout.png", 101), (&b, "normal.png", 102)] {
        let source = worktree.join("source");
        fs::copy(source.join(image), source.join("paint.png"))?;
        let sidecar = source.join("wide.gltf.meta.json");
        let mut settings: Value = serde_json::from_slice(&fs::read(&sidecar)?)?;
        settings["asset_id"] = Value::String(format!("{new_id:032x}"));
        fs::write(sidecar, serde_json::to_vec(&settings)?)?;
        git(worktree, &["add", "source"])?;
        git(
            worktree,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "edit equipment",
            ],
        )?;
    }
    let ours = Preview::read(Preview::start(a)?)?;
    let theirs = Preview::read(Preview::start(&b)?)?;
    let merge = Command::new("git")
        .current_dir(a)
        .args(["merge", "--no-edit", "edit"])
        .output()?;
    assert_eq!(merge.status.code(), Some(1));
    let conflict = Preview::read(Preview::start(a)?)?;
    assert_eq!(conflict.data["generation"], ours.data["generation"]);
    let diagnostic = conflict.data["diagnostic"].as_str().unwrap();
    for name in [
        "paint.png",
        "wide.gltf.meta.json",
        &AssetId::from_u128(1).to_string(),
    ] {
        assert!(diagnostic.contains(name), "{diagnostic}");
    }
    git(
        a,
        &[
            "checkout",
            "--theirs",
            "--",
            "source/paint.png",
            "source/wide.gltf.meta.json",
        ],
    )?;
    git(a, &["add", "source"])?;
    let resolved = Preview::read(Preview::start(a)?)?;
    assert!(resolved.data.get("diagnostic").is_none());
    assert_eq!(resolved.data["generation"], theirs.data["generation"]);
    assert_ne!(resolved.paint()?, ours.paint()?);
    Ok(())
}

#[test]
fn png_only_updates_match_full_cook_for_lfs_twins_and_buffer_inputs() -> Result {
    let project = workspace()?;
    let root = &project.0;
    let source = root.join("source");
    git(
        root,
        &["lfs", "install", "--local", "--skip-repo", "--skip-smudge"],
    )?;
    fs::write(
        root.join(".gitattributes"),
        "source/paint.png filter=lfs diff=lfs merge=lfs -text\nsource/geometry.png filter=lfs diff=lfs merge=lfs -text\n",
    )?;
    let lfs = root.join("test-lfs");
    git(root, &["config", "lfs.storage", lfs.to_str().unwrap()])?;
    fs::copy(source.join("paint.png"), source.join("twin.png"))?;
    let spare = source.join("spare.gltf");
    fs::write(
        &spare,
        fs::read_to_string(&spare)?.replace("paint.png", "twin.png"),
    )?;
    let wide = source.join("wide.gltf");
    fs::write(
        &wide,
        fs::read_to_string(&wide)?.replace("wide.bin", "geometry.png"),
    )?;
    fs::rename(source.join("wide.bin"), source.join("geometry.png"))?;
    git(root, &["add", ".gitattributes", "source"])?;
    git(root, &["add", "--renormalize", "source"])?;
    git(
        root,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "aliased image fixture",
        ],
    )?;
    let pointer = Command::new("git")
        .current_dir(root)
        .args(["show", "HEAD:source/paint.png"])
        .output()?;
    assert!(
        pointer
            .stdout
            .starts_with(b"version https://git-lfs.github.com/spec/v1")
    );

    let open = || -> Result<Value> {
        let view = Preview::read(Preview::start(root)?)?;
        assert!(view.data["diagnostic"].is_null(), "{}", view.data);
        assert_eq!(view.data["generation"], project.report()?["generation"]);
        Ok(view.data.clone())
    };
    let clean = open()?;
    // An off-line image edit must work even if the mesh input used by the
    // same equipment can no longer be obtained from LFS.
    let oid = renderer_asset::Digest::of(&fs::read(source.join("geometry.png"))?).to_string();
    let local_blob = lfs
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..4])
        .join(&oid);
    fs::rename(local_blob, root.join("withheld-mesh"))?;
    // The two paths initially refer to identical bytes. Changing just one
    // must leave the other's material binding as in an independent full cook.
    fs::copy(source.join("normal.png"), source.join("paint.png"))?;
    let edited = open()?;
    assert_ne!(edited["generation"], clean["generation"]);
    assert_eq!(edited["conversions"], 1);
    fs::copy(source.join("cutout.png"), source.join("twin.png"))?;
    assert_ne!(open()?["generation"], edited["generation"]);
    // A buffer may use a .png suffix. Its new positions require a mesh cook.
    let geometry = source.join("geometry.png");
    let mut bytes = fs::read(&geometry)?;
    bytes[..4].copy_from_slice(&0.375f32.to_le_bytes());
    fs::write(&geometry, bytes)?;
    assert_eq!(open()?["conversions"], 1);
    Ok(())
}
