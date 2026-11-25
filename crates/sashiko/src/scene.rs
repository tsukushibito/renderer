//! Versioned editable scenes. Prefab values and placement overrides are kept separately.
use crate::math::{LocalTransform, Rotation3, Vec3};
use glam::Affine3A;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::sync::Arc;
use std::{
    any::Any,
    collections::{BTreeMap, HashMap, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
};

pub type Fields = Map<String, Value>;

/// A transform-only placement for arranging children. Available in every registry.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Group {}

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
    pub translation: Vec3,
    /// Local unit quaternion, in XYZW order.
    #[serde(default)]
    pub rotation: Rotation3,
    #[serde(default = "unit_scale")]
    pub scale: Vec3,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub overrides: Fields,
}

impl Placement {
    pub fn local_transform(&self) -> LocalTransform {
        LocalTransform::new(self.translation, self.rotation, self.scale)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub schema_version: u32,
    pub prefabs: BTreeMap<String, Prefab>,
    pub placements: Vec<Placement>,
}

type Component = Box<dyn Any + Send + Sync>;
type FieldCheck = dyn Fn(&Fields) -> Result<Component, String> + Send + Sync;
type Tick<C> = Arc<dyn Fn(&mut Entity, &mut C) + Send + Sync>;
struct RegisteredType<C> {
    update: Option<Tick<C>>,
    fields: &'static [&'static str],
    validate: Box<FieldCheck>,
}

/// Editor and loader share the same registered type and semantic validation.
pub struct Registry<C = ()> {
    types: BTreeMap<&'static str, RegisteredType<C>>,
}
impl<C: 'static> Default for Registry<C> {
    fn default() -> Self {
        let mut registry = Self {
            types: BTreeMap::new(),
        };
        registry
            .register::<Group>("Group", &[], |_| Ok(()))
            .expect("fresh registry");
        registry
    }
}
impl<C: 'static> Registry<C> {
    pub fn register<T: DeserializeOwned + Send + Sync + 'static>(
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
                update: None,
                fields,
                validate: Box::new(move |data| {
                    if let Some(extra) = data.keys().find(|key| !fields.contains(&key.as_str())) {
                        return Err(format!("unknown field {extra}"));
                    }
                    let component = serde_json::from_value(Value::Object(data.clone()))
                        .map_err(|error| error.to_string())?;
                    check(&component)?;
                    Ok(Box::new(component))
                }),
            },
        );
        Ok(())
    }

    /// Register authored fields and an executable game rule together. The
    /// callback receives the runtime component and a game-owned tick context.
    pub fn register_behavior<T: DeserializeOwned + Send + Sync + 'static>(
        &mut self,
        name: &'static str,
        fields: &'static [&'static str],
        check: fn(&T) -> Result<(), String>,
        tick: fn(&mut T, EntityId, &mut C),
    ) -> Result<(), String> {
        self.register(name, fields, check)?;
        self.types.get_mut(name).expect("registered here").update =
            Some(Arc::new(move |entity, ctx| {
                let component = entity
                    .component
                    .downcast_mut::<T>()
                    .expect("registered decoder");
                tick(component, entity.id, ctx);
            }));
        Ok(())
    }

    pub fn has_behavior(&self, kind: &str) -> bool {
        self.types.get(kind).is_some_and(|t| t.update.is_some())
    }
    pub fn fields(&self, kind: &str) -> Option<&'static [&'static str]> {
        self.types.get(kind).map(|ty| ty.fields)
    }

    fn decode(&self, kind: &str, data: &Fields) -> Result<Component, String> {
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
    local: Affine3A,
    world: Affine3A,
    component: Component,
    pub world_translation: Vec3,
    /// Initial resolved editor properties. Runtime changes live in the typed component.
    pub fields: Fields,
}

impl Entity {
    /// The registered Rust value after prefab override resolution.
    pub fn component<T: Any>(&self) -> Option<&T> {
        self.component.downcast_ref()
    }
    pub fn transform_vector(&self, local: Vec3) -> Vec3 {
        Vec3(self.world.transform_vector3(local.0))
    }
    pub fn transform_point(&self, local: Vec3) -> Vec3 {
        Vec3(self.world.transform_point3(local.0))
    }
}

pub struct World<C = ()> {
    behaviors: BTreeMap<EntityId, Tick<C>>,
    entities: BTreeMap<EntityId, Entity>,
    by_placement: HashMap<String, EntityId>,
    pose_changes: Vec<(EntityId, Affine3A)>,
}

impl<C> Default for World<C> {
    fn default() -> Self {
        Self {
            behaviors: BTreeMap::new(),
            entities: BTreeMap::new(),
            by_placement: HashMap::new(),
            pose_changes: Vec::new(),
        }
    }
}

fn unit_scale() -> Vec3 {
    Vec3::ONE
}
fn compose(parent: Affine3A, local: Affine3A) -> Result<Affine3A, String> {
    let world = parent * local;
    if world.is_finite() {
        Ok(world)
    } else {
        Err("world transform overflow".into())
    }
}

impl<C: 'static> World<C> {
    /// Run registered behaviors once, in deterministic spawn order. Queue
    /// structural changes in the game's context and apply them after this tick.
    pub fn fixed_update(&mut self, context: &mut C) {
        for (id, tick) in &self.behaviors {
            tick(self.entities.get_mut(id).expect("live behavior"), context);
        }
    }

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
        registry: &Registry<C>,
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
        let local = placement.local_transform().affine();
        let world = compose(parent.map_or(Affine3A::IDENTITY, |p| p.world), local)
            .map_err(|e| format!("{}: {e}", placement.id))?;
        let parent = parent.map(|p| p.id);
        let mut fields = prefab.fields.clone();
        fields.extend(placement.overrides.clone());
        let component = registry.decode(&prefab.kind, &fields).map_err(|e| {
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
        if let Some(tick) = &registry.types[prefab.kind.as_str()].update {
            self.behaviors.insert(id, Arc::clone(tick));
        }
        self.by_placement.insert(placement.id.clone(), id);
        self.entities.insert(
            id,
            Entity {
                id,
                placement_id: placement.id.clone(),
                prefab: placement.prefab.clone(),
                kind: prefab.kind.clone(),
                parent,
                local,
                world,
                component,
                world_translation: Vec3(world.translation.into()),
                fields,
            },
        );
        self.pose_changes.clear();
        self.pose_changes.reserve(self.entities.len());
        Ok(id)
    }

    /// Atomically move one local transform and its descendants. Reuse scratch
    /// reserved when entities spawn, including on an overflowing composition.
    pub fn set_transform(
        &mut self,
        root: EntityId,
        transform: LocalTransform,
    ) -> Result<(), String> {
        let local = transform.affine();
        let node = self.entity(root).ok_or("unknown entity")?;
        let parent = node
            .parent
            .map_or(Affine3A::IDENTITY, |p| self.entities[&p].world);
        self.pose_changes.clear();
        self.pose_changes.push((root, compose(parent, local)?));
        for (&id, child) in self
            .entities
            .range((std::ops::Bound::Excluded(root), std::ops::Bound::Unbounded))
        {
            if let Some(parent) = child.parent
                && let Ok(index) = self
                    .pose_changes
                    .binary_search_by_key(&parent, |(id, _)| *id)
            {
                let world = compose(self.pose_changes[index].1, child.local)?;
                self.pose_changes.push((id, world));
            }
        }
        self.entities.get_mut(&root).expect("live root").local = local;
        for &(id, world) in &self.pose_changes {
            let child = self.entities.get_mut(&id).expect("live descendant");
            child.world = world;
            child.world_translation = Vec3(world.translation.into());
        }
        Ok(())
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
                self.behaviors.remove(id);
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
    pub fn instantiate<C: 'static>(&self, registry: &Registry<C>) -> Result<World<C>, String> {
        if self.schema_version != 1 {
            return Err(format!("unsupported scene version {}", self.schema_version));
        }
        for (name, prefab) in &self.prefabs {
            if name.is_empty() {
                return Err("empty prefab ID".into());
            }
            registry
                .decode(&prefab.kind, &prefab.fields)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Deserialize)]
    struct Weight {
        value: u32,
    }

    fn fixture() -> (Registry, Scene) {
        let mut registry = Registry::default();
        registry
            .register::<Weight>("Weight", &["value"], |w| {
                if w.value > 0 {
                    Ok(())
                } else {
                    Err("value must be positive".into())
                }
            })
            .unwrap();
        let scene=serde_json::from_value(json!({
            "schema_version":1, "prefabs":{"weight":{"kind":"Weight","fields":{"value":1}}},
            "placements":[{"id":"child","parent":"root","prefab":"weight","translation":[1,0,0],"overrides":{"value":2}},
            {"id":"root","prefab":"weight","translation":[10,0,0],"scale":[2,1,1]}]
        })).unwrap();
        (registry, scene)
    }
    #[test]
    fn hierarchy_resolves_forward_parent_and_override_and_retires_handles() {
        let (registry, scene) = fixture();
        let mut world = scene.instantiate(&registry).unwrap();
        let root = world.find("root").unwrap().id;
        let child = world.find("child").unwrap();
        assert_eq!(child.world_translation.to_array(), [12., 0., 0.]);
        assert_eq!(child.component::<Weight>().unwrap().value, 2);
        let child = child.id;
        world
            .set_transform(
                root,
                LocalTransform::new(
                    [3., 0., 0.].into(),
                    Rotation3::try_from([0., 0., 1., 0.]).unwrap(),
                    [4., 1., 1.].into(),
                ),
            )
            .unwrap();
        assert_eq!(
            world.entity(child).unwrap().world_translation.to_array(),
            [-1., 0., 0.]
        );
        let other = scene.instantiate(&registry).unwrap();
        assert!(other.entity(root).is_none());
        assert!(world.despawn(root));
        assert!(world.entity(child).is_none());
        assert!(world.find("child").is_none());
        assert!(
            world
                .spawn(&scene.placements[1], &scene.prefabs, &registry)
                .unwrap()
                != root
        );
    }
    #[test]
    fn invalid_graph_and_field_errors_identify_the_authored_location() {
        let (registry, mut scene) = fixture();
        scene.placements[1].parent = Some("child".into());
        assert!(
            scene
                .instantiate(&registry)
                .err()
                .unwrap()
                .contains("cycle")
        );
        scene.placements[1].parent = None;
        scene.placements[0]
            .overrides
            .insert("value".into(), json!(0));
        let error = scene.instantiate(&registry).err().unwrap();
        assert!(error.contains("placement child") && error.contains("value"));
    }
    #[test]
    fn rotated_child_preserves_the_shear_from_nonuniform_parent_scale() {
        let (registry, mut scene) = fixture();
        scene.placements[0].rotation =
            Rotation3::from_axis_angle(Vec3::Z, std::f32::consts::FRAC_PI_4).unwrap();
        let world = scene.instantiate(&registry).unwrap();
        let child = world.find("child").unwrap();
        let k = std::f32::consts::FRAC_1_SQRT_2;
        assert!((child.transform_vector(Vec3::X) - Vec3::new(2. * k, k, 0.)).length() < 1e-5);
        assert!((child.transform_vector(Vec3::Y) - Vec3::new(-2. * k, k, 0.)).length() < 1e-5);
    }
    #[test]
    fn overflowing_descendant_move_keeps_the_previous_world() {
        let (registry, mut scene) = fixture();
        scene.placements[0].scale = [2.; 3].into();
        let mut world = scene.instantiate(&registry).unwrap();
        let root = world.find("root").unwrap().id;
        assert!(
            world
                .set_transform(
                    root,
                    LocalTransform::new([99.; 3].into(), Rotation3::IDENTITY, [f32::MAX; 3].into())
                )
                .is_err()
        );
        assert_eq!(
            world.find("root").unwrap().world_translation.to_array(),
            [10., 0., 0.]
        );
        assert_eq!(
            world.find("child").unwrap().world_translation.to_array(),
            [12., 0., 0.]
        );
    }
}
