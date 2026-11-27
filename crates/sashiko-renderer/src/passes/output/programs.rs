//! Trusted fragment programs for the standard tone mapping and SDR presets.

use super::{SdrEncoding, SdrOutput};
use crate::shader_binaries::packaged_shaders;
use sashiko_renderer_core::ImageFormat;
use sashiko_renderer_core::program::{
    BindingLayout, BindingType, BlendState, DepthState, ManagedRasterProgram,
    RasterParameterLayout, RasterProgramDescriptor, SampledImageSamplerPair, ShaderStages,
    fullscreen_triangle_shaders,
};

pub(super) const fn tone_map_neutral() -> ManagedRasterProgram {
    // SAFETY: The reviewed fragment shader reads one exposure word and the
    // declared texture/sampler at LOD zero, then writes a single HDR color.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&TONE_NEUTRAL) }
}

pub(crate) const fn film_composite() -> ManagedRasterProgram {
    // SAFETY: The reviewed fullscreen fragment reads two 2D color images, one sky sampler
    // and six float4 constants; it writes a single display-linear HDR target.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&FILM_COMPOSITE) }
}

pub(super) const fn sdr(output: SdrOutput) -> ManagedRasterProgram {
    let descriptor = match (output.encoding, output.fxaa) {
        (SdrEncoding::Linear, false) => &SDR_LINEAR,
        (SdrEncoding::Linear, true) => &SDR_FXAA_LINEAR,
        (SdrEncoding::Srgb, false) => &SDR_SRGB,
        (SdrEncoding::Srgb, true) => &SDR_FXAA_SRGB,
    };
    // SAFETY: The four reviewed variants share the declared 2D image/sampler
    // and a single color output. FXAA samples LOD zero with a bounded search.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) }
}

macro_rules! output_shaders {
    ($name:literal, $inline_constants:expr) => {
        fullscreen_triangle_shaders(
            packaged_shaders!(module: $name, folder: "output/", nonfiltering: &[]),
            $inline_constants,
        )
    };
}

static SDR_SRGB: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 0,
    secondary_vertex_stride: 0,
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
    shaders: output_shaders!("sdr.frag", None),
};
static SDR_LINEAR: RasterProgramDescriptor = RasterProgramDescriptor {
    shaders: output_shaders!("sdr_linear.frag", None),
    ..SDR_SRGB
};
static SDR_FXAA_SRGB: RasterProgramDescriptor = RasterProgramDescriptor {
    shaders: output_shaders!("sdr_fxaa.frag", None),
    ..SDR_SRGB
};
static SDR_FXAA_LINEAR: RasterProgramDescriptor = RasterProgramDescriptor {
    shaders: output_shaders!("sdr_fxaa_linear.frag", None),
    ..SDR_SRGB
};
static TONE_NEUTRAL: RasterProgramDescriptor = RasterProgramDescriptor {
    parameters: RasterParameterLayout {
        size: 4,
        stages: ShaderStages::FRAGMENT,
    },
    color_outputs: &[ImageFormat::Rgba16Float],
    shaders: output_shaders!(
        "tone_neutral.frag",
        Some(sashiko_renderer_core::program::D3D12InlineConstantBinding {
            register: 0,
            space: 0
        })
    ),
    ..SDR_SRGB
};

static FILM_COMPOSITE: RasterProgramDescriptor = RasterProgramDescriptor {
    parameters: RasterParameterLayout {
        size: 96,
        stages: ShaderStages::FRAGMENT,
    },
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
            kind: BindingType::SampledImage,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 0,
            binding: 2,
            kind: BindingType::Sampler,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
    ],
    binding_bytes: &[0, 0, 0],
    sampling_pairs: &[SampledImageSamplerPair {
        image_group: 0,
        image_binding: 1,
        sampler_group: 0,
        sampler_binding: 2,
    }],
    shaders: fullscreen_triangle_shaders(
        packaged_shaders!(module:"film_composite.frag",folder:"",nonfiltering:&[(0,0)]),
        Some(sashiko_renderer_core::program::D3D12InlineConstantBinding {
            register: 7,
            space: 0,
        }),
    ),
    ..TONE_NEUTRAL
};
