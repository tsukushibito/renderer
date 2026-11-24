//! Research hall components registered with the shared scene authoring API.
use crate::{Arena, Obstacle};
use sashiko::scene::{EntityId, Registry, Scene, World};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lunge {
    pub range: f32,
    pub speed: f32,
    pub windup_ticks: u8,
    pub dash_ticks: u8,
    pub recovery_ticks: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectType {
    /// Number of consecutive entries in the repeating deterministic spawn sequence.
    pub copies: u16,
    pub hp: u8,
    pub walk_speed: f32,
    pub model: Option<String>,
    pub clip: String,
    pub lunge: Option<Lunge>,
}

impl SubjectType {
    fn validate(&self) -> Result<(), String> {
        if self.copies == 0
            || self.hp == 0
            || !self.walk_speed.is_finite()
            || self.walk_speed < 0.
            || self.clip.is_empty()
            || self.model.as_ref().is_some_and(String::is_empty)
        {
            return Err(
                "Subject needs positive copies/hp, usable clip/model and nonnegative walk_speed"
                    .into(),
            );
        }
        if let Some(l) = &self.lunge
            && (!l.range.is_finite()
                || l.range <= 0.
                || !l.speed.is_finite()
                || l.speed <= 0.
                || l.dash_ticks == 0
                || l.windup_ticks == 0)
        {
            return Err("invalid lunge range, speed or timing".into());
        }
        Ok(())
    }
}

pub(crate) fn default_subjects() -> Vec<SubjectType> {
    Arena::default().subjects
}

#[derive(Deserialize)]
struct Extent {
    half_extent: [f32; 2],
}
impl Extent {
    fn validate(&self) -> Result<(), String> {
        if self
            .half_extent
            .into_iter()
            .all(|x| x.is_finite() && x > 0.)
        {
            Ok(())
        } else {
            Err("half_extent coordinates must be positive".into())
        }
    }
}

#[derive(Deserialize)]
struct Empty {}

/// Values exchanged between scene behaviors and one gameplay update.
pub struct RuleContext {
    pub player: [f32; 2],
    pub marker: [f32; 2],
    pub entered_marker: bool,
}

#[derive(Deserialize)]
struct Marker {
    #[serde(default = "marker_radius")]
    radius: f32,
    #[serde(skip)]
    occupied: bool,
}
fn marker_radius() -> f32 {
    0.65
}
impl Marker {
    fn validate(&self) -> Result<(), String> {
        if self.radius.is_finite() && self.radius > 0. {
            Ok(())
        } else {
            Err("radius must be finite and positive".into())
        }
    }
    fn update(&mut self, _id: EntityId, ctx: &mut RuleContext) {
        let occupied =
            sashiko::space::distance_squared(ctx.player, ctx.marker) < self.radius * self.radius;
        ctx.entered_marker |= occupied && !self.occupied;
        self.occupied = occupied;
    }
}

pub fn registry() -> Registry<RuleContext> {
    let mut types = Registry::default();
    for name in ["Room", "Bench"] {
        types
            .register(name, &["half_extent"], Extent::validate)
            .expect("unique registered type");
    }
    types
        .register::<Empty>("PlayerSpawn", &[], |_| Ok(()))
        .expect("unique registered type");
    types
        .register_behavior("Marker", &["radius"], Marker::validate, Marker::update)
        .expect("unique registered type");
    types
        .register(
            "Subject",
            &["copies", "hp", "walk_speed", "model", "clip", "lunge"],
            SubjectType::validate,
        )
        .expect("unique registered type");
    types
}

pub(crate) fn decode_arena(data: Value) -> Result<Arena, String> {
    if data.get("schema_version").is_none() {
        return serde_json::from_value(data).map_err(|e| e.to_string());
    }
    let scene: Scene = serde_json::from_value(data).map_err(|e| e.to_string())?;
    let types = registry();
    let world = scene.instantiate(&types)?;
    let mut extent = None;
    let mut spawn = None;
    let mut marker = None;
    let mut obstacles = Vec::new();
    let mut subjects = Vec::new();
    for e in world.entities() {
        let [x, y, z] = e.world_translation;
        if y != 0. {
            return Err(format!(
                "{}: research hall placements must be on the XZ plane",
                e.placement_id
            ));
        }
        let pos = [x, z];
        let half_extent = || {
            let extent = e.component::<Extent>().ok_or("missing extent")?;
            // This game's navigation and visible benches share axis-aligned footprints.
            // Reject rotations and shears they cannot both represent.
            let edge_x = e.transform_vector([extent.half_extent[0], 0., 0.]);
            let edge_z = e.transform_vector([0., 0., extent.half_extent[1]]);
            if edge_x[1] != 0.
                || edge_z[1] != 0.
                || edge_x[2].abs() > 1e-4
                || edge_z[0].abs() > 1e-4
            {
                return Err(format!(
                    "{}: room and bench footprints must remain aligned with X/Z",
                    e.placement_id
                ));
            }
            Ok([edge_x[0].abs(), edge_z[2].abs()])
        };
        match e.kind.as_str() {
            "Room" => {
                if pos != [0., 0.] {
                    return Err(format!(
                        "{}: Room must be centered at the origin",
                        e.placement_id
                    ));
                }
                single(&mut extent, half_extent()?, "Room")?;
            }
            "Bench" => obstacles.push(Obstacle {
                center: pos,
                half_extent: half_extent()?,
            }),
            "PlayerSpawn" => single(&mut spawn, pos, "PlayerSpawn")?,
            "Marker" => single(&mut marker, pos, "Marker")?,
            "Subject" => subjects.push(
                e.component::<SubjectType>()
                    .ok_or("missing subject")?
                    .clone(),
            ),
            other if types.has_behavior(other) => {}
            other => return Err(format!("unsupported research hall component {other}")),
        }
    }
    Ok(Arena {
        half_extent: extent.ok_or("missing Room")?,
        player_spawn: spawn.ok_or("missing PlayerSpawn")?,
        marker: marker.ok_or("missing Marker")?,
        obstacles,
        subjects,
        authored_scene: Some(std::sync::Arc::new(scene)),
    })
}

pub(crate) fn game_world(arena: &Arena) -> World<RuleContext> {
    let registry = registry();
    if let Some(scene) = &arena.authored_scene {
        return scene
            .instantiate(&registry)
            .expect("validated immutable scene");
    }
    let marker = serde_json::from_value::<Scene>(serde_json::json!({
        "schema_version":1,
        "prefabs":{"marker":{"kind":"Marker"}},
        "placements":[{"id":"marker","prefab":"marker"}]
    }))
    .expect("marker placement");
    marker.instantiate(&registry).expect("marker type")
}

fn single<T>(slot: &mut Option<T>, value: T, kind: &str) -> Result<(), String> {
    if slot.replace(value).is_none() {
        Ok(())
    } else {
        Err(format!("multiple {kind} placements"))
    }
}

pub(crate) fn validate_subjects(subjects: &[SubjectType]) -> Result<(), String> {
    if subjects.is_empty() {
        return Err("arena requires a Subject placement".into());
    }
    for subject in subjects {
        subject.validate()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Game, Input, Scenario};
    use serde_json::json;

    #[test]
    fn another_subject_uses_authored_movement_without_new_game_code() {
        let mut scene: Scene =
            serde_json::from_str(include_str!("../../../assets/survivor/arena.json")).unwrap();
        // A stationary variant of the ordinary subject uses its own presentation.
        let mut sentry = scene.prefabs["walker"].clone();
        sentry.fields.insert("copies".into(), json!(1));
        sentry.fields.insert("walk_speed".into(), json!(0));
        sentry.fields.insert("model".into(), json!("alternate"));
        scene.prefabs.insert("sentry".into(), sentry);
        scene
            .placements
            .push(serde_json::from_value(json!({"id":"sentry","prefab":"sentry"})).unwrap());
        let arena = Arena::from_json(&serde_json::to_string(&scene).unwrap()).unwrap();
        let mut game = Game::new(arena, Scenario::Early, 506);
        let id = game
            .enemies
            .iter()
            .find(|e| game.arena.subjects[e.archetype].model.as_deref() == Some("alternate"))
            .unwrap()
            .id;
        let pos = game.enemies.iter().find(|e| e.id == id).unwrap().pos;
        game.step(Input::default());
        assert_eq!(game.enemies.iter().find(|e| e.id == id).unwrap().pos, pos);
    }

    #[test]
    fn registered_marker_behavior_runs_the_edited_rule_once_per_entry() {
        let mut scene: Scene =
            serde_json::from_str(include_str!("../../../assets/survivor/arena.json")).unwrap();
        scene
            .prefabs
            .get_mut("goal")
            .unwrap()
            .fields
            .insert("radius".into(), json!(4.0));
        let arena = Arena::from_json(&serde_json::to_string(&scene).unwrap()).unwrap();
        let mut game = Game::new(arena, Scenario::Early, 506);
        assert!(game.step(Input::default()).reached_marker);
        assert!(!game.step(Input::default()).reached_marker);
        assert_eq!(game.marker_entries, 1);
    }

    #[test]
    fn moving_a_parent_updates_the_bench_used_by_gameplay() {
        let mut scene: Scene =
            serde_json::from_str(include_str!("../../../assets/survivor/arena.json")).unwrap();
        let bench = scene
            .placements
            .iter_mut()
            .find(|p| p.id == "bench-west")
            .unwrap();
        bench.parent = Some("marker".into());
        bench.translation = [0., 0., 1.5];
        let arena = Arena::from_json(&serde_json::to_string(&scene).unwrap()).unwrap();
        assert!(arena.obstacles.iter().any(|b| b.center == [0., 4.5]));
        assert!(!arena.floor().open([0., 4.5], 0.1));
    }
}
