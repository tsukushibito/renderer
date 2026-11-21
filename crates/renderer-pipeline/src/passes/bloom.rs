//! Bloom for scene-linear HDR color before exposure and tone mapping.

mod effect;
#[allow(unsafe_code)]
mod programs;

pub use effect::Bloom;
use effect::{COMBINE_PASS, FILTER_PASSES, RESULT_IMAGE};

use renderer::capacity::Preparation;
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
            let mut stages = Preparation::new(preparation.frames_in_flight());
            let [half, quarter, blur] = FILTER_PASSES;
            let names = [
                (half, half),
                (quarter, quarter),
                (blur, blur),
                (RESULT_IMAGE, COMBINE_PASS),
            ];
            for ((image, (target, pass)), program) in
                images.into_iter().zip(names).zip(self.programs())
            {
                stages.reserve_transient_image(target, image)?;
                stages.reserve_dispatch(pass, program)?;
            }
            preparation.add_requirements(stages.graph_requirements(), stages.working_set())?;
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
