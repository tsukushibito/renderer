//! Worktree-local selection over a repository's immutable cooked data.
use crate::{
    Error, PROFILE,
    cache::{Cache, Report},
    cook_into,
    image_update::Manifest,
    inputs::Snapshot,
    retention::StoreGuard,
    store,
};
use renderer_asset::{Catalog, Digest};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

struct Project {
    root: PathBuf,
    git_dir: PathBuf,
    namespace: Digest,
}

impl Project {
    fn open(source: &Path) -> Result<Self, Error> {
        let output = Command::new("git")
            .current_dir(source)
            .args([
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
                "--git-dir",
                "--show-prefix",
            ])
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        let paths = std::str::from_utf8(&output.stdout)?;
        let mut paths = paths.lines();
        Ok(Self {
            root: Path::new(paths.next().ok_or("missing shared Git directory")?)
                .join("renderer-assets/v1"),
            git_dir: PathBuf::from(paths.next().ok_or("missing worktree Git directory")?),
            namespace: Digest::of(&serde_json::to_vec(&(
                PROFILE,
                paths.next().ok_or("missing source prefix")?,
            ))?),
        })
    }

    fn guard(&self) -> io::Result<StoreGuard> {
        StoreGuard::lock(&self.root, self.namespace)
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(root: &Path) -> Result<Self, Error> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|e| format!("allocating scratch directory: {e}"))?;
        let path = root
            .join("scratch")
            .join(format!("{:032x}", u128::from_le_bytes(nonce)));
        fs::create_dir_all(path.parent().unwrap())?;
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Serialize)]
struct Preview<'a> {
    root: &'a Path,
    generation: Digest,
    snapshot_hit: bool,
    conversions: usize,
    object_bytes_written: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
}

// GC may have swept data after the weak snapshot record was written.
fn cached_generation(root: &Path, snapshot: &Path) -> Result<Option<(Digest, Catalog)>, Error> {
    let Some(record) = store::read_if_present(snapshot)? else {
        return Ok(None);
    };
    let Some(id) = std::str::from_utf8(&record)
        .ok()
        .and_then(|s| s.parse::<Digest>().ok())
    else {
        return Ok(None);
    };
    let Some(bytes) = store::read_if_present(&root.join("catalogs").join(id.to_string()))? else {
        return Ok(None);
    };
    if Digest::of(&bytes) != id {
        return Ok(None);
    }
    let catalog: Catalog = serde_json::from_slice(&bytes)?;
    let mut visited = BTreeSet::new();
    for primitive in catalog
        .assets
        .values()
        .flat_map(|equipment| &equipment.primitives)
    {
        for object in [Some(primitive.mesh), Some(primitive.material)]
            .into_iter()
            .chain(primitive.images)
            .flatten()
        {
            if visited.insert(object.hash) {
                match fs::metadata(root.join("objects").join(object.hash.to_string())) {
                    Ok(metadata) if metadata.is_file() && metadata.len() == object.size => {}
                    Ok(_) => return Ok(None),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    Ok(Some((id, catalog)))
}

fn record_key(converter: Digest, inputs: Digest) -> Result<Digest, Error> {
    Ok(Digest::of(&serde_json::to_vec(&(
        PROFILE, converter, inputs,
    ))?))
}

fn build_snapshot(root: &Path, snapshot: &Snapshot, converter: Digest) -> Result<Report, Error> {
    let base_key = record_key(converter, snapshot.clean_key)?;
    let index = root.join("dependencies").join(base_key.to_string());
    if let Some(changes) = &snapshot.changed_images
        && let Some(bytes) = store::read_if_present(&index)?
        && let Ok((checksum, manifest)) = serde_json::from_slice::<(Digest, Manifest)>(&bytes)
        && let Some((generation, mut base)) =
            cached_generation(root, &root.join("snapshots").join(base_key.to_string()))?
        && checksum == Digest::of(&serde_json::to_vec(&(base_key, generation, &manifest))?)
    {
        let mut cache = Cache::with_converter(root, root, converter);
        if manifest.patch_images(changes, snapshot, &mut base, &mut cache)? {
            return crate::publish_catalog(root, cache, base);
        }
        let scratch = Scratch::new(root)?;
        let affected = manifest
            .models
            .iter()
            .filter(|(_, deps)| !deps.is_disjoint(changes))
            .map(|(path, _)| path.clone())
            .collect();
        snapshot.materialize_subset(&scratch.0, &affected)?;
        return cook_into(&scratch.0, root, root, base.assets);
    }
    let scratch = Scratch::new(root)?;
    let manifest = snapshot.materialize(&scratch.0)?;
    let report = cook_into(&scratch.0, root, root, BTreeMap::new())?;
    if snapshot.key == snapshot.clean_key {
        let checksum = Digest::of(&serde_json::to_vec(&(
            base_key,
            report.generation,
            &manifest,
        ))?);
        store::publish(&index, &serde_json::to_vec(&(checksum, manifest))?)?;
    }
    Ok(report)
}

fn prepare<'a>(source: &Path, project: &'a Project) -> Result<(Preview<'a>, File), Error> {
    let root = &project.root;
    // Serialize successive requests from this checkout while unrelated checkouts can cook.
    let _request = store::lock(&project.git_dir.join("renderer-preview.lock"))?;
    let guard = project.guard()?;
    let _build = StoreGuard::build(root)?;
    drop(guard);
    let snapshot = Snapshot::capture(source)?;
    let converter = Cache::converter()?;
    let key = record_key(converter, snapshot.key)?;
    let _same_snapshot = store::lock(&root.join("snapshot-locks").join(&key.to_string()[..2]))?;
    let record = root.join("snapshots").join(key.to_string());
    let cached = cached_generation(root, &record)?.map(|(id, _)| id);
    let (generation, conversions, written) = match cached {
        Some(id) => (id, 0, 0),
        None => {
            let report = build_snapshot(root, &snapshot, converter)?;
            store::publish(&record, report.generation.to_string().as_bytes())?;
            let conversions = report.events.iter().filter(|e| e.action == "built").count();
            (report.generation, conversions, report.published_bytes)
        }
    };
    // Recheck after acquiring publication rights: files can change while waiting
    // for another transaction or while materializing the original snapshot.
    let guard = project.guard()?;
    if Snapshot::capture(source)?.key != snapshot.key {
        return Err("asset inputs changed during cook; retry with current inputs".into());
    }
    let lease = guard.lease(generation)?;
    guard.select(&project.git_dir, generation)?;
    Ok((
        Preview {
            root,
            generation,
            snapshot_hit: cached.is_some(),
            conversions,
            object_bytes_written: written,
            diagnostic: None,
        },
        lease,
    ))
}

pub(super) fn serve(source: &Path) -> Result<(), Error> {
    let project = Project::open(source)?;
    let (preview, _lease) = match prepare(source, &project) {
        Ok(ready) => ready,
        Err(error) => {
            let guard = project.guard()?;
            let Some(generation) = guard.selected(&project.git_dir)? else {
                return Err(error);
            };
            eprintln!("keeping previous generation {generation}: {error}");
            let lease = guard.lease(generation)?;
            (
                Preview {
                    root: &project.root,
                    generation,
                    snapshot_hit: false,
                    conversions: 0,
                    object_bytes_written: 0,
                    diagnostic: Some(error.to_string()),
                },
                lease,
            )
        }
    };
    {
        let mut out = io::stdout().lock();
        serde_json::to_writer(&mut out, &preview)?;
        writeln!(out)?;
        out.flush()?;
    }
    // A game that exits, including an abnormal exit, closes its end of this pipe.
    io::copy(&mut io::stdin().lock(), &mut io::sink())?;
    Ok(())
}

pub(super) fn release(source: &Path, apply: bool) -> Result<(), Error> {
    let project = Project::open(source)?;
    let guard = project.guard()?;
    let selected = guard.selected(&project.git_dir)?;
    let bytes = if apply {
        guard.release(&project.git_dir)?
    } else {
        guard.estimate_release(&project.git_dir)?
    };
    println!(
        "{}",
        serde_json::json!({
            "released": apply && selected.is_some(), "reclaimable_bytes": bytes,
            "source": source, "profile": PROFILE, "selected_generation": selected,
            "restore": "Reopen with the project's pinned cooker. Evicted data requires the original Git or LFS objects and your local edits."
        })
    );
    Ok(())
}

pub(super) fn gc(source: &Path) -> Result<(), Error> {
    let project = Project::open(source)?;
    let bytes = project.guard()?.gc()?;
    println!("{}", serde_json::json!({ "reclaimed_bytes": bytes }));
    Ok(())
}
