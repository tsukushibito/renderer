//! Shared single-sample G-buffer and deferred lighting pass composition.

use super::{
    CAMERA_BYTES, DEFERRED_RECONSTRUCTION_BYTES, deferred_lighting_program, gbuffer_program,
};
use renderer::{
    graph::{
        Attachment, Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphError,
        ImageHandle, ImageView, LoadOp, RasterAttachments, RasterConstants, RasterPass, StoreOp,
    },
    program::{ManagedRasterProgram, SamplerDescriptor},
};

/// Names of the transient G-buffer material planes.
pub const PLANES: [&str; 3] = [
    "G-buffer base color and metallic",
    "G-buffer normals and roughness",
    "G-buffer emissive and AO",
];

/// Targets and shader inputs for G-buffer raster and full-screen lighting.
pub struct Inputs {
    pub hdr: ImageView,
    pub depth: ImageView,
    pub clear: [f32; 4],
    pub camera: BufferHandle,
    pub view_projection: [[f32; 4]; 4],
    pub lighting: [Binding; 9],
}

/// The typed RGBA16F material planes and their single-sample D32 depth.
#[derive(Clone, Copy, Debug)]
pub struct GBuffer {
    /// Linear base RGB and metallic.
    pub base_color_metallic: ImageHandle,
    /// Octahedral shading normal, roughness and packed geometric normal.
    pub normals_roughness: ImageHandle,
    /// Linear emissive RGB and ambient occlusion.
    pub emissive_ao: ImageHandle,
    pub depth: ImageHandle,
}

/// Declare G-buffer raster and deferred lighting for an application-built Graph.
///
/// Reserve the two audited Deferred Programs and matching G-buffer image classes
/// at preparation time. If a declaration fails, discard this frame's Graph.
pub fn append(
    graph: &mut Graph,
    scene: Inputs,
    draws: impl FnOnce(&mut RasterPass<'_, '_>, &ManagedRasterProgram) -> Result<(), GraphError>,
) -> Result<GBuffer, GraphError> {
    let reconstruction_bytes = inverse_bytes(scene.view_projection)?;
    let desc = graph.image_desc(scene.hdr.image())?;
    let base = graph.image(PLANES[0], desc)?;
    let normals = graph.image(PLANES[1], desc)?;
    let emissive = graph.image(PLANES[2], desc)?;
    let views = [
        graph.image_view(base, 0, 0)?,
        graph.image_view(normals, 0, 0)?,
        graph.image_view(emissive, 0, 0)?,
    ];
    let colors = views.map(|view| Attachment {
        view,
        load: LoadOp::Clear([0.0; 4]),
        store: StoreOp::Store,
    });
    graph.raster(
        "G-buffer opaque and mask",
        RasterAttachments {
            color_resolves: &[],
            colors: &colors,
            depth: Some(Attachment {
                view: scene.depth,
                load: LoadOp::Clear(1.0),
                store: StoreOp::Store,
            }),
        },
        |raster| draws(raster, &gbuffer_program()),
    )?;

    let reconstruction = graph.buffer(
        "deferred inverse view projection",
        BufferDesc {
            size: DEFERRED_RECONSTRUCTION_BYTES as u64,
        },
    )?;
    graph.upload_buffer(
        "deferred reconstruction update",
        reconstruction,
        0,
        &reconstruction_bytes,
    )?;
    let inverse = uniform(1, reconstruction, DEFERRED_RECONSTRUCTION_BYTES);
    let gbuffer = [
        uniform(0, scene.camera, CAMERA_BYTES),
        inverse,
        sample(0, views[0]),
        sample(1, views[1]),
        sample(2, views[2]),
        sample(3, scene.depth),
        Binding {
            group: 1,
            binding: 4,
            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
        },
    ];
    let mut bindings = [inverse; 16];
    bindings[..gbuffer.len()].copy_from_slice(&gbuffer);
    bindings[gbuffer.len()..].copy_from_slice(&scene.lighting);
    let program = deferred_lighting_program();
    graph.raster(
        "deferred PBR lighting",
        RasterAttachments {
            color_resolves: &[],
            colors: &[Attachment {
                view: scene.hdr,
                load: LoadOp::Clear(scene.clear),
                store: StoreOp::Store,
            }],
            depth: None,
        },
        |raster| raster.draw_fullscreen(&program, &bindings, RasterConstants::default()),
    )?;
    Ok(GBuffer {
        base_color_metallic: base,
        normals_roughness: normals,
        emissive_ao: emissive,
        depth: scene.depth.image(),
    })
}

fn uniform(binding: u32, buffer: BufferHandle, size: usize) -> Binding {
    Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size: size as u64,
        },
    }
}

fn sample(binding: u32, view: ImageView) -> Binding {
    Binding {
        group: 1,
        binding,
        resource: BindingResource::ImageView(view),
    }
}

fn inverse_bytes(matrix: [[f32; 4]; 4]) -> Result<[u8; DEFERRED_RECONSTRUCTION_BYTES], GraphError> {
    // Row operations use f64 and partial pivoting. Serialize the inverse in the
    // original column-major convention shared by the two shader binaries.
    let mut rows = std::array::from_fn::<_, 4, _>(|row| {
        let mut values = [0.0; 8];
        for (column, values_column) in matrix.iter().enumerate() {
            values[column] = f64::from(values_column[row]);
        }
        values[4 + row] = 1.0;
        values
    });
    for column in 0..4 {
        let pivot = (column..4)
            .max_by(|&a, &b| rows[a][column].abs().total_cmp(&rows[b][column].abs()))
            .unwrap();
        rows.swap(column, pivot);
        let diagonal = rows[column][column];
        if !diagonal.is_finite() || diagonal == 0.0 {
            return Err(GraphError::invalid_input(
                "deferred reconstruction needs an invertible view projection",
            ));
        }
        for value in &mut rows[column] {
            *value /= diagonal;
        }
        let normalized = rows[column];
        for (row_index, row) in rows.iter_mut().enumerate() {
            if row_index != column {
                let factor = row[column];
                for (value, reference) in row.iter_mut().zip(normalized) {
                    *value -= factor * reference;
                }
            }
        }
    }
    let mut bytes = [0; 64];
    for (word, output) in bytes.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let value = rows[word % 4][4 + word / 4] as f32;
        if !value.is_finite() {
            return Err(GraphError::invalid_input(
                "deferred inverse view projection is not finite",
            ));
        }
        *output = value.to_le_bytes();
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_view_projection_reconstructs_a_world_position() {
        let matrix = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 2.0, 0.0, 0.0],
            [0.0, 0.0, -1.001, -1.0],
            [0.25, -0.5, -0.1, 0.0],
        ];
        let bytes = inverse_bytes(matrix).unwrap();
        let words = bytes.as_chunks::<4>().0;
        let inverse = std::array::from_fn(|column| {
            std::array::from_fn(|row| f32::from_le_bytes(words[column * 4 + row]))
        });
        let multiply = |matrix: [[f32; 4]; 4], vector: [f32; 4]| {
            std::array::from_fn(|row| {
                matrix
                    .iter()
                    .zip(vector)
                    .map(|(col, scale)| col[row] * scale)
                    .sum::<f32>()
            })
        };
        let world = [0.5, -0.75, 0.25, 1.0];
        let clip = multiply(matrix, world);
        let restored = multiply(inverse, clip);
        for (actual, expected) in restored.into_iter().zip(world) {
            assert!((actual - expected).abs() <= 1e-4);
        }
        assert!(inverse_bytes([[0.0; 4]; 4]).is_err());
    }
}
