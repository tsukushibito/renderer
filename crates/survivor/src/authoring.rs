//! Research hall components registered with the shared scene authoring API.
use crate::{Arena, Obstacle};
use sashiko::scene::{Registry, Scene};
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
    #[derive(Deserialize)]
    struct Defaults {
        prefabs: std::collections::BTreeMap<String, sashiko::scene::Prefab>,
    }
    let defaults: Defaults =
        serde_json::from_str(include_str!("../../../assets/survivor/arena.json"))
            .expect("bundled scene prefabs");
    ["lunger", "walker"]
        .map(|id| {
            serde_json::from_value(Value::Object(defaults.prefabs[id].fields.clone()))
                .expect("bundled subject")
        })
        .into()
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

pub fn registry() -> Registry {
    let mut types = Registry::default();
    for name in ["Room", "Bench"] {
        types
            .register(name, &["half_extent"], Extent::validate)
            .expect("unique registered type");
    }
    for name in ["PlayerSpawn", "Marker"] {
        types
            .register::<Empty>(name, &[], |_| Ok(()))
            .expect("unique registered type");
    }
    types
        .register(
            "Subject",
            &["copies", "hp", "walk_speed", "model", "clip", "lunge"],
            SubjectType::validate,
        )
        .expect("unique registered type");
    types
}

pub(crate) fn decode_arena(source: &str) -> Result<Arena, String> {
    let data: Value = serde_json::from_str(source).map_err(|e| e.to_string())?;
    if data.get("schema_version").is_none() {
        return serde_json::from_value(data).map_err(|e| e.to_string());
    }
    let scene: Scene = serde_json::from_value(data).map_err(|e| e.to_string())?;
    let world = scene.instantiate(&registry())?;
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
        let fields = || Value::Object(e.fields.clone());
        let half_extent = || {
            serde_json::from_value::<Extent>(fields())
                .map(|c| c.half_extent)
                .map_err(|e| e.to_string())
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
            "Subject" => {
                subjects.push(serde_json::from_value(fields()).map_err(|e| e.to_string())?)
            }
            other => return Err(format!("unsupported research hall component {other}")),
        }
    }
    Ok(Arena {
        half_extent: extent.ok_or("missing Room")?,
        player_spawn: spawn.ok_or("missing PlayerSpawn")?,
        marker: marker.ok_or("missing Marker")?,
        obstacles,
        subjects,
    })
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
