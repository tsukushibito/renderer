use crate::shader_binaries::packaged_shaders;
use renderer::program::{
    BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
    ManagedProgram, ShaderStages,
};

pub const VISIBILITY_CAMERA_BYTES: usize = 64;
pub const VISIBILITY_THREADS: u32 = 64;
pub const VISIBILITY_MAX_WORKGROUPS: u32 = 65_535;
pub const VISIBILITY_AABB_BYTES: usize = 32;
pub const VISIBILITY_MASK_BYTES: usize = 4;

const BOUNDS_GROUP_BYTES: u64 = VISIBILITY_THREADS as u64 * VISIBILITY_AABB_BYTES as u64;
const MASK_GROUP_BYTES: u64 = VISIBILITY_THREADS as u64 * VISIBILITY_MASK_BYTES as u64;

static VISIBILITY: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [VISIBILITY_THREADS, 1, 1],
    bindings: &[
        BindingLayout {
            group: 0,
            binding: 0,
            kind: BindingType::Uniform,
            min_size: VISIBILITY_CAMERA_BYTES as u64,
            stages: ShaderStages::COMPUTE,
        },
        BindingLayout {
            group: 0,
            binding: 1,
            kind: BindingType::StorageRead,
            min_size: BOUNDS_GROUP_BYTES,
            stages: ShaderStages::COMPUTE,
        },
        BindingLayout {
            group: 0,
            binding: 2,
            kind: BindingType::StorageWrite,
            min_size: MASK_GROUP_BYTES,
            stages: ShaderStages::COMPUTE,
        },
    ],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::BufferGrid2d {
            group: 0,
            binding: 2,
            max_workgroups: [VISIBILITY_MAX_WORKGROUPS, 1],
            read_bindings: &[(0, 1)],
        },
        bytes: &[
            VISIBILITY_CAMERA_BYTES as u64,
            BOUNDS_GROUP_BYTES,
            MASK_GROUP_BYTES,
        ],
    },
    shaders: packaged_shaders!(
        include_bytes!("frustum_visibility.comp.spv"),
        include_bytes!("dxil/frustum_visibility.comp.dxil"),
    ),
};

pub const fn visibility_program() -> ManagedProgram {
    // SAFETY: Both assets match the exact column-major camera prefix and
    // padded record layout. On a bounded 1-D dispatch, each thread reads at
    // most its own 32-byte bounds record and writes its own 4-byte mask.
    // Even disabled records initialize the mask. Values never select an
    // address, and the accepted extents fit the D3D12 raw byte offsets.
    unsafe { ManagedProgram::from_trusted_descriptor(&VISIBILITY) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frustum_visibility_reflection_matches_packed_shader_inputs_and_output() {
        let spirv: serde_json::Value = serde_json::from_str(include_str!(
            "dxil/frustum_visibility.comp.spirv-reflection.json"
        ))
        .unwrap();
        let entry = &spirv["entryPoints"][0];
        assert_eq!(entry["name"], "main");
        assert_eq!(
            entry["workgroup_size"],
            serde_json::json!([VISIBILITY_THREADS, 1, 1])
        );

        let uniforms = spirv["ubos"].as_array().unwrap();
        assert_eq!(uniforms.len(), 1);
        let camera = &uniforms[0];
        assert_eq!(camera["set"], 0);
        assert_eq!(camera["binding"], 0);
        assert_eq!(camera["block_size"], VISIBILITY_CAMERA_BYTES);
        let matrix = &spirv["types"][camera["type"].as_str().unwrap()]["members"][0];
        assert_eq!(matrix["type"], "mat4");
        assert_eq!(matrix["offset"], 0);
        assert_eq!(matrix["matrix_stride"], 16);
        assert!(matrix.get("row_major").is_none());

        let storage = spirv["ssbos"].as_array().unwrap();
        assert_eq!(storage.len(), 2);
        for (slot, stride, qualifier) in [
            (1, VISIBILITY_AABB_BYTES, "readonly"),
            (2, VISIBILITY_MASK_BYTES, "writeonly"),
        ] {
            let buffer = storage
                .iter()
                .find(|buffer| buffer["binding"] == slot)
                .unwrap();
            assert_eq!(buffer["set"], 0);
            assert_eq!(buffer[qualifier], true);
            let member = &spirv["types"][buffer["type"].as_str().unwrap()]["members"][0];
            assert_eq!(member["offset"], 0);
            assert_eq!(member["array_stride"], stride);
            assert_eq!(member["array"], serde_json::json!([0]));
            if slot == 1 {
                let fields = spirv["types"][member["type"].as_str().unwrap()]["members"]
                    .as_array()
                    .unwrap();
                assert_eq!(fields.len(), 2);
                for (field, offset) in fields.iter().zip([0, 16]) {
                    assert_eq!(field["type"], "uvec4");
                    assert_eq!(field["offset"], offset);
                }
            } else {
                assert_eq!(member["type"], "uint");
            }
        }

        let dxil = include_str!("dxil/frustum_visibility.comp.reflection.txt");
        assert!(dxil.contains("; NumThreads=(64,1,1)"));
        assert!(
            dxil.lines().any(|line| line.ends_with("Size:    64")),
            "DXIL camera prefix differs"
        );
        let bindings: Vec<_> = dxil
            .split_once("; Resource Bindings:")
            .unwrap()
            .1
            .lines()
            .filter(|line| line.trim_end().ends_with(" 1"))
            .map(|line| line.split_whitespace().rev().nth(1).unwrap())
            .collect();
        assert_eq!(bindings, ["cb0", "t1", "u2"]);
    }
}
