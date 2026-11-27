//! Runtime scene package. Mesh payloads stay in local space and placements use
//! column-major affine matrices in metres.
use crate::{BlobRef, Primitive, invalid};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io, path::Path};

#[cfg(feature = "renderer")]
mod display;
#[cfg(feature = "renderer")]
pub use display::DisplayScene;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerspectiveView {
    pub position: [f32; 3],
    pub forward: [f32; 3],
    pub up: [f32; 3],
    pub vertical_fov_degrees: f32,
    pub mirror_x: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSettings {
    pub views: BTreeMap<String, PerspectiveView>,
    pub sun_direction: [f32; 3],
    pub sun_color: [f32; 3],
    pub sun_lux: f32,
    pub sky_color: [f32; 3],
    pub shadow_resolution: u32,
    pub shadow_distance: f32,
}

#[derive(Serialize, Deserialize)]
pub struct Placement {
    pub primitive: usize,
    pub transform: [[f32; 4]; 4],
}

#[derive(Serialize, Deserialize)]
pub struct ScenePackage {
    pub version: u32,
    pub profile: SceneProfile,
    pub environment: Option<Environment<BlobRef>>,
    pub settings: SceneSettings,
    pub primitives: Vec<Primitive>,
    pub placements: Vec<Placement>,
}

#[derive(Serialize, Deserialize)]
pub enum SceneProfile {
    #[serde(rename = "pbr-rgba8-rgba16f")]
    PbrRgba8Rgba16f,
}

/// Tightly packed linear RGBA16F maps, in irradiance, reflection and BRDF order.
#[derive(Serialize, Deserialize)]
pub struct Environment<T> {
    pub maps: [EnvironmentImage<T>; 3],
    pub nits_per_unit: f32,
    pub yaw_radians: f32,
}

#[derive(Serialize, Deserialize)]
pub struct EnvironmentImage<T> {
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub data: T,
}

impl EnvironmentImage<BlobRef> {
    pub fn validate(&self) -> io::Result<()> {
        if self.width == 0
            || self.height == 0
            || self.mip_levels == 0
            || self.mip_levels > self.width.max(self.height).ilog2() + 1
        {
            return Err(invalid("invalid RGBA16F environment dimensions"));
        }
        let size = (0..self.mip_levels).try_fold(0u64, |size, level| {
            let pixels =
                u64::from((self.width >> level).max(1)) * u64::from((self.height >> level).max(1));
            size.checked_add(pixels.checked_mul(8)?)
        });
        if size != Some(self.data.size) {
            return Err(invalid("environment map length mismatch"));
        }
        Ok(())
    }
}

impl ScenePackage {
    pub fn open(directory: &Path) -> io::Result<Self> {
        let scene: Self =
            serde_json::from_slice(&fs::read(directory.join("scene.json"))?).map_err(invalid)?;
        scene.validate()?;
        Ok(scene)
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.version != 1 || self.settings.views.is_empty() || self.placements.is_empty() {
            return Err(invalid("unsupported or empty scene package"));
        }
        for placement in &self.placements {
            let m = &placement.transform;
            if placement.primitive >= self.primitives.len()
                || m.iter().flatten().any(|x| !x.is_finite())
                || m.each_ref().map(|column| column[3]) != [0., 0., 0., 1.]
            {
                return Err(invalid("invalid scene placement"));
            }
        }
        if let Some(env) = &self.environment {
            if !env.yaw_radians.is_finite()
                || !env.nits_per_unit.is_finite()
                || env.nits_per_unit < 0.
            {
                return Err(invalid("invalid environment lighting"));
            }
            for image in &env.maps {
                image.validate()?;
            }
        }
        Ok(())
    }

    pub fn view(&self, name: &str) -> io::Result<&PerspectiveView> {
        self.settings
            .views
            .get(name)
            .ok_or_else(|| invalid(format!("unknown scene view {name:?}")))
    }
}
