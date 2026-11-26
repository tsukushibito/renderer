//! Audited model-transform variants for the standard PBR passes.

use sashiko_renderer_core::program::{
    BindingLayout, BindingType, ManagedRasterProgram, RasterParameterLayout,
    RasterProgramDescriptor, ShaderStages,
};

use crate::pbr::{
    instances::{InstanceTier, TRANSFORM_BINDING},
    scene::Programs,
};

use super::packaged_shaders;
use super::pbr_programs::{
    BLEND, OPAQUE, SHADOW,
    deferred::GBUFFER,
    forward_plus::{REFERENCE_OPAQUE, TILED_OPAQUE},
};

#[derive(Clone, Copy)]
pub(crate) enum TransformTechnique {
    Forward,
    ForwardPlus,
    AllLights,
    DeferredLights,
    Deferred,
}

const fn transform_binding(tier: InstanceTier) -> BindingLayout {
    BindingLayout {
        group: 0,
        binding: TRANSFORM_BINDING,
        kind: BindingType::StorageRead,
        stages: ShaderStages::VERTEX,
        min_size: tier.bytes() as u64,
    }
}

pub(super) const fn appended<T: Copy, const N: usize>(source: &[T], last: T) -> [T; N] {
    assert!(source.len() + 1 == N);
    let mut slots = [last; N];
    let mut i = 0;
    while i < source.len() {
        slots[i] = source[i];
        i += 1;
    }
    slots
}

macro_rules! descriptor {
    ($base:ident, $bindings:literal, $tier:expr, $vertex:literal, $fragment:literal, $bytes:literal) => {
        RasterProgramDescriptor {
            parameters: RasterParameterLayout {
                size: $bytes,
                stages: ShaderStages::VERTEX,
            },
            bindings: &appended::<_, $bindings>($base.bindings, transform_binding($tier)),
            binding_bytes: &appended::<_, $bindings>($base.binding_bytes, $tier.bytes() as u64),
            shaders: packaged_shaders!(
                raster: (concat!($vertex, ".vert"), concat!($fragment, ".frag")),
                inline_constants: Some(sashiko_renderer_core::program::D3D12InlineConstantBinding { register: 7, space: 0 }),
                nonfiltering: $base.shaders.webgpu_nonfiltering_samplers(),
            ),
            ..$base
        }
    };
}

macro_rules! tier_programs {
    ($name:ident, $tier:expr, $vertex:literal, $gbuffer:literal, $shadow:literal) => {
        pub(super) mod $name {
            use super::*;

            pub(in crate::passes::pbr) static FORWARD: RasterProgramDescriptor =
                descriptor!(OPAQUE, 22, $tier, $vertex, "pbr_transformed", 4);
            pub(in crate::passes::pbr) static FORWARD_BLEND: RasterProgramDescriptor =
                RasterProgramDescriptor {
                    blend: BLEND.blend,
                    depth: BLEND.depth,
                    ..FORWARD
                };
            pub(in crate::passes::pbr) static ALL_LIGHTS: RasterProgramDescriptor = descriptor!(
                REFERENCE_OPAQUE,
                24,
                $tier,
                $vertex,
                "pbr_all_lights_transformed",
                4
            );
            pub(in crate::passes::pbr) static ALL_LIGHTS_BLEND: RasterProgramDescriptor =
                RasterProgramDescriptor {
                    blend: BLEND.blend,
                    depth: BLEND.depth,
                    ..ALL_LIGHTS
                };
            pub(in crate::passes::pbr) static FORWARD_PLUS: RasterProgramDescriptor = descriptor!(
                TILED_OPAQUE,
                25,
                $tier,
                $vertex,
                "pbr_forward_plus_transformed",
                4
            );
            pub(in crate::passes::pbr) static FORWARD_PLUS_BLEND: RasterProgramDescriptor =
                RasterProgramDescriptor {
                    blend: BLEND.blend,
                    depth: BLEND.depth,
                    ..FORWARD_PLUS
                };
            pub(in crate::passes::pbr) static DEFERRED: RasterProgramDescriptor =
                descriptor!(GBUFFER, 12, $tier, $gbuffer, "gbuffer_transformed", 4);
            pub(in crate::passes::pbr) static CASCADE: RasterProgramDescriptor =
                descriptor!(SHADOW, 5, $tier, $shadow, "shadow_transformed", 8);
        }
    };
}

tier_programs!(
    small,
    InstanceTier::Small,
    "pbr_transformed_small",
    "gbuffer_transformed_small",
    "shadow_transformed_small"
);
tier_programs!(
    full,
    InstanceTier::Full,
    "pbr_transformed",
    "gbuffer_transformed",
    "shadow_transformed"
);

pub(crate) fn transformed_programs(tier: InstanceTier, technique: TransformTechnique) -> Programs {
    let (opaque, blend, shadow) = match tier {
        InstanceTier::Small => match technique {
            TransformTechnique::Forward => {
                (&small::FORWARD, &small::FORWARD_BLEND, &small::CASCADE)
            }
            TransformTechnique::AllLights => (
                &small::ALL_LIGHTS,
                &small::ALL_LIGHTS_BLEND,
                &small::CASCADE,
            ),
            TransformTechnique::DeferredLights => {
                (&small::DEFERRED, &small::ALL_LIGHTS_BLEND, &small::CASCADE)
            }
            TransformTechnique::ForwardPlus => (
                &small::FORWARD_PLUS,
                &small::FORWARD_PLUS_BLEND,
                &small::CASCADE,
            ),
            TransformTechnique::Deferred => {
                (&small::DEFERRED, &small::FORWARD_BLEND, &small::CASCADE)
            }
        },
        InstanceTier::Full => match technique {
            TransformTechnique::Forward => (&full::FORWARD, &full::FORWARD_BLEND, &full::CASCADE),
            TransformTechnique::AllLights => {
                (&full::ALL_LIGHTS, &full::ALL_LIGHTS_BLEND, &full::CASCADE)
            }
            TransformTechnique::DeferredLights => {
                (&full::DEFERRED, &full::ALL_LIGHTS_BLEND, &full::CASCADE)
            }
            TransformTechnique::ForwardPlus => (
                &full::FORWARD_PLUS,
                &full::FORWARD_PLUS_BLEND,
                &full::CASCADE,
            ),
            TransformTechnique::Deferred => (&full::DEFERRED, &full::FORWARD_BLEND, &full::CASCADE),
        },
    };
    Programs {
        opaque: import(opaque),
        blend: import(blend),
        shadow: import(shadow),
    }
}

/// Forward PBR Program with one 1,024-record transform page at the bound offset.
pub const fn transformed_opaque_program() -> ManagedRasterProgram {
    import(&full::FORWARD)
}

/// Depth-only shadow Program with one 1,024-record transform page at the bound offset.
pub const fn transformed_shadow_program() -> ManagedRasterProgram {
    import(&full::CASCADE)
}

const fn import(descriptor: &'static RasterProgramDescriptor<'static>) -> ManagedRasterProgram {
    // SAFETY: Only the fixed variants above use this helper. Each VS clamps
    // both index terms before adding, and its 96-byte record and fixed row
    // or parity loads fit the matching initialized prefix. Shadow also clamps
    // the camera cascade index. The fragments retain their paired audited
    // sampling contracts and only correct the interpolated front-face input.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transformed_forward_plus_dxil_links_position_and_mirror_at_matching_registers() {
        fn signature_register(
            reflection: &str,
            heading: &str,
            semantic: &str,
            semantic_index: &str,
        ) -> u32 {
            reflection
                .split_once(heading)
                .unwrap()
                .1
                .lines()
                .skip_while(|line| !line.starts_with("; --------------------"))
                .skip(1)
                .take_while(|line| line.trim() != ";")
                .find_map(|line| {
                    let mut columns = line.trim_start_matches(';').split_whitespace();
                    let name = columns.next()?;
                    let index = columns.next()?;
                    (name == semantic && index == semantic_index)
                        .then(|| columns.nth(1).unwrap().parse().unwrap())
                })
                .expect("reflected shader signature contains the semantic")
        }

        let fragment = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/shaders/dxil/pbr_forward_plus_transformed.frag.reflection.txt"
        ));
        for vertex in [
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/shaders/dxil/pbr_transformed_small.vert.reflection.txt"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/shaders/dxil/pbr_transformed.vert.reflection.txt"
            )),
        ] {
            for (semantic, index) in [("TEXCOORD", "4"), ("SV_Position", "0")] {
                assert_eq!(
                    signature_register(vertex, "; Output signature:", semantic, index),
                    signature_register(fragment, "; Input signature:", semantic, index),
                    "{semantic}{index} uses a different DXIL register",
                );
            }
        }
    }

    #[test]
    fn transformed_vertex_programs_match_reflection() {
        for (program, spirv, dxil, offsets, tier) in [
            (
                transformed_programs(InstanceTier::Small, TransformTechnique::ForwardPlus).opaque,
                include_str!(
                    "../../../shaders/dxil/pbr_transformed_small.vert.spirv-reflection.json"
                ),
                include_str!("../../../shaders/dxil/pbr_transformed_small.vert.reflection.txt"),
                &[0][..],
                InstanceTier::Small,
            ),
            (
                transformed_programs(InstanceTier::Small, TransformTechnique::Deferred).opaque,
                include_str!(
                    "../../../shaders/dxil/gbuffer_transformed_small.vert.spirv-reflection.json"
                ),
                include_str!("../../../shaders/dxil/gbuffer_transformed_small.vert.reflection.txt"),
                &[0][..],
                InstanceTier::Small,
            ),
            (
                transformed_opaque_program(),
                include_str!("../../../shaders/dxil/pbr_transformed.vert.spirv-reflection.json"),
                include_str!("../../../shaders/dxil/pbr_transformed.vert.reflection.txt"),
                &[0][..],
                InstanceTier::Full,
            ),
            (
                transformed_shadow_program(),
                include_str!("../../../shaders/dxil/shadow_transformed.vert.spirv-reflection.json"),
                include_str!("../../../shaders/dxil/shadow_transformed.vert.reflection.txt"),
                &[0, 4][..],
                InstanceTier::Full,
            ),
        ] {
            assert_eq!(program.bindings().last(), Some(&transform_binding(tier)));
            assert_eq!(program.binding_bytes().last(), Some(&(tier.bytes() as u64)),);
            assert_eq!(program.parameter_size(), (offsets.len() * 4) as u32);
            assert_eq!(program.parameter_layout().stages, ShaderStages::VERTEX);

            let reflected: serde_json::Value = serde_json::from_str(spirv).unwrap();
            let storage = reflected["ssbos"].as_array().unwrap();
            assert_eq!(storage.len(), 1);
            assert_eq!(storage[0]["set"], 0);
            assert_eq!(storage[0]["binding"], TRANSFORM_BINDING);
            assert_eq!(storage[0]["readonly"], true);
            let push = &reflected["push_constants"][0];
            let members = reflected["types"][push["type"].as_str().unwrap()]["members"]
                .as_array()
                .unwrap();
            assert_eq!(members.len(), offsets.len());
            for (member, &offset) in members.iter().zip(offsets) {
                assert_eq!(member["offset"], offset);
                assert_eq!(member["type"], "uint");
            }

            let resource_bindings = dxil.split_once("; Resource Bindings:").unwrap().1;
            let registers: Vec<_> = resource_bindings
                .lines()
                .skip_while(|line| !line.contains("----"))
                .skip(1)
                .take_while(|line| line.trim() != ";")
                .filter_map(|line| line.split_whitespace().rev().nth(1))
                .collect();
            assert_eq!(registers, ["cb0", "cb7", "t4"]);
        }
    }
}
