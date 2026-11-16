//! Consumer-owned Forward PBR assets. Renderer has no material/effect enum here.
//!
//! Vertices are world-space position/normal/tangent/UV, stride 48. Camera words
//! are column-major view-projection, eye, surface-to-sun direction, sun RGB and
//! diffuse ambient RGB (128 bytes). Material words are base RGBA, emissive RGB /
//! normal scale, metallic / roughness / AO strength / cutoff, then four u32 flags
//! (64 bytes). Flags are alpha mode (0 opaque, 1 mask, 2 blend), double-sided,
//! and two reserved zero words. The loader establishes finite values/ranges.
//!
//! Base/emissive textures use sRGB sampled formats; normal/ORM are linear data.
//! The linear RGBA16F target is blended before the final clamp/sRGB encode. This
//! B2 light is directional plus a declared constant diffuse ambient term: no
//! environment IBL, shadows, exposure, tone mapping, Bloom or AA is implied.
//! GGX uses alpha >= 1e-3 to regularize zero roughness; linear output saturates
//! to binary16's finite range before storage. Cooked factors stay unchanged.
//! Vertex clip coordinates use ordered `precise` FP32 products/additions.
//! This prevents compiler-specific contraction/reassociation from amplifying
//! world-space cancellation differently across APIs. It can cost fused-multiply-
//! add opportunities; it does not introduce optional GPU precision features or
//! change the camera ABI. Performance and real-GPU agreement remain measured.

use renderer::ImageFormat;
use renderer::program::{
    BindingLayout, BindingType, BlendState, CompareOp, DepthState, ManagedRasterProgram,
    RasterParameterLayout, RasterProgramDescriptor, ShaderStages, VertexAttribute, VertexFormat,
};

pub const VERTEX_BYTES: usize = 48;
pub const CAMERA_BYTES: usize = 128;
pub const MATERIAL_BYTES: usize = 64;

static OPAQUE: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: VERTEX_BYTES as u32,
    vertex_attributes: &[
        VertexAttribute {
            location: 0,
            offset: 0,
            format: VertexFormat::Float32x3,
        },
        VertexAttribute {
            location: 1,
            offset: 12,
            format: VertexFormat::Float32x3,
        },
        VertexAttribute {
            location: 2,
            offset: 24,
            format: VertexFormat::Float32x4,
        },
        VertexAttribute {
            location: 3,
            offset: 40,
            format: VertexFormat::Float32x2,
        },
    ],
    parameters: RasterParameterLayout {
        size: 0,
        stages: ShaderStages::EMPTY,
        dxil_register: 0,
        dxil_space: 0,
    },
    color_outputs: &[ImageFormat::Rgba16Float],
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::Uniform,
            min_size: CAMERA_BYTES as u64,
            stages: ShaderStages::VERTEX.union(ShaderStages::FRAGMENT),
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::Uniform,
            min_size: MATERIAL_BYTES as u64,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 1,
            binding: 0,
            kind: BindingType::SampledImage,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 1,
            binding: 1,
            kind: BindingType::SampledImage,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 1,
            binding: 2,
            kind: BindingType::SampledImage,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 1,
            binding: 3,
            kind: BindingType::SampledImage,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
        BindingLayout {
            group: 1,
            binding: 4,
            kind: BindingType::Sampler,
            min_size: 0,
            stages: ShaderStages::FRAGMENT,
        },
    ],
    binding_bytes: &[CAMERA_BYTES as u64, MATERIAL_BYTES as u64, 0, 0, 0, 0, 0],
    blend: BlendState::Replace,
    depth: DepthState::Required {
        compare: CompareOp::Less,
        write: true,
        require_load: false,
    },
    vertex_spirv: include_bytes!("shaders/pbr.vert.spv"),
    fragment_spirv: include_bytes!("shaders/pbr.frag.spv"),
    vertex_dxil: include_bytes!("shaders/dxil/pbr.vert.dxil"),
    fragment_dxil: include_bytes!("shaders/dxil/pbr.frag.dxil"),
};

static BLEND: RasterProgramDescriptor = RasterProgramDescriptor {
    blend: BlendState::Alpha,
    depth: DepthState::Required {
        compare: CompareOp::Less,
        write: false,
        require_load: true,
    },
    ..OPAQUE
};

static RESOLVE: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 8,
    vertex_attributes: &[VertexAttribute {
        location: 0,
        offset: 0,
        format: VertexFormat::Float32x2,
    }],
    parameters: RasterParameterLayout {
        size: 0,
        stages: ShaderStages::EMPTY,
        dxil_register: 0,
        dxil_space: 0,
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
    blend: BlendState::Replace,
    depth: DepthState::Disabled,
    vertex_spirv: include_bytes!("shaders/resolve.vert.spv"),
    fragment_spirv: include_bytes!("shaders/resolve.frag.spv"),
    vertex_dxil: include_bytes!("shaders/dxil/resolve.vert.dxil"),
    fragment_dxil: include_bytes!("shaders/dxil/resolve.frag.dxil"),
};

pub const fn opaque_program() -> ManagedRasterProgram {
    // SAFETY: Pinned modules/reflection implement precisely these vertex, UBO,
    // stage and descriptor ABIs. There are no inline constants, storage writes,
    // atomics, depth writes from the shader or hidden resources. The shaders
    // access only constant 128/64-byte UBO prefixes and normalized 2D sampled
    // images within the selected mip Views. Gradients are formed before any
    // discard; hardware sampling bounds all addresses even for arbitrary float
    // payloads. Floating-point guards avoid zero-length normalization/division.
    // No optional arithmetic/capabilities or inter-draw synchronization is used.
    // Source/binary/metadata changes require re-review and regeneration.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&OPAQUE) }
}

pub const fn blend_program() -> ManagedRasterProgram {
    // SAFETY: Same immutable shader/resource contract as opaque_program; only
    // fixed alpha blending and read-only, loaded depth differ. Zero alpha is
    // discarded before attachment writes; blend draws must be ordered by caller.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&BLEND) }
}

pub const fn resolve_program() -> ManagedRasterProgram {
    // SAFETY: Pinned modules use the declared vec2 stream, no parameters, and
    // one image plus sampler at explicit LOD0. Normalized coordinates and the
    // caller's nearest-clamp sampler bound access to that selected View. There
    // are no other resources, side effects, optional capabilities or shader depth.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&RESOLVE) }
}

pub const fn programs() -> [ManagedRasterProgram; 3] {
    [opaque_program(), blend_program(), resolve_program()]
}

/// Fullscreen indexed triangle for resolve, prepared once by the consumer.
pub const RESOLVE_VERTICES: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
pub const RESOLVE_INDICES: [u32; 3] = [0, 1, 2];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_clip_arithmetic_keeps_one_noncontracted_operation_tree() {
        use std::collections::BTreeSet;
        // SPIR-V's fixed binary grammar: Decorate=71, FAdd=129,
        // VectorTimesScalar=142, MatrixTimesVector=145, NoContraction=42.
        // Shader generation also validates the complete module with spirv-val.
        let words: Vec<_> = include_bytes!("shaders/pbr.vert.spv")
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_le_bytes(*bytes))
            .collect();
        let mut cursor = 5;
        let mut decorated = BTreeSet::new();
        let mut products = Vec::new();
        let mut additions = Vec::new();
        while cursor < words.len() {
            let length = (words[cursor] >> 16) as usize;
            assert!(length > 0 && cursor + length <= words.len());
            let instruction = &words[cursor..cursor + length];
            match instruction[0] & 0xffff {
                71 if instruction[2] == 42 => {
                    decorated.insert(instruction[1]);
                }
                129 => additions.push((instruction[2], [instruction[3], instruction[4]])),
                142 => products.push(instruction[2]),
                145 => panic!("implicit matrix-dot reassociation is outside the PBR clip contract"),
                _ => {}
            }
            cursor += length;
        }
        assert_eq!(products.len(), 3);
        assert_eq!(additions.len(), 3);
        for id in products
            .iter()
            .copied()
            .chain(additions.iter().map(|value| value.0))
        {
            assert!(
                decorated.contains(&id),
                "clip arithmetic lacks NoContraction"
            );
        }
        assert_eq!(additions[0].1, [products[0], products[1]]);
        assert_eq!(additions[1].1, [additions[0].0, products[2]]);
        assert_eq!(additions[2].1[0], additions[1].0);
        let hlsl = include_str!("shaders/dxil/pbr.vert.hlsl");
        assert_eq!(hlsl.matches("precise float4 ").count(), 6);
        assert!(!hlsl.contains("mul("));
    }

    #[test]
    fn opacity_and_resolve_keep_depth_and_color_contracts_distinct() {
        let opaque = opaque_program();
        let blend = blend_program();
        assert_eq!(opaque.parameter_size(), 0);
        assert_eq!(opaque.vertex_stride(), 48);
        assert_eq!(opaque.color_outputs(), &[ImageFormat::Rgba16Float]);
        assert!(matches!(
            opaque.depth(),
            DepthState::Required { write: true, .. }
        ));
        assert!(matches!(
            blend.depth(),
            DepthState::Required {
                write: false,
                require_load: true,
                ..
            }
        ));
        assert_eq!(blend.blend(), BlendState::Alpha);
        assert_eq!(
            resolve_program().color_outputs(),
            &[ImageFormat::Rgba8Unorm]
        );
        assert_eq!(opaque.bindings().len(), opaque.binding_bytes().len());
    }

    #[test]
    fn real_shader_reflection_matches_the_host_vertex_and_binding_abi() {
        use std::collections::BTreeMap;
        let vertex: serde_json::Value =
            serde_json::from_str(include_str!("shaders/dxil/pbr.vert.spirv-reflection.json"))
                .unwrap();
        let fragment: serde_json::Value =
            serde_json::from_str(include_str!("shaders/dxil/pbr.frag.spirv-reflection.json"))
                .unwrap();
        let actual_inputs: BTreeMap<_, _> = vertex["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|input| {
                (
                    input["location"].as_u64().unwrap(),
                    input["type"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            actual_inputs,
            BTreeMap::from([(0, "vec3"), (1, "vec3"), (2, "vec4"), (3, "vec2")])
        );
        let program = opaque_program();
        let mut reflected: BTreeMap<(u32, u32), (BindingType, u64, ShaderStages)> = BTreeMap::new();
        for (module, stages) in [
            (&vertex, ShaderStages::VERTEX),
            (&fragment, ShaderStages::FRAGMENT),
        ] {
            assert!(module.get("push_constants").is_none());
            for (key, kind) in [
                ("ubos", BindingType::Uniform),
                ("separate_images", BindingType::SampledImage),
                ("separate_samplers", BindingType::Sampler),
            ] {
                for entry in module[key].as_array().into_iter().flatten() {
                    let coordinate = (
                        entry["set"].as_u64().unwrap() as u32,
                        entry["binding"].as_u64().unwrap() as u32,
                    );
                    let size = entry["block_size"].as_u64().unwrap_or(0);
                    let value =
                        reflected
                            .entry(coordinate)
                            .or_insert((kind, size, ShaderStages::EMPTY));
                    assert_eq!((value.0, value.1), (kind, size));
                    value.2 = value.2.union(stages);
                    if key == "ubos" {
                        let members = module["types"][entry["type"].as_str().unwrap()]["members"]
                            .as_array()
                            .unwrap();
                        let offsets: Vec<_> = members
                            .iter()
                            .map(|member| member["offset"].as_u64().unwrap())
                            .collect();
                        assert_eq!(
                            offsets,
                            if size == CAMERA_BYTES as u64 {
                                vec![0, 64, 80, 96, 112]
                            } else {
                                vec![0, 16, 32, 48]
                            }
                        );
                    }
                }
            }
        }
        assert_eq!(reflected.len(), program.bindings().len());
        for layout in program.bindings() {
            assert_eq!(
                reflected[&(layout.group, layout.binding)],
                (layout.kind, layout.min_size, layout.stages)
            );
        }
        for (reflection, expected) in [
            (
                include_str!("shaders/dxil/pbr.vert.reflection.txt"),
                &["cb0"][..],
            ),
            (
                include_str!("shaders/dxil/pbr.frag.reflection.txt"),
                &[
                    "cb0",
                    "cb1",
                    "t0,space1",
                    "t1,space1",
                    "t2,space1",
                    "t3,space1",
                    "s4,space1",
                ][..],
            ),
        ] {
            let resources = reflection
                .split_once("; Resource Bindings:")
                .unwrap()
                .1
                .split_once("; ViewId state:")
                .unwrap()
                .0;
            let actual: Vec<_> = resources
                .lines()
                .filter(|line| line.trim_end().ends_with(" 1"))
                .map(|line| line.split_whitespace().rev().nth(1).unwrap())
                .collect();
            assert_eq!(actual.len(), expected.len());
            for binding in expected {
                assert!(actual.contains(binding), "missing DXIL binding {binding}");
            }
        }
    }
}
