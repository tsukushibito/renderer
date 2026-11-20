#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub use native::DefaultResourceError;
#[cfg(any(test, feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::graph::{GraphCapacity, GraphPool};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::runtime::{ExecutionError, FrameId, Runtime};
use renderer::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    graph::{Graph, GraphError, ImageDesc, ImageHandle, ImageView, ImportImage},
};

use super::Environment;

const NAMES: [&str; 4] = [
    "PBR default sRGB white",
    "PBR default normal",
    "PBR default ORM",
    "PBR disabled IBL",
];

const NAME_BYTES: usize = NAMES[0].len() + NAMES[1].len() + NAMES[2].len() + NAMES[3].len();
const PIXELS: [&[u8]; 4] = [
    &[255, 255, 255, 255],
    // RGBA16F encodes a flat normal exactly: (0.5, 0.5, 1.0, 1.0).
    &[0, 0x38, 0, 0x38, 0, 0x3c, 0, 0x3c],
    &[255, 255, 255, 255],
    &[0, 0, 0, 0, 0, 0, 0, 0x3c],
];

fn image_descriptions() -> [ImageDesc; 4] {
    [
        ImageDesc::rgba8_srgb(1, 1),
        ImageDesc::rgba16_float(1, 1),
        ImageDesc::rgba8(1, 1),
        ImageDesc::rgba16_float(1, 1),
    ]
}

/// Views of the common PBR defaults in the current Graph.
#[derive(Clone, Copy, Debug)]
pub struct DefaultViews {
    srgb_white: ImageView,
    normal: ImageView,
    orm: ImageView,
    black_ibl: ImageView,
}

impl DefaultViews {
    /// Graph declaration requirements of [`Self::upload`], including its four
    /// images. Use this for startup or single-Graph rendering.
    pub fn upload_requirements() -> GraphRequirements {
        GraphRequirements {
            resources: 4,
            passes: 4,
            accesses: 4,
            name_bytes: 2 * NAME_BYTES,
            upload_bytes: PIXELS.iter().map(|pixel| pixel.len()).sum(),
            ..GraphRequirements::default()
        }
    }

    /// Declare and initialize four 1×1 images. Use this when the defaults are
    /// consumed by the same Graph, or prepare [`DefaultResources`] once to
    /// reuse the images across frames.
    pub fn upload(graph: &mut Graph) -> Result<Self, GraphError> {
        let mut images = [None; 4];
        for (index, desc) in image_descriptions().into_iter().enumerate() {
            let image = graph.image(NAMES[index], desc)?;
            graph.upload_image(NAMES[index], image, PIXELS[index])?;
            images[index] = Some(image);
        }
        Self::from_images(
            graph,
            images.map(|image| image.expect("each default image was uploaded")),
        )
    }

    fn from_images(graph: &Graph, images: [ImageHandle; 4]) -> Result<Self, GraphError> {
        let [white, normal, orm, black_ibl] = images;
        Ok(Self {
            srgb_white: graph.image_view(white, 0, 0)?,
            normal: graph.image_view(normal, 0, 0)?,
            orm: graph.image_view(orm, 0, 0)?,
            black_ibl: graph.image_view(black_ibl, 0, 0)?,
        })
    }

    /// Supply absent maps from the default views. Inputs and outputs are in
    /// base-color, normal, ORM and emissive order. Supplied maps must refer
    /// to the same Graph and satisfy the material binding contract.
    pub fn material_textures(self, textures: [Option<ImageView>; 4]) -> [ImageView; 4] {
        let defaults = [self.srgb_white, self.normal, self.orm, self.srgb_white];
        std::array::from_fn(|index| textures[index].unwrap_or(defaults[index]))
    }

    /// An environment with IBL disabled. All three image bindings share the
    /// same black view, so a directly lit scene needs no other IBL assets.
    pub fn environment(self) -> Environment {
        Environment {
            irradiance: self.black_ibl,
            reflection: self.black_ibl,
            brdf_lut: self.black_ibl,
            strength: 0.0,
            yaw_radians: 0.0,
            max_prefilter_lod: 0,
        }
    }
}

/// Four prepared, resident PBR default images. Call `clear` when
/// the application no longer needs their runtime registrations.
pub struct DefaultResources {
    images: [Option<ImportImage>; 4],
}

impl DefaultResources {
    /// Reserve one shared physical set and four possible imports in each
    /// frame's Graph. The runtime limits must include the image registrations
    /// and the startup upload Graph described by [`DefaultViews::upload_requirements`].
    pub fn reserve(preparation: &mut Preparation) -> Result<(), GraphError> {
        let [srgb, hdr, linear, _] = image_descriptions();
        let resources = [
            ResourceClass::Image {
                desc: srgb,
                count: 1,
            },
            ResourceClass::Image {
                desc: linear,
                count: 1,
            },
            ResourceClass::Image {
                desc: hdr,
                count: 2,
            },
        ];
        preparation.add_requirements(
            GraphRequirements {
                resources: 4,
                name_bytes: NAME_BYTES,
                ..GraphRequirements::default()
            },
            WorkingSet {
                resources: &resources,
                ..WorkingSet::default()
            },
        )
    }

    /// Submit the startup uploads and capture their scheduled contents. The
    /// same runtime can accept following Graphs importing these views while
    /// the startup upload is pending, subject to runtime capacity. Use the
    /// returned frame ID to drain the exact upload before a steady-state
    /// frame window.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn initialize(runtime: &mut Runtime) -> Result<(Self, FrameId), DefaultResourceError> {
        let mut pool = GraphPool::new(upload_graph_capacity(), 1)?;
        let mut graph = pool.begin()?;
        let views = DefaultViews::upload(&mut graph)?;
        let images =
            [views.srgb_white, views.normal, views.orm, views.black_ibl].map(ImageView::image);
        for image in images {
            graph.export_image(image)?;
        }
        let compiled = graph.compile()?;
        let submitted = runtime.submit(&compiled)?;
        let resources = Self {
            images: images.map(|image| {
                // These handles belong to the accepted Graph and were all
                // declared as image exports before its compilation.
                Some(
                    submitted
                        .export_image(image)
                        .expect("accepted PBR default export"),
                )
            }),
        };
        Ok((resources, submitted.id()))
    }

    /// Import this immutable set of images into the current Graph.
    pub fn import(&self, graph: &mut Graph) -> Result<DefaultViews, GraphError> {
        let [Some(white), Some(normal), Some(orm), Some(black_ibl)] = &self.images else {
            return Err(GraphError::invalid_input(
                "PBR default resources have been released",
            ));
        };
        let images = [
            graph.import_image(NAMES[0], white)?,
            graph.import_image(NAMES[1], normal)?,
            graph.import_image(NAMES[2], orm)?,
            graph.import_image(NAMES[3], black_ibl)?,
        ];
        DefaultViews::from_images(graph, images)
    }

    /// Release the registrations at a change boundary. Accepted work keeps
    /// its leases until retirement. If a deletion fails, this method can be
    /// retried for the remaining registrations.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn clear(&mut self, runtime: &mut Runtime) -> Result<(), ExecutionError> {
        for image in &mut self.images {
            if let Some(current) = image {
                runtime.delete(current.key())?;
                *image = None;
            }
        }
        Ok(())
    }
}

#[cfg(any(test, feature = "vulkan", all(windows, feature = "d3d12")))]
fn upload_graph_capacity() -> GraphCapacity {
    let requirements = DefaultViews::upload_requirements();
    GraphCapacity {
        resources: requirements.resources,
        passes: requirements.passes,
        accesses: requirements.accesses,
        draws: 0,
        raster_constant_words: Some(0),
        bindings: 0,
        attachments: 0,
        dependencies: 0,
        initial_ranges: 0,
        name_bytes: requirements.name_bytes,
        upload_bytes: requirements.upload_bytes,
        edges: 8,
        segments: 16,
        readers: 8,
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
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
        let mut pool = GraphPool::new(upload_graph_capacity(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let views = DefaultViews::upload(&mut graph).unwrap();
        let [white, normal, orm, emissive] = views.material_textures([None; 4]);
        assert_eq!(white, emissive);
        assert_ne!(normal, orm);
        assert_eq!(
            views.material_textures([None, None, Some(normal), None]),
            [white, normal, normal, emissive]
        );
        let environment = views.environment();
        assert_eq!(environment.irradiance, environment.reflection);
        assert_eq!(environment.irradiance, environment.brdf_lut);
        assert_eq!(environment.strength, 0.0);
        for image in [white, normal, orm, environment.irradiance] {
            graph.export_image(image.image()).unwrap();
        }
        assert!(graph.compile().is_ok());
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    fn accept_and_release_defaults(config: renderer::runtime::RuntimeConfig) {
        use renderer::runtime::{RuntimeConfig, RuntimeLimits};
        use std::{num::NonZeroUsize, time::Duration};

        let limits = GraphCapacity::default();
        let mut preparation = Preparation::new(NonZeroUsize::MIN);
        DefaultResources::reserve(&mut preparation).unwrap();
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
        let (mut resources, startup) = DefaultResources::initialize(&mut runtime).unwrap();
        let mut pool = GraphPool::new(limits, 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let view = resources.import(&mut graph).unwrap();
        let readback = graph
            .readback_image("Default normal", view.normal.image())
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
        accept_and_release_defaults(renderer::runtime::RuntimeConfig::Vulkan(Default::default()));
    }

    #[cfg(all(feature = "d3d12", windows))]
    #[test]
    #[ignore = "requires a real D3D12 device"]
    fn d3d12_defaults_can_be_unregistered_during_accepted_reads() {
        accept_and_release_defaults(renderer::runtime::RuntimeConfig::D3D12(Default::default()));
    }
}
