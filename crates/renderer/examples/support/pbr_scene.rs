use renderer::material::{MaterialHandle, MaterialInstance, Materials, PbrParams};
use std::num::NonZeroUsize;

#[cfg(test)]
use renderer::pbr::{AlphaMode, Transform};
use renderer::{
    passes::{
        bloom::{Bloom, PreparedBloom},
        output::SdrOutput,
        pbr::{MATERIAL_BYTES, VERTEX_BYTES},
    },
    pbr::{
        Bounds, CASCADE_COUNT, CameraStorage, Config, DefaultResources, DefaultViews, Defaults,
        Draw, FrameShadows, GraphMaterial, IndexedDraw, LocalLight, MaterialFactors, PbrDrawInput,
        PbrVertex, PreparedDefaults, Projection, SceneLimits, ShadowHistory, ShadowSettings,
        ShadowUpdates, Sun, ViewInput,
    },
};
use renderer_core::{
    capacity::Preparation,
    geometry::GeometryData,
    graph::{
        BufferDesc, CompiledGraph, Graph, GraphError, GraphPool, ImageDesc, PassHandle, SampleCount,
    },
};

pub use renderer::{pbr::Technique, pipeline::GeometryPipeline as Prepared};

pub const EXTENT: [u32; 2] = [64, 48];
pub const SHADOW_RESOLUTION: u32 = 64;
pub const COPY_PASS: &str = "application HDR copy";

pub struct Triangle {
    geometry: GeometryData,
    materials: Materials,
    material: MaterialHandle,
    view: ViewFixture,
}

impl Triangle {
    #[cfg(test)]
    pub fn new(glowing: bool, placement: Placement) -> Result<Self, GraphError> {
        Self::for_view(glowing, placement, ViewFixture::Centered)
    }

    pub fn for_view(
        glowing: bool,
        placement: Placement,
        view: ViewFixture,
    ) -> Result<Self, GraphError> {
        let (normal, tangent, object_scale) = match placement {
            #[cfg(test)]
            Placement::Tilted(scale_exponent) => (
                [0.8, 0.0, 0.6],
                [0.0, 1.0, 0.0, 1.0],
                scale_exponent.map_or(1.0, |exponent| 2.0_f32.powi(-exponent)),
            ),
            _ => ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], 1.0),
        };
        #[cfg(test)]
        let baked_mirror = matches!(placement, Placement::Mirrored { baked: true, .. });
        #[cfg(not(test))]
        let baked_mirror = false;
        let mirror = if baked_mirror { -1.0 } else { 1.0 };
        let normal = view.orient([normal[0] * mirror, normal[1], normal[2]]);
        let [tx, ty, tz] = view.orient([tangent[0] * mirror, tangent[1], tangent[2]]);
        let tangent = [tx, ty, tz, tangent[3] * mirror * view.z_axis()];
        let positions = [
            ([-0.8_f32, -0.7, 0.0], [0.0, 1.0]),
            ([0.8, -0.7, 0.0], [1.0, 1.0]),
            ([0.0, 0.7, 0.0], [0.5, 0.0]),
        ];
        let mut vertices = [0; 3 * VERTEX_BYTES];
        for ((position, uv), bytes) in positions.into_iter().zip(vertices.as_chunks_mut().0) {
            let [x, y, z] = position.map(|value| value * object_scale);
            let position = [x * mirror, y, z];
            *bytes = PbrVertex::new(view.orient(position), normal, tangent, uv)?.encode();
        }
        let indices = if baked_mirror { [0, 2, 1] } else { [0, 1, 2] };
        let material = MaterialFactors {
            base_color: [0.75, 0.015, 0.009, 1.0],
            emissive: [if glowing { 2.0 } else { 0.0 }, 0.0, 0.0],
            #[cfg(test)]
            alpha_mode: if matches!(view, ViewFixture::LeftHandedPerspective { .. }) {
                AlphaMode::Mask
            } else {
                AlphaMode::Opaque
            },
            #[cfg(test)]
            double_sided: matches!(
                view,
                ViewFixture::LeftHandedPerspective { double_sided: true }
            ),
            ..MaterialFactors::default()
        };
        #[cfg(test)]
        let material = if matches!(placement, Placement::Mirrored { blend: true, .. }) {
            MaterialFactors {
                base_color: [0.75, 0.015, 0.009, 0.5],
                alpha_mode: AlphaMode::Blend,
                ..material
            }
        } else {
            material
        };
        let mut materials = Materials::default();
        let material = materials.create_pbr(PbrParams {
            factors: material,
            ..Default::default()
        })?;
        Ok(Self {
            materials,
            geometry: GeometryData::new(VERTEX_BYTES as u32, &vertices, &indices)?,
            material,
            view,
        })
    }

    fn buffer_descs(&self) -> [BufferDesc; 3] {
        [
            self.geometry.vertex_byte_len(),
            self.geometry.index_byte_len(),
            MATERIAL_BYTES as u64,
        ]
        .map(|size| BufferDesc { size })
    }

    pub fn reserve(&self, preparation: &mut Preparation, copy_hdr: bool) -> Result<(), GraphError> {
        for desc in self.buffer_descs() {
            preparation.reserve_buffer(desc, 1)?;
        }
        if copy_hdr {
            preparation.reserve_image(hdr_desc(), 1)?;
        }
        Ok(())
    }

    fn upload(
        &self,
        graph: &mut Graph,
        defaults: &DefaultViews,
    ) -> Result<(IndexedDraw, [GraphMaterial; 1]), GraphError> {
        let [vertex_desc, index_desc, _] = self.buffer_descs();
        let vertices = graph.buffer("triangle vertices", vertex_desc)?;
        let indices = graph.buffer("triangle indices", index_desc)?;
        self.geometry
            .upload_vertices(graph, "upload vertices", vertices)?;
        self.geometry
            .upload_indices(graph, "upload indices", indices)?;
        let MaterialInstance::Pbr(material) = self.materials.get(self.material)? else {
            unreachable!("triangle uses PBR")
        };

        Ok((
            IndexedDraw {
                vertices,
                indices,
                first_index: 0,
                index_count: self.geometry.index_count(),
                material: 0,
            },
            [material.resolve(graph, "triangle material factors", Some(*defaults))?],
        ))
    }
}

fn hdr_desc() -> ImageDesc {
    ImageDesc::rgba16_float(EXTENT[0], EXTENT[1])
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Placement {
    Single,
    #[cfg(test)]
    Individual,
    #[cfg(test)]
    Instanced,
    #[cfg(test)]
    PagedInstanced,
    #[cfg(test)]
    Tilted(Option<i32>),
    #[cfg(test)]
    Mirrored {
        baked: bool,
        blend: bool,
    },
}

impl Placement {
    fn uses_transforms(self) -> bool {
        match self {
            Self::Single => false,
            #[cfg(test)]
            Self::Tilted(exponent) => exponent.is_some(),
            #[cfg(test)]
            Self::Mirrored { baked, .. } => !baked,
            #[cfg(test)]
            Self::Individual | Self::Instanced | Self::PagedInstanced => true,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Settings<'a> {
    pub frame_lights: &'a [&'a [LocalLight]],
    pub technique: Technique,
    pub samples: SampleCount,
    pub placement: Placement,
    pub view: ViewFixture,
}

impl<'a> Settings<'a> {
    fn local_lights(self) -> &'a [LocalLight] {
        self.frame_lights
            .first()
            .copied()
            .unwrap_or(if self.technique == Technique::ForwardPlus {
                &POINT_LIGHTS
            } else {
                &[]
            })
    }

    pub fn new(samples: SampleCount) -> Self {
        Self {
            frame_lights: &[],
            technique: Technique::Forward,
            samples,
            placement: Placement::Single,
            view: ViewFixture::Centered,
        }
    }
}

pub const POINT_LIGHTS: [LocalLight; 1] = [LocalLight::Point {
    position: [0.1, 0.3, 1.5],
    radius: 3.0,
    color: [0.9, 0.6, 0.2],
    intensity: 2.0,
}];

#[derive(Clone, Copy)]
pub enum ViewFixture {
    Centered,
    #[cfg(test)]
    OverheadOrthographic,
    #[cfg(test)]
    OffsetPerspective,
    #[cfg(test)]
    LeftHandedPerspective {
        double_sided: bool,
    },
}

impl ViewFixture {
    fn z_axis(self) -> f32 {
        match self {
            #[cfg(test)]
            Self::LeftHandedPerspective { .. } => -1.0,
            _ => 1.0,
        }
    }

    fn orient(self, value: [f32; 3]) -> [f32; 3] {
        match self {
            #[cfg(test)]
            Self::OverheadOrthographic => [value[0], value[2], -value[1]],
            _ => [value[0], value[1], self.z_axis() * value[2]],
        }
    }

    // The app supplies its projection and camera pose. This fixture is
    // aligned to the camera axes and only composes a Z translation.
    fn input(self) -> Result<ViewInput, GraphError> {
        let near = 0.1_f32;
        let far = 10.0_f32;
        let aspect = EXTENT[0] as f32 / EXTENT[1] as f32;
        #[cfg(test)]
        if let Self::OverheadOrthographic = self {
            let depth = (near - far).recip();
            return ViewInput::new(
                [
                    [1.0 / aspect, 0.0, 0.0, 0.0],
                    [0.0, 0.0, depth, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, (near - 3.0) * depth, 1.0],
                ],
                [0.0, 3.0, 0.0],
                [0.0, -1.0, 0.0],
                [near, far],
                Projection::Orthographic,
            );
        }
        let eye = [0.0, 0.0, 3.0];
        let vertical_fov_degrees = 55.0_f32;
        let scale = (vertical_fov_degrees.to_radians() * 0.5).tan().recip();
        let depth = far / (near - far);
        let (shift, projection) = match self {
            #[cfg(test)]
            Self::OffsetPerspective | Self::LeftHandedPerspective { .. } => {
                (0.45, Projection::Perspective)
            }
            _ => (
                0.0,
                Projection::SymmetricPerspective {
                    vertical_fov_degrees,
                    aspect,
                },
            ),
        };
        let z_axis = self.z_axis();
        ViewInput::new(
            [
                [scale / aspect, 0.0, 0.0, 0.0],
                [0.0, -scale, 0.0, 0.0],
                [-shift * z_axis, 0.0, depth * z_axis, -z_axis],
                [shift * eye[2], 0.0, near * depth - depth * eye[2], eye[2]],
            ],
            self.orient(eye),
            [0.0, 0.0, -f64::from(z_axis)],
            [near, far],
            projection,
        )
    }
}

pub fn prepare(frames_in_flight: NonZeroUsize, settings: Settings) -> Result<Prepared, GraphError> {
    let config = Config {
        extent: EXTENT,
        clear_color: [0.0, 0.0, 0.0, 1.0],
        shadows: Some(ShadowSettings {
            resolution: SHADOW_RESOLUTION,
            ..ShadowSettings::default()
        }),
        output: Some(SdrOutput::capture(false)),
        samples: settings.samples,
        camera_storage: CameraStorage::Transient,
    };
    let count = if settings.placement == Placement::Single {
        1
    } else {
        2
    };
    let limits = SceneLimits {
        frames_in_flight,
        max_draws: count,
        max_materials: 1,
        max_local_lights: settings
            .frame_lights
            .iter()
            .map(|lights| lights.len())
            .max()
            .unwrap_or_else(|| settings.local_lights().len()),
        max_casters_per_cascade: count,
        max_transforms: match settings.placement {
            #[cfg(test)]
            Placement::PagedInstanced => 1_026,
            placement if placement.uses_transforms() => count,
            _ => 0,
        },
    };
    Prepared::new(settings.technique, config, limits)
}

pub struct PostProcess {
    copy_hdr: bool,
    bloom: Option<PreparedBloom>,
    pub(super) defaults: PreparedDefaults,
}

pub fn reserve(
    prepared: &Prepared,
    triangle: &Triangle,
    copy_hdr: bool,
    bloom: Bloom,
) -> Result<(Preparation, PostProcess), GraphError> {
    let recipe = prepared.preparation();
    let mut preparation = Preparation::new(recipe.frames_in_flight());
    preparation.add_requirements(recipe.graph_requirements(), recipe.working_set())?;
    let defaults = Defaults::Materials.prepare(&mut preparation)?;
    triangle.reserve(&mut preparation, copy_hdr)?;
    let bloom = if bloom == Bloom::Off {
        None
    } else {
        Some(bloom.prepare(&mut preparation, hdr_desc())?)
    };
    Ok((
        preparation,
        PostProcess {
            copy_hdr,
            bloom,
            defaults,
        },
    ))
}

pub struct Readbacks {
    pub sdr: PassHandle,
    pub depth: Option<PassHandle>,
    #[cfg(test)]
    pub shadow_maps: [Option<PassHandle>; CASCADE_COUNT],
}

pub fn build_graph(
    pool: &mut GraphPool,
    prepared: &mut Prepared,
    triangle: &Triangle,
    defaults: Option<&DefaultResources>,
    post: &PostProcess,
    settings: Settings,
    shadow_history: Option<(&ShadowHistory, [u64; CASCADE_COUNT])>,
) -> Result<(CompiledGraph, Readbacks, Option<ShadowUpdates>), GraphError> {
    let placement = settings.placement;
    let mut graph = pool.begin()?;
    let defaults = match defaults {
        Some(defaults) => defaults.import(&mut graph)?,
        None => post.defaults.selection().upload(&mut graph)?,
    };
    let (indexed, materials) = triangle.upload(&mut graph, &defaults)?;
    let single = [Draw::WorldSpaceIndexed(indexed)];
    #[cfg(test)]
    let (test_draws, transforms) = placed_draws(indexed, placement)?;
    let draws = match placement {
        Placement::Single => &single[..],
        #[cfg(test)]
        Placement::Individual => &test_draws[..],
        #[cfg(test)]
        Placement::Instanced
        | Placement::PagedInstanced
        | Placement::Tilted(Some(_))
        | Placement::Mirrored { baked: false, .. } => &test_draws[..1],
        #[cfg(test)]
        Placement::Tilted(None) | Placement::Mirrored { baked: true, .. } => &single[..],
    };
    let fixture = triangle.view;
    let sun = Sun::new(fixture.orient([0.0, 0.0, 1.0]), [1.5; 3])?;
    let half_extents = fixture.orient([1.0, 0.7, 0.01]).map(f32::abs);
    let bounds = Bounds::new(half_extents.map(|value| -value), half_extents)?;
    let view = prepared.prepare_view(fixture.input()?, sun, bounds, defaults.environment()?)?;
    let shadows = shadow_history
        .map(|(history, revisions)| history.begin_view(&mut graph, &view, revisions))
        .transpose()?;
    let (opaque, transparent) =
        if matches!(materials[0].alpha_mode(), renderer::pbr::AlphaMode::Blend) {
            (&[][..], draws)
        } else {
            (draws, &[][..])
        };
    let mut frame = PbrDrawInput::new(&view, &materials, opaque, transparent, opaque, defaults)?;
    #[cfg(test)]
    let mut paged_transforms;
    #[cfg(test)]
    if placement == Placement::PagedInstanced {
        // Exercise 1,025 active instances beginning at a nonzero index. Only
        // the two straddling the page boundary are visible to the view or sun.
        let outside = Transform::new([
            [1.0, 0.0, 0.0, 1_000.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ])?;
        paged_transforms = vec![outside; 1_026];
        paged_transforms[1_023..1_025].copy_from_slice(&transforms);
        frame.transforms = &paged_transforms;
    } else if placement.uses_transforms() {
        frame.transforms = &transforms;
    }
    if let Some(shadows) = &shadows {
        frame.shadows = FrameShadows::Cascades(std::array::from_fn(|cascade| {
            shadows.source(cascade, opaque)
        }));
    }
    let lights = settings.local_lights();
    let (sdr, scene_depth, shadow_images) = if post.copy_hdr || post.bloom.is_some() {
        let exposure = 1.0;
        let scene = prepared.draw_hdr(&mut graph, frame.with_lights(lights))?;
        let mut hdr = scene.hdr;
        if post.copy_hdr {
            let target = graph.image("copied HDR", hdr_desc())?;
            graph.copy_image(COPY_PASS, hdr, target)?;
            hdr = target;
        }
        if let Some(bloom) = &post.bloom {
            hdr = bloom.add_passes(&mut graph, hdr)?;
        }
        (
            prepared.write_output(&mut graph, hdr, exposure)?.sdr,
            scene.depth,
            scene.shadows,
        )
    } else {
        let images =
            prepared.add_scene_and_sdr_output(&mut graph, frame.with_lights(lights), 1.0)?;
        (images.sdr, images.depth, images.shadows)
    };
    #[cfg(test)]
    let mut shadow_maps = [None; CASCADE_COUNT];
    #[cfg(test)]
    if placement != Placement::Single {
        for (target, image) in shadow_maps
            .iter_mut()
            .zip(shadow_images.expect("the fixture renders all cascades"))
        {
            *target = Some(graph.readback_image("read cascade", image)?);
        }
    }
    #[cfg(not(test))]
    let _ = shadow_images;
    let readbacks = Readbacks {
        sdr: sdr
            .capture()
            .expect("the fixture selects capture output")
            .readback(&mut graph, "read SDR")?,
        depth: scene_depth
            .map(|depth| graph.readback_image("read depth", depth))
            .transpose()?,
        #[cfg(test)]
        shadow_maps,
    };
    let updates = shadows
        .map(|shadows| shadows.export(&mut graph))
        .transpose()?;
    Ok((graph.compile()?, readbacks, updates))
}

#[cfg(test)]
fn placed_draws(
    indexed: IndexedDraw,
    placement: Placement,
) -> Result<([Draw; 2], [Transform; 2]), GraphError> {
    if let Placement::Tilted(Some(exponent)) = placement {
        let scale = 2.0_f32.powi(exponent);
        let transform = Transform::new([
            [scale, 0.0, 0.0, 0.0],
            [0.0, scale, 0.0, 0.0],
            [0.0, 0.0, scale, 0.0],
        ])?;
        let draw = Draw::model(indexed, 0);
        return Ok(([draw; 2], [transform, Transform::IDENTITY]));
    }
    if let Placement::Mirrored { .. } = placement {
        let transform = Transform::new([
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ])?;
        let draw = Draw::model(indexed, 0);
        return Ok(([draw; 2], [transform, Transform::IDENTITY]));
    }
    let place = |x| {
        Transform::new([
            [0.5, 0.0, 0.0, x],
            [0.0, 0.65, 0.0, 0.0],
            [0.0, 0.0, 0.5, 0.0],
        ])
    };
    Ok((
        [
            Draw::Instanced {
                geometry: indexed,
                first_transform: u32::from(placement == Placement::PagedInstanced),
                instance_count: match placement {
                    Placement::PagedInstanced => 1_025,
                    Placement::Instanced => 2,
                    _ => 1,
                },
            },
            Draw::model(indexed, 1),
        ],
        [place(-0.45)?, place(0.45)?],
    ))
}
