#[path = "support/project.rs"]
mod project;

use project::{Project, Result};
use renderer_asset::{AssetId, Catalog, Generation, ObjectStore};
use serde_json::Value;
use std::{collections::BTreeSet, fs};

#[test]
fn a_cached_tangent_vector_does_not_accept_matrix_input() -> Result {
    let project = Project::new()?;
    project.report()?;
    let path = project.0.join("source/narrow.gltf");
    let mut document: Value = serde_json::from_slice(&fs::read(&path)?)?;
    let tangent = document["meshes"][0]["primitives"][0]["attributes"]["TANGENT"]
        .as_u64()
        .unwrap() as usize;
    document["accessors"][tangent]["type"] = Value::String("MAT2".into());
    fs::write(path, serde_json::to_vec(&document)?)?;
    assert!(!project.cook()?.status.success());
    Ok(())
}

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

#[test]
fn clean_cook_matches_reference_generation_and_loads_without_sources() -> Result {
    let project = Project::new()?;
    project.report()?;
    let actual = project.0.join("dist");
    fs::remove_dir_all(project.0.join("source"))?;
    fs::remove_dir_all(project.0.join("cache"))?;
    let generation = Generation::open(&actual)?;
    // The original distribution's catalog digest also commits to every object hash and size.
    assert_eq!(
        generation.id().to_string(),
        "cf05211e283db404a55445283f3a99501925d7da73b536759f3799d64b93095f"
    );
    let catalog: Catalog = serde_json::from_slice(&fs::read(
        actual.join("catalogs").join(generation.id().to_string()),
    )?)?;
    let store = ObjectStore::new(&actual);
    let mut expected = BTreeSet::new();
    for asset in catalog.assets.values() {
        for primitive in &asset.primitives {
            for object in [&primitive.mesh, &primitive.material]
                .into_iter()
                .chain(primitive.images.iter().flatten())
            {
                if expected.insert(object.hash) {
                    store.load(object)?;
                }
            }
        }
    }
    assert_eq!(fs::read_dir(actual.join("catalogs"))?.count(), 1);
    assert_eq!(
        fs::read_dir(actual.join("objects"))?.count(),
        expected.len()
    );
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
