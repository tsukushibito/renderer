//! Bistro's deterministic light workload and its explicitly composed Graph input.

use renderer::pbr_forward_plus::{self, LocalLight, LocalLightLayout, TILE_SIZE};
use renderer_core::graph::{BufferHandle, Graph, GraphError, ImageDesc, ImageView};

pub(crate) const PRESET_LEN: usize = 64;
pub(crate) type Preset = [LocalLight; PRESET_LEN];

pub(crate) fn layout() -> LocalLightLayout {
    LocalLightLayout::new(PRESET_LEN).expect("fixed Bistro light grid fits a local-light table")
}
/// Additional staging space for D3D12's 256-byte row and 512-byte placement alignments.
pub(crate) fn upload_padding() -> usize {
    let layout = layout();
    let rows = layout.image_desc().height as usize;
    let row = layout.byte_len() / rows;
    (row.next_multiple_of(256) - row) * rows + 511
}

pub(crate) fn identity(fixture: bool) -> serde_json::Value {
    serde_json::json!({
        "profile": if fixture { "fixture-grid-v1" } else { "bistro-street-grid-v1" },
        "count": PRESET_LEN,
        "tile_size": TILE_SIZE,
    })
}

pub(crate) fn preset(fixture: bool) -> Preset {
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
        LocalLight::Point {
            position,
            radius,
            intensity,
            color: [[1.0, 0.75, 0.55], [0.60, 0.80, 1.0], [0.74, 1.0, 0.70]][index % 3],
        }
    })
}

/// Prepared once per viewport for the manual and comparison passes.
pub(crate) struct LightTable {
    layout: LocalLightLayout,
    bytes: Box<[u8]>,
    masks: Option<ImageDesc>,
}

impl LightTable {
    pub(crate) fn new(fixture: bool, extent: [u32; 2], tiled: bool) -> Result<Self, GraphError> {
        let layout = layout();
        let mut bytes = vec![0; layout.byte_len()].into_boxed_slice();
        layout.encode(extent, &preset(fixture), &mut bytes)?;
        let masks = tiled
            .then(|| layout.tile_mask_desc(extent[0], extent[1]))
            .transpose()?;
        Ok(Self {
            layout,
            bytes,
            masks,
        })
    }

    pub(crate) fn append(
        &self,
        graph: &mut Graph,
        camera: BufferHandle,
    ) -> Result<(ImageView, Option<ImageView>), GraphError> {
        let lights = self.layout.upload(graph, &self.bytes)?;
        let tiles = if let Some(desc) = self.masks {
            let image = graph.image("tile light masks", desc)?;
            let view = graph.image_view(image, 0, 0)?;
            pbr_forward_plus::append_culling(graph, camera, lights, view)?;
            Some(view)
        } else {
            None
        };
        Ok((lights, tiles))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_reproducible_and_keep_all_fixture_lights_active() {
        let full = LightTable::new(false, [32, 16], true).unwrap();
        let fixture = LightTable::new(true, [32, 16], true).unwrap();
        assert_eq!(
            full.bytes,
            LightTable::new(false, [32, 16], true).unwrap().bytes
        );
        assert_ne!(full.bytes, fixture.bytes);
        assert_eq!(fixture.bytes.len(), layout().byte_len());
        assert!(
            preset(true)
                .iter()
                .all(|light| matches!(light, LocalLight::Point {intensity,..} if *intensity > 0.))
        );
    }
}
