//! Offline import, conversion, and atomic catalog publication for static equipment.
mod cache;
mod development;
mod geometry;
mod gltf;
mod image;
mod image_update;
mod inputs;
mod pbrt;
mod retention;
mod scene;
mod setup;
mod store;

use cache::{Cache, Report};
use sashiko_asset::{AssetId, Catalog, Digest, Equipment, Primitive};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

type Error = Box<dyn StdError>;
const PROFILE: &str = "pbr-rgba8-v1";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Sidecar {
    schema: u32,
    asset_id: AssetId,
    profile: String,
}

fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".meta.json");
    PathBuf::from(name)
}

fn read_sidecar(path: &Path) -> Result<Sidecar, Error> {
    let bytes = fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let meta: Sidecar = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid sidecar {}: {e}", path.display()))?;
    if meta.schema != 1 || meta.profile != PROFILE {
        return Err(format!(
            "unsupported sidecar schema {} or profile {:?} at {}",
            meta.schema,
            meta.profile,
            path.display()
        )
        .into());
    }
    Ok(meta)
}

fn import(source: &Path) -> Result<AssetId, Error> {
    if !is_gltf(source) || !source.is_file() {
        return Err(format!("expected a glTF file: {}", source.display()).into());
    }
    let path = sidecar(source);
    if !path.try_exists()? {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| format!("generating asset ID: {e}"))?;
        let id = AssetId::from_u128(u128::from_be_bytes(random));
        let meta = Sidecar {
            schema: 1,
            asset_id: id,
            profile: PROFILE.into(),
        };
        let mut bytes = serde_json::to_vec_pretty(&meta)?;
        bytes.push(b'\n');
        store::create_sidecar(&path, &bytes)?;
    }
    Ok(read_sidecar(&path)?.asset_id)
}

fn is_gltf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gltf"))
}

fn sidecar_source(path: &Path) -> Option<PathBuf> {
    if path.extension()? != "json" {
        return None;
    }
    let meta = path.with_extension("");
    if meta.extension()? != "meta" {
        return None;
    }
    let source = meta.with_extension("");
    is_gltf(&source).then_some(source)
}

fn discover(directory: &Path, sources: &mut BTreeMap<AssetId, PathBuf>) -> Result<(), Error> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let path = entry.path();
        if ty.is_dir() {
            discover(&path, sources)?;
        } else if ty.is_file()
            && let Some(original) = sidecar_source(&path)
            && !original.is_file()
        {
            return Err(format!(
                "missing source {} for {}",
                original.display(),
                path.display()
            )
            .into());
        } else if ty.is_file() && is_gltf(&path) {
            let meta = read_sidecar(&sidecar(&path))?;
            if let Some(other) = sources.insert(meta.asset_id, path.clone()) {
                return Err(format!(
                    "duplicate asset ID {} in {} and {}",
                    meta.asset_id,
                    other.display(),
                    path.display()
                )
                .into());
            }
        }
    }
    Ok(())
}

fn cook_equipment(
    id: AssetId,
    path: &Path,
    source_root: &Path,
    cache: &mut Cache<'_>,
) -> Result<Equipment, Error> {
    let src = gltf::Source::open(path, source_root)?;
    if src.len() == 0 {
        return Err("equipment has no triangle primitives".into());
    }
    let mut primitives = Vec::with_capacity(src.len());
    for i in 0..src.len() {
        let mesh = cache.convert(id, "mesh", "", &src.mesh_inputs(i)?, || src.cook_mesh(i))?;
        let material = cache.convert(id, "material", "", &src.material_inputs(i)?, || {
            src.cook_material(i)
        })?;
        let mut images = [None, None, None, None];
        for (slot, image) in src.images(i).into_iter().enumerate() {
            if let Some((index, role)) = image {
                images[slot] = Some(cache.convert(
                    id,
                    "image",
                    role.name(),
                    src.image_inputs(index)?,
                    || src.cook_image(index, role),
                )?);
            }
        }
        primitives.push(Primitive {
            mesh,
            material,
            images,
        });
    }
    Ok(Equipment { primitives })
}

fn cook(source: &Path, output: &Path, cache_dir: &Path) -> Result<Report, Error> {
    let report = cook_catalog(source, output, cache_dir)?;
    let document = format!("{}\n", report.generation);
    let current = output.join("current");
    if store::read_if_present(&current)?.as_deref() != Some(document.as_bytes()) {
        store::publish(&current, document.as_bytes())?;
    }
    Ok(report)
}

fn cook_catalog(source: &Path, output: &Path, cache_dir: &Path) -> Result<Report, Error> {
    cook_into(source, output, cache_dir, BTreeMap::new())
}

fn cook_into(
    source: &Path,
    output: &Path,
    cache_dir: &Path,
    mut assets: BTreeMap<AssetId, Equipment>,
) -> Result<Report, Error> {
    let mut sources = BTreeMap::new();
    let source = source.canonicalize()?;
    discover(&source, &mut sources)?;
    let mut cache = Cache::new(cache_dir, output)?;
    for (id, path) in sources {
        let asset = cook_equipment(id, &path, &source, &mut cache)
            .map_err(|e| format!("cooking {id} ({}): {e}", path.display()))?;
        assets.insert(id, asset);
    }
    publish_catalog(output, cache, Catalog { version: 1, assets })
}

fn publish_catalog(output: &Path, cache: Cache<'_>, catalog: Catalog) -> Result<Report, Error> {
    let bytes = serde_json::to_vec(&catalog)?;
    let generation = Digest::of(&bytes);
    let path = output.join("catalogs").join(generation.to_string());
    if store::read_if_present(&path)?.as_deref() != Some(bytes.as_slice()) {
        store::publish(&path, &bytes)?;
    }
    Ok(cache.report(generation))
}

fn run(args: &[OsString]) -> Result<(), Error> {
    match args {
        [cmd] if cmd == "setup" => setup::install()?,
        [cmd, source] if cmd == "dev" => development::serve(Path::new(source))?,
        [cmd, source] if cmd == "repair" => development::repair(Path::new(source))?,
        [cmd, source] if cmd == "gc" => development::gc(Path::new(source))?,
        [cmd, source] if cmd == "release" => development::release(Path::new(source), false)?,
        [cmd, source, opt] if cmd == "release" && opt == "--apply" => development::release(Path::new(source), true)?,
        [cmd, source] if cmd == "import" => println!("{}", import(Path::new(source))?),
        [cmd, source, settings, directory] if cmd == "scene" => {
            scene::cook(Path::new(source), Path::new(settings), Path::new(directory))?;
        }
        [cmd, source, output, opt, cache] if cmd == "cook" && opt == "--cache" => {
            let report = cook(Path::new(source), Path::new(output), Path::new(cache))?;
            println!("{}", serde_json::to_string(&report)?);
        }
        _ => return Err("usage: sashiko-asset-cook setup | dev <source> | repair <source> | release <source> [--apply] | gc <source> | import <source.gltf> | cook <source> <output> --cache <cache> | scene <source.gltf> <settings.json> <output>".into()),
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    if let Err(e) = run(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
        eprintln!("{e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Result<Self, Error> {
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes).map_err(|e| format!("creating test directory: {e}"))?;
            let path = std::env::temp_dir().join(format!(
                "asset-cook-test-{:032x}",
                u128::from_le_bytes(bytes)
            ));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn simultaneous_imports_keep_one_id() -> Result<(), Error> {
        let tmp = Sandbox::new()?;
        let path = tmp.0.join("armor.gltf");
        fs::write(&path, b"{}")?;
        std::thread::scope(|scope| {
            let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
            let workers: Vec<_> = (0..4)
                .map(|_| {
                    let gate = gate.clone();
                    let file = &path;
                    scope.spawn(move || {
                        gate.wait();
                        import(file).unwrap()
                    })
                })
                .collect();
            let ids: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
            assert!(ids.iter().all(|id| *id == ids[0]));
        });
        assert_eq!(read_sidecar(&sidecar(&path))?.asset_id, import(&path)?);
        Ok(())
    }

    #[test]
    fn lost_source_does_not_publish_a_catalog_that_drops_its_id() -> Result<(), Error> {
        let tmp = Sandbox::new()?;
        let sources = tmp.0.join("sources");
        let output = tmp.0.join("dist");
        fs::create_dir(&sources)?;
        fs::create_dir(&output)?;
        fs::write(output.join("current"), b"previous-generation")?;
        let path = sources.join("armor.gltf");
        fs::write(&path, b"{}")?;
        import(&path)?;
        fs::remove_file(&path)?;
        let error = cook(&sources, &output, &tmp.0.join("cache")).err().unwrap();
        assert!(error.to_string().contains("missing source"));
        assert_eq!(fs::read(output.join("current"))?, b"previous-generation");
        Ok(())
    }

    #[test]
    fn repairs_damaged_cache_and_redelivers_from_verified_cache() -> Result<(), Error> {
        let tmp = Sandbox::new()?;
        let dir = tmp.0.join("cache");
        let dist = tmp.0.join("dist");
        let id = AssetId::from_u128(1);
        let bake = || Ok(b"expected".to_vec());
        let convert = |cache: &mut Cache<'_>| cache.convert(id, "mesh", "", b"input", bake);
        let reference = convert(&mut Cache::new(&dir, &dist)?)?;
        let object = Path::new("objects").join(reference.hash.to_string());
        fs::write(dir.join(&object), b"corrupt!")?;
        assert_eq!(convert(&mut Cache::new(&dir, &dist)?)?, reference);
        assert_eq!(fs::read(dir.join(&object))?, b"expected");
        fs::write(dist.join(&object), b"corrupt!")?;
        Cache::new(&dir, &dist)?.convert(id, "mesh", "", b"input", || {
            panic!("valid cached output must be reused")
        })?;
        assert_eq!(fs::read(dist.join(object))?, b"expected");
        Ok(())
    }
}
