//! Reviewed model-transform variants for the shared Forward and CSM passes.

use renderer::program::{
    BindingLayout, BindingType, ManagedRasterProgram, RasterParameterLayout,
    RasterProgramDescriptor, ShaderStages,
};

#[cfg(all(windows, feature = "d3d12"))]
use renderer::program::D3D12InlineConstantBinding;

use crate::pbr::instances::TRANSFORM_TABLE_BYTES;

use super::packaged_shaders;
use super::pbr_programs::{OPAQUE, SHADOW};

#[cfg(all(windows, feature = "d3d12"))]
use super::pbr_programs::PBR_FRAGMENT_DXIL;
#[cfg(feature = "vulkan")]
use super::pbr_programs::PBR_FRAGMENT_SPIRV;

const TRANSFORMS: BindingLayout = BindingLayout {
    group: 0,
    binding: 2,
    kind: BindingType::StorageRead,
    stages: ShaderStages::VERTEX,
    min_size: TRANSFORM_TABLE_BYTES as u64,
};

const fn appended<T: Copy, const N: usize>(source: &[T], last: T) -> [T; N] {
    assert!(source.len() + 1 == N);
    let mut slots = [last; N];
    let mut i = 0;
    while i < source.len() {
        slots[i] = source[i];
        i += 1;
    }
    slots
}

static TRANSFORMED_OPAQUE: RasterProgramDescriptor = RasterProgramDescriptor {
    parameters: RasterParameterLayout {
        size: 4,
        stages: ShaderStages::VERTEX,
    },
    bindings: &appended::<_, 17>(OPAQUE.bindings, TRANSFORMS),
    binding_bytes: &appended::<_, 17>(OPAQUE.binding_bytes, TRANSFORMS.min_size),
    shaders: packaged_shaders!(
        vulkan: (include_bytes!("pbr_transformed.vert.spv"), Some(PBR_FRAGMENT_SPIRV)),
        d3d12: (include_bytes!("dxil/pbr_transformed.vert.dxil"), Some(PBR_FRAGMENT_DXIL)),
        inline_constants: Some(D3D12InlineConstantBinding { register: 7, space: 0 }),
    ),
    ..OPAQUE
};

static TRANSFORMED_SHADOW: RasterProgramDescriptor = RasterProgramDescriptor {
    parameters: RasterParameterLayout {
        size: 8,
        stages: ShaderStages::VERTEX,
    },
    bindings: &appended::<_, 5>(SHADOW.bindings, TRANSFORMS),
    binding_bytes: &appended::<_, 5>(SHADOW.binding_bytes, TRANSFORMS.min_size),
    shaders: packaged_shaders!(
        vulkan: (include_bytes!("shadow_transformed.vert.spv"), Some(include_bytes!("shadow.frag.spv"))),
        d3d12: (include_bytes!("dxil/shadow_transformed.vert.dxil"), Some(include_bytes!("dxil/shadow.frag.dxil"))),
        inline_constants: Some(D3D12InlineConstantBinding { register: 7, space: 0 }),
    ),
    ..SHADOW
};

/// Forward PBR Program using a model transform for each instance.
pub const fn transformed_opaque_program() -> ManagedRasterProgram {
    // SAFETY: The pinned vertex modules keep the audited PBR vertex/fragment
    // interfaces and read only the additional fixed raw transform prefix.
    // Both index terms are clamped before addition, so even arbitrary
    // allowed inline words and SV_InstanceID select a record below 1024.
    // The 96-byte record and its six fixed 16-byte loads fit that prefix.
    // Arithmetic has no memory or shader synchronization side effects.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&TRANSFORMED_OPAQUE) }
}

/// Matching depth-only shadow Program for transformed geometry.
pub const fn transformed_shadow_program() -> ManagedRasterProgram {
    // SAFETY: This variant reads the first three rows of the same bounded
    // transform records, with a separately clamped camera cascade index.
    // Position and UV inputs and the alpha-test fragment shader retain the
    // reviewed shadow ABI. Neither stage has undeclared side effects.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&TRANSFORMED_SHADOW) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transformed_vertex_programs_match_reflection() {
        for (program, spirv, dxil, offsets) in [
            (
                transformed_opaque_program(),
                include_str!("dxil/pbr_transformed.vert.spirv-reflection.json"),
                include_str!("dxil/pbr_transformed.vert.reflection.txt"),
                &[0][..],
            ),
            (
                transformed_shadow_program(),
                include_str!("dxil/shadow_transformed.vert.spirv-reflection.json"),
                include_str!("dxil/shadow_transformed.vert.reflection.txt"),
                &[0, 4][..],
            ),
        ] {
            assert_eq!(program.bindings().last(), Some(&TRANSFORMS));
            assert_eq!(
                program.binding_bytes().last(),
                Some(&(TRANSFORM_TABLE_BYTES as u64)),
            );
            assert_eq!(program.parameter_size(), (offsets.len() * 4) as u32);
            assert_eq!(program.parameter_layout().stages, ShaderStages::VERTEX);

            let reflected: serde_json::Value = serde_json::from_str(spirv).unwrap();
            let storage = reflected["ssbos"].as_array().unwrap();
            assert_eq!(storage.len(), 1);
            assert_eq!(storage[0]["set"], 0);
            assert_eq!(storage[0]["binding"], 2);
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
            assert_eq!(registers, ["cb0", "cb7", "t2"]);
        }
    }
}
