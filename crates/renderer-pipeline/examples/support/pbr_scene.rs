use std::num::NonZeroUsize;

use renderer::{
    capacity::{Preparation, ResourceClass},
    geometry::GeometryData,
    graph::{
        BufferDesc, CompiledGraph, Graph, GraphError, GraphPool, ImageDesc, PassHandle, SampleCount,
    },
};
#[cfg(test)]
use renderer_pipeline::pbr::Transform;
use renderer_pipeline::{
    passes::{
        bloom::{Bloom, PreparedBloom},
        output::{SdrEncoding, SdrOutput},
        pbr::{MATERIAL_BYTES, VERTEX_BYTES},
    },
    pbr::{
        Bounds, CASCADE_COUNT, Camera, CameraStorage, DefaultResources, DefaultViews, Draw, Frame,
        IndexedDraw, Material, MaterialFactors, PbrVertex, SceneLimits, ShadowHistory,
        ShadowSettings, ShadowUpdates, Sun,
    },
    pbr_forward::{self, Config, Prepared},
};

pub const EXTENT: [u32; 2] = [64, 48];
pub const SHADOW_RESOLUTION: u32 = 64;
pub const COPY_PASS: &str = "application HDR copy";

pub struct Triangle {
    geometry: GeometryData,
    material: [u8; MATERIAL_BYTES],
}

impl Triangle {
    pub fn new(glowing: bool, placement: Placement) -> Result<Self, GraphError> {
        let (normal, tangent, object_scale) = match placement {
            #[cfg(test)]
            Placement::Tilted(scale_exponent) => (
                [0.8, 0.0, 0.6],
                [0.0, 1.0, 0.0, 1.0],
                scale_exponent.map_or(1.0, |exponent| 2.0_f32.powi(-exponent)),
            ),
            _ => ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], 1.0),
        };
        let positions = [
            ([-0.8_f32, -0.7, 0.0], [0.0, 1.0]),
            ([0.8, -0.7, 0.0], [1.0, 1.0]),
            ([0.0, 0.7, 0.0], [0.5, 0.0]),
        ];
        let mut vertices = [0; 3 * VERTEX_BYTES];
        for ((position, uv), bytes) in positions.into_iter().zip(vertices.as_chunks_mut().0) {
            let position = position.map(|value| value * object_scale);
            *bytes = PbrVertex::new(position, normal, tangent, uv)?.encode();
        }
        Ok(Self {
            geometry: GeometryData::new(VERTEX_BYTES as u32, &vertices, &[0, 1, 2])?,
            material: MaterialFactors {
                base_color: [0.75, 0.015, 0.009, 1.0],
                emissive: [if glowing { 2.0 } else { 0.0 }, 0.0, 0.0],
                ..MaterialFactors::default()
            }
            .encode()?,
        })
    }

    fn buffer_descs(&self) -> [BufferDesc; 3] {
        [
            self.geometry.vertex_byte_len(),
            self.geometry.index_byte_len(),
            self.material.len() as u64,
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
    ) -> Result<(IndexedDraw, [Material; 1]), GraphError> {
        let [vertex_desc, index_desc, material_desc] = self.buffer_descs();
        let vertices = graph.buffer("triangle vertices", vertex_desc)?;
        let indices = graph.buffer("triangle indices", index_desc)?;
        let factors = graph.buffer("triangle material factors", material_desc)?;
        self.geometry
            .upload_vertices(graph, "upload vertices", vertices)?;
        self.geometry
            .upload_indices(graph, "upload indices", indices)?;
        graph.upload_buffer("upload material factors", factors, 0, &self.material)?;

        Ok((
            IndexedDraw {
                vertices,
                indices,
                first_index: 0,
                index_count: self.geometry.index_count(),
                material: 0,
            },
            [Material {
                factors,
                factor_offset: 0,
                textures: defaults.material_textures([None; 4]),
            }],
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
    Tilted(Option<i32>),
}

impl Placement {
    fn uses_transforms(self) -> bool {
        match self {
            Self::Single => false,
            #[cfg(test)]
            Self::Tilted(exponent) => exponent.is_some(),
            #[cfg(test)]
            Self::Individual | Self::Instanced => true,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Settings {
    pub samples: SampleCount,
    pub placement: Placement,
}

impl Settings {
    pub fn new(samples: SampleCount) -> Self {
        Self {
            samples,
            placement: Placement::Single,
        }
    }
}

pub fn prepare(frames_in_flight: NonZeroUsize, settings: Settings) -> Result<Prepared, GraphError> {
    let config = Config {
        extent: EXTENT,
        clear_color: [0.0, 0.0, 0.0, 1.0],
        shadows: ShadowSettings {
            resolution: SHADOW_RESOLUTION,
            ..ShadowSettings::default()
        },
        output: SdrOutput {
            encoding: SdrEncoding::Srgb,
            fxaa: false,
        },
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
        max_casters_per_cascade: count,
    };
    if settings.placement.uses_transforms() {
        pbr_forward::prepare_with_transforms(config, limits)
    } else {
        pbr_forward::prepare(config, limits)
    }
}

pub struct PostProcess {
    copy_hdr: bool,
    bloom: Option<PreparedBloom>,
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
    DefaultResources::reserve(&mut preparation)?;
    triangle.reserve(&mut preparation, copy_hdr)?;
    let bloom = if bloom == Bloom::Off {
        None
    } else {
        Some(bloom.prepare(&mut preparation, hdr_desc())?)
    };
    Ok((preparation, PostProcess { copy_hdr, bloom }))
}

pub fn resource_count(preparation: &Preparation) -> usize {
    preparation
        .working_set()
        .resources
        .iter()
        .map(|class| match *class {
            ResourceClass::Buffer { count, .. } | ResourceClass::Image { count, .. } => count,
        })
        .sum()
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
    placement: Placement,
    shadow_history: Option<(&ShadowHistory, [u64; CASCADE_COUNT])>,
) -> Result<(CompiledGraph, Readbacks, Option<ShadowUpdates>), GraphError> {
    let mut graph = pool.begin()?;
    let defaults = match defaults {
        Some(defaults) => defaults.import(&mut graph)?,
        None => DefaultViews::upload(&mut graph)?,
    };
    let (indexed, materials) = triangle.upload(&mut graph, &defaults)?;
    let single = [Draw::Indexed(indexed)];
    #[cfg(test)]
    let (test_draws, transforms) = placed_draws(indexed, placement)?;
    let draws = match placement {
        Placement::Single => &single[..],
        #[cfg(test)]
        Placement::Individual => &test_draws[..],
        #[cfg(test)]
        Placement::Instanced | Placement::Tilted(Some(_)) => &test_draws[..1],
        #[cfg(test)]
        Placement::Tilted(None) => &single[..],
    };
    let view = prepared.prepare_view(
        Camera {
            position: [0.0, 0.0, 3.0],
            target: [0.0; 3],
            fov_y_degrees: 55.0,
            near: 0.1,
            far: 10.0,
        },
        Sun::new([0.0, 0.0, 1.0], [1.5; 3])?,
        Bounds::new([-1.0, -0.7, -0.01], [1.0, 0.7, 0.01])?,
        defaults.environment(),
    )?;
    let shadows = shadow_history
        .map(|(history, revisions)| history.begin_view(&mut graph, &view, revisions))
        .transpose()?;
    let mut frame = Frame::new(&view, &materials, draws, &[], draws);
    #[cfg(test)]
    if placement.uses_transforms() {
        frame.transforms = &transforms;
    }
    if let Some(shadows) = &shadows {
        frame.shadows = std::array::from_fn(|cascade| shadows.source(cascade, draws));
    }
    let (sdr, scene_depth, shadow_images) = if post.copy_hdr || post.bloom.is_some() {
        let scene = prepared.add_scene(&mut graph, frame)?;
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
            prepared.add_output(&mut graph, hdr, 1.0)?.sdr,
            scene.depth,
            scene.shadows,
        )
    } else {
        let images = prepared.add_passes(&mut graph, frame)?;
        (images.sdr, images.depth, images.shadows)
    };
    #[cfg(test)]
    let mut shadow_maps = [None; CASCADE_COUNT];
    #[cfg(test)]
    if placement != Placement::Single {
        for (target, image) in shadow_maps.iter_mut().zip(shadow_images) {
            *target = Some(graph.readback_image("read cascade", image)?);
        }
    }
    #[cfg(not(test))]
    let _ = shadow_images;
    let readbacks = Readbacks {
        sdr: graph.readback_image("read SDR", sdr)?,
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
        let draw = Draw::Instanced {
            geometry: indexed,
            first_transform: 0,
            instance_count: 1,
        };
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
                first_transform: 0,
                instance_count: if placement == Placement::Instanced {
                    2
                } else {
                    1
                },
            },
            Draw::Instanced {
                geometry: indexed,
                first_transform: 1,
                instance_count: 1,
            },
        ],
        [place(-0.45)?, place(0.45)?],
    ))
}
