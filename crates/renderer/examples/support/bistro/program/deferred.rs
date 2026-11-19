use super::packaged_shaders;
use super::{
    BindingLayout, BindingType, CAMERA, CAMERA_BYTES, ImageFormat, MATERIAL, MATERIAL_BYTES,
    ManagedRasterProgram, NO_PARAMETERS, OPAQUE, RasterProgramDescriptor, ShaderStages, image,
    pair, sampler,
};

pub const DEFERRED_RECONSTRUCTION_BYTES: usize = 64;

static GBUFFER: RasterProgramDescriptor = RasterProgramDescriptor {
    color_outputs: &[ImageFormat::Rgba16Float; 3],
    bindings: &[
        BindingLayout {
            stages: ShaderStages::VERTEX,
            ..CAMERA
        },
        MATERIAL,
        image(1, 0),
        image(1, 1),
        image(1, 2),
        image(1, 3),
        sampler(1, 4),
    ],
    binding_bytes: &[CAMERA_BYTES as u64, MATERIAL_BYTES as u64, 0, 0, 0, 0, 0],
    sampling_pairs: &[pair(1, 0, 4), pair(1, 1, 4), pair(1, 2, 4), pair(1, 3, 4)],
    shaders: packaged_shaders!(
        vulkan: (
            include_bytes!("../shaders/gbuffer.vert.spv"),
            Some(include_bytes!("../shaders/gbuffer.frag.spv")),
        ),
        d3d12: (
            include_bytes!("../shaders/dxil/gbuffer.vert.dxil"),
            Some(include_bytes!("../shaders/dxil/gbuffer.frag.dxil")),
        ),
        inline_constants: None,
    ),
    ..OPAQUE
};

static LIGHTING: RasterProgramDescriptor = RasterProgramDescriptor {
    color_outputs: &[ImageFormat::Rgba16Float],
    bindings: &[
        BindingLayout {
            stages: ShaderStages::FRAGMENT,
            ..CAMERA
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::Uniform,
            min_size: DEFERRED_RECONSTRUCTION_BYTES as u64,
            stages: ShaderStages::FRAGMENT,
        },
        image(1, 0),
        image(1, 1),
        image(1, 2),
        image(1, 3),
        sampler(1, 4),
        image(2, 0),
        image(2, 1),
        image(2, 2),
        sampler(2, 3),
        image(3, 0),
        image(3, 1),
        image(3, 2),
        image(3, 3),
        sampler(3, 4),
    ],
    binding_bytes: &[
        CAMERA_BYTES as u64,
        DEFERRED_RECONSTRUCTION_BYTES as u64,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ],
    sampling_pairs: &[
        pair(1, 0, 4),
        pair(1, 1, 4),
        pair(1, 2, 4),
        pair(1, 3, 4),
        pair(2, 0, 3),
        pair(2, 1, 3),
        pair(2, 2, 3),
        pair(3, 0, 4),
        pair(3, 1, 4),
        pair(3, 2, 4),
        pair(3, 3, 4),
    ],
    shaders: packaged_shaders!(
        vulkan: (
            include_bytes!("../../../../shaders/composite.vert.spv"),
            Some(include_bytes!("../shaders/deferred_lighting.frag.spv")),
        ),
        d3d12: (
            include_bytes!("../../../../shaders/dxil/composite.vert.dxil"),
            Some(include_bytes!("../shaders/dxil/deferred_lighting.frag.dxil")),
        ),
        inline_constants: None,
    ),
    entry: "main",
    vertex_stride: 0,
    vertex_attributes: &[],
    parameters: NO_PARAMETERS,
    blend: renderer::program::BlendState::Replace,
    depth: renderer::program::DepthState::Disabled,
};

pub const fn gbuffer_program() -> ManagedRasterProgram {
    // SAFETY: Validated VS/FS use the declared 48-byte stream and only the
    // camera and material UBOs and four selected material images. Gradients
    // precede alpha discard. Outputs are three float4s; the normal encoding
    // performs FP32 arithmetic and preserves a finite binary16 payload.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&GBUFFER) }
}

pub const fn deferred_lighting_program() -> ManagedRasterProgram {
    // SAFETY: Validated VS generates vertices for IDs 0..2 and FS matches all
    // bindings. The fragment shader uses bounded, normalized image samples,
    // explicit LOD and guards inverse projection division. Dynamic indices
    // stay under four shadow entries. It writes only one float4 output.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(&LIGHTING) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};

    fn check_reflection(
        program: ManagedRasterProgram,
        stage: ShaderStages,
        spirv: &str,
        dxil: &str,
    ) {
        let spirv: Value = serde_json::from_str(spirv).unwrap();
        assert!(spirv.get("push_constants").is_none());
        assert!(spirv.get("ssbos").is_none());
        let mut descriptor_slots = BTreeMap::new();
        let mut register_names = BTreeSet::new();
        for layout in program
            .bindings()
            .iter()
            .filter(|layout| layout.stages.contains(stage))
        {
            let column = match layout.kind {
                BindingType::Uniform => "cb",
                BindingType::Sampler => "s",
                BindingType::SampledImage => "t",
                _ => panic!("unexpected binding type"),
            };
            let mut name = format!("{column}{}", layout.binding);
            if layout.group != 0 {
                name.push_str(&format!(",space{}", layout.group));
            }
            register_names.insert(name);
            descriptor_slots.insert(
                (layout.group, layout.binding),
                (layout.kind, layout.min_size),
            );
        }

        let mut reflected_slots = BTreeMap::new();
        for (key, kind) in [
            ("ubos", BindingType::Uniform),
            ("separate_images", BindingType::SampledImage),
            ("separate_samplers", BindingType::Sampler),
        ] {
            for entry in spirv[key].as_array().into_iter().flatten() {
                reflected_slots.insert(
                    (
                        entry["set"].as_u64().unwrap() as u32,
                        entry["binding"].as_u64().unwrap() as u32,
                    ),
                    (kind, entry["block_size"].as_u64().unwrap_or(0)),
                );
            }
        }
        assert_eq!(reflected_slots, descriptor_slots);

        let resources = dxil.split_once("; Resource Bindings:").unwrap().1;
        let reflected_registers: BTreeSet<_> = resources
            .lines()
            .skip(3)
            .take_while(|line| *line != ";")
            .filter(|line| line.trim_end().ends_with(" 1"))
            .map(|line| line.split_whitespace().rev().nth(1).unwrap().to_owned())
            .collect();
        assert_eq!(reflected_registers, register_names);

        if stage == ShaderStages::FRAGMENT {
            let reflected_outputs: BTreeSet<_> = spirv["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    assert_eq!(entry["type"], "vec4");
                    entry["location"].as_u64().unwrap() as usize
                })
                .collect();
            let output_slots: BTreeSet<_> = (0..program.color_outputs().len()).collect();
            assert_eq!(reflected_outputs, output_slots);

            let reflected_targets: BTreeSet<_> = dxil
                .lines()
                .take_while(|line| !line.starts_with("; shader hash"))
                .filter_map(|line| {
                    let mut fields = line.strip_prefix("; SV_Target")?.split_whitespace();
                    let location = fields.next()?.parse::<usize>().unwrap();
                    assert_eq!(fields.next(), Some("xyzw"));
                    Some(location)
                })
                .collect();
            assert_eq!(reflected_targets, output_slots);
        }
    }

    #[test]
    fn deferred_shader_reflection_matches_both_programs() {
        check_reflection(
            gbuffer_program(),
            ShaderStages::VERTEX,
            include_str!("../shaders/dxil/gbuffer.vert.spirv-reflection.json"),
            include_str!("../shaders/dxil/gbuffer.vert.reflection.txt"),
        );
        check_reflection(
            gbuffer_program(),
            ShaderStages::FRAGMENT,
            include_str!("../shaders/dxil/gbuffer.frag.spirv-reflection.json"),
            include_str!("../shaders/dxil/gbuffer.frag.reflection.txt"),
        );
        check_reflection(
            deferred_lighting_program(),
            ShaderStages::FRAGMENT,
            include_str!("../shaders/dxil/deferred_lighting.frag.spirv-reflection.json"),
            include_str!("../shaders/dxil/deferred_lighting.frag.reflection.txt"),
        );
    }
}
