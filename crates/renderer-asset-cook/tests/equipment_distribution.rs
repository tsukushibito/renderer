use renderer_asset::{AssetId, Generation, ObjectStore};
use serde_json::Value;
use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

struct Project(PathBuf);

#[test]
fn moved_equipment_can_share_dependencies_from_its_parent_directory() -> Result {
    let project = Project::new()?;
    let before = project.report()?;
    let root = project.0.join("source");
    let nested = root.join("armor");
    fs::create_dir(&nested)?;
    let original = root.join("narrow.gltf");
    let mut document: Value = serde_json::from_slice(&fs::read(&original)?)?;
    for key in ["buffers", "images"] {
        for entry in document[key].as_array_mut().unwrap() {
            let relative = entry["uri"].as_str().unwrap();
            entry["uri"] = Value::String(format!("../{relative}"));
        }
    }
    fs::rename(&original, nested.join("narrow.gltf"))?;
    fs::rename(
        root.join("narrow.gltf.meta.json"),
        nested.join("narrow.gltf.meta.json"),
    )?;
    fs::write(nested.join("narrow.gltf"), serde_json::to_vec(&document)?)?;
    assert_eq!(project.report()?["generation"], before["generation"]);

    fs::copy(root.join("paint.png"), project.0.join("outside.png"))?;
    document["images"][0]["uri"] = Value::String("../../outside.png".into());
    fs::write(nested.join("narrow.gltf"), serde_json::to_vec(&document)?)?;
    let result = project.cook()?;
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("escapes source directory"));
    Ok(())
}

impl Project {
    fn new() -> Result<Self> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root =
            std::env::temp_dir().join(format!("renderer-armor-{}-{nonce}", std::process::id()));
        fs::create_dir(&root)?;
        let project = Self(root);
        copy_directory(&fixture().join("source"), &project.0.join("source"))?;
        Ok(project)
    }

    fn cook(&self) -> io::Result<Output> {
        Command::new(env!("CARGO_BIN_EXE_renderer-asset-cook"))
            .args(["cook", "source", "dist", "--cache", "cache"])
            .current_dir(&self.0)
            .output()
    }

    fn report(&self) -> Result<Value> {
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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../training-yard/assets/equipment")
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

#[test]
fn checked_in_package_matches_clean_cook_and_loads_without_sources() -> Result {
    let project = Project::new()?;
    project.report()?;
    let actual = project.0.join("dist");
    let expected = fixture().join("dist");
    assert_eq!(
        fs::read(actual.join("current"))?,
        fs::read(expected.join("current"))?
    );
    for folder in ["catalogs", "objects"] {
        let shipped = expected.join(folder);
        let cooked = actual.join(folder);
        assert_eq!(
            fs::read_dir(&cooked)?.count(),
            fs::read_dir(&shipped)?.count()
        );
        for entry in fs::read_dir(shipped)? {
            let entry = entry?;
            assert_eq!(
                fs::read(entry.path())?,
                fs::read(cooked.join(entry.file_name()))?
            );
        }
    }

    fs::remove_dir_all(project.0.join("source"))?;
    fs::remove_dir_all(project.0.join("cache"))?;
    let generation = Generation::open(&actual)?;
    let store = ObjectStore::new(&actual);
    let saved = AssetId::from_u128(1);
    let armor = generation.equipment(&saved)?;
    let primitive = &armor.primitives[0];
    let mesh = store.load(&primitive.mesh)?;
    assert_eq!(mesh.mesh()?.indices.len(), 6);
    let paint = primitive.images[3]
        .as_ref()
        .expect("armor has emissive paint");
    assert_eq!(store.load(paint)?.image()?.pixels, [255, 255, 0, 255]);
    let wide = generation.equipment(&AssetId::from_u128(2))?;
    assert_eq!(wide.primitives[0].images[3].as_ref(), Some(paint));
    Ok(())
}

#[test]
fn rename_preserves_save_id_and_edit_rebuilds_only_shared_paint() -> Result {
    let project = Project::new()?;
    let source = project.0.join("source");
    let first = project.report()?;
    for suffix in ["gltf", "gltf.meta.json"] {
        fs::rename(
            source.join(format!("narrow.{suffix}")),
            source.join(format!("renamed.{suffix}")),
        )?;
    }
    let renamed = project.report()?;
    assert_eq!(first["generation"], renamed["generation"]);
    assert!(
        renamed["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["action"] == "reused")
    );

    fs::copy(source.join("normal.png"), source.join("paint.png"))?;
    let updated = project.report()?;
    assert_ne!(renamed["generation"], updated["generation"]);
    let built: Vec<_> = updated["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["action"] == "built")
        .collect();
    assert_eq!(built.len(), 1, "{updated}");
    assert_eq!(built[0]["stage"], "image");
    let package = project.0.join("dist");
    let current = fs::read(package.join("current"))?;
    fs::write(source.join("paint.png"), b"truncated PNG")?;
    let failed = project.cook()?;
    assert!(!failed.status.success());
    assert_eq!(fs::read(package.join("current"))?, current);
    Generation::open(package)?.equipment(&AssetId::from_u128(1))?;
    Ok(())
}
