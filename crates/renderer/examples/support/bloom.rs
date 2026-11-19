//! Consumer-owned scene-linear HDR post processing.

use crate::shader_binaries::packaged_shaders;

use renderer::ImageFormat;
use renderer::graph::{Binding, BindingResource, Graph, GraphError, ImageDesc, ImageHandle};
use renderer::program::{
    BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
    ManagedProgram, ShaderStages,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Bloom {
    #[default]
    Off,
    Tent,
    Box,
}

const GROUP_SIZE: u32 = 8;
const WRITE_HDR: BindingType = BindingType::StorageImageWrite(ImageFormat::Rgba16Float);

const fn slot(group: u32, binding: u32, kind: BindingType) -> BindingLayout {
    BindingLayout {
        group,
        binding,
        kind,
        stages: ShaderStages::COMPUTE,
        min_size: 0,
    }
}

const FILTER_BINDINGS: &[BindingLayout] =
    &[slot(0, 0, BindingType::SampledImage), slot(1, 0, WRITE_HDR)];
const COMBINE_BINDINGS: &[BindingLayout] = &[
    slot(0, 0, BindingType::SampledImage),
    slot(0, 1, BindingType::SampledImage),
    slot(1, 0, WRITE_HDR),
];

macro_rules! bloom_program {
    ($name:ident, $shader:literal, $bindings:ident, $bytes:expr) => {
        const $name: ManagedProgram = {
            static DESCRIPTOR: ComputeProgramDescriptor = ComputeProgramDescriptor {
                entry: "main",
                workgroup_size: [GROUP_SIZE, GROUP_SIZE, 1],
                bindings: $bindings,
                dispatch: DispatchContract {
                    workgroups: DispatchWorkgroups::Image2d {
                        group: 1,
                        binding: 0,
                    },
                    bytes: $bytes,
                },
                shaders: packaged_shaders!(
                    include_bytes!(concat!("bistro/shaders/", $shader, ".spv")),
                    include_bytes!(concat!("bistro/shaders/dxil/", $shader, ".dxil")),
                ),
            };
            // SAFETY: The paired artifacts implement the declared ABI. Each invocation
            // guards its destination coordinate, reads only clamped coordinates in
            // the declared input Views, and writes a disjoint output texel. There are
            // no buffer accesses, workgroup communication or other side effects.
            unsafe { ManagedProgram::from_trusted_descriptor(&DESCRIPTOR) }
        };
    };
}

bloom_program!(EXTRACT, "bloom_extract.comp", FILTER_BINDINGS, &[0, 0]);
bloom_program!(
    DOWNSAMPLE,
    "bloom_downsample.comp",
    FILTER_BINDINGS,
    &[0, 0]
);
bloom_program!(BLUR_TENT, "bloom_blur_tent.comp", FILTER_BINDINGS, &[0, 0]);
bloom_program!(BLUR_BOX, "bloom_blur_box.comp", FILTER_BINDINGS, &[0, 0]);
bloom_program!(COMBINE, "bloom_combine.comp", COMBINE_BINDINGS, &[0, 0, 0]);

impl Bloom {
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Tent => "tent",
            Self::Box => "box",
        }
    }

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

    /// Append Bloom and return same-size, single-subresource HDR color. The input
    /// must contain initialized scene-linear Rec.709 color when the graph executes.
    /// Disabled Bloom returns the input immediately. On construction failure,
    /// abandon the partially composed graph and return its pool slot.
    pub fn append(self, graph: &mut Graph, input: ImageHandle) -> Result<ImageHandle, GraphError> {
        let desc = graph.image_desc(input)?;
        if desc != ImageDesc::rgba16_float(desc.width, desc.height) {
            return Err(GraphError::custom(
                "Bloom requires single-subresource RGBA16F scene color",
            ));
        }
        let Some([full, half, quarter, blur, output]) = self.images(desc) else {
            return Ok(input);
        };
        let extract = filter(graph, "Bloom extract", EXTRACT, full, input)?;
        let down_half = filter(graph, "Bloom downsample half", DOWNSAMPLE, half, extract)?;
        let down_quarter = filter(
            graph,
            "Bloom downsample quarter",
            DOWNSAMPLE,
            quarter,
            down_half,
        )?;
        let kernel = if self == Self::Tent {
            BLUR_TENT
        } else {
            BLUR_BOX
        };
        let blurred = filter(graph, "Bloom blur", kernel, blur, down_quarter)?;
        let result = graph.image("Bloom HDR", output)?;
        graph.dispatch(
            "Bloom combine",
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
