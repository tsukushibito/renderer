//! Preparation and reuse of the standard PBR fallback images.

pub use native::DefaultResourceError;
use sashiko_renderer_core::graph::{GraphCapacity, GraphPool, ImageHandle};
use sashiko_renderer_core::runtime::{ExecutionError, FrameId, Runtime};
use sashiko_renderer_core::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    graph::{Graph, GraphError, ImageDesc, ImageView, ImportImage},
};

use super::{
    Config, Environment, MaterialTextures, UnshadowedView,
    unshadowed::{NAME as SHADOW_NAME, desc as shadow_desc},
};

use super::ReflectionProbes;
use sashiko_renderer_core::graph::{BufferHandle, ImportBuffer};

const MATERIAL_NAMES: [&str; 5] = [
    "PBR default sRGB white",
    "PBR default normal",
    "PBR default ORM",
    "PBR disabled IBL",
    "PBR split-sum BRDF",
];
const MATERIAL_NAME_BYTES: usize = MATERIAL_NAMES[0].len()
    + MATERIAL_NAMES[1].len()
    + MATERIAL_NAMES[2].len()
    + MATERIAL_NAMES[3].len()
    + MATERIAL_NAMES[4].len();
const PIXELS: [&[u8]; 5] = [
    &[255, 255, 255, 255],
    // RGBA16F encodes a flat normal exactly: (0.5, 0.5, 1.0, 1.0).
    &[0, 0x38, 0, 0x38, 0, 0x3c, 0, 0x3c],
    &[255, 255, 255, 255],
    &[0, 0, 0, 0, 0, 0, 0, 0x3c],
    // tools/bistro/ibl.py: rgba16_bytes([brdf_lut(32, 32, 1024)]), correlated Smith GGX.
    include_bytes!("brdf-lut.rgba16f"),
];

fn material_descriptions() -> [ImageDesc; 5] {
    [
        ImageDesc::rgba8_srgb(1, 1),
        ImageDesc::rgba16_float(1, 1),
        ImageDesc::rgba8(1, 1),
        ImageDesc::rgba16_float(1, 1),
        ImageDesc::rgba16_float(32, 32),
    ]
}

/// Which images to share across standard PBR views. Use
/// [`Self::for_recipe`] for scenes using the standard material or IBL defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Defaults {
    /// Fallback material maps and a disabled IBL environment.
    Materials,
    /// Cleared depth for draws with directional shadows disabled.
    Unshadowed,
    /// All material, environment and depth defaults in one startup submission.
    MaterialsAndUnshadowed,
}

impl Defaults {
    pub const fn for_recipe(config: Config) -> Self {
        if config.shadows.is_some() {
            Self::Materials
        } else {
            Self::MaterialsAndUnshadowed
        }
    }

    const fn materials(self) -> bool {
        matches!(self, Self::Materials | Self::MaterialsAndUnshadowed)
    }

    const fn unshadowed(self) -> bool {
        matches!(self, Self::Unshadowed | Self::MaterialsAndUnshadowed)
    }

    /// Reserve one shared resident set and its imports in the frame Graph.
    /// Runtime limits must include all registrations and the startup work
    /// described by [`Self::upload_requirements`].
    pub fn prepare(self, preparation: &mut Preparation) -> Result<PreparedDefaults, GraphError> {
        let [color, normal, orm, ibl, brdf] = material_descriptions();
        let resources = [color, normal, orm, ibl, brdf, shadow_desc()]
            .map(|desc| ResourceClass::Image { desc, count: 1 });
        let resources = match self {
            Self::Materials => &resources[..5],
            Self::Unshadowed => &resources[5..],
            Self::MaterialsAndUnshadowed => &resources[..],
        };
        let name_bytes = usize::from(self.materials()) * MATERIAL_NAME_BYTES
            + usize::from(self.unshadowed()) * SHADOW_NAME.len();
        if self.materials() {
            preparation.add_requirements(
                GraphRequirements {
                    resources: 1,
                    initial_ranges: 1,
                    name_bytes: "reflection metadata".len(),
                    ..Default::default()
                },
                WorkingSet {
                    resources: &[ResourceClass::Buffer {
                        desc: ReflectionProbes::metadata_desc(),
                        count: 1,
                    }],
                    ..Default::default()
                },
            )?;
        }
        preparation.add_requirements(
            GraphRequirements {
                resources: resources.len(),
                name_bytes,
                ..GraphRequirements::default()
            },
            WorkingSet {
                resources,
                ..WorkingSet::default()
            },
        )?;
        Ok(PreparedDefaults(self))
    }

    /// Graph declaration requirements of [`Self::upload`], including images.
    pub fn upload_requirements(self) -> GraphRequirements {
        let shadows = if self.unshadowed() {
            UnshadowedView::requirements()
        } else {
            GraphRequirements::default()
        };
        if !self.materials() {
            return shadows;
        }
        GraphRequirements {
            resources: shadows.resources + MATERIAL_NAMES.len() + 1,
            passes: shadows.passes + MATERIAL_NAMES.len() + 1,
            accesses: shadows.accesses + MATERIAL_NAMES.len() + 1,
            name_bytes: shadows.name_bytes
                + 2 * (MATERIAL_NAME_BYTES + "reflection metadata".len()),
            upload_bytes: PIXELS.iter().map(|pixel| pixel.len()).sum::<usize>()
                + ReflectionProbes::METADATA_BYTES,
            ..shadows
        }
    }

    /// Initialize the selected images in a one-off Graph. Reserve their image
    /// classes for the submissions that may overlap when using this route.
    pub fn upload(self, graph: &mut Graph) -> Result<DefaultViews, GraphError> {
        let materials = if self.materials() {
            let mut images = [None; 5];
            for (index, desc) in material_descriptions().into_iter().enumerate() {
                let image = graph.image(MATERIAL_NAMES[index], desc)?;
                graph.upload_image(MATERIAL_NAMES[index], image, PIXELS[index])?;
                images[index] = Some(graph.image_view(image, 0, 0)?);
            }
            Some(images.map(|view| view.expect("each default material image was initialized")))
        } else {
            None
        };
        let unshadowed = self
            .unshadowed()
            .then(|| UnshadowedView::append(graph))
            .transpose()?;
        let metadata = self
            .materials()
            .then(|| {
                ReflectionProbes::upload_metadata(graph, &[0; ReflectionProbes::METADATA_BYTES])
            })
            .transpose()?;
        Ok(DefaultViews {
            metadata,
            materials,
            unshadowed,
        })
    }
}

/// Views of the defaults selected for the current Graph.
#[derive(Clone, Copy, Debug)]
pub struct DefaultViews {
    metadata: Option<BufferHandle>,
    materials: Option<[ImageView; 5]>,
    unshadowed: Option<UnshadowedView>,
}

impl DefaultViews {
    /// Fill missing maps. Provided views must belong to the current Graph.
    pub fn material_textures(
        self,
        textures: MaterialTextures<Option<ImageView>>,
    ) -> Result<MaterialTextures, GraphError> {
        let [white, normal, orm, _, _] = self.material_maps()?;
        Ok(MaterialTextures {
            base_color: textures.base_color.unwrap_or(white),
            normal: textures.normal.unwrap_or(normal),
            orm: textures.orm.unwrap_or(orm),
            emissive: textures.emissive.unwrap_or(white),
        })
    }

    /// Build a disabled IBL environment for a directly lit scene.
    pub fn environment(self) -> Result<Environment, GraphError> {
        let [_, _, _, black, brdf] = self.material_maps()?;
        Ok(Environment {
            irradiance: black,
            reflection_metadata: self
                .metadata
                .expect("material defaults include probe metadata"),
            local_reflections: None,
            diffuse_lighting: Default::default(),
            reflection: black,
            brdf_lut: brdf,
            nits_per_unit: 0.0,
            yaw_radians: 0.0,
            max_prefilter_lod: 0,
        })
    }

    /// Use the initialized depth for a view with shadow cascades disabled.
    pub fn unshadowed(self) -> Result<UnshadowedView, GraphError> {
        self.unshadowed
            .ok_or_else(|| GraphError::invalid_input("unshadowed PBR defaults were not prepared"))
    }

    fn material_maps(self) -> Result<[ImageView; 5], GraphError> {
        self.materials
            .ok_or_else(|| GraphError::invalid_input("PBR material defaults were not prepared"))
    }

    fn images(self) -> [Option<ImageHandle>; 6] {
        let [white, normal, orm, ibl, brdf] = self
            .materials
            .map_or([None; 5], |views| views.map(|view| Some(view.image())));
        [
            white,
            normal,
            orm,
            ibl,
            brdf,
            self.unshadowed.map(|view| view.view().image()),
        ]
    }
}

/// Reservation for a shared, immutable set of default images.
#[derive(Clone, Copy)]
pub struct PreparedDefaults(Defaults);

impl PreparedDefaults {
    pub const fn selection(&self) -> Defaults {
        self.0
    }

    /// Submit one startup Graph and retain its scheduled images. The Runtime
    /// can accept consumers while initialization is pending if a frame slot
    /// is available. Wait for the returned frame at the startup boundary
    /// when all prepared slots are needed for normal frames.
    pub fn initialize(
        &self,
        runtime: &mut Runtime,
    ) -> Result<(DefaultResources, FrameId), DefaultResourceError> {
        let mut pool = GraphPool::new(upload_graph_capacity(self.0), 1)?;
        let mut graph = pool.begin()?;
        let views = self.0.upload(&mut graph)?;
        if let Some(buffer) = views.metadata {
            graph.export_buffer(buffer)?;
        }
        let images = views.images();
        for image in images.into_iter().flatten() {
            graph.export_image(image)?;
        }
        let compiled = graph.compile()?;
        let submitted = runtime.submit(&compiled)?;
        let resources = DefaultResources {
            selection: self.0,
            metadata: views.metadata.map(|b| {
                submitted
                    .export_buffer(b)
                    .expect("accepted PBR metadata export")
            }),
            images: images.map(|image| {
                image.map(|image| {
                    submitted
                        .export_image(image)
                        .expect("accepted PBR default export")
                })
            }),
        };
        Ok((resources, submitted.id()))
    }
}

/// Registered defaults shared by frame Graphs. Call `clear` when the
/// application no longer needs the Runtime registrations.
pub struct DefaultResources {
    metadata: Option<ImportBuffer>,
    selection: Defaults,
    images: [Option<ImportImage>; 6],
}

impl DefaultResources {
    /// Import the selected immutable defaults into this Graph.
    pub fn import(&self, graph: &mut Graph) -> Result<DefaultViews, GraphError> {
        let [white, normal, orm, ibl, brdf, shadow] = &self.images;
        let missing = || GraphError::invalid_input("PBR default resources have been released");
        let materials = if self.selection.materials() {
            self.metadata.as_ref().ok_or_else(missing)?;
            let [Some(white), Some(normal), Some(orm), Some(ibl), Some(brdf)] =
                [white, normal, orm, ibl, brdf]
            else {
                return Err(missing());
            };
            let mut views = [None; 5];
            for (index, snapshot) in [white, normal, orm, ibl, brdf].into_iter().enumerate() {
                let handle = graph.import_image(MATERIAL_NAMES[index], snapshot)?;
                views[index] = Some(graph.image_view(handle, 0, 0)?);
            }
            Some(views.map(|view| view.expect("each default material image was imported")))
        } else {
            None
        };
        let unshadowed = if self.selection.unshadowed() {
            let snapshot = shadow.as_ref().ok_or_else(missing)?;
            let handle = graph.import_image(SHADOW_NAME, snapshot)?;
            Some(UnshadowedView::from_view(graph.image_view(handle, 0, 0)?))
        } else {
            None
        };
        Ok(DefaultViews {
            metadata: self
                .metadata
                .as_ref()
                .map(|b| graph.import_buffer("reflection metadata", b))
                .transpose()?,
            materials,
            unshadowed,
        })
    }

    /// Release the registrations at a change boundary. Accepted work keeps
    /// its leases until retirement. If a deletion fails, retry this method
    /// to release the remaining registrations.
    pub fn clear(&mut self, runtime: &mut Runtime) -> Result<(), ExecutionError> {
        if let Some(b) = &self.metadata {
            runtime.delete(b.key())?;
            self.metadata = None;
        }
        for image in &mut self.images {
            if let Some(current) = image {
                runtime.delete(current.key())?;
                *image = None;
            }
        }
        Ok(())
    }
}

fn upload_graph_capacity(selection: Defaults) -> GraphCapacity {
    let requirements = selection.upload_requirements();
    GraphCapacity {
        resources: requirements.resources,
        passes: requirements.passes,
        accesses: requirements.accesses,
        draws: 0,
        raster_constant_words: Some(0),
        bindings: 0,
        attachments: requirements.attachments,
        dependencies: 0,
        initial_ranges: 0,
        name_bytes: requirements.name_bytes,
        upload_bytes: requirements.upload_bytes,
        edges: 8,
        segments: 16,
        readers: 8,
    }
}

mod native {
    use super::{ExecutionError, GraphError};
    use std::{error::Error, fmt};

    /// Failure to build or submit the startup upload Graph for PBR defaults.
    #[derive(Debug)]
    pub enum DefaultResourceError {
        Graph(GraphError),
        Execution(ExecutionError),
    }

    impl fmt::Display for DefaultResourceError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Graph(error) => error.fmt(formatter),
                Self::Execution(error) => error.fmt(formatter),
            }
        }
    }

    impl Error for DefaultResourceError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            match self {
                Self::Graph(error) => Some(error),
                Self::Execution(error) => Some(error),
            }
        }
    }

    impl From<GraphError> for DefaultResourceError {
        fn from(error: GraphError) -> Self {
            Self::Graph(error)
        }
    }

    impl From<ExecutionError> for DefaultResourceError {
        fn from(error: ExecutionError) -> Self {
            Self::Execution(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_preserves_defaults_and_overrides_each_channel_independently() {
        let selection = Defaults::MaterialsAndUnshadowed;
        let mut pool = GraphPool::new(upload_graph_capacity(selection), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let views = selection.upload(&mut graph).unwrap();
        let textures = views
            .material_textures(MaterialTextures::default())
            .unwrap();
        let MaterialTextures {
            base_color: white,
            normal,
            orm,
            emissive,
        } = textures;
        assert_eq!(white, emissive);
        assert_ne!(normal, orm);
        assert_eq!(
            views
                .material_textures(MaterialTextures {
                    orm: Some(normal),
                    ..Default::default()
                })
                .unwrap(),
            MaterialTextures {
                orm: normal,
                ..textures
            }
        );
        let environment = views.environment().unwrap();
        assert_eq!(environment.irradiance, environment.reflection);
        assert_ne!(environment.irradiance, environment.brdf_lut);
        assert_eq!(environment.nits_per_unit, 0.0);
        assert_eq!(
            graph
                .image_desc(views.unshadowed().unwrap().view().image())
                .unwrap(),
            shadow_desc(),
        );
        for image in views.images().into_iter().flatten() {
            graph.export_image(image).unwrap();
        }
        assert_eq!(graph.compile().unwrap().pass_count(), 6);
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    fn accept_and_release_defaults(config: sashiko_renderer_core::runtime::RuntimeConfig) {
        use sashiko_renderer_core::runtime::{RuntimeConfig, RuntimeLimits};
        use std::{num::NonZeroUsize, time::Duration};

        let limits = GraphCapacity::default();
        let mut preparation = Preparation::new(NonZeroUsize::MIN);
        let defaults = Defaults::MaterialsAndUnshadowed
            .prepare(&mut preparation)
            .unwrap();
        let mut runtime = Runtime::new(
            config,
            RuntimeLimits {
                graph: limits,
                ..RuntimeLimits::default()
            },
            preparation.working_set(),
        )
        .unwrap();
        #[cfg(feature = "vulkan")]
        if let Some(vulkan) = runtime.as_vulkan() {
            assert_ne!(vulkan.device_info().device_type, "CPU");
        }
        let (mut resources, startup) = defaults.initialize(&mut runtime).unwrap();
        let mut pool = GraphPool::new(limits, 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let view = resources.import(&mut graph).unwrap();
        let textures = view.material_textures(MaterialTextures::default()).unwrap();
        let readback = graph
            .readback_image("Default normal", textures.normal.image())
            .unwrap();
        let depth = graph
            .readback_image("Default depth", view.unshadowed().unwrap().view().image())
            .unwrap();
        let sent = runtime.submit(&graph.compile().unwrap()).unwrap();
        resources.clear(&mut runtime).unwrap();
        drop(
            runtime
                .wait_for_frame(startup, Duration::from_secs(10))
                .unwrap()
                .unwrap(),
        );
        let mut completion = runtime
            .wait_for_frame(sent.id(), Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let mut pixel = [0; 8];
        completion.readback_into(readback, &mut pixel).unwrap();
        assert_eq!(pixel.as_slice(), PIXELS[1]);
        let mut value = [0; 4];
        completion.readback_into(depth, &mut value).unwrap();
        assert_eq!(f32::from_le_bytes(value), 1.0);
        drop(completion);

        let strict = cfg!(feature = "development-validation");
        match config {
            #[cfg(feature = "vulkan")]
            RuntimeConfig::Vulkan(_) => {
                let monitor = runtime.as_vulkan().unwrap().validation_monitor();
                drop(runtime);
                let report = monitor.report();
                assert!(report.is_clean(), "{report:#?}");
                if strict {
                    assert_eq!(report.core_validation, Some(true));
                    assert_eq!(report.synchronization_validation, Some(true));
                }
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeConfig::D3D12(_) => {
                let monitor = runtime.as_d3d12().unwrap().validation_monitor();
                drop(runtime);
                let report = monitor.snapshot();
                assert!(report.is_clean(), "{report:#?}");
                assert_eq!(report.debug_layer_enabled, strict);
                assert_eq!(report.gpu_based_validation_enabled, strict);
            }
        }
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a real Vulkan device"]
    fn vulkan_defaults_can_be_unregistered_during_accepted_reads() {
        accept_and_release_defaults(sashiko_renderer_core::runtime::RuntimeConfig::Vulkan(
            Default::default(),
        ));
    }

    #[cfg(all(feature = "d3d12", windows))]
    #[test]
    #[ignore = "requires a real D3D12 device"]
    fn d3d12_defaults_can_be_unregistered_during_accepted_reads() {
        accept_and_release_defaults(sashiko_renderer_core::runtime::RuntimeConfig::D3D12(
            Default::default(),
        ));
    }
}
