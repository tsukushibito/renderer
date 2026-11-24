//! Editable room geometry and survivor-specific spawn and marker placements.
use crate::PLAYER_RADIUS;
use sashiko::space::Floor;
pub use sashiko::space::Footprint as Obstacle;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Arena {
    pub half_extent: [f32; 2],
    pub player_spawn: [f32; 2],
    pub marker: [f32; 2],
    pub obstacles: Vec<Obstacle>,
    #[serde(default = "crate::authoring::default_subjects")]
    pub subjects: Vec<crate::authoring::SubjectType>,
}

impl Default for Arena {
    fn default() -> Self {
        Self::from_json(include_str!("../../../assets/survivor/arena.json")).expect("bundled arena")
    }
}

impl Arena {
    pub fn from_json(source: &str) -> Result<Self, String> {
        let a = crate::authoring::decode_arena(source)?;
        crate::authoring::validate_subjects(&a.subjects)?;
        if !a
            .half_extent
            .into_iter()
            .all(|v| (2.0..=128.0).contains(&v))
        {
            return Err("arena half extents must be between 2 and 128 metres".to_owned());
        }
        a.floor().validate()?;
        if !a.floor().open(a.player_spawn, PLAYER_RADIUS)
            || !a.floor().open(a.marker, PLAYER_RADIUS)
        {
            return Err("spawn point and marker must be on passable floor".to_owned());
        }
        Ok(a)
    }

    pub fn floor(&self) -> Floor<'_> {
        Floor {
            half_extent: self.half_extent,
            obstacles: &self.obstacles,
        }
    }
}
