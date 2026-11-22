//! Shared single-sample G-buffer and deferred lighting pass composition.

use super::{
    CAMERA_BYTES, DEFERRED_RECONSTRUCTION_BYTES, deferred_lighting_program, gbuffer_program,
};
use renderer_core::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    graph::{
        Attachment, Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphError,
        ImageDesc, ImageHandle, ImageView, LoadOp, RasterAttachments, RasterConstants, RasterPass,
        StoreOp,
    },
    program::{BindingType, ManagedRasterProgram, SamplerDescriptor},
};

/// Names of the transient G-buffer material planes.
pub const PLANES: [&str; 3] = [
    "G-buffer base color and metallic",
    "G-buffer normals and roughness",
    "G-buffer emissive and AO",
];

const GEOMETRY_PASS: &str = "G-buffer opaque and mask";
const INVERSE_VIEW_PROJECTION: &str = "deferred inverse view projection";
const UPDATE_PASS: &str = "deferred reconstruction update";
const LIGHTING_PASS: &str = "deferred PBR lighting";
const RECONSTRUCTION: BufferDesc = BufferDesc {
    size: DEFERRED_RECONSTRUCTION_BYTES as u64,
};

/// Targets and shader inputs for G-buffer raster and full-screen lighting.
pub struct Inputs {
    /// A 1×, single-mip, single-layer RGBA16F image.
    pub hdr: ImageView,
    /// A 1×, single-mip, single-layer D32 image matching the HDR extent.
    pub depth: ImageView,
    pub clear: [f32; 4],
    pub camera: BufferHandle,
    pub view_projection: [[f32; 4]; 4],
    pub lighting: [Binding; 10],
    /// Optional graph-local table encoded by [PointLightLayout](crate::pbr_forward_plus::PointLightLayout).
    pub point_lights: Option<ImageView>,
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

/// Reserve the built-in Programs, transient resources and fixed declarations
/// for one invocation per frame. Reserve the targets, camera and lighting
/// inputs, and any geometry draw and binding declarations separately.
pub fn reserve(preparation: &mut Preparation, extent: [u32; 2]) -> Result<(), GraphError> {
    reserve_passes(preparation, extent, false, true)
}

pub(super) fn reserve_passes(
    preparation: &mut Preparation,
    [width, height]: [u32; 2],
    opaque_pass_reserved: bool,
    point_lights: bool,
) -> Result<(), GraphError> {
    let frames = preparation.frames_in_flight().get();
    let planes = PLANES.len();
    let plane_count = frames
        .checked_mul(planes)
        .ok_or_else(|| GraphError::invalid_input("Deferred G-buffer reservation overflow"))?;
    let lighting = if point_lights {
        crate::passes::pbr::deferred::deferred_local_lighting_program()
    } else {
        deferred_lighting_program()
    };
    let programs = [
        gbuffer_program().into(),
        deferred_lighting_program().into(),
        lighting.clone().into(),
    ];
    let lighting_accesses = lighting
        .bindings()
        .iter()
        .filter(|binding| binding.kind != BindingType::Sampler)
        .count();
    let attachments = planes + 2;
    let passes = [GEOMETRY_PASS, UPDATE_PASS, LIGHTING_PASS];
    preparation.add_requirements(
        GraphRequirements {
            resources: planes + 1,
            passes: passes.len() - usize::from(opaque_pass_reserved),
            // Count all deferred attachment accesses and the uniform upload.
            accesses: attachments + lighting_accesses + 1,
            draws: 1,
            bindings: lighting.bindings().len(),
            // The standard scene budget includes color and depth for opaque.
            attachments: attachments - 2 * usize::from(opaque_pass_reserved),
            name_bytes: PLANES
                .into_iter()
                .chain(passes)
                .chain([INVERSE_VIEW_PROJECTION])
                .map(str::len)
                .sum(),
            upload_bytes: DEFERRED_RECONSTRUCTION_BYTES,
            ..GraphRequirements::default()
        },
        WorkingSet {
            resources: &[
                ResourceClass::Image {
                    desc: ImageDesc::rgba16_float(width, height),
                    count: plane_count,
                },
                ResourceClass::Buffer {
                    desc: RECONSTRUCTION,
                    count: frames,
                },
            ],
            raster: &programs[..2 + usize::from(point_lights)],
            ..WorkingSet::default()
        },
    )
}

/// Declare G-buffer raster and deferred lighting for an application-built Graph.
///
/// Use [`reserve`] at preparation time. Both targets must belong to the current
/// Graph. Their formats, dimensions and single-subresource layout are checked
/// before declaring work. If a later declaration fails, discard this frame's Graph.
pub fn append(
    graph: &mut Graph,
    scene: Inputs,
    draws: impl FnOnce(&mut RasterPass<'_, '_>, &ManagedRasterProgram) -> Result<(), GraphError>,
) -> Result<GBuffer, GraphError> {
    let hdr = graph.image_desc(scene.hdr.image())?;
    let desc = ImageDesc::rgba16_float(hdr.width, hdr.height);
    if hdr != desc {
        return Err(GraphError::invalid_input(
            "Deferred HDR target requires a 1× single-mip, single-layer RGBA16F image",
        ));
    }
    if graph.image_desc(scene.depth.image())? != ImageDesc::depth32(desc.width, desc.height) {
        return Err(GraphError::invalid_input(
            "Deferred depth requires a 1× single-mip, single-layer D32 image matching the HDR extent",
        ));
    }
    let reconstruction_bytes = inverse_bytes(scene.view_projection)?;
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
        GEOMETRY_PASS,
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

    let reconstruction = graph.buffer(INVERSE_VIEW_PROJECTION, RECONSTRUCTION)?;
    graph.upload_buffer(UPDATE_PASS, reconstruction, 0, &reconstruction_bytes)?;
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
    let mut bindings = [inverse; 18];
    bindings[..gbuffer.len()].copy_from_slice(&gbuffer);
    bindings[gbuffer.len()..17].copy_from_slice(&scene.lighting);
    let program = if let Some(lights) = scene.point_lights {
        bindings[17] = crate::passes::pbr::forward_plus::light_binding(lights);
        crate::passes::pbr::deferred::deferred_local_lighting_program()
    } else {
        deferred_lighting_program()
    };
    graph.raster(
        LIGHTING_PASS,
        RasterAttachments {
            color_resolves: &[],
            colors: &[Attachment {
                view: scene.hdr,
                load: LoadOp::Clear(scene.clear),
                store: StoreOp::Store,
            }],
            depth: None,
        },
        |raster| {
            raster.draw_fullscreen(
                &program,
                &bindings[..program.bindings().len()],
                RasterConstants::default(),
            )
        },
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
