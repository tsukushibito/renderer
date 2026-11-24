//! Versioned editable scenes. Prefab values and placement overrides are kept separately.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
};

pub type Fields = Map<String, Value>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prefab {
    pub kind: String,
    #[serde(default)]
    pub fields: Fields,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub id: String,
    pub prefab: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default)]
    pub translation: [f32; 3],
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub overrides: Fields,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub schema_version: u32,
    pub prefabs: BTreeMap<String, Prefab>,
    pub placements: Vec<Placement>,
}

type FieldCheck = dyn Fn(&Fields) -> Result<(), String> + Send + Sync;
struct RegisteredType {
    fields: &'static [&'static str],
    validate: Box<FieldCheck>,
}

/// Editor and loader share the same registered type and semantic validation.
#[derive(Default)]
pub struct Registry {
    types: BTreeMap<&'static str, RegisteredType>,
}
impl Registry {
    pub fn register<T: DeserializeOwned + 'static>(
        &mut self,
        name: &'static str,
        fields: &'static [&'static str],
        check: fn(&T) -> Result<(), String>,
    ) -> Result<(), String> {
        if name.is_empty() || self.types.contains_key(name) {
            return Err(format!("duplicate or empty registered type {name:?}"));
        }
        self.types.insert(
            name,
            RegisteredType {
                fields,
                validate: Box::new(move |data| {
                    if let Some(extra) = data.keys().find(|key| !fields.contains(&key.as_str())) {
                        return Err(format!("unknown field {extra}"));
                    }
                    let component = serde_json::from_value(Value::Object(data.clone()))
                        .map_err(|error| error.to_string())?;
                    check(&component)
                }),
            },
        );
        Ok(())
    }

    pub fn fields(&self, kind: &str) -> Option<&'static [&'static str]> {
        self.types.get(kind).map(|ty| ty.fields)
    }

    fn validate(&self, kind: &str, data: &Fields) -> Result<(), String> {
        let ty = self
            .types
            .get(kind)
            .ok_or_else(|| format!("unknown type {kind:?}"))?;
        (ty.validate)(data)
    }
}

/// Transient, process-unique identity. Save a placement ID for authored objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityId(u64);

#[derive(Debug)]
pub struct Entity {
    pub id: EntityId,
    pub placement_id: String,
    pub prefab: String,
    pub kind: String,
    pub parent: Option<EntityId>,
    pub translation: [f32; 3],
    pub world_translation: [f32; 3],
    pub fields: Fields,
}

#[derive(Default)]
pub struct World {
    entities: BTreeMap<EntityId, Entity>,
    by_placement: HashMap<String, EntityId>,
}

fn compose(parent: [f32; 3], local: [f32; 3]) -> Result<[f32; 3], String> {
    let world = std::array::from_fn(|axis| parent[axis] + local[axis]);
    if world.into_iter().all(f32::is_finite) {
        Ok(world)
    } else {
        Err("world translation must be finite".into())
    }
}

impl World {
    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }

    pub fn entity(&self, id: EntityId) -> Option<&Entity> {
        self.entities.get(&id)
    }

    pub fn find(&self, placement: &str) -> Option<&Entity> {
        self.entity(*self.by_placement.get(placement)?)
    }

    pub fn spawn(
        &mut self,
        placement: &Placement,
        prefabs: &BTreeMap<String, Prefab>,
        registry: &Registry,
    ) -> Result<EntityId, String> {
        if placement.id.is_empty() || self.by_placement.contains_key(&placement.id) {
            return Err(format!("duplicate or empty placement {:?}", placement.id));
        }
        let prefab = prefabs
            .get(&placement.prefab)
            .ok_or_else(|| format!("{}: missing prefab {}", placement.id, placement.prefab))?;
        let parent = placement
            .parent
            .as_ref()
            .map(|name| {
                self.find(name)
                    .ok_or_else(|| format!("{}: missing parent {name}", placement.id))
            })
            .transpose()?;
        let translation = compose(
            parent.map_or([0.; 3], |p| p.world_translation),
            placement.translation,
        )?;
        let parent = parent.map(|p| p.id);
        let mut fields = prefab.fields.clone();
        fields.extend(placement.overrides.clone());
        registry.validate(&prefab.kind, &fields).map_err(|e| {
            format!(
                "placement {}, prefab {}: {e}",
                placement.id, placement.prefab
            )
        })?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = EntityId(
            NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| "entity IDs exhausted")?,
        );
        self.by_placement.insert(placement.id.clone(), id);
        self.entities.insert(
            id,
            Entity {
                id,
                placement_id: placement.id.clone(),
                prefab: placement.prefab.clone(),
                kind: prefab.kind.clone(),
                parent,
                translation: placement.translation,
                world_translation: translation,
                fields,
            },
        );
        Ok(id)
    }

    /// Remove this entity and all descendants, invalidating their runtime IDs.
    pub fn despawn(&mut self, root: EntityId) -> bool {
        if !self.entities.contains_key(&root) {
            return false;
        }
        let mut removed = std::collections::HashSet::from([root]);
        // Parents precede children because spawn requires a live parent.
        self.entities.retain(|id, entity| {
            if *id == root || entity.parent.is_some_and(|p| removed.contains(&p)) {
                removed.insert(*id);
                self.by_placement.remove(&entity.placement_id);
                false
            } else {
                true
            }
        });
        true
    }
}

impl Scene {
    pub fn instantiate(&self, registry: &Registry) -> Result<World, String> {
        if self.schema_version != 1 {
            return Err(format!("unsupported scene version {}", self.schema_version));
        }
        for (name, prefab) in &self.prefabs {
            if name.is_empty() {
                return Err("empty prefab ID".into());
            }
            registry
                .validate(&prefab.kind, &prefab.fields)
                .map_err(|e| format!("prefab {name}: {e}"))?;
        }
        let mut ids = HashMap::new();
        for (index, placement) in self.placements.iter().enumerate() {
            if placement.id.is_empty() || ids.insert(&placement.id, index).is_some() {
                return Err(format!("duplicate or empty placement {:?}", placement.id));
            }
        }
        let mut children = vec![Vec::new(); self.placements.len()];
        let mut queue = VecDeque::new();
        for (index, placement) in self.placements.iter().enumerate() {
            if let Some(parent) = &placement.parent {
                let parent = *ids
                    .get(parent)
                    .ok_or_else(|| format!("{}: missing parent {parent}", placement.id))?;
                children[parent].push(index);
            } else {
                queue.push_back(index);
            }
        }
        let mut world = World::default();
        while let Some(index) = queue.pop_front() {
            world.spawn(&self.placements[index], &self.prefabs, registry)?;
            queue.extend(&children[index]);
        }
        if let Some(node) = self.placements.iter().find(|p| world.find(&p.id).is_none()) {
            return Err(format!("parent cycle in ancestry of {}", node.id));
        }
        Ok(world)
    }
}
