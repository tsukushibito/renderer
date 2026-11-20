//! Forward+ and all-light reference raster programs.

#[path = "forward_plus/culling.rs"]
mod culling;

use super::packaged_shaders;
use renderer::graph::{Binding, BufferHandle, ImageView};
use renderer::program::{
    BindingLayout, BindingType, ManagedRasterProgram, RasterProgramDescriptor, ShaderStages,
};

use super::{BLEND, OPAQUE};
use culling::{LIGHTS, TILES, buffer_binding, buffer_slot, tile_binding};

pub(crate) use culling::CULL_PASS;
#[allow(
    unused_imports,
    reason = "the raw Bistro tests assert the public tile limit"
)]
pub use culling::MAX_TILES;
#[cfg_attr(
    not(any(feature = "vulkan", all(windows, feature = "d3d12"))),
    allow(
        unused_imports,
        reason = "the standalone Bistro prepares this program only with a native backend"
    )
)]
pub use culling::compute_program;
pub use culling::{
    LIGHT_BUFFER_BYTES, MAX_LIGHTS, PointLight, TILE_SIZE, active_tile_count, append_culling,
    light_bytes, tile_mask_desc,
};

#[allow(
    dead_code,
    reason = "the Rust Bistro builds these bindings from the shared shader ABI"
)]
pub fn raster_bindings(
    lights: BufferHandle,
    tiles: Option<ImageView>,
) -> impl Iterator<Item = Binding> {
    let bindings = match tiles {
        Some(tiles) => tiled_raster_bindings(lights, tiles).map(Some),
        None => [
            Some(buffer_binding(LIGHTS, lights, LIGHT_BUFFER_BYTES)),
            None,
        ],
    };
    bindings.into_iter().flatten()
}

pub fn tiled_raster_bindings(lights: BufferHandle, tiles: ImageView) -> [Binding; 2] {
    [
        buffer_binding(LIGHTS, lights, LIGHT_BUFFER_BYTES),
        tile_binding(tiles),
    ]
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
        bindings[17] = buffer_slot(TILES, BindingType::SampledImage, ShaderStages::FRAGMENT, 0);
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
    bytes
}

static REFERENCE_OPAQUE: RasterProgramDescriptor = RasterProgramDescriptor {
    bindings: &pbr_bindings::<17>(),
    binding_bytes: &pbr_bytes::<17>(),
    shaders: packaged_shaders!(
        vulkan: (
            super::PBR_VERTEX_SPIRV,
            Some(include_bytes!("../pbr_all_lights.frag.spv")),
        ),
        d3d12: (
            super::PBR_VERTEX_DXIL,
            Some(include_bytes!("../dxil/pbr_all_lights.frag.dxil")),
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
pub(in crate::passes::pbr) static TILED_OPAQUE: RasterProgramDescriptor = RasterProgramDescriptor {
    bindings: &pbr_bindings::<18>(),
    binding_bytes: &pbr_bytes::<18>(),
    shaders: packaged_shaders!(
        vulkan: (
            super::PBR_VERTEX_SPIRV,
            Some(include_bytes!("../pbr_forward_plus.frag.spv")),
        ),
        d3d12: (
            super::PBR_VERTEX_DXIL,
            Some(include_bytes!("../dxil/pbr_forward_plus.frag.dxil")),
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
    // The fragment shader bounds image loads by the actual view dimensions.
    // It confines conversions of arbitrary float pixels to [0, 255] and
    // bounds all light-buffer indices by the clamped light count.
    unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_reflection_agrees_on_light_data_and_tile_mask_slots() {
        for (tiled, spirv, dxil) in [
            (
                false,
                include_str!("../dxil/pbr_all_lights.frag.spirv-reflection.json"),
                include_str!("../dxil/pbr_all_lights.frag.reflection.txt"),
            ),
            (
                true,
                include_str!("../dxil/pbr_forward_plus.frag.spirv-reflection.json"),
                include_str!("../dxil/pbr_forward_plus.frag.reflection.txt"),
            ),
        ] {
            let module: serde_json::Value = serde_json::from_str(spirv).unwrap();
            let buffers = module["ssbos"].as_array().unwrap();
            let program = raster_program(tiled, false);
            let slots = &program.bindings()[OPAQUE.bindings.len()..];
            assert_eq!(buffers.len(), 1);
            let light = &buffers[0];
            assert_eq!(light["binding"], slots[0].binding);
            assert_eq!(light["set"], slots[0].group);
            assert_eq!(light["readonly"], true);
            assert_eq!(light["block_size"], LIGHT_BUFFER_BYTES);
            assert!(dxil.split_whitespace().any(|word| word == "t2"));
            let mask = module["separate_images"]
                .as_array()
                .unwrap()
                .iter()
                .find(|image| image["name"] == "tile_masks");
            if tiled {
                let mask = mask.unwrap();
                assert_eq!(mask["binding"], slots[1].binding);
                assert_eq!(mask["set"], slots[1].group);
                assert_eq!(mask["type"], "texture2D");
                assert_eq!(slots[1].kind, BindingType::SampledImage);
            } else {
                assert!(mask.is_none());
            }
            assert_eq!(dxil.split_whitespace().any(|word| word == "t3"), tiled);
        }
    }
}
