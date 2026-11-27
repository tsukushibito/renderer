use super::{Environment, EnvironmentImage, PerspectiveView, ScenePackage, SceneSettings};
use crate::{BlobRef, MaterialSlot, ObjectStore};
use glam::{Mat4, Vec3};
use sashiko_renderer::{core::graph::ImageDesc, pipeline::EnvironmentMaps};
use sashiko_renderer::{
    core::runtime::{ManagedImage, Runtime},
    material::{MaterialHandle, Materials, PbrParams},
    mesh::{MeshCapacity, Meshes},
    pbr::{Bounds, Projection, Sun, Transform, ViewInput},
    pipeline::{
        AssetPreparation, CameraView, PipelineResult, RenderObject, SceneAssets, SceneInput, ViewId,
    },
};
use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, Instant},
};

impl PerspectiveView {
    pub fn camera(&self, extent: [u32; 2], clip: [f32; 2]) -> PipelineResult<CameraView> {
        let eye = Vec3::from(self.position);
        let direction = Vec3::from(self.forward)
            .try_normalize()
            .ok_or("zero camera direction")?;
        let up = Vec3::from(self.up)
            .try_normalize()
            .ok_or("zero camera up")?;
        if direction.cross(up).length_squared() < f32::EPSILON
            || extent.contains(&0)
            || !(0. < self.vertical_fov_degrees && self.vertical_fov_degrees < 180.)
            || !(0. < clip[0] && clip[0] < clip[1])
        {
            return Err("invalid scene camera".into());
        }
        let aspect = extent[0] as f32 / extent[1] as f32;
        let mut projection = Mat4::perspective_rh(
            self.vertical_fov_degrees.to_radians(),
            aspect,
            clip[0],
            clip[1],
        );
        projection.y_axis.y *= -1.;
        if self.mirror_x {
            projection.x_axis.x *= -1.;
        }
        let matrix = projection * Mat4::look_to_rh(eye, direction, up);
        Ok(CameraView {
            id: ViewId(0),
            extent,
            camera: ViewInput::new(
                matrix.to_cols_array_2d(),
                self.position,
                direction.as_dvec3().to_array(),
                clip,
                Projection::SymmetricPerspective {
                    vertical_fov_degrees: self.vertical_fov_degrees,
                    aspect,
                },
            )?,
        })
    }
}

/// CPU-side draw registrations and device image owners for a cooked scene.
pub struct DisplayScene {
    store: ObjectStore,
    meshes: Meshes,
    materials: Materials,
    objects: Vec<RenderObject>,
    paints: Vec<(MaterialHandle, PbrParams, [Option<BlobRef>; 4])>,
    images: HashMap<BlobRef, ManagedImage>,
    environment: Option<Environment<BlobRef>>,
    environment_maps: Option<EnvironmentMaps>,
    environment_images: Vec<ManagedImage>,
    pub settings: SceneSettings,
    pub camera: CameraView,
    sun: Sun,
    bounds: Bounds,
}

impl DisplayScene {
    fn config(&self) -> sashiko_renderer::pipeline::StandardConfig {
        let mut config = sashiko_renderer::pipeline::StandardConfig::new(self.camera.extent);
        let [r, g, b] = self.settings.sky_color;
        config.clear_color = [r, g, b, 1.];
        config.shadows = Some(sashiko_renderer::pbr::ShadowSettings {
            resolution: self.settings.shadow_resolution,
            distance: self.settings.shadow_distance,
            ..Default::default()
        });
        config
    }

    pub fn headless(
        directory: &Path,
        view: &str,
        extent: [u32; 2],
        backend: sashiko_renderer::core::runtime::RuntimeConfig,
    ) -> PipelineResult<(Self, sashiko_renderer::Renderer)> {
        let mut scene = Self::open(directory, view, extent)?;
        let mut renderer = sashiko_renderer::Renderer::headless_configured(
            backend,
            scene.camera,
            scene.config(),
            scene.assets(),
            std::num::NonZeroUsize::MIN,
            |_| Ok(()),
        )?;
        scene.upload_materials(&mut renderer)?;
        renderer.prepare_scene(scene.assets())?;
        Ok((scene, renderer))
    }

    #[cfg(all(windows, feature = "vulkan-presentation"))]
    pub fn window(
        directory: &Path,
        view: &str,
        extent: [u32; 2],
        window: impl raw_window_handle::HasWindowHandle + 'static,
        backend: sashiko_renderer::core::runtime::RuntimeConfig,
    ) -> PipelineResult<(Self, sashiko_renderer::WindowRenderer)> {
        let mut scene = Self::open(directory, view, extent)?;
        let mut renderer = sashiko_renderer::WindowRenderer::new_configured(
            window,
            backend,
            scene.camera,
            scene.config(),
            scene.assets(),
            std::num::NonZeroUsize::MIN,
            |_| Ok(()),
        )?;
        scene.upload_materials(&mut renderer)?;
        renderer.prepare_scene(scene.assets())?;
        Ok((scene, renderer))
    }
    fn open(directory: &Path, view: &str, extent: [u32; 2]) -> PipelineResult<Self> {
        let scene = ScenePackage::open(directory)?;
        let camera = scene.view(view)?.camera(extent, [0.02, 1500.])?;
        let settings = scene.settings;
        let sun = Sun::from_lux(settings.sun_direction, settings.sun_color, settings.sun_lux)?;
        let store = ObjectStore::new(directory);
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: scene.primitives.len(),
            retired_mesh_versions: 1,
        })?;
        let mut materials = Materials::default();
        let mut mesh_handles = HashMap::new();
        let mut mat_handles = HashMap::new();
        let mut paints = Vec::new();
        let mut parts = Vec::new();
        for part in scene.primitives {
            let mesh = match mesh_handles.entry(part.mesh) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let bytes = store.load(&part.mesh)?;
                    let blob = bytes.mesh()?;
                    let mut min = Vec3::splat(f32::INFINITY);
                    let mut max = Vec3::splat(f32::NEG_INFINITY);
                    for v in &blob.vertices {
                        min = min.min(v.position.into());
                        max = max.max(v.position.into());
                    }
                    *e.insert((meshes.create(blob.mesh_data()?)?, min, max))
                }
            };
            let material = match mat_handles.entry((part.material, part.images)) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let params = store.load(&part.material)?.material()?.parameters()?;
                    let id = materials.create_pbr(params.clone())?;
                    paints.push((id, params, part.images));
                    *e.insert(id)
                }
            };
            parts.push((mesh, material));
        }
        let mut objects = Vec::with_capacity(scene.placements.len());
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for placement in scene.placements {
            let ((mesh, lo, hi), material) = parts[placement.primitive];
            let t = Transform::new(std::array::from_fn(|r| {
                placement.transform.map(|col| col[r])
            }))?;
            for bits in 0..8 {
                let p = t.transform_point(std::array::from_fn(|i| {
                    if bits & (1 << i) == 0 { lo[i] } else { hi[i] }
                }));
                min = min.min(p.into());
                max = max.max(p.into());
            }
            objects.push(RenderObject::new(mesh, material).with_transform(t));
        }
        Ok(Self {
            store,
            meshes,
            materials,
            objects,
            paints,
            images: HashMap::new(),
            environment: scene.environment,
            environment_maps: None,
            environment_images: Vec::new(),
            settings,
            camera,
            sun,
            bounds: Bounds::new(min.to_array(), max.to_array())?,
        })
    }

    pub fn assets(&mut self) -> SceneAssets<'_> {
        SceneAssets::new(&mut self.meshes, &mut self.materials, &self.objects)
    }

    pub fn input(&self) -> SceneInput<'_> {
        let mut input = SceneInput::new(&self.meshes, &self.materials, &self.objects)
            .with_sun(self.sun)
            .with_bounds(self.bounds);
        input.environment = self.environment_maps.as_ref();
        input
    }

    /// Initial preparation on a fresh renderer at a scene change boundary.
    fn upload_materials(&mut self, renderer: &mut impl AssetPreparation) -> PipelineResult<()> {
        let mut descriptors = HashMap::new();
        for (_, _, references) in &self.paints {
            for slot in MaterialSlot::ALL {
                if let Some(reference) = references[slot.index()] {
                    let object = self.store.load(&reference)?;
                    let desc = object.image()?.texture_desc(slot)?;
                    descriptors.insert(reference, (desc, object));
                }
            }
        }
        let env_desc = |image: &EnvironmentImage<BlobRef>| {
            let mut desc = ImageDesc::rgba16_float(image.width, image.height);
            desc.mip_levels = image.mip_levels;
            desc
        };
        let mut descs: Vec<_> = descriptors.values().map(|(desc, _)| *desc).collect();
        if let Some(env) = &self.environment {
            descs.extend(env.maps.iter().map(env_desc));
        }
        let samplers: Vec<_> = self
            .paints
            .iter()
            .flat_map(|(_, p, _)| MaterialSlot::ALL.map(|s| *s.texture(&p.samplers)))
            .collect();
        if renderer
            .replace_asset_reservations("scene texture", &[], &descs, &samplers)?
            .is_pending()
        {
            return Err("scene upload requires an idle renderer".into());
        }
        for (reference, (desc, object)) in descriptors {
            let image = object.image()?;
            let owner = upload(renderer, desc, &image.pixels)?;
            self.images.insert(reference, owner);
        }
        if let Some(env) = &self.environment {
            for map in &env.maps {
                let bytes = self.store.load(&map.data)?;
                self.environment_images
                    .push(upload(renderer, env_desc(map), &bytes)?);
            }
            let images = &self.environment_images;
            self.environment_maps = Some(EnvironmentMaps {
                irradiance: images[0].snapshot().clone(),
                reflection: images[1].snapshot().clone(),
                brdf_lut: images[2].snapshot().clone(),
                nits_per_unit: env.nits_per_unit,
                yaw_radians: env.yaw_radians,
                max_prefilter_lod: env.maps[1].mip_levels - 1,
            });
        }
        for (handle, params, refs) in &mut self.paints {
            let [color, normal, orm, emissive] =
                refs.map(|r| r.map(|r| self.images[&r].snapshot().clone().into()));
            params.textures = sashiko_renderer::material::MaterialTextures {
                base_color: color,
                normal,
                orm,
                emissive,
            };
            self.materials.update_pbr(*handle, params.clone())?;
        }
        Ok(())
    }

    pub fn release(mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        self.meshes.release_gpu(runtime)?;
        self.materials.clear(runtime)?;
        Ok(())
    }
}

fn upload(
    renderer: &mut impl AssetPreparation,
    desc: ImageDesc,
    bytes: &[u8],
) -> PipelineResult<ManagedImage> {
    renderer.check_texture_support(desc, true)?;
    let (id, owner) =
        renderer.upload_texture_mips("scene texture", desc, 0..desc.mip_levels, bytes)?;
    let start = Instant::now();
    while !renderer.poll_texture_upload(id)? {
        if start.elapsed() > Duration::from_secs(120) {
            return Err("scene texture upload timed out".into());
        }
        std::thread::yield_now();
    }
    Ok(owner)
}
