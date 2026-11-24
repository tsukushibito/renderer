//! Shared scene commands for a game's property inspector and command-line tools.
use crate::scene::{Placement, Registry, Scene};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{error::Error, io};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Inspect,
    Move { id: String, translation: [f32; 3] },
    Set { id: String, field: String, value: Value },
    Reset { id: String, field: String },
    Place { id: String, prefab: String, translation: [f32; 3] },
    SetDefault { prefab: String, field: String, value: Value },
}

/// Apply an edit to an owned candidate. Validate the entire result, including game
/// invariants, before returning any data to the document writer.
pub fn inspect(
    mut scene: Scene,
    registry: &Registry,
    edit: Edit,
    validate: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Value, String> {
    let find = |s: &Scene, id: &str| {
        s.placements.iter().position(|p| p.id == id).ok_or_else(|| format!("unknown placement {id}"))
    };
    match edit {
        Edit::Inspect => {}
        Edit::Move { id, translation } => {
            let index = find(&scene, &id)?;
            scene.placements[index].translation = translation;
        }
        Edit::Set {id,field,value} => {
            let index = find(&scene, &id)?;
            scene.placements[index].overrides.insert(field, value);
        }
        Edit::Reset {id,field} => {
            let index = find(&scene, &id)?;
            scene.placements[index].overrides.remove(&field).ok_or_else(|| format!("{id}.{field} has no override"))?;
        }
        Edit::Place {id,prefab,translation} => {
            scene.placements.push(Placement { id, prefab, translation, parent:None, overrides:Default::default() });
        }
        Edit::SetDefault {prefab,field,value} => {
            scene.prefabs.get_mut(&prefab).ok_or_else(|| format!("unknown prefab {prefab}"))?.fields.insert(field,value);
        }
    }
    let world = scene.instantiate(registry)?;
    let cooked = serde_json::to_string(&scene).map_err(|e| e.to_string())?;
    validate(&cooked)?;
    let mut inspected = Vec::new();
    for entity in world.entities() {
        inspected.push(json!({
            "id": entity.placement_id,
            "kind": entity.kind,
            "world": entity.world_translation,
            "fields": entity.fields,
            "registered_fields": registry.fields(&entity.kind),
        }));
    }
    Ok(json!({ "document":scene, "entities":inspected }))
}

#[derive(Deserialize)]
struct Request {
    document: Scene,
    edit: Edit,
}

/// Run one JSON request through the application's registry. No disk I/O or GPU
/// is required. The editor and batch clients use this exact same entry point.
pub fn serve_once(
    registry: &Registry,
    validate: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), Box<dyn Error>> {
    let req: Request = serde_json::from_reader(io::stdin().lock())?;
    let result = inspect(req.document, registry, req.edit, validate)?;
    serde_json::to_writer(io::stdout().lock(), &result)?;
    Ok(())
}
