//! Initialized placeholder for recipes prepared without shadow cascades.

use renderer_core::{
    capacity::GraphRequirements,
    graph::{
        Attachment, Graph, GraphError, ImageDesc, ImageView, LoadOp, RasterAttachments, StoreOp,
    },
};

pub(super) const NAME: &str = "pbr disabled shadows";
const CLEAR: &str = "initialize disabled shadows";

pub(super) fn desc() -> ImageDesc {
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

    pub(super) fn from_view(view: ImageView) -> Self {
        Self(view)
    }

    pub(crate) fn view(self) -> ImageView {
        self.0
    }
}
