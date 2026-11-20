//! Bistro's deterministic light presets and the all-light comparison input.

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use super::Error;
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::{
    graph::{BufferDesc, BufferHandle, Graph, GraphPool, ImportBuffer},
    runtime::Runtime,
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12"), test))]
use renderer_pipeline::pbr_forward_plus;
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer_pipeline::pbr_forward_plus::Tiles;
use renderer_pipeline::pbr_forward_plus::{MAX_LIGHTS, PointLight, TILE_SIZE};

pub(super) type Preset = [PointLight; MAX_LIGHTS];

pub(super) fn identity(fixture: bool) -> serde_json::Value {
    serde_json::json!({
        "profile": if fixture { "fixture-grid-v1" } else { "bistro-street-grid-v1" },
        "count": MAX_LIGHTS,
        "tile_size": TILE_SIZE,
    })
}

pub(super) fn preset(fixture: bool) -> Preset {
    std::array::from_fn(|index| {
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
        PointLight {
            position,
            radius,
            intensity,
            color: [[1.0, 0.75, 0.55], [0.60, 0.80, 1.0], [0.74, 1.0, 0.70]][index % 3],
        }
    })
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(super) fn initialize_tiles(
    runtime: &mut Runtime,
    camera: &ImportBuffer,
) -> Result<Tiles, Error> {
    let mut pool = GraphPool::new(pbr_forward_plus::TILE_INITIALIZATION_CAPACITY, 1)?;
    let mut graph = pool.begin()?;
    let camera = graph.import_buffer("tile initialization camera", camera)?;
    let mut initialization = pbr_forward_plus::append_tile_initialization(&mut graph, camera)?;
    let pending = runtime.submit(&graph.compile()?)?;
    let tiles = initialization.accept(&pending)?;
    drop(super::backend::wait_for(runtime, pending.id())?);
    Ok(tiles)
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(super) fn snapshot(
    runtime: &Runtime,
    tiles: Option<&Tiles>,
) -> Result<Option<ImportBuffer>, Error> {
    Ok(tiles.map(|tiles| tiles.snapshot(runtime)).transpose()?)
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(super) fn append_reference(
    graph: &mut Graph,
    runtime: &Runtime,
    extent: [u32; 2],
    camera: BufferHandle,
    preset: &Preset,
    tiles: Option<&Tiles>,
) -> Result<(BufferHandle, Option<BufferHandle>), Error> {
    let [width, height] = extent;
    let bytes = pbr_forward_plus::light_bytes(width, height, preset)?;
    let lights = graph.buffer(
        "local point lights",
        BufferDesc {
            size: pbr_forward_plus::LIGHT_BUFFER_BYTES,
        },
    )?;
    graph.upload_buffer("local point light update", lights, 0, &bytes)?;
    let tiles = snapshot(runtime, tiles)?
        .map(|tiles| -> Result<_, Error> {
            let tiles = graph.import_buffer("persistent tile light lists", &tiles)?;
            pbr_forward_plus::append_culling(graph, width, height, camera, lights, tiles)?;
            Ok(tiles)
        })
        .transpose()?;
    Ok((lights, tiles))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_reproducible_and_keep_all_fixture_lights_active() {
        let full = pbr_forward_plus::light_bytes(1, 1, &preset(false)).unwrap();
        let fixture = pbr_forward_plus::light_bytes(1, 1, &preset(true)).unwrap();
        assert_eq!(
            full,
            pbr_forward_plus::light_bytes(1, 1, &preset(false)).unwrap()
        );
        assert_ne!(full, fixture);
        assert_eq!(
            u32::from_le_bytes(fixture[8..12].try_into().unwrap()),
            MAX_LIGHTS as u32
        );
    }
}
