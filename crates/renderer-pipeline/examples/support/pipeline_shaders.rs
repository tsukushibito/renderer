//! Application-side resolution of logical names to reviewed shader fixtures.

use std::{error::Error, fs, io, path::Path};

use renderer::{
    ImageFormat,
    program::{
        BindingLayout, BindingType, BlendState, ComputeProgramDescriptor,
        D3D12InlineConstantBinding, D3D12RasterShaders, DispatchContract, DispatchWorkgroups,
        ManagedProgram, ManagedRasterProgram, RasterProgramDescriptor, RasterShaderStages,
        ShaderBinaries, ShaderStages,
    },
    standard::passes::{SdrEncoding, SdrOutput},
};
use renderer_pipeline::{Program, ShaderRequest};

type AssetError = Box<dyn Error + Send + Sync>;

#[derive(Clone, Copy, Debug)]
pub(super) enum Backend {
    Vulkan,
    D3D12,
}

pub(super) fn resolve(backend: Backend, request: ShaderRequest<'_>) -> Result<Program, AssetError> {
    let program = match request {
        ShaderRequest::Program("scene/depth") => {
            Program::Raster(ManagedRasterProgram::depth_only())
        }
        ShaderRequest::Program("scene/opaque") => Program::Raster(ManagedRasterProgram::mesh()),
        ShaderRequest::Program("scene/prepassed") => {
            Program::Raster(ManagedRasterProgram::mesh_prepassed())
        }
        ShaderRequest::Program("post/copy") => Program::Raster(load_fullscreen(backend)?),
        ShaderRequest::Program("post/invert") => Program::Compute(load_transform(backend, true)?),
        ShaderRequest::Program("post/swap-red-blue") => {
            Program::Compute(load_transform(backend, false)?)
        }
        ShaderRequest::Template {
            template: "scene/layers",
            material_type: "replace",
        } => Program::Raster(load_layer(backend, BlendState::Replace)?),
        ShaderRequest::Template {
            template: "scene/layers",
            material_type: "additive",
        } => Program::Raster(load_layer(backend, BlendState::Additive)?),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("unknown shader request {request:?}"),
            )
            .into());
        }
    };
    Ok(program)
}

fn checked_fixture(name: &str, reviewed: &[u8]) -> io::Result<Vec<u8>> {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../renderer")
        .join(name);
    let bytes = fs::read(&file)?;
    if bytes != reviewed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("fixture changed without review: {}", file.display()),
        ));
    }
    Ok(bytes)
}

macro_rules! load {
    ($name:literal) => {
        checked_fixture($name, include_bytes!(concat!("../../../renderer/", $name)))
    };
}

fn load_fullscreen(backend: Backend) -> Result<ManagedRasterProgram, AssetError> {
    let shaders = match backend {
        Backend::Vulkan => ShaderBinaries::vulkan(RasterShaderStages {
            vertex: load!("shaders/composite.vert.spv")?,
            fragment: Some(load!("shaders/sdr_linear.frag.spv")?),
        }),
        Backend::D3D12 => ShaderBinaries::d3d12(D3D12RasterShaders {
            stages: RasterShaderStages {
                vertex: load!("shaders/dxil/composite.vert.dxil")?,
                fragment: Some(load!("shaders/dxil/sdr_linear.frag.dxil")?),
            },
            inline_constants: None,
        }),
    };
    let standard = SdrOutput {
        encoding: SdrEncoding::Linear,
        fxaa: false,
    }
    .pipeline()
    .program;
    // SAFETY: These files were checked byte-for-byte against the reviewed
    // fullscreen program. It uses vertex IDs 0..2, reads only the declared
    // image through the supplied clamp sampler at LOD 0, and writes color 0.
    unsafe { import_raster(standard.descriptor(), shaders) }
}

fn load_layer(backend: Backend, blend: BlendState) -> Result<ManagedRasterProgram, AssetError> {
    let shaders = match backend {
        Backend::Vulkan => ShaderBinaries::vulkan(RasterShaderStages {
            vertex: load!("shaders/textured.vert.spv")?,
            fragment: Some(load!("shaders/textured.frag.spv")?),
        }),
        Backend::D3D12 => ShaderBinaries::d3d12(D3D12RasterShaders {
            stages: RasterShaderStages {
                vertex: load!("shaders/dxil/textured.vert.dxil")?,
                fragment: Some(load!("shaders/dxil/textured.frag.dxil")?),
            },
            inline_constants: Some(D3D12InlineConstantBinding {
                register: 1,
                space: 0,
            }),
        }),
    };
    let standard = ManagedRasterProgram::textured_mesh();
    let mut desc = standard.descriptor();
    desc.blend = blend;
    // SAFETY: Both shader files match the reviewed textured mesh bytes, with
    // the same vertex/inline-constant layout, uniform and full selected-mip
    // sample footprint. Fixed-function blending leaves that contract intact.
    unsafe { import_raster(desc, shaders) }
}

/// Import exact reviewed variants with the supplied descriptor's ABI and
/// execution contract. The caller establishes the same trust guarantees as
/// ManagedRasterProgram::from_trusted_owned_descriptor.
unsafe fn import_raster(
    desc: RasterProgramDescriptor<'_>,
    shaders: ShaderBinaries<RasterShaderStages<Vec<u8>>, D3D12RasterShaders<Vec<u8>>>,
) -> Result<ManagedRasterProgram, AssetError> {
    // SAFETY: Delegated to the caller at this application asset boundary.
    let program = unsafe {
        ManagedRasterProgram::from_trusted_owned_descriptor(RasterProgramDescriptor {
            entry: desc.entry,
            vertex_stride: desc.vertex_stride,
            vertex_attributes: desc.vertex_attributes,
            parameters: desc.parameters,
            color_outputs: desc.color_outputs,
            bindings: desc.bindings,
            binding_bytes: desc.binding_bytes,
            sampling_pairs: desc.sampling_pairs,
            blend: desc.blend,
            depth: desc.depth,
            shaders,
        })
    }?;
    Ok(program)
}

const COMPUTE_BINDINGS: &[BindingLayout] = &[
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

fn load_transform(backend: Backend, invert: bool) -> Result<ManagedProgram, AssetError> {
    let shaders = match (backend, invert) {
        (Backend::Vulkan, true) => {
            ShaderBinaries::vulkan(load!("fixtures/shaders/invert_rgba8.comp.spv")?)
        }
        (Backend::Vulkan, false) => {
            ShaderBinaries::vulkan(load!("fixtures/shaders/swap_red_blue_rgba8.comp.spv")?)
        }
        (Backend::D3D12, true) => {
            ShaderBinaries::d3d12(load!("fixtures/shaders/dxil/invert_rgba8.comp.dxil")?)
        }
        (Backend::D3D12, false) => ShaderBinaries::d3d12(load!(
            "fixtures/shaders/dxil/swap_red_blue_rgba8.comp.dxil"
        )?),
    };
    // SAFETY: The file matches the reviewed shader for the declared two image
    // slots and 8x8x1 domain. The shader guards fringe invocations and
    // initializes each output texel once. Graph checks equal storage extents.
    let program = unsafe {
        ManagedProgram::from_trusted_owned_descriptor(ComputeProgramDescriptor {
            entry: "main",
            workgroup_size: [8, 8, 1],
            bindings: COMPUTE_BINDINGS,
            dispatch: DispatchContract {
                workgroups: DispatchWorkgroups::Image2d {
                    group: 0,
                    binding: 1,
                },
                bytes: &[0, 0],
            },
            shaders,
        })
    }?;
    Ok(program)
}
