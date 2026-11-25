use super::{ExecutionError, FrameId, ManagedImage, Runtime};
use crate::graph::{GraphError, GraphPool, ImageDesc, ImageRegion};
use std::ops::Range;
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

    /// Create an immutable sampled image version. New bytes are tightly packed
    /// in increasing mip order. Previously initialized levels outside this range
    /// can be copied from another version without changing its revision.
    pub fn upload_texture_mips(
        &mut self,
        pool: &mut GraphPool,
        name: &str,
        desc: ImageDesc,
        previous: Option<&ManagedImage>,
        levels: Range<u32>,
        mut bytes: &[u8],
    ) -> Result<(FrameId, ManagedImage), TextureUploadError> {
        let mut build = || -> Result<_, GraphError> {
            if levels.is_empty()
                || levels.end > desc.mip_levels
                || previous.is_some_and(|p| p.snapshot().desc() != desc)
            {
                return Err(GraphError::invalid_input(
                    "invalid texture mip selection or source description",
                ));
            }
            let mut graph = pool.begin()?;
            let image = graph.image(name, desc)?;
            let source = previous
                .map(|p| graph.import_image(name, p.snapshot()))
                .transpose()?;
            for mip in 0..desc.mip_levels {
                let region = ImageRegion::mip(desc, mip)?;
                if levels.contains(&mip) {
                    let size = desc
                        .format
                        .packed_byte_len(region.extent[0], region.extent[1])
                        .and_then(|v| usize::try_from(v).ok())
                        .ok_or_else(|| GraphError::invalid_input("texture mip size overflow"))?;
                    let (data, rest) = bytes
                        .split_at_checked(size)
                        .ok_or_else(|| GraphError::invalid_input("truncated texture mip"))?;
                    bytes = rest;
                    graph.upload_image_region(name, image, region, data)?;
                } else if let Some(src) = source
                    && previous.is_some_and(|p| p.snapshot().is_mip_initialized(mip))
                {
                    graph.copy_image_region(name, src, region, image, mip, [0, 0])?;
                } else {
                    continue;
                }
                graph.export_image_mips(image, mip..mip + 1)?;
            }
            if !bytes.is_empty() {
                return Err(GraphError::invalid_input("trailing texture mip data"));
            }
            Ok((graph.compile()?, image))
        };
        let (plan, image) = build().map_err(TextureUploadError::Graph)?;
        let (submission, owner) = self
            .submit_with_image(&plan, image)
            .map_err(TextureUploadError::Execution)?;
        Ok((submission.id(), owner))
    }
}
