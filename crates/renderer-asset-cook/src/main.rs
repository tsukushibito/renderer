//! Offline import, conversion, and atomic catalog publication for static equipment.
mod cache;
mod gltf;
mod store;

use cache::{Cache, Report};
use renderer_asset::{AssetId, Catalog, Digest, Equipment, Primitive};
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

fn discover(directory: &Path, sources: &mut BTreeMap<AssetId, PathBuf>) -> Result<(), Error> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let path = entry.path();
        if ty.is_dir() {
            discover(&path, sources)?;
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

fn cook_equipment(id: AssetId, path: &Path, cache: &mut Cache<'_>) -> Result<Equipment, Error> {
    let src = gltf::Source::open(path)?;
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
    let mut sources = BTreeMap::new();
    discover(source, &mut sources)?;
    let mut cache = Cache::new(cache_dir, output)?;
    let mut assets = BTreeMap::new();
    for (id, path) in sources {
        let asset = cook_equipment(id, &path, &mut cache)
            .map_err(|e| format!("cooking {id} ({}): {e}", path.display()))?;
        assets.insert(id, asset);
    }
    let catalog = Catalog { version: 1, assets };
    let bytes = serde_json::to_vec(&catalog)?;
    let generation = Digest::of(&bytes);
    let path = output.join("catalogs").join(generation.to_string());
    if store::read_if_present(&path)?.as_deref() != Some(bytes.as_slice()) {
        store::publish(&path, &bytes)?;
    }
    let document = format!("{generation}\n");
    let current = output.join("current");
    if store::read_if_present(&current)?.as_deref() != Some(document.as_bytes()) {
        store::publish(&current, document.as_bytes())?;
    }
    Ok(cache.report(generation))
}

fn run(args: &[OsString]) -> Result<(), Error> {
    match args {
        [cmd, source] if cmd == "import" => println!("{}", import(Path::new(source))?),
        [cmd, source, output, opt, cache] if cmd == "cook" && opt == "--cache" => {
            let report = cook(Path::new(source), Path::new(output), Path::new(cache))?;
            println!("{}", serde_json::to_string(&report)?);
        }
        _ => return Err("usage: renderer-asset-cook import <source.gltf> | cook <source-directory> <output-directory> --cache <cache-directory>".into()),
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
