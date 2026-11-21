use super::{ExecutionError, FrameId, ManagedImage, Runtime};
use crate::graph::{GraphError, GraphPool, ImageDesc};
use std::{error::Error, fmt};

/// Failure to declare or submit a packed texture upload.
#[derive(Debug)]
pub enum TextureUploadError {
    Graph(GraphError),
    Execution(ExecutionError),
}

impl fmt::Display for TextureUploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Graph(error) => error.fmt(f),
            Self::Execution(error) => error.fmt(f),
        }
    }
}

impl Error for TextureUploadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(match self {
            Self::Graph(error) => error,
            Self::Execution(error) => error,
        })
    }
}

impl Runtime {
    /// Upload a packed texture from engine-owned asset data. The result can
    /// be used immediately by later submissions on this Runtime. Retain the
    /// owner as long as future Graphs need the texture. Reserve one pool slot
    /// per concurrent upload or follow-up Graph and poll the frame
    /// through the usual completion API. No host wait is performed here.
    ///
    /// To batch several textures and other work in one submission, use
    /// [`crate::graph::Graph::upload_texture_for_export`] and [`Runtime::submit_with_images`].
    pub fn upload_texture(
        &mut self,
        pool: &mut GraphPool,
        name: &str,
        desc: ImageDesc,
        bytes: &[u8],
    ) -> Result<(FrameId, ManagedImage), TextureUploadError> {
        let mut graph = pool.begin().map_err(TextureUploadError::Graph)?;
        let image = graph
            .upload_texture_for_export(name, desc, bytes)
            .map_err(TextureUploadError::Graph)?;
        let plan = graph.compile().map_err(TextureUploadError::Graph)?;
        let (submission, image) = self
            .submit_with_image(&plan, image)
            .map_err(TextureUploadError::Execution)?;
        Ok((submission.id(), image))
    }
}
