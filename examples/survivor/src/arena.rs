//! Editable room geometry and survivor-specific spawn and marker placements.
use crate::PLAYER_RADIUS;
use sashiko::math::Vec2;
use sashiko::space::Floor;
pub use sashiko::space::Footprint as Obstacle;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Arena {
    pub half_extent: Vec2,
    pub player_spawn: Vec2,
    pub marker: Vec2,
    pub obstacles: Vec<Obstacle>,
    #[serde(default)]
    pub cargo: Option<Obstacle>,
    #[serde(default = "crate::authoring::default_subjects")]
    pub subjects: Vec<crate::authoring::SubjectType>,
    #[serde(skip)]
    pub(crate) view: Option<crate::authoring::LabView>,
    #[serde(skip)]
    pub(crate) authored_scene: Option<std::sync::Arc<sashiko::scene::Scene>>,
}

impl Default for Arena {
    fn default() -> Self {
        Self::from_json(include_str!("../../../assets/survivor/arena.json")).expect("bundled arena")
    }
}

impl Arena {
    pub fn from_json(source: &str) -> Result<Self, String> {
        Self::from_value(serde_json::from_str(source).map_err(|e| e.to_string())?)
    }

    pub fn from_value(source: serde_json::Value) -> Result<Self, String> {
        let a = crate::authoring::decode_arena(source)?;
        crate::authoring::validate_subjects(&a.subjects)?;
        if !a
            .half_extent
            .to_array()
            .into_iter()
            .all(|v| (2.0..=128.0).contains(&v))
        {
            return Err("arena half extents must be between 2 and 128 metres".to_owned());
        }
        a.floor().validate()?;
        if let Some(b) = a.cargo {
            Floor {
                half_extent: a.half_extent,
                obstacles: std::slice::from_ref(&b),
            }
            .validate()?;
            if a.obstacles.iter().any(|o| {
                (0..2).all(|i| {
                    (b.center[i] - o.center[i]).abs() < b.half_extent[i] + o.half_extent[i]
                })
            }) || [a.player_spawn, a.marker].into_iter().any(|p| {
                (0..2).all(|i| (p[i] - b.center[i]).abs() < b.half_extent[i] + PLAYER_RADIUS)
            }) {
                return Err("cargo must start clear of benches, spawn and marker".into());
            }
        }
        if !a.floor().open(a.player_spawn, PLAYER_RADIUS)
            || !a.floor().open(a.marker, PLAYER_RADIUS)
        {
            return Err("spawn point and marker must be on passable floor".to_owned());
        }
        Ok(a)
    }

    /// Prepare camera and lighting once when opening a view. Legacy layouts
    /// derive the authored lighting rig from their own floor dimensions.
    pub fn prepare_view(&self) -> Result<std::borrow::Cow<'_, crate::authoring::LabView>, String> {
        match &self.view {
            Some(view) => Ok(std::borrow::Cow::Borrowed(view)),
            None => Ok(std::borrow::Cow::Owned(crate::authoring::LabView::legacy(
                self.half_extent,
            )?)),
        }
    }
    pub fn floor(&self) -> Floor<'_> {
        Floor {
            half_extent: self.half_extent,
            obstacles: &self.obstacles,
        }
    }
}
