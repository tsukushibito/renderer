//! Capture a Git asset subtree plus a content-checked overlay for local edits.
//! A clean cache lookup needs only the subtree ID and Git's scoped status.
use crate::{Error, gltf};
use renderer_asset::{AssetId, Digest};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio},
};

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).env("GIT_OPTIONAL_LOCKS", "0");
    cmd
}

fn success(output: Output) -> Result<Vec<u8>, Error> {
    if !output.status.success() {
        return Err(format!(
            "Git failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(output.stdout)
}

pub(super) fn setup(repo: &Path) -> Result<(), Error> {
    success(
        git(repo)
            .args(["lfs", "install", "--local", "--skip-repo", "--skip-smudge"])
            .output()?,
    )?;
    Ok(())
}

fn records(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|b| *b == 0).filter(|s| !s.is_empty())
}

fn relative(path: &str) -> Result<&Path, Error> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(format!("unexpected asset path {}", path.display()).into());
    }
    Ok(path)
}

fn normalized(source: &Path, dependency: &Path) -> Result<PathBuf, Error> {
    let mut path = source
        .parent()
        .ok_or("missing asset directory")?
        .to_path_buf();
    for component in dependency.components() {
        match component {
            Component::Normal(name) => path.push(name),
            Component::CurDir => (),
            Component::ParentDir if path.pop() => (),
            _ => {
                return Err(format!(
                    "{}: asset URI escapes source directory: {}",
                    source.display(),
                    dependency.display()
                )
                .into());
            }
        }
    }
    Ok(path)
}

pub(super) type Manifest = BTreeMap<PathBuf, BTreeSet<PathBuf>>;

#[derive(Debug)]
pub(super) struct Snapshot {
    pub(super) key: Digest,
    pub(super) clean_key: Digest,
    pub(super) changed_images: Option<BTreeSet<PathBuf>>,
    source: PathBuf,
    tree: Option<String>,
    // None records deletion. Only edited or untracked files have a digest here.
    local: BTreeMap<String, Option<Digest>>,
}

impl Snapshot {
    pub(super) fn capture(source: &Path) -> Result<Self, Error> {
        let source = source.canonicalize()?;
        let revision = git(&source)
            .args([
                "rev-parse",
                "--show-prefix",
                "--verify",
                "--quiet",
                "HEAD:./",
            ])
            .output()?;
        // rev-parse emits the source prefix first, including an empty line at
        // the repository root. A new source directory can have no HEAD tree.
        if !matches!(revision.status.code(), Some(0 | 1)) {
            return Err(format!(
                "resolving asset subtree: {}",
                String::from_utf8_lossy(&revision.stderr).trim()
            )
            .into());
        }
        let revision = std::str::from_utf8(&revision.stdout)?;
        let mut lines = revision.lines();
        let prefix = lines.next().ok_or("missing Git source prefix")?;
        let tree = lines.next().map(str::to_owned);
        let status = success(
            git(&source)
                .args([
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--no-renames",
                    "--untracked-files=all",
                    "--",
                    ".",
                ])
                .output()?,
        )?;
        let mut local = BTreeMap::new();
        let mut conflicts = BTreeSet::new();
        let mut changed_images = Some(BTreeSet::new());
        for record in records(&status) {
            let (xy, name) = record.split_at_checked(3).ok_or("invalid Git status")?;
            let name = std::str::from_utf8(name)?
                .strip_prefix(prefix)
                .ok_or("Git returned a path outside the input root")?;
            let path = relative(name)?;
            if xy.contains(&b'U') || matches!(xy, b"AA " | b"DD ") {
                conflicts.insert(path.to_owned());
                continue;
            }
            let file = source.join(path);
            let digest = match fs::symlink_metadata(&file) {
                Ok(meta) if meta.is_file() => {
                    if !file.canonicalize()?.starts_with(&source) {
                        return Err(format!("asset path escaped source: {name}").into());
                    }
                    Some(hash_reader(File::open(&file)?)?)
                }
                Ok(meta) if meta.is_dir() => None,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    None
                }
                Ok(_) => return Err(format!("unsupported asset file type: {name}").into()),
                Err(e) => return Err(e.into()),
            };
            if digest.is_some()
                && matches!(xy, b" M " | b"M  " | b"MM ")
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
            {
                if let Some(images) = &mut changed_images {
                    images.insert(path.to_owned());
                }
            } else {
                changed_images = None;
            }
            local.insert(name.to_owned(), digest);
        }
        if !conflicts.is_empty() {
            return Err(conflict_error(&source, &conflicts));
        }
        let snapshot_key = |overlay: &BTreeMap<String, Option<Digest>>| -> Result<Digest, Error> {
            Ok(Digest::of(&serde_json::to_vec(&(
                "git-input-v1",
                &tree,
                overlay,
            ))?))
        };
        let key = snapshot_key(&local)?;
        let clean_key = if local.is_empty() {
            key
        } else {
            snapshot_key(&BTreeMap::new())?
        };
        let changed_images = changed_images.filter(|set| !set.is_empty());
        Ok(Self {
            key,
            clean_key,
            changed_images,
            source,
            tree,
            local,
        })
    }

    /// Clean inputs come from the captured tree. Only actual scene dependencies
    /// are expanded in the caller's private scratch directory.
    pub(super) fn materialize(&self, destination: &Path) -> Result<Manifest, Error> {
        self.materialize_models(destination, None)
    }

    pub(super) fn materialize_subset(
        &self,
        destination: &Path,
        models: &BTreeSet<PathBuf>,
    ) -> Result<Manifest, Error> {
        for name in models {
            relative(name.to_str().ok_or("expected UTF-8 asset path")?)?;
            if !crate::is_gltf(name) {
                return Err(format!("invalid model path {}", name.display()).into());
            }
        }
        self.materialize_models(destination, Some(models))
    }

    fn materialize_models(
        &self,
        destination: &Path,
        models: Option<&BTreeSet<PathBuf>>,
    ) -> Result<Manifest, Error> {
        fs::create_dir_all(destination)?;
        if fs::read_dir(destination)?.next().is_some() {
            return Err("input snapshot needs an empty scratch directory".into());
        }
        let roots = models.map(|models| {
            models
                .iter()
                .flat_map(|p| [p.clone(), crate::sidecar(p)])
                .collect()
        });
        let mut files = self.files(roots.as_ref())?;
        let mut blobs = BlobReader::new(&self.source)?;
        for (name, input) in &files {
            if crate::is_gltf(name) || crate::sidecar_source(name).is_some() {
                self.copy_input(name, input, destination, &mut blobs)?;
            }
        }
        let all_models;
        let models = if let Some(models) = models {
            models
        } else {
            all_models = files
                .keys()
                .filter(|p| crate::is_gltf(p))
                .cloned()
                .collect();
            &all_models
        };
        let mut manifest = Manifest::new();
        let mut needed = BTreeMap::new();
        for name in models {
            let sidecar = crate::sidecar(name);
            let id = crate::read_sidecar(&destination.join(&sidecar))?.asset_id;
            let mut inputs = BTreeSet::from([name.clone(), sidecar]);
            let model = fs::read(destination.join(name))
                .map_err(|e| format!("asset {id} ({}): {e}", name.display()))?;
            for path in gltf::external_paths(&model)
                .map_err(|e| format!("asset {id} ({}): {e}", name.display()))?
            {
                let path = normalized(name, &path)?;
                needed.insert(path.clone(), id);
                inputs.insert(path);
            }
            manifest.insert(name.clone(), inputs);
        }
        if roots.is_some() {
            files = self.files(Some(&needed.keys().cloned().collect()))?;
        }
        for (name, id) in needed {
            let file = files
                .get(&name)
                .ok_or_else(|| format!("asset {id}: missing input {}", name.display()))?;
            if !destination.join(&name).try_exists()? {
                self.copy_input(&name, file, destination, &mut blobs)
                    .map_err(|e| format!("asset {id}: {e}"))?;
            }
        }
        Ok(manifest)
    }

    fn files(
        &self,
        selected: Option<&BTreeSet<PathBuf>>,
    ) -> Result<BTreeMap<PathBuf, Input>, Error> {
        let mut files = BTreeMap::new();
        if let Some(tree) = &self.tree {
            let mut list = |paths: &[&Path]| -> Result<(), Error> {
                let output = git(&self.source)
                    .args([
                        "--literal-pathspecs",
                        "ls-tree",
                        "-rz",
                        "--full-tree",
                        tree,
                        "--",
                    ])
                    .args(paths)
                    .output()?;
                let listing = success(output)?;
                for record in records(&listing) {
                    let (header, name) = std::str::from_utf8(record)?
                        .split_once('\t')
                        .ok_or("invalid Git tree entry")?;
                    let mut fields = header.split_whitespace();
                    let mode = fields.next();
                    let kind = fields.next();
                    let oid = fields.next().ok_or("missing Git blob ID")?;
                    if kind != Some("blob") || !matches!(mode, Some("100644" | "100755")) {
                        return Err(format!("unsupported Git entry in asset tree: {name}").into());
                    }
                    files.insert(relative(name)?.to_owned(), Input::Blob(oid.to_owned()));
                }
                Ok(())
            };
            if let Some(selected) = selected {
                // Bound each argument vector on platforms with small command-line limits.
                let mut paths = Vec::new();
                let mut length = 0;
                for path in selected {
                    relative(path.to_str().ok_or("expected UTF-8 asset path")?)?;
                    let cost = path.as_os_str().as_encoded_bytes().len() + 4;
                    if length + cost > 16000 && !paths.is_empty() {
                        list(&paths)?;
                        paths.clear();
                        length = 0;
                    }
                    paths.push(path.as_path());
                    length += cost;
                }
                if !paths.is_empty() {
                    list(&paths)?;
                }
            } else {
                list(&[])?;
            }
        }
        for (name, digest) in &self.local {
            let path = relative(name)?;
            if selected.is_some_and(|names| !names.contains(path)) {
                continue;
            }
            match digest {
                Some(hash) => {
                    files.insert(path.to_owned(), Input::Edited(*hash));
                }
                None => {
                    files.remove(path);
                }
            }
        }
        Ok(files)
    }

    fn copy_input(
        &self,
        name: &Path,
        input: &Input,
        destination: &Path,
        blobs: &mut BlobReader,
    ) -> Result<(), Error> {
        let target = destination.join(name);
        fs::create_dir_all(target.parent().ok_or("missing output parent")?)?;
        let output = File::create_new(&target)?;
        match input {
            Input::Blob(oid) => blobs.copy(oid, output)?,
            Input::Edited(expected) => {
                let from = self.source.join(name).canonicalize()?;
                if !from.starts_with(&self.source) {
                    return Err("edited input escaped source".into());
                }
                let mut input = File::open(from)?;
                let mut output = output;
                io::copy(&mut input, &mut output)?;
                drop(output);
                if hash_reader(File::open(&target)?)? != *expected {
                    return Err(format!("asset changed during capture: {}", name.display()).into());
                }
            }
        }
        // A Git LFS pointer is at most 1 KiB. Expand only inputs the importer reads.
        if target.metadata()?.len() <= 1024 {
            let pointer = fs::read(&target)?;
            if let Some((oid, size)) = lfs_pointer(&pointer)? {
                let mut child = git(&self.source)
                    .args(["lfs", "smudge", "--"])
                    .arg(name)
                    .env_remove("GIT_LFS_SKIP_SMUDGE")
                    .stdin(Stdio::piped())
                    .stdout(File::create(&target)?)
                    .stderr(Stdio::piped())
                    .spawn()?;
                child
                    .stdin
                    .take()
                    .ok_or("missing git-lfs stdin")?
                    .write_all(&pointer)?;
                success(child.wait_with_output()?)
                    .map_err(|e| format!("LFS object {oid} for {}: {e}", name.display()))?;
                if target.metadata()?.len() != size || hash_reader(File::open(&target)?)? != oid {
                    return Err(format!(
                        "LFS object {oid} for {} unavailable or invalid (expected {size} bytes)",
                        name.display()
                    )
                    .into());
                }
            }
        }
        Ok(())
    }
}

/// Keep a single Git process for all blobs in a cold import. The protocol
/// frames each response with a byte count, so payloads can be streamed.
struct BlobReader {
    child: Child,
    request: ChildStdin,
    response: BufReader<ChildStdout>,
}

impl BlobReader {
    fn new(source: &Path) -> Result<Self, Error> {
        let mut child = git(source)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        Ok(Self {
            request: child.stdin.take().ok_or("missing git cat-file stdin")?,
            response: BufReader::new(child.stdout.take().ok_or("missing git cat-file stdout")?),
            child,
        })
    }

    fn copy(&mut self, oid: &str, mut output: File) -> Result<(), Error> {
        writeln!(self.request, "{oid}")?;
        let mut header = String::new();
        self.response.read_line(&mut header)?;
        let mut fields = header.split_whitespace();
        if fields.next() != Some(oid) || fields.next() != Some("blob") {
            return Err(format!("invalid git cat-file response for {oid}: {header}").into());
        }
        let size: u64 = fields.next().ok_or("missing Git blob size")?.parse()?;
        if io::copy(&mut self.response.by_ref().take(size), &mut output)? != size {
            return Err(format!("incomplete Git blob {oid}").into());
        }
        let mut delimiter = [0];
        self.response.read_exact(&mut delimiter)?;
        if delimiter != *b"\n" {
            return Err("invalid Git blob delimiter".into());
        }
        Ok(())
    }
}

impl Drop for BlobReader {
    fn drop(&mut self) {
        // On a partial write Git can be blocked in stdout; terminate before
        // waiting so an I/O failure cannot strand a child or lock the scratch.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum Input {
    Blob(String),
    Edited(Digest),
}

fn hash_reader(mut reader: impl Read) -> Result<Digest, Error> {
    let mut hash = Sha256::new();
    io::copy(&mut reader, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()).parse()?)
}

fn lfs_pointer(bytes: &[u8]) -> Result<Option<(Digest, u64)>, Error> {
    const VERSION: &str = "version https://git-lfs.github.com/spec/v1";
    if !bytes.starts_with(VERSION.as_bytes()) {
        return Ok(None);
    }
    let text = std::str::from_utf8(bytes)?;
    if text.lines().next() != Some(VERSION) {
        return Ok(None);
    }
    let oid = text
        .lines()
        .find_map(|l| l.strip_prefix("oid sha256:"))
        .ok_or("missing LFS oid")?
        .parse()?;
    let size = text
        .lines()
        .find_map(|l| l.strip_prefix("size "))
        .ok_or("missing LFS size")?
        .parse()?;
    Ok(Some((oid, size)))
}

// Look at both index sides when a merge has left conflict markers in the
// working file. Extra diagnostics are collected only on the conflict path.
fn versions(source: &Path, name: &Path) -> Vec<Vec<u8>> {
    let mut values = Vec::new();
    let path = source.join(name);
    if path
        .canonicalize()
        .is_ok_and(|path| path.starts_with(source))
        && let Ok(bytes) = fs::read(&path)
    {
        values.push(bytes);
    }
    for stage in [2, 3] {
        let object = format!(":{stage}:./{}", name.to_string_lossy().replace('\\', "/"));
        if let Ok(output) = git(source).args(["show", &object]).output()
            && output.status.success()
            && !values.contains(&output.stdout)
        {
            values.push(output.stdout);
        }
    }
    values
}

fn conflict_error(source: &Path, conflicts: &BTreeSet<PathBuf>) -> Error {
    #[derive(Deserialize)]
    struct Id {
        asset_id: AssetId,
    }
    let mut messages: BTreeMap<_, BTreeSet<AssetId>> = conflicts
        .iter()
        .map(|p| (p.clone(), BTreeSet::new()))
        .collect();
    let listing = git(source)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            ".",
        ])
        .output();
    if let Ok(listing) = listing
        && listing.status.success()
    {
        let models: BTreeSet<_> = records(&listing.stdout)
            .filter_map(|b| std::str::from_utf8(b).ok().and_then(|s| relative(s).ok()))
            .filter(|p| crate::is_gltf(p))
            .collect();
        for model in models {
            let sidecar = crate::sidecar(model);
            let ids: BTreeSet<_> = versions(source, &sidecar)
                .iter()
                .filter_map(|b| serde_json::from_slice::<Id>(b).ok().map(|s| s.asset_id))
                .collect();
            let mut paths = vec![model.to_owned(), sidecar];
            for bytes in versions(source, model) {
                if let Ok(deps) = gltf::external_paths(&bytes) {
                    paths.extend(deps.iter().filter_map(|p| normalized(model, p).ok()));
                }
            }
            for p in paths {
                if let Some(affected) = messages.get_mut(&p) {
                    affected.extend(&ids);
                }
            }
        }
    }
    let mut message = String::from("unresolved Git asset conflicts");
    for (path, ids) in messages {
        message.push_str(&format!("\n  {}", source.join(path).display()));
        for id in ids {
            message.push_str(&format!(" {id}"));
        }
    }
    message.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Checkout(PathBuf);
    impl Checkout {
        fn new() -> Result<Self, Error> {
            let id = getrandom::u64().map_err(|e| format!("temporary repository ID: {e}"))?;
            let path = std::env::temp_dir().join(format!("renderer-asset-inputs-{id:x}"));
            fs::create_dir(&path)?;
            success(git(&path).args(["init", "-q", "-b", "main"]).output()?)?;
            for (key, value) in [
                ("user.name", "fixture"),
                ("user.email", "fixture@invalid"),
                ("commit.gpgSign", "false"),
                ("core.autocrlf", "false"),
                ("core.hooksPath", ".disabled-hooks"),
            ] {
                success(git(&path).args(["config", key, value]).output()?)?;
            }
            Ok(Self(path))
        }
        fn commit(&self) -> Result<(), Error> {
            success(git(&self.0).args(["add", "."]).output()?)?;
            success(git(&self.0).args(["commit", "-qm", "fixture"]).output()?)?;
            Ok(())
        }
    }
    impl Drop for Checkout {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn pin_git_tree_and_check_local_edits_before_materializing() -> Result<(), Error> {
        let repo = Checkout::new()?;
        let source = repo.0.join("equipment");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../training-yard/assets/equipment/source");
        fs::create_dir(&source)?;
        for entry in fs::read_dir(fixture)? {
            let entry = entry?;
            fs::copy(entry.path(), source.join(entry.file_name()))?;
        }
        repo.commit()?;
        let clean = Snapshot::capture(&source)?;
        let image = source.join("paint.png");
        fs::write(&image, b"edited")?;
        let edited = Snapshot::capture(&source)?;
        assert_ne!(edited.key, clean.key);
        assert_eq!(edited.clean_key, clean.key);
        assert_eq!(
            edited.changed_images,
            Some(BTreeSet::from([PathBuf::from("paint.png")]))
        );
        let subset = repo.0.join("subset");
        let narrow = PathBuf::from("narrow.gltf");
        let manifest = edited.materialize_subset(&subset, &BTreeSet::from([narrow.clone()]))?;
        assert_eq!(
            manifest[&narrow],
            [
                "narrow.gltf",
                "narrow.gltf.meta.json",
                "paint.png",
                "narrow.bin"
            ]
            .map(PathBuf::from)
            .into_iter()
            .collect()
        );
        assert!(!subset.join("wide.gltf").exists());
        assert_eq!(fs::read(subset.join("paint.png"))?, b"edited");
        fs::write(&image, b"later")?;
        let dest = repo.0.join("snapshot");
        assert!(
            edited
                .materialize(&dest)
                .unwrap_err()
                .to_string()
                .contains("changed during capture")
        );
        clean.materialize(&repo.0.join("clean"))?;
        assert_ne!(fs::read(repo.0.join("clean/paint.png"))?, b"later");
        fs::write(source.join("local.json"), b"new")?;
        let untracked = Snapshot::capture(&source)?;
        fs::remove_file(source.join("local.json"))?;
        assert_ne!(untracked.key, Snapshot::capture(&source)?.key);
        Ok(())
    }

    #[test]
    fn conflicted_shared_image_reports_dependent_ids_until_resolved() -> Result<(), Error> {
        let repo = Checkout::new()?;
        let source = repo.0.join("equipment");
        fs::create_dir(&source)?;
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../training-yard/assets/equipment/source");
        for entry in fs::read_dir(fixture)? {
            let entry = entry?;
            fs::copy(entry.path(), source.join(entry.file_name()))?;
        }
        let id = crate::read_sidecar(&source.join("narrow.gltf.meta.json"))?.asset_id;
        repo.commit()?;
        success(git(&repo.0).args(["checkout", "-qb", "a"]).output()?)?;
        fs::write(source.join("paint.png"), b"branch A")?;
        repo.commit()?;
        success(git(&repo.0).args(["checkout", "-q", "main"]).output()?)?;
        fs::write(source.join("paint.png"), b"branch B")?;
        repo.commit()?;
        assert!(
            !git(&repo.0)
                .args(["merge", "--no-edit", "a"])
                .output()?
                .status
                .success()
        );
        let error = Snapshot::capture(&source).unwrap_err().to_string();
        assert!(
            error.contains("paint.png") && error.contains(&id.to_string()),
            "{error}"
        );
        fs::write(source.join("paint.png"), b"resolved")?;
        success(git(&repo.0).args(["add", "equipment/paint.png"]).output()?)?;
        Snapshot::capture(&source)?;
        Ok(())
    }

    #[test]
    fn lfs_fetches_only_used_files_and_never_expands_checkout() -> Result<(), Error> {
        let repo = Checkout::new()?;
        setup(&repo.0)?;
        let source = repo.0.join("equipment");
        fs::create_dir(&source)?;
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../training-yard/assets/equipment/source");
        fs::write(
            repo.0.join(".gitattributes"),
            "*.png filter=lfs diff=lfs merge=lfs -text\n",
        )?;
        for name in [
            "narrow.gltf",
            "narrow.gltf.meta.json",
            "narrow.bin",
            "paint.png",
            "normal.png",
            "cutout.png",
        ] {
            fs::copy(fixture.join(name), source.join(name))?;
        }
        repo.commit()?;
        let pointer = success(git(&source).args(["show", "HEAD:./paint.png"]).output()?)?;
        assert!(lfs_pointer(&pointer)?.is_some());
        fs::write(source.join("paint.png"), &pointer)?;
        // An unused LFS input deliberately points at an unavailable object.
        fs::write(
            source.join("unrelated.bin"),
            format!(
                "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 42\n",
                "0".repeat(64)
            ),
        )?;
        repo.commit()?;
        let dest = repo.0.join("snapshot");
        Snapshot::capture(&source)?.materialize(&dest)?;
        assert_eq!(
            fs::read(dest.join("paint.png"))?,
            fs::read(fixture.join("paint.png"))?
        );
        assert_eq!(fs::read(source.join("paint.png"))?, pointer);
        assert!(!dest.join("unrelated.bin").exists());
        Ok(())
    }
}
