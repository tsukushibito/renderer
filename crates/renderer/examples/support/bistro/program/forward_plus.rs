//! Forward+ and all-light reference raster programs.

#[path = "forward_plus/culling.rs"]
mod culling;

use super::packaged_shaders;
use renderer::graph::{Binding, BufferHandle};
use renderer::program::{
    BindingLayout, BindingType, ManagedRasterProgram, RasterProgramDescriptor, ShaderStages,
};

use super::{BLEND, OPAQUE};
use culling::{LIGHTS, TILES, buffer_binding, buffer_slot};

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub use culling::compute_program;
pub use culling::{
    LIGHT_BUFFER_BYTES, MAX_LIGHTS, MAX_TILES, PointLight, TILE_SIZE, active_tile_count,
    append_culling, light_bytes,
};

pub const TILE_BUFFER_BYTES: u64 = MAX_TILES as u64 * culling::TILE_BYTES;

#[allow(
    dead_code,
    reason = "the Rust Bistro builds these bindings from the shared shader ABI"
)]
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
    shaders: packaged_shaders!(
        vulkan: (
            super::PBR_VERTEX_SPIRV,
            Some(include_bytes!("../shaders/pbr_all_lights.frag.spv")),
        ),
        d3d12: (
            super::PBR_VERTEX_DXIL,
            Some(include_bytes!("../shaders/dxil/pbr_all_lights.frag.dxil")),
        ),
        inline_constants: None,
    ),
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
    shaders: packaged_shaders!(
        vulkan: (
            super::PBR_VERTEX_SPIRV,
            Some(include_bytes!("../shaders/pbr_forward_plus.frag.spv")),
        ),
        d3d12: (
            super::PBR_VERTEX_DXIL,
            Some(include_bytes!("../shaders/dxil/pbr_forward_plus.frag.dxil")),
        ),
        inline_constants: None,
    ),
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
    fn raster_reflection_agrees_on_light_data_and_tile_list_slots() {
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
