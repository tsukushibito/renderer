//! Shared scene commands for a game's property inspector and command-line tools.
use crate::math::{Rotation3, Vec3};
use crate::scene::{Placement, Registry, Scene};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{error::Error, io};

/// Evaluate a cooked clip through the same timeline as the runtime for the
/// offline inspector's geometry preview.
pub fn clip_pose(
    path: &std::path::Path,
    model_name: &str,
    clip_name: &str,
    tick: u64,
    rate: std::num::NonZeroU32,
) -> Result<Value, Box<dyn Error>> {
    let catalog = crate::animation::CharacterCatalog::open(path)?;
    let model = catalog.load(catalog.id(model_name).ok_or("unknown model")?)?;
    let palette = model
        .clip(clip_name)
        .ok_or("unknown clip")?
        .sample(tick, rate);
    let mut vertices = Vec::new();
    for (vertex, skin) in model.model.vertices() {
        let mut position = [0.; 3];
        for (joint, weight) in skin.joints.into_iter().zip(skin.weights) {
            if weight > 0. {
                let pose = palette
                    .get(usize::from(joint))
                    .ok_or("skin joint exceeds palette")?;
                let point = pose.transform_point(vertex.position());
                for (sum, p) in position.iter_mut().zip(point) {
                    *sum += p * weight;
                }
            }
        }
        vertices.push(position);
    }
    Ok(json!({ "vertices": vertices, "indices": model.model.indices() }))
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Inspect,
    Move {
        id: String,
        translation: Vec3,
    },
    TranslateWorld {
        id: String,
        delta: Vec3,
    },
    Rotate {
        id: String,
        rotation: Rotation3,
    },
    Scale {
        id: String,
        scale: Vec3,
    },
    Set {
        id: String,
        field: String,
        value: Value,
    },
    Reset {
        id: String,
        field: String,
    },
    Place {
        id: String,
        prefab: String,
        translation: Vec3,
    },
    SetDefault {
        prefab: String,
        field: String,
        value: Value,
    },
}

/// Apply an edit to an owned candidate. Validate the entire result, including game
/// invariants, before returning any data to the document writer.
pub fn inspect<C: 'static>(
    mut scene: Scene,
    registry: &Registry<C>,
    edit: Edit,
    validate: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Value, String> {
    let find = |s: &Scene, id: &str| {
        s.placements
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| format!("unknown placement {id}"))
    };
    match edit {
        Edit::Inspect => {}
        Edit::Move { id, translation } => {
            let index = find(&scene, &id)?;
            scene.placements[index].translation = translation;
        }
        Edit::TranslateWorld { id, delta } => {
            let index = find(&scene, &id)?;
            let p = &scene.placements[index];

            let delta = match &p.parent {
                None => delta,
                Some(parent) => {
                    let world = scene.instantiate(registry)?;
                    let e = world.find(parent).ok_or("missing parent")?;
                    let basis = glam::Mat3::from_cols(
                        e.transform_vector(Vec3::X).0,
                        e.transform_vector(Vec3::Y).0,
                        e.transform_vector(Vec3::Z).0,
                    );
                    if basis.determinant() == 0. {
                        return Err(format!("{id}: parent transform is singular"));
                    }
                    Vec3(basis.inverse() * delta.0)
                }
            };
            let p = &mut scene.placements[index];
            p.translation += delta;
        }
        Edit::Rotate { id, rotation } => {
            let index = find(&scene, &id)?;
            scene.placements[index].rotation = rotation;
        }
        Edit::Scale { id, scale } => {
            let index = find(&scene, &id)?;
            scene.placements[index].scale = scale;
        }
        Edit::Set { id, field, value } => {
            let index = find(&scene, &id)?;
            scene.placements[index].overrides.insert(field, value);
        }
        Edit::Reset { id, field } => {
            let index = find(&scene, &id)?;
            scene.placements[index]
                .overrides
                .remove(&field)
                .ok_or_else(|| format!("{id}.{field} has no override"))?;
        }
        Edit::Place {
            id,
            prefab,
            translation,
        } => {
            scene.placements.push(Placement {
                id,
                prefab,
                translation,
                parent: None,
                rotation: Rotation3::IDENTITY,
                scale: Vec3::ONE,
                overrides: Default::default(),
            });
        }
        Edit::SetDefault {
            prefab,
            field,
            value,
        } => {
            scene
                .prefabs
                .get_mut(&prefab)
                .ok_or_else(|| format!("unknown prefab {prefab}"))?
                .fields
                .insert(field, value);
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
            "axis_x": entity.transform_vector(Vec3::X),
            "axis_z": entity.transform_vector(Vec3::Z),
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
pub fn serve_once<C: 'static>(
    registry: &Registry<C>,
    validate: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), Box<dyn Error>> {
    let req: Request = serde_json::from_reader(io::stdin().lock())?;
    let result = inspect(req.document, registry, req.edit, validate)?;
    serde_json::to_writer(io::stdout().lock(), &result)?;
    Ok(())
}
