//! A deterministic point-light workload shared by tiled and all-light Bistro.

use super::{Error, backend::Backend, program::forward_plus};
use renderer::graph::{BufferDesc, BufferHandle, Graph, GraphCapacity, GraphPool, ImportBuffer};

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

pub(super) fn initialize_tiles<B: Backend>(
    backend: &mut B,
    camera: &ImportBuffer,
) -> Result<ImportBuffer, Error> {
    const INITIAL_EXTENT: u32 = 4096;
    let mut pool = GraphPool::new(
        GraphCapacity {
            resources: 3,
            passes: 2,
            accesses: 4,
            draws: 0,
            raster_constant_words: Some(0),
            bindings: 3,
            attachments: 0,
            dependencies: 0,
            initial_ranges: 1,
            name_bytes: 192,
            upload_bytes: forward_plus::LIGHT_BUFFER_BYTES as usize,
            edges: 8,
            segments: 8,
            readers: 8,
        },
        1,
    )?;
    // The zero-light dispatch spans all 65,536 tile records. Later frames only
    // overwrite the prefix required by their active extent.
    let mut graph = pool.begin()?;
    let camera = graph.import_buffer("tile initialization camera", camera)?;
    let empty = forward_plus::light_bytes(INITIAL_EXTENT, INITIAL_EXTENT, &[])?;
    let lights = light_upload(&mut graph, &empty)?;
    let tiles = graph.buffer(
        "persistent tile light lists",
        BufferDesc {
            size: forward_plus::TILE_BUFFER_BYTES,
        },
    )?;
    forward_plus::append_culling(
        &mut graph,
        INITIAL_EXTENT,
        INITIAL_EXTENT,
        camera,
        lights,
        tiles,
    )?;
    graph.export_buffer(tiles)?;
    let plan = graph.compile()?;
    let mut pending = backend.submit(&plan)?;
    let imported = B::export_buffer(&pending, tiles)?;
    drop(B::wait(&mut pending, super::WAIT)?);
    Ok(imported)
}

pub(super) fn append(
    graph: &mut Graph,
    extent: [u32; 2],
    camera: BufferHandle,
    preset: &LightBytes,
    tiles: Option<&ImportBuffer>,
) -> Result<(BufferHandle, Option<BufferHandle>), Error> {
    let [width, height] = extent;
    let mut bytes = *preset;
    bytes[..4].copy_from_slice(&width.to_le_bytes());
    bytes[4..8].copy_from_slice(&height.to_le_bytes());
    let lights = light_upload(graph, &bytes)?;
    let tiles = if let Some(tiles) = tiles {
        let tiles = graph.import_buffer("persistent tile light lists", tiles)?;
        forward_plus::append_culling(graph, width, height, camera, lights, tiles)?;
        graph.export_buffer(tiles)?;
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
