//! Forward+ tile culling and the shared point-light buffer layout.

use super::super::packaged_shaders;
use renderer::graph::{Binding, BindingResource, BufferHandle, Graph, GraphError, PassHandle};
use renderer::program::{
    BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
    ManagedProgram, ShaderStages,
};

pub const TILE_SIZE: u32 = 16;
pub const MAX_LIGHTS: usize = 64;
pub const MAX_TILES: u32 = 65_536;
pub const TILES_PER_GROUP: u32 = 32;
pub const LIGHT_BUFFER_BYTES: u64 = 16 + MAX_LIGHTS as u64 * 32;
pub const TILE_BYTES: u64 = (1 + MAX_LIGHTS as u64) * 4;
const GROUP_BYTES: u64 = TILES_PER_GROUP as u64 * TILE_BYTES;
const CULL_CAMERA_BYTES: u64 = 64;
const MAX_EXTENT: u32 = 65_535;
pub(super) const LIGHTS: u32 = 2;
pub(super) const TILES: u32 = 3;

#[derive(Clone, Copy, Debug)]
pub struct PointLight {
    pub position: [f32; 3],
    pub radius: f32,
    pub color: [f32; 3],
    pub intensity: f32,
}

pub fn active_tile_count(width: u32, height: u32) -> Result<u32, GraphError> {
    let cells = width
        .div_ceil(TILE_SIZE)
        .saturating_mul(height.div_ceil(TILE_SIZE));
    // A single invocation owns each packed tile record.
    if cells == 0 || cells > MAX_TILES || width > MAX_EXTENT || height > MAX_EXTENT {
        return Err(GraphError::custom(
            "Forward+ requires a nonempty extent with at most 65536 tiles",
        ));
    }
    Ok(cells)
}

/// Round to a full workgroup. Its padding invocations also initialize records.
pub fn workgroups(width: u32, height: u32) -> Result<[u32; 2], GraphError> {
    Ok([
        active_tile_count(width, height)?.div_ceil(TILES_PER_GROUP),
        1,
    ])
}

pub fn light_bytes(
    width: u32,
    height: u32,
    lights: &[PointLight],
) -> Result<[u8; LIGHT_BUFFER_BYTES as usize], GraphError> {
    active_tile_count(width, height)?;
    if lights.len() > MAX_LIGHTS {
        return Err(GraphError::custom("Forward+ point-light capacity exceeded"));
    }
    let mut bytes = [0; LIGHT_BUFFER_BYTES as usize];
    for (destination, word) in
        bytes[..16]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip([width, height, lights.len() as u32, 0])
    {
        *destination = word.to_le_bytes();
    }
    for (record, light) in bytes[16..].as_chunks_mut::<32>().0.iter_mut().zip(lights) {
        let values = [
            light.position[0],
            light.position[1],
            light.position[2],
            light.radius,
            light.color[0],
            light.color[1],
            light.color[2],
            light.intensity,
        ];
        if values.iter().any(|value| !value.is_finite())
            || light.radius <= 0.0
            || light.color.iter().any(|value| *value < 0.0)
            || light.intensity < 0.0
        {
            return Err(GraphError::custom("invalid Forward+ point light"));
        }
        for (destination, word) in record.as_chunks_mut::<4>().0.iter_mut().zip(values) {
            *destination = word.to_le_bytes();
        }
    }
    Ok(bytes)
}

pub(super) const fn buffer_slot(
    binding: u32,
    kind: BindingType,
    stages: ShaderStages,
    min_size: u64,
) -> BindingLayout {
    BindingLayout {
        group: 0,
        binding,
        kind,
        stages,
        min_size,
    }
}

static CULL_PROGRAM: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [TILES_PER_GROUP, 1, 1],
    bindings: &[
        buffer_slot(
            0,
            BindingType::Uniform,
            ShaderStages::COMPUTE,
            CULL_CAMERA_BYTES,
        ),
        buffer_slot(
            LIGHTS,
            BindingType::StorageRead,
            ShaderStages::COMPUTE,
            LIGHT_BUFFER_BYTES,
        ),
        buffer_slot(
            TILES,
            BindingType::StorageWrite,
            ShaderStages::COMPUTE,
            GROUP_BYTES,
        ),
    ],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::BufferGrid2d {
            group: 0,
            binding: TILES,
            max_workgroups: [MAX_TILES / TILES_PER_GROUP, 1],
            read_bindings: &[],
        },
        bytes: &[CULL_CAMERA_BYTES, LIGHT_BUFFER_BYTES, GROUP_BYTES],
    },
    shaders: packaged_shaders!(
        include_bytes!("../../shaders/forward_plus.comp.spv"),
        include_bytes!("../../shaders/dxil/forward_plus.comp.dxil"),
    ),
};

pub const fn compute_program() -> ManagedProgram {
    // SAFETY: Each GlobalInvocationID.x writes all TILE_BYTES at its unique
    // index, including unused words and any padding tiles in its workgroup.
    // The grid contract proves the resulting prefix fits the descriptor.
    // The clamped light count and arbitrary header values and matrices can
    // change membership, while output addresses and coverage are invariant.
    unsafe { ManagedProgram::from_trusted_descriptor(&CULL_PROGRAM) }
}

pub(super) fn buffer_binding(binding: u32, buffer: BufferHandle, size: u64) -> Binding {
    Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size,
        },
    }
}

/// Append tile culling from Bistro's camera matrix prefix and local lights.
///
/// The inactive tile records are preserved. A later Raster binding of the
/// full buffer requires all records to have been initialized once beforehand.
pub fn append_culling(
    graph: &mut Graph,
    width: u32,
    height: u32,
    camera: BufferHandle,
    lights: BufferHandle,
    tiles: BufferHandle,
) -> Result<PassHandle, GraphError> {
    let workgroups = workgroups(width, height)?;
    graph.dispatch_buffer_grid(
        "Forward+ tile light lists",
        &compute_program(),
        &[
            buffer_binding(0, camera, CULL_CAMERA_BYTES),
            buffer_binding(LIGHTS, lights, LIGHT_BUFFER_BYTES),
            buffer_binding(TILES, tiles, u64::from(workgroups[0]) * GROUP_BYTES),
        ],
        workgroups,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn culling_reflection_matches_the_host_layout() {
        let cull: serde_json::Value = serde_json::from_str(include_str!(
            "../../shaders/dxil/forward_plus.comp.spirv-reflection.json"
        ))
        .unwrap();
        assert_eq!(cull["ubos"][0]["block_size"], CULL_CAMERA_BYTES);
        assert_eq!(cull["ssbos"][0]["binding"], LIGHTS);
        assert_eq!(cull["ssbos"][0]["block_size"], LIGHT_BUFFER_BYTES);
        assert_eq!(cull["ssbos"][0]["readonly"], true);
        assert_eq!(cull["ssbos"][1]["binding"], TILES);
        assert_eq!(cull["ssbos"][1]["writeonly"], true);
        let module_type = cull["ssbos"][1]["type"].as_str().unwrap();
        assert_eq!(cull["types"][module_type]["members"][0]["array_stride"], 4);
        let dxil = include_str!("../../shaders/dxil/forward_plus.comp.reflection.txt");
        assert!(dxil.split_whitespace().any(|word| word == "t2"));
        assert!(dxil.split_whitespace().any(|word| word == "u3"));
    }
}
