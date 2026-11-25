use sashiko_renderer::material::{AlphaMode, MaterialDefinition, Parameter, Value};
use sashiko_renderer_core::program::{
    BlendState, CompareOp, D3D12InlineConstantBinding, D3D12RasterShaders, DepthState,
    ManagedRasterProgram, RasterShaderStages, ShaderBinaries,
};

pub fn toon(blend: bool) -> Result<MaterialDefinition, Box<dyn std::error::Error>> {
    let base = ManagedRasterProgram::mesh_hdr();
    let mut desc = base.descriptor();
    let stages = RasterShaderStages {
        vertex: &include_bytes!("../../../sashiko-renderer-core/shaders/mesh.vert.spv")[..],
        fragment: Some(&include_bytes!("../../shaders/toon.frag.spv")[..]),
    };
    let dxil = D3D12RasterShaders {
        stages: RasterShaderStages {
            vertex: &include_bytes!("../../../sashiko-renderer-core/shaders/dxil/mesh.vert.dxil")[..],
            fragment: Some(&include_bytes!("../../shaders/dxil/toon.frag.dxil")[..]),
        },
        inline_constants: Some(D3D12InlineConstantBinding {
            register: 0,
            space: 0,
        }),
    };
    desc.shaders = ShaderBinaries::both(stages, dxil);
    if blend {
        desc.blend = BlendState::Alpha;
        desc.depth = DepthState::Required {
            compare: CompareOp::Less,
            write: false,
            require_load: true,
        };
    }
    // SAFETY: the unchanged mesh vertex stage consumes its original position/color
    // layout and 80-byte transform/tint. toon.frag reads only TEXCOORD0 and
    // writes a single color, with no descriptors, side effects or indexing.
    let program = unsafe { ManagedRasterProgram::from_trusted_owned_descriptor(desc)? };
    Ok(MaterialDefinition::new(
        program,
        (!blend).then(ManagedRasterProgram::depth_only),
        if blend {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        },
        vec![Parameter::Constant {
            name: "color".into(),
            word: 16,
            default: Value::Color([1.; 4]),
        }],
    )?)
}
