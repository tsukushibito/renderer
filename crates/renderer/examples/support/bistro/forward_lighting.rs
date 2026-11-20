//! A deterministic point-light workload shared by tiled and all-light Bistro.

use super::{Error, program::forward_plus};
use renderer::graph::{BufferDesc, BufferHandle, Graph, ImageView};

pub(super) type LightBytes = [u8; forward_plus::LIGHT_BUFFER_BYTES as usize];

pub(super) fn identity(fixture: bool) -> serde_json::Value {
    serde_json::json!({
        "profile": if fixture { "fixture-grid-v1" } else { "bistro-street-grid-v1" },
        "count": forward_plus::MAX_LIGHTS,
        "tile_size": forward_plus::TILE_SIZE,
    })
}

pub(super) fn preset(fixture: bool) -> Result<LightBytes, Error> {
    let lights: [_; forward_plus::MAX_LIGHTS] = std::array::from_fn(|index| {
        let column = (index % 8) as f32;
        let row = (index / 8) as f32;
        let level = (index % 3) as f32;
        let (position, radius, intensity) = if fixture {
            (
                [column * 0.6 - 2.1, row * 0.6 - 2.1, 1.1 + level * 0.2],
                1.8,
                0.5,
            )
        } else {
            (
                [column * 9.0 - 24.0, 3.0 + level * 1.5, row * 13.0 - 51.0],
                10.0,
                15.0,
            )
        };
        forward_plus::PointLight {
            position,
            radius,
            intensity,
            color: [[1.0, 0.75, 0.55], [0.60, 0.80, 1.0], [0.74, 1.0, 0.70]][index % 3],
        }
    });
    Ok(forward_plus::light_bytes(1, 1, &lights)?)
}

fn light_upload(graph: &mut Graph, bytes: &LightBytes) -> Result<BufferHandle, Error> {
    let lights = graph.buffer(
        "local point lights",
        BufferDesc {
            size: forward_plus::LIGHT_BUFFER_BYTES,
        },
    )?;
    graph.upload_buffer("local point light update", lights, 0, bytes)?;
    Ok(lights)
}

pub(super) fn append(
    graph: &mut Graph,
    extent: [u32; 2],
    camera: BufferHandle,
    preset: &LightBytes,
    tiled: bool,
) -> Result<(BufferHandle, Option<ImageView>), Error> {
    let [width, height] = extent;
    let mut bytes = *preset;
    bytes[..4].copy_from_slice(&width.to_le_bytes());
    bytes[4..8].copy_from_slice(&height.to_le_bytes());
    let lights = light_upload(graph, &bytes)?;
    let tiles = if tiled {
        let image = graph.image(
            "tile light masks",
            forward_plus::tile_mask_desc(width, height)?,
        )?;
        let tiles = graph.image_view(image, 0, 0)?;
        forward_plus::append_culling(graph, camera, lights, tiles)?;
        Some(tiles)
    } else {
        None
    };
    Ok((lights, tiles))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_reproducible_and_keep_all_fixture_lights_active() {
        let full = preset(false).unwrap();
        let fixture = preset(true).unwrap();
        assert_eq!(full, preset(false).unwrap());
        assert_ne!(full, fixture);
        assert_eq!(
            u32::from_le_bytes(fixture[8..12].try_into().unwrap()) as usize,
            forward_plus::MAX_LIGHTS
        );
        assert_eq!(
            forward_plus::active_tile_count(4096, 4096).unwrap(),
            forward_plus::MAX_TILES
        );
    }
}
