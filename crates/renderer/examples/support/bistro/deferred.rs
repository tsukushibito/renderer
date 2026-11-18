//! Bistro's single-sample G-buffer and deferred lighting recipe.

use super::{
    Error,
    program::{self, deferred::DEFERRED_RECONSTRUCTION_BYTES},
};
use renderer::{
    graph::{
        Attachment, Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphError,
        ImageView, LoadOp, RasterAttachments, RasterConstants, RasterPass, StoreOp,
    },
    program::{ManagedRasterProgram, SamplerDescriptor},
};

pub(super) const PLANES: [&str; 3] = [
    "G-buffer base color and metallic",
    "G-buffer normals and roughness",
    "G-buffer emissive and AO",
];

pub(super) struct Inputs {
    pub hdr: ImageView,
    pub depth: ImageView,
    pub clear: [f32; 4],
    pub camera: BufferHandle,
    pub view_projection: [[f32; 4]; 4],
    pub lighting: [Binding; 9],
}

pub(super) fn append(
    graph: &mut Graph,
    scene: Inputs,
    draws: impl FnOnce(&mut RasterPass<'_, '_>, &ManagedRasterProgram) -> Result<(), GraphError>,
) -> Result<(), Error> {
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
        |raster| draws(raster, &program::deferred::gbuffer_program()),
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
        &inverse_bytes(scene.view_projection)?,
    )?;
    let inverse = uniform(1, reconstruction, DEFERRED_RECONSTRUCTION_BYTES);
    let gbuffer = [
        uniform(0, scene.camera, program::CAMERA_BYTES),
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
    let program = program::deferred::deferred_lighting_program();
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
    Ok(())
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

fn inverse_bytes(matrix: [[f32; 4]; 4]) -> Result<[u8; 64], Error> {
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
            return Err("deferred reconstruction needs an invertible view projection".into());
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
            return Err("deferred inverse view projection is not finite".into());
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
        let camera = super::super::camera::Camera::fixture();
        let matrix = camera.matrix(1.75, 0.1, 100.0).unwrap();
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
