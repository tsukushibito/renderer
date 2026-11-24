//! Research hall components registered with the shared scene authoring API.
use crate::{Arena, Obstacle};
use sashiko::scene::{EntityId, Registry, Scene, World};
use sashiko::view::{PointLamp, PointLight, TrackingCamera};
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
    sashiko::view::register_components(&mut types).expect("standard view types");
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

#[derive(Clone, Debug)]
pub struct LabView {
    pub camera: TrackingCamera,
    pub anchor: [f32; 2],
    pub point_lights: Vec<PointLight>,
}
impl LabView {
    fn read(world: &World<RuleContext>) -> Result<Self, String> {
        let mut camera = None;
        let mut point_lights = Vec::new();
        for e in world.entities() {
            if let Some(lamp) = e.component::<PointLamp>() {
                point_lights.push(lamp.at(e.world_translation)?);
            }
            if let Some(rig) = e.component::<TrackingCamera>() {
                let [x, _, z] = e.world_translation;
                single(&mut camera, (*rig, [x, z]), "GroundCamera")?;
            }
        }
        let (camera, anchor) = camera.ok_or("place one GroundCamera for the research hall")?;
        Ok(Self {
            camera,
            anchor,
            point_lights,
        })
    }
    pub(crate) fn legacy(extent: [f32; 2]) -> Result<Self, String> {
        // Older flat layouts and version-one scenes predate authored views.
        // Instantiate today's bundled rig with the old room's dimensions.
        let mut scene: Scene =
            serde_json::from_str(include_str!("../../../assets/survivor/arena.json"))
                .map_err(|e| e.to_string())?;
        let lights = scene
            .placements
            .iter_mut()
            .find(|p| p.id == "lighting-frame")
            .ok_or("bundled scene has no lighting frame")?;
        lights.scale = [extent[0], 1., extent[1]];
        Self::read(&scene.instantiate(&registry())?)
    }
}
pub(crate) fn decode_arena(data: Value) -> Result<Arena, String> {
    if data.get("schema_version").is_none() {
        return serde_json::from_value(data).map_err(|e| e.to_string());
    }
    let scene: Scene = serde_json::from_value(data).map_err(|e| e.to_string())?;
    let types = registry();
    let world = scene.instantiate(&types)?;
    let authored_view = scene
        .prefabs
        .values()
        .any(|p| matches!(p.kind.as_str(), "PointLight" | "GroundCamera"));
    let view = authored_view.then(|| LabView::read(&world)).transpose()?;
    let mut extent = None;
    let mut spawn = None;
    let mut marker = None;
    let mut obstacles = Vec::new();
    let mut subjects = Vec::new();
    for e in world.entities() {
        let [x, y, z] = e.world_translation;
        if matches!(e.kind.as_str(), "PointLight" | "Group") {
            continue;
        }
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
            "GroundCamera" => {}
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
        view,
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
    fn lights_preserve_room_child_and_root_bench_equipment_order() {
        // Equipment variants in lab::build follow footprint order. The four
        // room children follow any root bench, in authored sibling order.
        let mut scene: Scene =
            serde_json::from_str(include_str!("../../../assets/survivor/arena.json")).unwrap();
        let bench_centers = |s: &Scene| {
            Arena::from_json(&serde_json::to_string(s).unwrap())
                .unwrap()
                .obstacles
                .into_iter()
                .map(|b| b.center)
                .collect::<Vec<_>>()
        };
        let original = vec![[-3.2, 0.], [3.2, 0.], [-10., -6.], [10., 6.]];
        assert_eq!(bench_centers(&scene), original);
        scene.placements.push(serde_json::from_value(json!({"id":"root-bench","prefab":"table","translation":[0.,0.,4.5],"overrides":{"half_extent":[0.8,0.3]}})).unwrap());
        let mut edited = vec![[0., 4.5]];
        edited.extend(original);
        assert_eq!(bench_centers(&scene), edited);
    }

    #[test]
    fn inspector_light_override_reaches_prepared_render_inputs() {
        use sashiko::authoring::{Edit, inspect};
        let scene: Scene =
            serde_json::from_str(include_str!("../../../assets/survivor/arena.json")).unwrap();
        let inspect_edit = |edit| {
            inspect(scene.clone(), &registry(), edit, |s| {
                Arena::from_json(s).map(|_| ())
            })
        };
        assert!(
            inspect_edit(Edit::Set {
                id: "lamp-nw".into(),
                field: "candela".into(),
                value: json!(-1)
            })
            .is_err()
        );
        let edited = inspect_edit(Edit::Set {
            id: "lamp-nw".into(),
            field: "candela".into(),
            value: json!(0),
        })
        .unwrap();
        let arena = Arena::from_value(edited["document"].clone()).unwrap();
        let view = arena.prepare_view().unwrap();
        assert_eq!(view.point_lights.len(), 4);
        assert_eq!(
            view.point_lights[0].position(),
            [-0.55 * 18., 4.5, -0.52 * 14.]
        );
        assert_eq!(view.point_lights[0].rgb_candela(), [0.; 3]);
        assert!(view.point_lights[1].rgb_candela()[0] > 0.);
        let edited = inspect_edit(Edit::Set {
            id: "camera".into(),
            field: "height".into(),
            value: json!(32),
        })
        .unwrap();
        let arena = Arena::from_value(edited["document"].clone()).unwrap();
        assert_eq!(arena.prepare_view().unwrap().camera.height, 32.);
        let old: Arena = Arena::from_json(
            r#"{"half_extent":[9,7],"player_spawn":[0,6],"marker":[0,3],"obstacles":[]}"#,
        )
        .unwrap();
        let old_view = old.prepare_view().unwrap();
        assert_eq!(old_view.camera.height, 22.);
        assert_eq!(
            old_view.point_lights[0].position(),
            [-0.55 * 9., 4.5, -0.52 * 7.]
        );
        let default_arena = Arena::default();
        let baseline = default_arena.prepare_view().unwrap();
        assert_eq!(
            baseline.point_lights[0].rgb_candela(),
            PointLight::from_candela([0.; 3], 14., [0.69, 0.84, 1.], 72.)
                .unwrap()
                .rgb_candela()
        );
    }

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
