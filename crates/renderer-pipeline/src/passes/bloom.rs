//! Bloom for scene-linear HDR color before exposure and tone mapping.

mod effect;
#[allow(unsafe_code)]
mod programs;

pub use effect::Bloom;
use effect::{COMBINE_PASS, FILTER_PASSES, RESULT_IMAGE};

use renderer::capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet};
use renderer::graph::{Graph, GraphError, ImageDesc, ImageHandle};

/// Bloom configured for a specific HDR extent and blur kernel.
pub struct PreparedBloom {
    mode: Bloom,
    input: ImageDesc,
    images: Option<[ImageDesc; 4]>,
}

impl Bloom {
    /// Reserve the programs, temporary images and Graph declarations for one
    /// invocation per frame. The source image is reserved by its producer.
    ///
    /// Active Bloom requires a single-sample, single-mip, single-layer RGBA16F
    /// image with scene-linear Rec.709 RGB. Disabled Bloom reserves nothing.
    /// The preparation keeps its original reservations if this call fails.
    pub fn prepare(
        self,
        preparation: &mut Preparation,
        input: ImageDesc,
    ) -> Result<PreparedBloom, GraphError> {
        let images = self.images(input);
        if let Some(images) = images {
            if input != ImageDesc::rgba16_float(input.width, input.height) {
                return Err(GraphError::invalid_input(
                    "Bloom requires single-subresource 1× RGBA16F scene color",
                ));
            }
            let count = preparation.frames_in_flight().get();
            let resources = images.map(|desc| ResourceClass::Image { desc, count });
            let passes = FILTER_PASSES.len() + 1;
            // Each filter samples and writes one image. Combine reads two
            // source images and writes one. Each binding is one image access.
            let bindings = 2 * FILTER_PASSES.len() + 3;
            let name_bytes = 2 * FILTER_PASSES.iter().map(|name| name.len()).sum::<usize>()
                + COMBINE_PASS.len()
                + RESULT_IMAGE.len();
            preparation.add_requirements(
                GraphRequirements {
                    resources: resources.len(),
                    passes,
                    compute_dispatches: passes,
                    accesses: bindings,
                    bindings,
                    name_bytes,
                    ..GraphRequirements::default()
                },
                WorkingSet {
                    resources: &resources,
                    raster: &[],
                    compute: self.programs(),
                },
            )?;
        }
        Ok(PreparedBloom {
            mode: self,
            input,
            images,
        })
    }
}

impl PreparedBloom {
    /// Append Bloom and return a same-size scene-linear HDR image with the
    /// original alpha. Disabled Bloom returns the input handle. The input must
    /// belong to the current Graph and match the active prepared description.
    ///
    /// Active Bloom extracts highlights above 1, downsamples twice, blurs at
    /// quarter resolution, and adds one eighth of that contribution to RGB.
    /// If a declaration fails, discard this frame's Graph.
    pub fn add_passes(
        &self,
        graph: &mut Graph,
        input: ImageHandle,
    ) -> Result<ImageHandle, GraphError> {
        if let Some(images) = self.images {
            if graph.image_desc(input)? != self.input {
                return Err(GraphError::invalid_input(
                    "Bloom input must match the prepared 1× RGBA16F image",
                ));
            }
            self.mode.append(graph, input, images)
        } else {
            graph.image_desc(input)?;
            Ok(input)
        }
    }
}
