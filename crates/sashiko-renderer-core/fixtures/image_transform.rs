//! Consumer-owned RGBA8 effects shared with the acceptance workloads.

include!("shader_binaries.rs");

use sashiko_renderer_core::{
    ImageFormat,
    graph::{Binding, BindingResource, Graph, GraphError, ImageHandle, PassHandle},
    program::{
        BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
        ManagedProgram, ShaderBinaries, ShaderStages,
    },
};

const LOCAL_SIZE: u32 = 8;

#[derive(Clone, Copy)]
#[allow(
    dead_code,
    reason = "each executable selects the effects in its workload"
)]
pub(crate) enum Transform {
    Invert,
    SwapRedBlue,
}

const BINDINGS: &[BindingLayout] = &[
    BindingLayout {
        group: 0,
        binding: 0,
        kind: BindingType::StorageImageRead(ImageFormat::Rgba8Unorm),
        stages: ShaderStages::COMPUTE,
        min_size: 0,
    },
    BindingLayout {
        group: 0,
        binding: 1,
        kind: BindingType::StorageImageWrite(ImageFormat::Rgba8Unorm),
        stages: ShaderStages::COMPUTE,
        min_size: 0,
    },
];

const fn descriptor(shaders: ShaderBinaries<&'static [u8]>) -> ComputeProgramDescriptor<'static> {
    ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [LOCAL_SIZE, LOCAL_SIZE, 1],
        bindings: BINDINGS,
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::Image2d {
                group: 0,
                binding: 1,
            },
            bytes: &[0, 0],
        },
        shaders,
    }
}

static INVERT: ComputeProgramDescriptor = descriptor(fixture_shaders!(
    include_bytes!("shaders/invert_rgba8.comp.spv"),
    include_bytes!("shaders/dxil/invert_rgba8.comp.dxil"),
));
static SWAP_RED_BLUE: ComputeProgramDescriptor = descriptor(fixture_shaders!(
    include_bytes!("shaders/swap_red_blue_rgba8.comp.spv"),
    include_bytes!("shaders/dxil/swap_red_blue_rgba8.comp.dxil"),
));

pub(crate) const fn program(transform: Transform) -> ManagedProgram {
    let descriptor = match transform {
        Transform::Invert => &INVERT,
        Transform::SwapRedBlue => &SWAP_RED_BLUE,
    };
    // SAFETY: The pinned shader artifacts match the two typed image bindings.
    // For equal selected mip extents, the fringe guard bounds every read and
    // write, and exactly one invocation initializes each output texel.
    // There are no other bindings, shared memory, or side effects.
    unsafe { ManagedProgram::from_trusted_descriptor(descriptor) }
}

pub(crate) fn dispatch(
    graph: &mut Graph,
    name: &str,
    source: ImageHandle,
    destination: ImageHandle,
    transform: Transform,
) -> Result<PassHandle, GraphError> {
    let bindings = [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::ImageView(graph.image_view(source, 0, 0)?),
        },
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::ImageView(graph.image_view(destination, 0, 0)?),
        },
    ];
    graph.dispatch(name, &program(transform), &bindings)
}
