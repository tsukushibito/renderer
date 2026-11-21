//! Application-owned scene declarations shared by both windowed backends.

use renderer::unlit_forward;
use renderer_core::{
    graph::{
        Attachment, BufferDesc, BufferHandle, Graph, GraphCapacity, GraphError, ImageDesc,
        ImageHandle, ImportBuffer, LoadOp, RasterAttachments, RasterDraw, RasterParameters,
        StoreOp,
    },
    program::ManagedRasterProgram,
};

pub const VERTEX_BYTES: u64 = 224;
pub const INDEX_BYTES: u64 = 144;
pub const COLORS: [[u8; 4]; 4] = [
    [8, 16, 32, 255],
    [255, 64, 32, 255],
    [32, 128, 255, 255],
    [64, 96, 64, 255],
];
const CLEAR: [f32; 4] = [8.0 / 255.0, 16.0 / 255.0, 32.0 / 255.0, 1.0];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Composition {
    Standard,
    Custom,
}

pub fn capacity() -> GraphCapacity {
    GraphCapacity {
        resources: 8,
        passes: 8,
        accesses: 32,
        draws: 8,
        raster_constant_words: None,
        bindings: 0,
        attachments: 4,
        dependencies: 8,
        initial_ranges: 16,
        name_bytes: 512,
        upload_bytes: 512,
        edges: 64,
        segments: 64,
        readers: 64,
    }
}

/// Upload once at the explicit scene preparation boundary. Index provenance
/// follows these immutable registered contents into ordinary imported Graphs.
pub fn upload(graph: &mut Graph) -> Result<(BufferHandle, BufferHandle), GraphError> {
    let vertices = graph.buffer("scene vertices", BufferDesc { size: VERTEX_BYTES })?;
    let indices = graph.buffer("scene indices", BufferDesc { size: INDEX_BYTES })?;
    let mut vertex_bytes = [0; VERTEX_BYTES as usize];
    for (vertex, [x, y, z]) in vertex_bytes.as_chunks_mut::<28>().0.iter_mut().zip([
        [-0.5_f32, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [0.5, 0.5, -0.5],
        [-0.5, -0.5, 0.5],
        [0.5, -0.5, 0.5],
        [-0.5, 0.5, 0.5],
        [0.5, 0.5, 0.5],
    ]) {
        for (bytes, value) in vertex
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip([x, y, z, 1.0, 1.0, 1.0, 1.0])
        {
            bytes.copy_from_slice(&value.to_le_bytes());
        }
    }
    let mut index_bytes = [0; INDEX_BYTES as usize];
    for (bytes, value) in index_bytes.as_chunks_mut::<4>().0.iter_mut().zip([
        0_u32, 2, 1, 1, 2, 3, 4, 5, 6, 5, 7, 6, 0, 1, 4, 1, 5, 4, 2, 6, 3, 3, 6, 7, 0, 4, 2, 2, 4,
        6, 1, 3, 5, 3, 7, 5,
    ]) {
        bytes.copy_from_slice(&value.to_le_bytes());
    }
    graph.upload_buffer("upload scene vertices", vertices, 0, &vertex_bytes)?;
    graph.upload_buffer("upload scene indices", indices, 0, &index_bytes)?;
    graph.export_buffer(vertices)?;
    graph.export_buffer(indices)?;
    Ok((vertices, indices))
}

fn multiply(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    std::array::from_fn(|column| {
        std::array::from_fn(|row| (0..4).map(|k| a[k][row] * b[column][k]).sum())
    })
}

pub fn build(
    graph: &mut Graph,
    buffers: &[ImportBuffer; 2],
    clip_from_world: [[f32; 4]; 4],
    extent: (u32, u32),
    composition: Composition,
) -> Result<ImageHandle, GraphError> {
    let vertices = graph.import_buffer("scene vertices", &buffers[0])?;
    let indices = graph.import_buffer("scene indices", &buffers[1])?;
    let draws: [RasterDraw; 3] = std::array::from_fn(|index| {
        let (scale, position) = [
            ([1.2, 1.5, 1.2], [-1.1, 0.0, 0.0]),
            ([1.0, 1.0, 1.0], [1.0, -0.25, 0.75]),
            ([5.0, 0.1, 4.0], [0.0, -0.8, 0.0]),
        ][index];
        let world_from_model = [
            [scale[0], 0.0, 0.0, 0.0],
            [0.0, scale[1], 0.0, 0.0],
            [0.0, 0.0, scale[2], 0.0],
            [position[0], position[1], position[2], 1.0],
        ];
        RasterDraw {
            vertices,
            indices,
            first_index: 0,
            index_count: 36,
            instance_count: 1,
            bindings: None,
            parameters: RasterParameters::new(
                multiply(clip_from_world, world_from_model),
                COLORS[index + 1].map(|value| value as f32 / 255.0),
            )
            .expect("finite application camera and model matrices")
            .into(),
        }
    });
    let color_desc = ImageDesc::rgba8(extent.0, extent.1);
    let color = match composition {
        Composition::Standard => {
            unlit_forward::forward(
                graph,
                "standard forward scene",
                draws,
                unlit_forward::ForwardConfig {
                    color: color_desc,
                    clear_color: CLEAR,
                    depth: true,
                },
            )?
            .color
        }
        Composition::Custom => {
            let color = graph.image("custom scene color", color_desc)?;
            let depth =
                graph.image("custom scene depth", ImageDesc::depth32(extent.0, extent.1))?;
            let color_attachment = Attachment {
                view: graph.image_view(color, 0, 0)?,
                load: LoadOp::Clear(CLEAR),
                store: StoreOp::Store,
            };
            let depth_attachment = Attachment {
                view: graph.image_view(depth, 0, 0)?,
                load: LoadOp::Clear(1.0),
                store: StoreOp::Store,
            };
            graph.raster(
                "application raster scene",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[color_attachment],
                    depth: Some(depth_attachment),
                },
                |raster| raster.draw(&ManagedRasterProgram::mesh(), draws),
            )?;
            color
        }
    };
    graph.export_image(color)?;
    Ok(color)
}

pub fn verify(bytes: &[u8]) -> Result<(u64, usize), &'static str> {
    if bytes.first_chunk::<4>() != Some(&COLORS[0]) {
        return Err(
            "fixed-camera source corner does not contain the independent expected clear color",
        );
    }
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    let mut geometry = 0;
    for pixel in bytes.as_chunks::<4>().0 {
        if !COLORS.contains(pixel) {
            return Err(
                "windowed readback contains a color outside the independently fixed scene palette",
            );
        }
        geometry += usize::from(*pixel != COLORS[0]);
        for &value in pixel {
            digest = (digest ^ u64::from(value)).wrapping_mul(0x100_0000_01b3);
        }
    }
    if geometry == 0 {
        return Err("windowed camera did not render any scene geometry");
    }
    Ok((digest, geometry))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readback_acceptance_checks_palette_and_visible_geometry() {
        assert!(verify(&COLORS[0]).is_err());
        assert!(verify(&[0, 0, 0, 255]).is_err());
        assert!(verify(&COLORS[1]).is_err());
        assert_eq!(verify(&[COLORS[0], COLORS[1]].concat()).unwrap().1, 1);
    }
}
