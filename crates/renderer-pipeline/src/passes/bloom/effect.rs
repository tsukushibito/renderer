//! Shared Graph declarations for the standard HDR Bloom effect.

#[allow(unsafe_code)]
#[path = "programs.rs"]
mod programs;

use renderer::graph::{Binding, BindingResource, Graph, GraphError, ImageDesc, ImageHandle};
use renderer::program::ManagedProgram;

use programs::{BLUR_BOX, BLUR_TENT, COMBINE, DOWNSAMPLE, EXTRACT};

/// Choose the quarter-resolution Bloom blur kernel, or bypass Bloom.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Bloom {
    #[default]
    Off,
    Tent,
    Box,
}

pub(super) const FILTER_PASSES: [&str; 4] = [
    "Bloom extract",
    "Bloom downsample half",
    "Bloom downsample quarter",
    "Bloom blur",
];
pub(super) const COMBINE_PASS: &str = "Bloom combine";
pub(super) const RESULT_IMAGE: &str = "Bloom HDR";

impl Bloom {
    /// A short identifier suitable for display and settings.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Tent => "tent",
            Self::Box => "box",
        }
    }

    /// Audited Programs needed by the selected Bloom kernel.
    pub fn programs(self) -> &'static [ManagedProgram] {
        static TENT: [ManagedProgram; 4] = [EXTRACT, DOWNSAMPLE, BLUR_TENT, COMBINE];
        static BOX: [ManagedProgram; 4] = [EXTRACT, DOWNSAMPLE, BLUR_BOX, COMBINE];
        match self {
            Self::Off => &[],
            Self::Tent => &TENT,
            Self::Box => &BOX,
        }
    }

    /// Extra scratch images per execution, in declaration order. Equal descriptions
    /// can occur at small sizes and still consume separate slots.
    pub fn images(self, input: ImageDesc) -> Option<[ImageDesc; 5]> {
        if self == Self::Off {
            return None;
        }
        let half = ImageDesc::rgba16_float(input.width.div_ceil(2), input.height.div_ceil(2));
        let quarter = ImageDesc::rgba16_float(input.width.div_ceil(4), input.height.div_ceil(4));
        Some([input, half, quarter, quarter, input])
    }

    /// Declare Bloom for a caller-prepared Graph. Use [`Self::prepare`] to
    /// combine this pass's reservations with other components when possible.
    pub fn add_passes(
        self,
        graph: &mut Graph,
        input: ImageHandle,
    ) -> Result<ImageHandle, GraphError> {
        let desc = graph.image_desc(input)?;
        match self.images(desc) {
            Some(images) if desc == ImageDesc::rgba16_float(desc.width, desc.height) => {
                self.append(graph, input, images)
            }
            Some(_) => Err(GraphError::invalid_input(
                "Bloom input must be single-subresource 1× RGBA16F scene color",
            )),
            None => Ok(input),
        }
    }

    pub(super) fn append(
        self,
        graph: &mut Graph,
        input: ImageHandle,
        [full, half, quarter, blur, output]: [ImageDesc; 5],
    ) -> Result<ImageHandle, GraphError> {
        let [extract_name, half_name, quarter_name, blur_name] = FILTER_PASSES;
        let extract = filter(graph, extract_name, EXTRACT, full, input)?;
        let down_half = filter(graph, half_name, DOWNSAMPLE, half, extract)?;
        let down_quarter = filter(graph, quarter_name, DOWNSAMPLE, quarter, down_half)?;
        let kernel = if self == Self::Tent {
            BLUR_TENT
        } else {
            BLUR_BOX
        };
        let blurred = filter(graph, blur_name, kernel, blur, down_quarter)?;
        let result = graph.image(RESULT_IMAGE, output)?;
        graph.dispatch(
            COMBINE_PASS,
            &COMBINE,
            &[
                image_binding(graph, 0, 0, input)?,
                image_binding(graph, 0, 1, blurred)?,
                image_binding(graph, 1, 0, result)?,
            ],
        )?;
        Ok(result)
    }
}

fn filter(
    graph: &mut Graph,
    name: &'static str,
    program: ManagedProgram,
    desc: ImageDesc,
    input: ImageHandle,
) -> Result<ImageHandle, GraphError> {
    let output = graph.image(name, desc)?;
    graph.dispatch(
        name,
        &program,
        &[
            image_binding(graph, 0, 0, input)?,
            image_binding(graph, 1, 0, output)?,
        ],
    )?;
    Ok(output)
}

fn image_binding(
    graph: &Graph,
    group: u32,
    binding: u32,
    image: ImageHandle,
) -> Result<Binding, GraphError> {
    Ok(Binding {
        group,
        binding,
        resource: BindingResource::ImageView(graph.image_view(image, 0, 0)?),
    })
}
