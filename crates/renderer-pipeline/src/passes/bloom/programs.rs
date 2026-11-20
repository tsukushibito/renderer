//! Audited shader descriptors for the Bloom compute passes.

use crate::shader_binaries::packaged_shaders;

use renderer::ImageFormat;
use renderer::program::{
    BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
    ManagedProgram, ShaderStages,
};

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
        pub(super) const $name: ManagedProgram = {
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
                    include_bytes!(concat!("../../../shaders/", $shader, ".spv")),
                    include_bytes!(concat!("../../../shaders/dxil/", $shader, ".dxil")),
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
