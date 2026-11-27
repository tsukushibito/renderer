//! Runtime scene package. Mesh payloads stay in local space and placements use
//! column-major affine matrices in metres.
use crate::{BlobRef, Primitive, invalid};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io, path::Path};

#[cfg(feature = "renderer")]
mod display;
#[cfg(feature = "renderer")]
pub use display::DisplayScene;
#[cfg(feature = "renderer")]
mod optics;
#[cfg(feature = "renderer")]
pub use optics::{FilmProjection, FilmRay, FilmRayBasis, SkyRadiance};
#[cfg(feature = "renderer")]
mod spatial;
#[cfg(feature = "renderer")]
pub use spatial::{PlacementVisibility, SceneSpatialIndex};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_split_lambda: Option<f32>,
    #[serde(default)]
    pub fxaa: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Placement {
    pub primitive: usize,
    /// Stable key into the baked surface index, independent of spatial selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lightmap: Option<u32>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baked: Option<SceneBake>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical: Option<PhysicalScene>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backdrop: Option<InfiniteSky>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mesh_lods: Vec<MeshLod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<Attribution>,
}

#[derive(Serialize, Deserialize)]
pub struct SceneBake {
    pub index: BlobRef,
    pub pages: Vec<BlobRef>,
    pub diffuse: BlobRef,
    pub reflections: BlobRef,
    /// Normalized little-endian UV pairs keyed by the complete mesh digest (including LODs).
    pub mesh_uvs: BTreeMap<crate::Digest, BlobRef>,
}

#[derive(Serialize, Deserialize)]
pub struct MeshLod {
    pub source: BlobRef,
    pub reduced: BlobRef,
    pub min_pixels: f32,
}

#[derive(Serialize, Deserialize)]
pub struct Attribution {
    pub title: String,
    pub authors: String,
    pub source: String,
    pub license: String,
    pub changes: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct PhysicalScene {
    /// Source displacement heights are measured in the imported scene's units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub displacement_unit_metres: Option<f32>,
    pub cameras: BTreeMap<String, FilmCamera>,
    pub sky: Option<InfiniteSky>,
    /// Same primitive index as the draw list. Values and images retain the
    /// source BSDF independently of the interactive base-color approximation.
    pub surfaces: Vec<Surface>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct FilmCamera {
    pub lens_radius_metres: f32,
    pub focus_metres: f32,
    pub iso: f32,
    pub white_balance_kelvin: Option<f32>,
    pub film_extent: [u32; 2],
    pub samples_per_pixel: u32,
}

#[derive(Serialize, Deserialize)]
pub struct InfiniteSky {
    pub equal_area_radiance: EnvironmentImage<BlobRef>,
    pub world_from_light: [[f32; 4]; 4],
    pub scale: f32,
}

#[derive(Serialize, Deserialize)]
pub enum Scattering {
    Diffuse,
    Conductor,
    CoatedDiffuse,
    DiffuseTransmission,
    Dielectric,
}

#[derive(Serialize, Deserialize)]
pub struct Surface {
    pub scattering: Scattering,
    pub reflectance: [f32; 3],
    pub color: Option<BlobRef>,
    /// DiffuseTransmission without an image uses the PBRT default 0.25 in every channel.
    pub transmittance: Option<BlobRef>,
    pub displacement: Option<BlobRef>,
    pub alpha: Option<BlobRef>,
    pub eta: f32,
    pub roughness: f32,
    pub scale: f32,
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
        if self
            .settings
            .shadow_split_lambda
            .is_some_and(|v| !(0.0..=1.0).contains(&v))
        {
            return Err(invalid("invalid shadow split fraction"));
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
        let lods: BTreeMap<_, _> = self
            .mesh_lods
            .iter()
            .map(|lod| (lod.source.hash, lod.reduced.hash))
            .collect();
        let mut mapped = std::collections::BTreeSet::new();
        for placement in self.placements.iter().filter(|p| p.lightmap.is_some()) {
            let bake = self
                .baked
                .as_ref()
                .ok_or_else(|| invalid("missing surface lighting"))?;
            let mesh = self.primitives[placement.primitive].mesh.hash;
            if !mapped.insert(placement.lightmap)
                || !bake.mesh_uvs.contains_key(&mesh)
                || lods
                    .get(&mesh)
                    .is_some_and(|lod| !bake.mesh_uvs.contains_key(lod))
            {
                return Err(invalid(
                    "duplicate lightmap placement or missing mesh UV stream",
                ));
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
        if !self.mesh_lods.is_empty() {
            let mut available: std::collections::HashSet<_> =
                self.primitives.iter().map(|part| part.mesh).collect();
            for lod in &self.mesh_lods {
                if !lod.min_pixels.is_finite()
                    || lod.min_pixels <= 0.
                    || lod.source == lod.reduced
                    || !available.remove(&lod.source)
                {
                    return Err(invalid("invalid or duplicate mesh LOD"));
                }
            }
        }
        if let Some(physical) = &self.physical {
            if physical
                .displacement_unit_metres
                .is_some_and(|unit| !unit.is_finite() || unit <= 0.)
            {
                return Err(invalid("invalid displacement unit"));
            }
            if physical.surfaces.iter().any(|s| {
                !s.eta.is_finite()
                    || s.eta <= 0.
                    || !s.roughness.is_finite()
                    || s.roughness < 0.
                    || !s.scale.is_finite()
                    || s.scale < 0.
                    || s.reflectance.iter().any(|v| !v.is_finite())
            }) {
                return Err(invalid("invalid physical surface parameters"));
            }
            if physical.surfaces.len() != self.primitives.len()
                || physical
                    .cameras
                    .keys()
                    .any(|k| !self.settings.views.contains_key(k))
            {
                return Err(invalid(
                    "physical scene bindings do not match the draw scene",
                ));
            }
            for camera in physical.cameras.values() {
                if camera.film_extent.contains(&0)
                    || camera.samples_per_pixel == 0
                    || !(camera.lens_radius_metres >= 0. && camera.lens_radius_metres.is_finite())
                    || !(camera.focus_metres > 0. && camera.focus_metres.is_finite())
                    || !(camera.iso > 0. && camera.iso.is_finite())
                    || camera
                        .white_balance_kelvin
                        .is_some_and(|k| !k.is_finite() || k <= 0.)
                {
                    return Err(invalid("invalid film or lens settings"));
                }
            }
        }
        for sky in self
            .backdrop
            .iter()
            .chain(self.physical.iter().filter_map(|p| p.sky.as_ref()))
        {
            sky.equal_area_radiance.validate()?;
            if sky.scale < 0.
                || !sky.scale.is_finite()
                || sky.equal_area_radiance.width != sky.equal_area_radiance.height
                || sky
                    .world_from_light
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite())
            {
                return Err(invalid("invalid equal-area infinite light"));
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
