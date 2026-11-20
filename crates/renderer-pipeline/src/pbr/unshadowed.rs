//! Initialized placeholder for recipes prepared without shadow cascades.

use renderer::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    graph::{
        Attachment, Graph, GraphError, ImageDesc, ImageView, ImportImage, LoadOp,
        RasterAttachments, StoreOp,
    },
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::{
    graph::{GraphCapacity, GraphPool},
    runtime::{ExecutionError, FrameId, Runtime},
};

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use super::DefaultResourceError;

const NAME: &str = "pbr disabled shadows";
const CLEAR: &str = "initialize disabled shadows";

fn desc() -> ImageDesc {
    ImageDesc::depth32(1, 1)
}

/// An initialized 1×1 D32 view for the shader's shadow binding slots.
#[derive(Clone, Copy, Debug)]
pub struct UnshadowedView(ImageView);

impl UnshadowedView {
    /// Graph capacity for creating and clearing the placeholder.
    pub fn requirements() -> GraphRequirements {
        GraphRequirements {
            resources: 1,
            passes: 1,
            accesses: 1,
            attachments: 1,
            name_bytes: NAME.len() + CLEAR.len(),
            ..GraphRequirements::default()
        }
    }

    /// For a one-off Graph, declare a clear and consume the view in that Graph.
    /// Reserve one transient D32 1×1 image per overlapping submission.
    /// For recurring frames, initialize and import one shared placeholder.
    pub fn append(graph: &mut Graph) -> Result<Self, GraphError> {
        let image = graph.image(NAME, desc())?;
        let view = graph.image_view(image, 0, 0)?;
        graph.attachment_pass(
            CLEAR,
            RasterAttachments {
                colors: &[],
                color_resolves: &[],
                depth: Some(Attachment {
                    view,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )?;
        Ok(Self(view))
    }

    pub(crate) fn view(self) -> ImageView {
        self.0
    }
}

/// One prepared shadow placeholder that can be imported by multiple views.
pub struct Unshadowed {
    image: Option<ImportImage>,
}

impl Unshadowed {
    /// Register one physical image and room for its import in a frame Graph.
    /// The Runtime must also have room for the startup clear Graph.
    pub fn reserve(preparation: &mut Preparation) -> Result<(), GraphError> {
        preparation.add_requirements(
            GraphRequirements {
                resources: 1,
                name_bytes: NAME.len(),
                ..GraphRequirements::default()
            },
            WorkingSet {
                resources: &[ResourceClass::Image {
                    desc: desc(),
                    count: 1,
                }],
                ..WorkingSet::default()
            },
        )
    }

    /// Submit the clear once. The same Runtime can accept a Graph importing
    /// the scheduled contents while the clear is pending, subject to capacity.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn initialize(runtime: &mut Runtime) -> Result<(Self, FrameId), DefaultResourceError> {
        let requirements = UnshadowedView::requirements();
        let mut pool = GraphPool::new(
            GraphCapacity {
                resources: requirements.resources,
                passes: requirements.passes,
                accesses: requirements.accesses,
                attachments: requirements.attachments,
                name_bytes: requirements.name_bytes,
                ..GraphCapacity::default()
            },
            1,
        )?;
        let mut graph = pool.begin()?;
        let target = UnshadowedView::append(&mut graph)?.view().image();
        graph.export_image(target)?;
        let submitted = runtime.submit(&graph.compile()?)?;
        Ok((
            Self {
                image: Some(submitted.export_image(target)?),
            },
            submitted.id(),
        ))
    }

    /// Import the immutable clear into the current Graph.
    pub fn import(&self, graph: &mut Graph) -> Result<UnshadowedView, GraphError> {
        let snapshot = self
            .image
            .as_ref()
            .ok_or_else(|| GraphError::invalid_input("the shadow placeholder has been released"))?;
        let image = graph.import_image(NAME, snapshot)?;
        Ok(UnshadowedView(graph.image_view(image, 0, 0)?))
    }

    /// Release the registration. Accepted work retains its leases.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn clear(&mut self, runtime: &mut Runtime) -> Result<(), ExecutionError> {
        if let Some(image) = &self.image {
            runtime.delete(image.key())?;
            self.image = None;
        }
        Ok(())
    }
}
