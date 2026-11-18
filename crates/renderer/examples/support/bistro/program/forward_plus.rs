//! Fixed-stride point-light lists shared by Bistro Forward+ and its all-light reference.

use renderer::graph::{Binding, BindingResource, BufferHandle, Graph, GraphError, PassHandle};
use renderer::program::{
    BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
    ManagedProgram, ManagedRasterProgram, RasterProgramDescriptor, ShaderStages,
};

use super::{BLEND, OPAQUE};

pub const TILE_SIZE: u32 = 16;
pub const MAX_LIGHTS: usize = 64;
pub const MAX_TILES: u32 = 65_536;
pub const TILES_PER_GROUP: u32 = 32;
pub const LIGHT_BUFFER_BYTES: u64 = 16 + MAX_LIGHTS as u64 * 32;
pub const TILE_BYTES: u64 = (1 + MAX_LIGHTS as u64) * 4;
pub const TILE_BUFFER_BYTES: u64 = MAX_TILES as u64 * TILE_BYTES;
const GROUP_BYTES: u64 = TILES_PER_GROUP as u64 * TILE_BYTES;
const CULL_CAMERA_BYTES: u64 = 64;
const MAX_EXTENT: u32 = 65_535;
const LIGHTS: u32 = 2;
const TILES: u32 = 3;

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
pub fn workgroups(width: u32, height: u32) -> Result<[u32; 3], GraphError> {
    Ok([
        active_tile_count(width, height)?.div_ceil(TILES_PER_GROUP),
        1,
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

const fn buffer_slot(
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
        },
        bytes: &[CULL_CAMERA_BYTES, LIGHT_BUFFER_BYTES, GROUP_BYTES],
    },
    spirv: include_bytes!("../shaders/forward_plus.comp.spv"),
    dxil: include_bytes!("../shaders/dxil/forward_plus.comp.dxil"),
};

pub const fn compute_program() -> ManagedProgram {
    // SAFETY: Each GlobalInvocationID.x writes all TILE_BYTES at its unique
    // index, including unused words and any padding tiles in its workgroup.
    // The grid contract proves the resulting prefix fits the descriptor.
    // The clamped light count and arbitrary header values and matrices can
    // change membership, while output addresses and coverage are invariant.
    unsafe { ManagedProgram::from_trusted_descriptor(&CULL_PROGRAM) }
}

fn buffer_binding(binding: u32, buffer: BufferHandle, size: u64) -> Binding {
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
    graph.dispatch(
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

pub fn raster_bindings(
    lights: BufferHandle,
    tiles: Option<BufferHandle>,
) -> impl Iterator<Item = Binding> {
    std::iter::once(buffer_binding(LIGHTS, lights, LIGHT_BUFFER_BYTES))
        .chain(tiles.map(|buffer| buffer_binding(TILES, buffer, TILE_BUFFER_BYTES)))
}

const fn pbr_bindings<const N: usize>() -> [BindingLayout; N] {
    let mut bindings = [buffer_slot(
        LIGHTS,
        BindingType::StorageRead,
        ShaderStages::FRAGMENT,
        LIGHT_BUFFER_BYTES,
    ); N];
    let mut index = 0;
    while index < OPAQUE.bindings.len() {
        bindings[index] = OPAQUE.bindings[index];
        index += 1;
    }
    if N == 18 {
        bindings[17] = buffer_slot(
            TILES,
            BindingType::StorageRead,
            ShaderStages::FRAGMENT,
            TILE_BUFFER_BYTES,
        );
    }
    bindings
}

const fn pbr_bytes<const N: usize>() -> [u64; N] {
    let mut bytes = [0; N];
    let mut index = 0;
    while index < OPAQUE.binding_bytes.len() {
        bytes[index] = OPAQUE.binding_bytes[index];
        index += 1;
    }
    bytes[16] = LIGHT_BUFFER_BYTES;
    if N == 18 {
        bytes[17] = TILE_BUFFER_BYTES;
    }
    bytes
}

static REFERENCE_OPAQUE: RasterProgramDescriptor = RasterProgramDescriptor {
    bindings: &pbr_bindings::<17>(),
    binding_bytes: &pbr_bytes::<17>(),
    fragment_spirv: include_bytes!("../shaders/pbr_all_lights.frag.spv"),
    fragment_dxil: include_bytes!("../shaders/dxil/pbr_all_lights.frag.dxil"),
    ..OPAQUE
};
static REFERENCE_BLEND: RasterProgramDescriptor = RasterProgramDescriptor {
    blend: BLEND.blend,
    depth: BLEND.depth,
    ..REFERENCE_OPAQUE
};
static TILED_OPAQUE: RasterProgramDescriptor = RasterProgramDescriptor {
    bindings: &pbr_bindings::<18>(),
    binding_bytes: &pbr_bytes::<18>(),
    fragment_spirv: include_bytes!("../shaders/pbr_forward_plus.frag.spv"),
    fragment_dxil: include_bytes!("../shaders/dxil/pbr_forward_plus.frag.dxil"),
    ..OPAQUE
};
static TILED_BLEND: RasterProgramDescriptor = RasterProgramDescriptor {
    blend: BLEND.blend,
    depth: BLEND.depth,
    ..TILED_OPAQUE
};

pub const fn raster_program(tiled: bool, blend: bool) -> ManagedRasterProgram {
    let descriptor = match (tiled, blend) {
        (false, false) => &REFERENCE_OPAQUE,
        (false, true) => &REFERENCE_BLEND,
        (true, false) => &TILED_OPAQUE,
        (true, true) => &TILED_BLEND,
    };
    // SAFETY: Original Bistro PBR slots and side effects are unchanged.
    // Additional slots are bounded read-only storage. The fragment shader
    // clamps tile index and list count, rejects IDs beyond the light count,
    // and bounds the fragment-coordinate conversion before integer indexing.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_reflection_agrees_on_light_data_and_tile_list_slots() {
        let cull: serde_json::Value = serde_json::from_str(include_str!(
            "../shaders/dxil/forward_plus.comp.spirv-reflection.json"
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
        let dxil = include_str!("../shaders/dxil/forward_plus.comp.reflection.txt");
        assert!(dxil.split_whitespace().any(|word| word == "t2"));
        assert!(dxil.split_whitespace().any(|word| word == "u3"));

        for (tiled, spirv, dxil) in [
            (
                false,
                include_str!("../shaders/dxil/pbr_all_lights.frag.spirv-reflection.json"),
                include_str!("../shaders/dxil/pbr_all_lights.frag.reflection.txt"),
            ),
            (
                true,
                include_str!("../shaders/dxil/pbr_forward_plus.frag.spirv-reflection.json"),
                include_str!("../shaders/dxil/pbr_forward_plus.frag.reflection.txt"),
            ),
        ] {
            let module: serde_json::Value = serde_json::from_str(spirv).unwrap();
            let buffers = module["ssbos"].as_array().unwrap();
            let program = raster_program(tiled, false);
            let slots = &program.bindings()[OPAQUE.bindings.len()..];
            assert_eq!(buffers.len(), slots.len());
            for (buffer, slot) in buffers.iter().zip(slots) {
                assert_eq!(buffer["binding"], slot.binding);
                assert_eq!(buffer["set"], slot.group);
                assert_eq!(buffer["readonly"], true);
                let register = format!("t{}", slot.binding);
                assert!(dxil.split_whitespace().any(|word| word == register));
            }
            assert_eq!(dxil.split_whitespace().any(|word| word == "t3"), tiled);
        }
    }
}
