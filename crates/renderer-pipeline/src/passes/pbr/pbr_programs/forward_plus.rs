//! Forward+ and all-light reference raster programs.

mod culling;

use super::packaged_shaders;
use renderer::graph::{Binding, ImageView};
use renderer::program::{
    BindingLayout, BindingType, ManagedRasterProgram, RasterProgramDescriptor, ShaderStages,
};

use super::{BLEND, OPAQUE};
use culling::{LIGHTS, TILES, buffer_slot, tile_binding};

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
pub(crate) use culling::{CULL_PASS, light_binding};
pub use culling::{PointLight, PointLightLayout, TILE_SIZE, active_tile_count, append_culling};

#[allow(
    dead_code,
    reason = "the Rust Bistro builds these bindings from the shared shader ABI"
)]
pub fn raster_bindings(
    lights: ImageView,
    tiles: Option<ImageView>,
) -> impl Iterator<Item = Binding> {
    let bindings = match tiles {
        Some(tiles) => tiled_raster_bindings(lights, tiles).map(Some),
        None => [Some(light_binding(lights)), None],
    };
    bindings.into_iter().flatten()
}

pub fn tiled_raster_bindings(lights: ImageView, tiles: ImageView) -> [Binding; 2] {
    [light_binding(lights), tile_binding(tiles)]
}

const fn pbr_bindings<const N: usize>() -> [BindingLayout; N] {
    let mut bindings =
        [buffer_slot(LIGHTS, BindingType::SampledImage, ShaderStages::FRAGMENT, 0); N];
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
    bytes
}

pub(in crate::passes::pbr) static REFERENCE_OPAQUE: RasterProgramDescriptor =
    RasterProgramDescriptor {
        bindings: &pbr_bindings::<17>(),
        binding_bytes: &pbr_bytes::<17>(),
        shaders: packaged_shaders!(
            vulkan: (
                include_bytes!("../../../../shaders/pbr.vert.spv"),
                Some(include_bytes!("../../../../shaders/pbr_all_lights.frag.spv")),
            ),
            d3d12: (
                include_bytes!("../../../../shaders/dxil/pbr.vert.dxil"),
                Some(include_bytes!("../../../../shaders/dxil/pbr_all_lights.frag.dxil")),
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
            include_bytes!("../../../../shaders/pbr.vert.spv"),
            Some(include_bytes!("../../../../shaders/pbr_forward_plus.frag.spv")),
        ),
        d3d12: (
            include_bytes!("../../../../shaders/dxil/pbr.vert.dxil"),
            Some(include_bytes!("../../../../shaders/dxil/pbr_forward_plus.frag.dxil")),
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
    // SAFETY: These PBR fragment variants only add read-only images. The
    // shaders clamp decoded headers, light indices, mask words and tile
    // coordinates to the sampled views before loading.
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
                include_str!("../../../../shaders/dxil/pbr_all_lights.frag.spirv-reflection.json"),
                include_str!("../../../../shaders/dxil/pbr_all_lights.frag.reflection.txt"),
            ),
            (
                true,
                include_str!(
                    "../../../../shaders/dxil/pbr_forward_plus.frag.spirv-reflection.json"
                ),
                include_str!("../../../../shaders/dxil/pbr_forward_plus.frag.reflection.txt"),
            ),
        ] {
            let module: serde_json::Value = serde_json::from_str(spirv).unwrap();
            let images = module["separate_images"].as_array().unwrap();
            let program = raster_program(tiled, false);
            let slots = &program.bindings()[OPAQUE.bindings.len()..];
            let light = images
                .iter()
                .find(|image| image["name"] == "local_lights")
                .unwrap();
            assert_eq!(light["binding"], slots[0].binding);
            assert_eq!(light["set"], slots[0].group);
            assert_eq!(light["type"], "texture2D");
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
