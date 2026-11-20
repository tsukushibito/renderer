//! Trusted fullscreen output programs shared by the public Graph builders.

use super::{
    BlendState, D3D12InlineConstantBinding, D3D12RasterShaders, DepthState, ManagedRasterProgram,
    RasterParameterLayout, RasterProgramDescriptor, RasterShaderStages, SampledImageSamplerPair,
};
use crate::ImageFormat;
use crate::program::{
    BindingLayout, BindingType, ShaderBinaries, ShaderStages, embedded_shader_binaries,
};

impl ManagedRasterProgram {
    /// Fullscreen program applying exposure and Khronos PBR Neutral tone mapping.
    pub const fn tone_map_neutral() -> Self {
        // SAFETY: The reviewed shaders use vertex IDs 0..2, the declared
        // fragment word, and bounded normalized LOD-zero sampling. Arithmetic
        // and the inline word do not affect the resource footprint. The
        // fragment writes only one float4 color.
        unsafe { Self::from_trusted_descriptor(&TONE_NEUTRAL) }
    }

    /// Fullscreen program preserving display-linear RGB in RGBA8, with optional FXAA.
    pub const fn sdr_linear(fxaa: bool) -> Self {
        let descriptor = if fxaa { &SDR_FXAA_LINEAR } else { &SDR_LINEAR };
        // SAFETY: Both binaries use vertex IDs 0..2 and one matching 2D float
        // image/sampler, normalized LOD-zero samples, and color target zero.
        // FXAA uses guarded arithmetic and a bounded search.
        unsafe { Self::from_trusted_descriptor(descriptor) }
    }

    /// Fullscreen program encoding display-linear RGB as sRGB, with optional FXAA.
    pub const fn sdr_srgb(fxaa: bool) -> Self {
        let descriptor = if fxaa { &SDR_FXAA_SRGB } else { &SDR_SRGB };
        // SAFETY: Both binaries use vertex IDs 0..2 and one matching 2D float
        // image/sampler, normalized LOD-zero samples, and color target zero.
        // FXAA uses guarded arithmetic and a bounded search.
        unsafe { Self::from_trusted_descriptor(descriptor) }
    }
}

/// Pair Renderer's built-in fullscreen triangle vertex shader with fragment
/// binaries. The vertex shader uses vertex IDs 0–2, no inputs or bindings, and
/// entry point `main`. Only enabled backends with supplied fragments are kept.
///
/// The returned binaries still need a descriptor that matches their actual
/// behavior before they can be [imported as a Program](ManagedRasterProgram::from_trusted_descriptor).
pub const fn fullscreen_triangle_shaders(
    fragments: ShaderBinaries<&'static [u8]>,
    d3d12_inline_constants: Option<D3D12InlineConstantBinding>,
) -> ShaderBinaries<RasterShaderStages, D3D12RasterShaders> {
    ShaderBinaries {
        vulkan: match fragments.vulkan {
            #[cfg(feature = "vulkan")]
            Some(fragment) => Some(RasterShaderStages {
                vertex: include_bytes!("../../../shaders/composite.vert.spv"),
                fragment: Some(fragment),
            }),
            _ => None,
        },
        d3d12: match (fragments.d3d12, d3d12_inline_constants) {
            #[cfg(all(feature = "d3d12", windows))]
            (Some(fragment), inline_constants) => Some(D3D12RasterShaders {
                stages: RasterShaderStages {
                    vertex: include_bytes!("../../../shaders/dxil/composite.vert.dxil"),
                    fragment: Some(fragment),
                },
                inline_constants,
            }),
            _ => None,
        },
    }
}

macro_rules! output_shaders {
    ($spirv:literal, $dxil:literal, $inline_constants:expr) => {
        fullscreen_triangle_shaders(
            embedded_shader_binaries!(include_bytes!($spirv), include_bytes!($dxil)),
            $inline_constants,
        )
    };
}

static SDR_SRGB: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 0,
    vertex_attributes: &[],
    parameters: RasterParameterLayout {
        size: 0,
        stages: ShaderStages::EMPTY,
    },
    color_outputs: &[ImageFormat::Rgba8Unorm],
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::SampledImage,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::Sampler,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
    ],
    binding_bytes: &[0, 0],
    sampling_pairs: &[SampledImageSamplerPair {
        image_group: 0,
        image_binding: 0,
        sampler_group: 0,
        sampler_binding: 1,
    }],
    blend: BlendState::Replace,
    depth: DepthState::Disabled,
    shaders: output_shaders!(
        "../../../shaders/sdr.frag.spv",
        "../../../shaders/dxil/sdr.frag.dxil",
        None
    ),
};
static SDR_LINEAR: RasterProgramDescriptor = RasterProgramDescriptor {
    shaders: output_shaders!(
        "../../../shaders/sdr_linear.frag.spv",
        "../../../shaders/dxil/sdr_linear.frag.dxil",
        None
    ),
    ..SDR_SRGB
};
static SDR_FXAA_SRGB: RasterProgramDescriptor = RasterProgramDescriptor {
    shaders: output_shaders!(
        "../../../shaders/sdr_fxaa.frag.spv",
        "../../../shaders/dxil/sdr_fxaa.frag.dxil",
        None
    ),
    ..SDR_SRGB
};
static SDR_FXAA_LINEAR: RasterProgramDescriptor = RasterProgramDescriptor {
    shaders: output_shaders!(
        "../../../shaders/sdr_fxaa_linear.frag.spv",
        "../../../shaders/dxil/sdr_fxaa_linear.frag.dxil",
        None
    ),
    ..SDR_SRGB
};
static TONE_NEUTRAL: RasterProgramDescriptor = RasterProgramDescriptor {
    parameters: RasterParameterLayout {
        size: 4,
        stages: ShaderStages::FRAGMENT,
    },
    color_outputs: &[ImageFormat::Rgba16Float],
    shaders: output_shaders!(
        "../../../shaders/tone_neutral.frag.spv",
        "../../../shaders/dxil/tone_neutral.frag.dxil",
        Some(crate::program::D3D12InlineConstantBinding {
            register: 0,
            space: 0
        })
    ),
    ..SDR_SRGB
};
