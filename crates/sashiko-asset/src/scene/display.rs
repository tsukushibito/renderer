use super::{
    Environment, EnvironmentImage, FilmProjection, InfiniteSky, PerspectiveView, ScenePackage,
    SceneSettings,
};
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
    bake: Option<(
        sashiko_renderer::pbr::SurfaceLightmaps,
        sashiko_renderer::pbr::ReflectionProbes,
    )>,
    lighting: Option<sashiko_renderer::pipeline::ResidentBakedLighting>,
    store: ObjectStore,
    meshes: Meshes,
    materials: Materials,
    objects: Vec<RenderObject>,
    lods: Vec<PlacementLod>,
    lod_pairs: Vec<(sashiko_renderer::mesh::MeshHandle, MaterialHandle)>,
    paints: Vec<(MaterialHandle, PbrParams, [Option<BlobRef>; 4])>,
    images: HashMap<BlobRef, ManagedImage>,
    environment: Option<Environment<BlobRef>>,
    environment_maps: Option<EnvironmentMaps>,
    environment_images: Vec<ManagedImage>,
    filter_backdrop: bool,
    film: Option<(FilmProjection, InfiniteSky, glam::Mat3)>,
    sky_image: Option<ManagedImage>,
    pub settings: SceneSettings,
    pub camera: CameraView,
    sun: Sun,
    bounds: Bounds,
}

struct PlacementLod {
    object: usize,
    full: sashiko_renderer::mesh::MeshHandle,
    coarse: sashiko_renderer::mesh::MeshHandle,
    center: Vec3,
    diameter: f32,
    min_pixels: f32,
}

impl DisplayScene {
    /// Choose from the geometry prepared at scene load. The same draw is used for
    /// the visible surface and its directional shadow in this frame.
    pub fn select_lods(&mut self, view: &PerspectiveView, height: u32) {
        let eye = Vec3::from(view.position);
        let focal = 0.5 * height as f32 / (0.5 * view.vertical_fov_degrees.to_radians()).tan();
        for lod in &self.lods {
            // Keep the current level inside a 10% distance band to prevent
            // subpixel camera jitter repeatedly changing silhouettes.
            let mesh = &mut self.objects[lod.object].mesh;
            let margin = if *mesh == lod.full { 1.05 } else { 0.95 };
            let cutoff = focal * lod.diameter / lod.min_pixels * margin;
            *mesh = if lod.center.distance_squared(eye) <= cutoff * cutoff {
                lod.full
            } else {
                lod.coarse
            };
        }
    }
    fn config(&self) -> sashiko_renderer::pipeline::StandardConfig {
        let mut config = sashiko_renderer::pipeline::StandardConfig::new(self.camera.extent);
        let [r, g, b] = self.settings.sky_color;
        config.film_output = self.film.is_some();
        config.fxaa = self.settings.fxaa;
        config.clear_color = if config.film_output {
            [0.; 4]
        } else {
            [r, g, b, 1.]
        };
        config.shadows = self.settings.direct_shadows();
        config
    }

    pub fn headless(
        directory: &Path,
        view: &str,
        extent: [u32; 2],
        backend: sashiko_renderer::core::runtime::RuntimeConfig,
    ) -> PipelineResult<(Self, sashiko_renderer::Renderer)> {
        Self::open(directory, view, extent, None)?.prepare_headless(backend)
    }

    /// Prepare a fixed set of camera samples. The selected meshes and casters
    /// cover all supplied views, including offscreen directional casters.
    pub fn headless_views(
        directory: &Path,
        view: &str,
        samples: &[CameraView],
        backend: sashiko_renderer::core::runtime::RuntimeConfig,
    ) -> PipelineResult<(Self, sashiko_renderer::Renderer)> {
        let first = samples
            .first()
            .ok_or("fixed capture needs at least one view")?;
        if samples.iter().any(|c| c.extent != first.extent) {
            return Err("fixed capture views must share an extent".into());
        }
        let mut scene = Self::open(directory, view, first.extent, Some(samples))?;
        scene.camera = *first;
        scene.prepare_headless(backend)
    }

    fn prepare_headless(
        mut self,
        backend: sashiko_renderer::core::runtime::RuntimeConfig,
    ) -> PipelineResult<(Self, sashiko_renderer::Renderer)> {
        let mut renderer = sashiko_renderer::Renderer::headless_configured(
            backend,
            self.camera,
            self.config(),
            self.assets(),
            std::num::NonZeroUsize::MIN,
            |_| Ok(()),
        )?;
        self.upload_materials(&mut renderer)?;
        if let Some((surfaces, reflections)) = self.bake.take() {
            self.lighting = Some(renderer.load_surface_lighting(&surfaces, Some(&reflections))?);
        }
        renderer.prepare_scene(self.assets())?;
        Ok((self, renderer))
    }

    #[cfg(all(windows, feature = "vulkan-presentation"))]
    pub fn window(
        directory: &Path,
        view: &str,
        extent: [u32; 2],
        window: impl raw_window_handle::HasWindowHandle + 'static,
        backend: sashiko_renderer::core::runtime::RuntimeConfig,
    ) -> PipelineResult<(Self, sashiko_renderer::WindowRenderer)> {
        let mut scene = Self::open(directory, view, extent, None)?;
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
        if let Some((surfaces, reflections)) = scene.bake.take() {
            scene.lighting = Some(renderer.load_surface_lighting(&surfaces, Some(&reflections))?);
        }
        renderer.prepare_scene(scene.assets())?;
        Ok((scene, renderer))
    }
    fn open(
        directory: &Path,
        view: &str,
        extent: [u32; 2],
        samples: Option<&[CameraView]>,
    ) -> PipelineResult<Self> {
        let mut scene = ScenePackage::open(directory)?;
        let store = ObjectStore::new(directory);
        let bake = scene
            .baked
            .as_ref()
            .map(|b| b.lighting(&store))
            .transpose()?;
        if let Some((surfaces, _)) = &bake
            && scene
                .placements
                .iter()
                .filter_map(|p| p.lightmap)
                .any(|id| !surfaces.placements().contains_key(&id))
        {
            return Err("placement absent from baked surface index".into());
        }
        let all_bounds = if let Some(samples) = samples {
            Some(super::spatial::select(&mut scene, &store, samples)?)
        } else {
            None
        };
        let pose = *scene.view(view)?;
        let camera = pose.camera(extent, [0.02, 1500.])?;
        let physical_film = scene
            .physical
            .take()
            .and_then(|mut p| p.cameras.remove(view).zip(p.sky));
        let filter_backdrop = physical_film.is_none() && scene.backdrop.is_some();
        let film = physical_film
            .or_else(|| {
                scene.backdrop.take().map(|sky| {
                    (
                        super::FilmCamera {
                            lens_radius_metres: 0.,
                            focus_metres: 1.,
                            iso: 100.,
                            white_balance_kelvin: None,
                            film_extent: extent,
                            samples_per_pixel: 1,
                        },
                        sky,
                    )
                })
            })
            .map(|(film, sky)| -> PipelineResult<_> {
                let transform =
                    glam::Mat3::from_mat4(Mat4::from_cols_array_2d(&sky.world_from_light))
                        .inverse();
                Ok((FilmProjection::new(&pose, &film, extent)?, sky, transform))
            })
            .transpose()?;
        if film.as_ref().is_some_and(|(_, _, m)| !m.is_finite()) {
            return Err("singular infinite light transform".into());
        }
        let settings = scene.settings;
        let sun = Sun::from_lux(settings.sun_direction, settings.sun_color, settings.sun_lux)?;
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: (scene.primitives.len() + scene.mesh_lods.len()).max(1),
            retired_mesh_versions: 1,
        })?;
        let mut materials = Materials::default();
        let mut mesh_handles = HashMap::new();
        let mut load_mesh = |reference: BlobRef| -> PipelineResult<_> {
            Ok(match mesh_handles.entry(reference) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let bytes = store.load(&reference)?;
                    let blob = bytes.mesh()?;
                    let mut data = blob.mesh_data()?;
                    if let Some(uv) = scene
                        .baked
                        .as_ref()
                        .and_then(|b| b.mesh_uvs.get(&reference.hash))
                    {
                        let bytes = store.load(uv)?;
                        let (pairs, remainder) = bytes.as_chunks::<8>();
                        if !remainder.is_empty() || pairs.len() != blob.vertices.len() {
                            return Err("invalid baked mesh UV stream length".into());
                        }
                        let uvs: Vec<_> = pairs
                            .iter()
                            .map(|p| {
                                [
                                    f32::from_le_bytes(p[..4].try_into().unwrap()),
                                    f32::from_le_bytes(p[4..].try_into().unwrap()),
                                ]
                            })
                            .collect();
                        data = data.with_lightmap_uvs(&uvs)?;
                    }
                    *e.insert((meshes.create(data)?, blob.local_bounds()?))
                }
            })
        };
        let mut mat_handles = HashMap::new();
        let mut paints = Vec::new();
        let mut parts = Vec::new();
        let mut coarse_meshes = HashMap::new();
        for lod in &scene.mesh_lods {
            let coarse = load_mesh(lod.reduced)?;
            coarse_meshes.insert(lod.source, (coarse.0, lod.min_pixels));
        }
        let mut lod_pairs = Vec::new();
        let mut lods = Vec::new();
        for part in scene.primitives {
            let mesh = load_mesh(part.mesh)?;
            let material = match mat_handles.entry((part.material, part.images)) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let params = store.load(&part.material)?.material()?.parameters()?;
                    let id = materials.create_pbr(params.clone())?;
                    paints.push((id, params, part.images));
                    *e.insert(id)
                }
            };
            let coarse = coarse_meshes.get(&part.mesh).copied();
            if let Some((low, _)) = coarse {
                lod_pairs.push((mesh.0, material));
                lod_pairs.push((low, material));
            }
            parts.push((mesh, material, coarse));
        }
        let mut objects = Vec::with_capacity(scene.placements.len());
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for placement in scene.placements {
            let ((mesh, local), material, coarse) = parts[placement.primitive];
            let t = Transform::new(std::array::from_fn(|r| {
                placement.transform.map(|col| col[r])
            }))?;
            if all_bounds.is_none() {
                let world = t
                    .bounds(local)
                    .ok_or("unrepresentable static placement bounds")?;
                min = min.min(world.min().into());
                max = max.max(world.max().into());
            }
            if let Some((coarse, min_pixels)) = coarse {
                let world = t.bounds(local).ok_or("unrepresentable LOD bounds")?;
                let lo = Vec3::from(world.min());
                let hi = Vec3::from(world.max());
                lods.push(PlacementLod {
                    object: objects.len(),
                    full: mesh,
                    coarse,
                    min_pixels,
                    center: (hi + lo) * 0.5,
                    diameter: (hi - lo).length(),
                });
            }
            let mut object = RenderObject::new(mesh, material).with_transform(t);
            if let Some(id) = placement.lightmap {
                object = object.with_lightmap(id);
            }
            objects.push(object);
        }
        Ok(Self {
            bake,
            lighting: None,
            store,
            meshes,
            materials,
            objects,
            lods,
            lod_pairs,
            paints,
            images: HashMap::new(),
            environment: scene.environment,
            environment_maps: None,
            environment_images: Vec::new(),
            filter_backdrop,
            film,
            sky_image: None,
            settings,
            camera,
            sun,
            bounds: match all_bounds {
                Some(bounds) => bounds,
                None => Bounds::new(min.to_array(), max.to_array())?,
            },
        })
    }

    pub fn assets(&mut self) -> SceneAssets<'_> {
        let mut assets = SceneAssets::new(&mut self.meshes, &mut self.materials, &self.objects);
        for &(mesh, material) in &self.lod_pairs {
            assets.preload_pair(mesh, material);
        }
        assets
    }

    pub fn input(&self) -> SceneInput<'_> {
        let mut input = SceneInput::new(&self.meshes, &self.materials, &self.objects)
            .with_sun(self.sun)
            .with_bounds(self.bounds);
        input.environment = self.environment_maps.as_ref();
        if let Some(lighting) = &self.lighting {
            input = input.with_baked_lighting(lighting, 0.);
        }
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
        if let Some((_, sky, _)) = &self.film {
            descs.push(env_desc(&sky.equal_area_radiance));
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
        if let Some((_, sky, _)) = &self.film {
            let map = &sky.equal_area_radiance;
            self.sky_image = Some(upload(
                renderer,
                env_desc(map),
                &self.store.load(&map.data)?[..],
            )?);
        }
        Ok(())
    }

    /// Draw at the frame's actual scene resolution, including after window or
    /// dynamic-resolution changes. The selected film follows the moving view.
    pub fn draw(&self, frame: &mut sashiko_renderer::Frame<'_>) -> PipelineResult<()> {
        if let (Some((projection, sky, matrix)), Some(image)) = (&self.film, &self.sky_image) {
            frame.draw_film(
                self.input(),
                projection.output(
                    image.snapshot(),
                    *matrix,
                    sky.scale,
                    self.filter_backdrop,
                    frame.scene_view(),
                ),
            )?;
        } else {
            frame.draw_scene(self.input(), 1.)?;
        }
        Ok(())
    }

    pub fn release(mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        self.meshes.release_gpu(runtime)?;
        self.materials.clear(runtime)?;
        Ok(())
    }
}

impl super::SceneBake {
    fn lighting(
        &self,
        store: &ObjectStore,
    ) -> PipelineResult<(
        sashiko_renderer::pbr::SurfaceLightmaps,
        sashiko_renderer::pbr::ReflectionProbes,
    )> {
        use sashiko_renderer::pbr::{ReflectionProbes, SurfaceLightmapIndex};
        let index = SurfaceLightmapIndex::from_bytes(&store.load(&self.index)?[..])?;
        let diffuse = store.load(&self.diffuse)?;
        let pages: HashMap<_, _> = self.pages.iter().map(|p| (p.hash, p)).collect();
        let required: std::collections::HashSet<_> = index
            .pages()
            .iter()
            .map(|page| crate::Digest::from_sha256(*page.sha256()))
            .collect();
        if pages.len() != self.pages.len() || pages.len() != required.len() {
            return Err("surface page references do not match the baked index".into());
        }
        let surfaces = index.load(0..index.page_count(), Some(&diffuse), |hash| {
            let page = pages
                .get(&crate::Digest::from_sha256(*hash))
                .ok_or("missing surface page")?;
            Ok(store.load(page)?.to_vec())
        })?;
        Ok((
            surfaces,
            ReflectionProbes::from_bytes(&store.load(&self.reflections)?[..])?,
        ))
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

#[cfg(all(test, feature = "vulkan-presentation"))]
mod visibility_tests;
