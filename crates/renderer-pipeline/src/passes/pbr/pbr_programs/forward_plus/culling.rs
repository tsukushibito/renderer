//! Forward+ tile culling and the shared point-light buffer layout.

use crate::shader_binaries::packaged_shaders;
use renderer::ImageFormat;
use renderer::graph::{
    Binding, BindingResource, BufferHandle, Graph, GraphError, ImageDesc, ImageView, PassHandle,
};
use renderer::program::{
    BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
    ManagedProgram, ShaderStages,
};

pub const TILE_SIZE: u32 = 16;
pub const MAX_LIGHTS: usize = 64;
pub const MAX_TILES: u32 = 65_536;
pub const LIGHT_BUFFER_BYTES: u64 = 16 + MAX_LIGHTS as u64 * 32;
pub(crate) const CULL_PASS: &str = "Forward+ tile light masks";
const WORKGROUP: [u32; 3] = [8, 4, 1];
const CULL_CAMERA_BYTES: u64 = 64;
const MAX_EXTENT: u32 = 65_535;
pub(super) const LIGHTS: u32 = 2;
pub(super) const TILES: u32 = 3;

/// A local light in world space. Radius must be positive. RGB and intensity
/// are linear, finite and nonnegative.
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
    if cells == 0 || cells > MAX_TILES || width > MAX_EXTENT || height > MAX_EXTENT {
        return Err(GraphError::invalid_input(
            "Forward+ requires a nonempty extent with at most 65536 tiles",
        ));
    }
    Ok(cells)
}

/// Two RGBA16F texels per tile encode the 64 possible light IDs.
pub fn tile_mask_desc(width: u32, height: u32) -> Result<ImageDesc, GraphError> {
    active_tile_count(width, height)?;
    Ok(ImageDesc::rgba16_float(
        2 * width.div_ceil(TILE_SIZE),
        height.div_ceil(TILE_SIZE),
    ))
}

pub fn light_bytes(
    width: u32,
    height: u32,
    lights: &[PointLight],
) -> Result<[u8; LIGHT_BUFFER_BYTES as usize], GraphError> {
    active_tile_count(width, height)?;
    if lights.len() > MAX_LIGHTS {
        return Err(GraphError::invalid_input(
            "Forward+ point-light capacity exceeded",
        ));
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
            return Err(GraphError::invalid_input("invalid Forward+ point light"));
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
    workgroup_size: WORKGROUP,
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
            BindingType::StorageImageWrite(ImageFormat::Rgba16Float),
            ShaderStages::COMPUTE,
            0,
        ),
    ],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::Image2d {
            group: 0,
            binding: TILES,
        },
        bytes: &[CULL_CAMERA_BYTES, LIGHT_BUFFER_BYTES, 0],
    },
    shaders: packaged_shaders!(
        include_bytes!("../../../../../shaders/forward_plus.comp.spv"),
        include_bytes!("../../../../../shaders/dxil/forward_plus.comp.dxil"),
    ),
};

pub const fn compute_program() -> ManagedProgram {
    // SAFETY: Image2d covers the bound view. Each in-range invocation writes
    // exactly its own texel. The shader clamps the light count to the fixed
    // buffer ABI; arbitrary header values and matrices affect only membership.
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

pub(super) fn tile_binding(view: ImageView) -> Binding {
    Binding {
        group: 0,
        binding: TILES,
        resource: BindingResource::ImageView(view),
    }
}

/// Initialize the complete tile mask view from the camera and light buffer.
pub fn append_culling(
    graph: &mut Graph,
    camera: BufferHandle,
    lights: BufferHandle,
    tiles: ImageView,
) -> Result<PassHandle, GraphError> {
    graph.dispatch(
        CULL_PASS,
        &compute_program(),
        &[
            buffer_binding(0, camera, CULL_CAMERA_BYTES),
            buffer_binding(LIGHTS, lights, LIGHT_BUFFER_BYTES),
            tile_binding(tiles),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn culling_reflection_matches_the_host_layout() {
        let cull: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../shaders/dxil/forward_plus.comp.spirv-reflection.json"
        ))
        .unwrap();
        assert_eq!(cull["ubos"][0]["block_size"], CULL_CAMERA_BYTES);
        assert_eq!(cull["ssbos"][0]["binding"], LIGHTS);
        assert_eq!(cull["ssbos"][0]["block_size"], LIGHT_BUFFER_BYTES);
        assert_eq!(cull["ssbos"][0]["readonly"], true);
        assert_eq!(cull["ssbos"].as_array().unwrap().len(), 1);
        assert_eq!(cull["images"][0]["binding"], TILES);
        assert_eq!(cull["images"][0]["set"], 0);
        assert_eq!(cull["images"][0]["type"], "image2D");
        assert_eq!(cull["images"][0]["format"], "rgba16f");
        assert_eq!(cull["images"][0]["writeonly"], true);
        assert_eq!(
            cull["entryPoints"][0]["workgroup_size"],
            serde_json::json!(WORKGROUP)
        );
        let dxil = include_str!("../../../../../shaders/dxil/forward_plus.comp.reflection.txt");
        assert!(dxil.split_whitespace().any(|word| word == "t2"));
        assert!(dxil.split_whitespace().any(|word| word == "u3"));
    }
}
