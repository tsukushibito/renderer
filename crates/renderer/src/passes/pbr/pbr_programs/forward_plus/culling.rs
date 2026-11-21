//! Forward+ tile culling and the shared point-light table layout.

use crate::shader_binaries::packaged_shaders;
use renderer_core::ImageFormat;
use renderer_core::graph::{
    Binding, BindingResource, BufferHandle, Graph, GraphError, ImageDesc, ImageView, PassHandle,
};
use renderer_core::program::{
    BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
    ManagedProgram, ShaderStages,
};

pub const TILE_SIZE: u32 = 16;
pub const MAX_TILES: u32 = 65_536;
pub(crate) const CULL_PASS: &str = "Forward+ tile light masks";
const WORKGROUP: [u32; 3] = [8, 4, 1];
const CULL_CAMERA_BYTES: u64 = 64;
const MAX_EXTENT: u32 = 65_535;
pub(super) const LIGHTS: u32 = 2;
pub(super) const TILES: u32 = 3;

/// A local light in world space. Radius must be positive. RGB and intensity
/// are linear, finite and nonnegative.
#[derive(Clone, Copy, Debug)]
pub struct PointLight {
    pub position: [f32; 3],
    pub radius: f32,
    pub color: [f32; 3],
    pub intensity: f32,
}

pub fn active_tile_count(width: u32, height: u32) -> Result<u32, GraphError> {
    let cells = width
        .div_ceil(TILE_SIZE)
        .saturating_mul(height.div_ceil(TILE_SIZE));
    if cells == 0 || cells > MAX_TILES || width > MAX_EXTENT || height > MAX_EXTENT {
        return Err(GraphError::invalid_input(
            "Forward+ requires a nonempty extent with at most 65536 tiles",
        ));
    }
    Ok(cells)
}

/// Layout for a prepared, caller-sized point-light table. The first pair of
/// RGBA32F texels holds the viewport and active count. Each light occupies
/// one more pair (position/radius, linear RGB/intensity).
#[derive(Clone, Copy, Debug)]
pub struct PointLightLayout {
    capacity: usize,
    desc: ImageDesc,
    bytes: usize,
}

impl PointLightLayout {
    pub fn new(capacity: usize) -> Result<Self, GraphError> {
        let slots = u32::try_from(capacity)
            .ok()
            .and_then(|n| n.checked_add(1))
            .filter(|_| capacity != 0)
            .ok_or_else(|| GraphError::invalid_input("invalid point-light table capacity"))?;
        let columns = slots.min(256);
        let desc = ImageDesc::rgba32_float(2 * columns, slots.div_ceil(columns));
        let bytes = desc
            .byte_len()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| GraphError::invalid_input("point-light table size overflow"))?;
        Ok(Self {
            capacity,
            desc,
            bytes,
        })
    }

    pub const fn image_desc(self) -> ImageDesc {
        self.desc
    }

    pub const fn byte_len(self) -> usize {
        self.bytes
    }

    /// One RGBA16F texel stores a 32-light mask per tile. The selected
    /// capacity determines the number of horizontal words per tile.
    pub fn tile_mask_desc(self, width: u32, height: u32) -> Result<ImageDesc, GraphError> {
        active_tile_count(width, height)?;
        let columns = width
            .div_ceil(TILE_SIZE)
            .checked_mul((self.capacity as u32).div_ceil(32))
            .ok_or_else(|| GraphError::invalid_input("point-light mask width overflow"))?;
        Ok(ImageDesc::rgba16_float(columns, height.div_ceil(TILE_SIZE)))
    }

    /// Write into reusable, initialized storage. Slots beyond the active
    /// count are ignored by both the culling and lighting shaders.
    pub fn encode(
        self,
        [width, height]: [u32; 2],
        lights: &[PointLight],
        bytes: &mut [u8],
    ) -> Result<(), GraphError> {
        if width == 0 || height == 0 || width > MAX_EXTENT || height > MAX_EXTENT {
            return Err(GraphError::invalid_input("invalid point-light viewport"));
        }
        if bytes.len() != self.bytes || lights.len() > self.capacity {
            return Err(GraphError::invalid_input(
                "point-light table capacity exceeded",
            ));
        }
        let count = lights.len() as u32;
        let header = [width, height, count & 0xffff, count >> 16];
        for (word, value) in bytes[..16].as_chunks_mut::<4>().0.iter_mut().zip(header) {
            *word = (value as f32).to_le_bytes();
        }
        for (record, light) in bytes[32..].as_chunks_mut::<32>().0.iter_mut().zip(lights) {
            let values = [
                light.position[0],
                light.position[1],
                light.position[2],
                light.radius,
                light.color[0],
                light.color[1],
                light.color[2],
                light.intensity,
            ];
            if values.iter().any(|value| !value.is_finite())
                || light.radius <= 0.0
                || light.color.iter().any(|v| *v < 0.0)
                || light.intensity < 0.0
            {
                return Err(GraphError::invalid_input("invalid PBR point light"));
            }
            for (word, value) in record.as_chunks_mut::<4>().0.iter_mut().zip(values) {
                *word = value.to_le_bytes();
            }
        }
        Ok(())
    }

    pub fn upload(self, graph: &mut Graph, bytes: &[u8]) -> Result<ImageView, GraphError> {
        let image = graph.image("local point lights", self.desc)?;
        graph.upload_image("local point light update", image, bytes)?;
        graph.image_view(image, 0, 0)
    }
}

pub(super) const fn buffer_slot(
    binding: u32,
    kind: BindingType,
    stages: ShaderStages,
    min_size: u64,
) -> BindingLayout {
    BindingLayout {
        group: 0,
        binding,
        kind,
        stages,
        min_size,
    }
}

static CULL_PROGRAM: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: WORKGROUP,
    bindings: &[
        buffer_slot(
            0,
            BindingType::Uniform,
            ShaderStages::COMPUTE,
            CULL_CAMERA_BYTES,
        ),
        buffer_slot(LIGHTS, BindingType::SampledImage, ShaderStages::COMPUTE, 0),
        buffer_slot(
            TILES,
            BindingType::StorageImageWrite(ImageFormat::Rgba16Float),
            ShaderStages::COMPUTE,
            0,
        ),
    ],
    dispatch: DispatchContract {
        workgroups: DispatchWorkgroups::Image2d {
            group: 0,
            binding: TILES,
        },
        bytes: &[CULL_CAMERA_BYTES, 0, 0],
    },
    shaders: packaged_shaders!(
        include_bytes!("../../../../../shaders/forward_plus.comp.spv"),
        include_bytes!("../../../../../shaders/dxil/forward_plus.comp.dxil"),
    ),
};

pub const fn compute_program() -> ManagedProgram {
    // SAFETY: Image2d covers the bound view. Each in-range invocation writes
    // exactly its own texel. Counts and point-light loads are bounded by
    // the actual sampled view; arbitrary header values cannot enlarge it.
    unsafe { ManagedProgram::from_trusted_descriptor(&CULL_PROGRAM) }
}

pub(super) fn buffer_binding(binding: u32, buffer: BufferHandle, size: u64) -> Binding {
    Binding {
        group: 0,
        binding,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size,
        },
    }
}

pub(crate) fn light_binding(view: ImageView) -> Binding {
    Binding {
        group: 0,
        binding: LIGHTS,
        resource: BindingResource::ImageView(view),
    }
}

pub(super) fn tile_binding(view: ImageView) -> Binding {
    Binding {
        group: 0,
        binding: TILES,
        resource: BindingResource::ImageView(view),
    }
}

/// Initialize the complete tile mask view from the camera and point-light table.
pub fn append_culling(
    graph: &mut Graph,
    camera: BufferHandle,
    lights: ImageView,
    tiles: ImageView,
) -> Result<PassHandle, GraphError> {
    graph.dispatch(
        CULL_PASS,
        &compute_program(),
        &[
            buffer_binding(0, camera, CULL_CAMERA_BYTES),
            light_binding(lights),
            tile_binding(tiles),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn culling_reflection_matches_the_host_layout() {
        let cull: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../shaders/dxil/forward_plus.comp.spirv-reflection.json"
        ))
        .unwrap();
        assert_eq!(cull["ubos"][0]["block_size"], CULL_CAMERA_BYTES);
        assert_eq!(cull["separate_images"][0]["binding"], LIGHTS);
        assert_eq!(cull["separate_images"][0]["type"], "texture2D");
        assert_eq!(cull["images"][0]["binding"], TILES);
        assert_eq!(cull["images"][0]["set"], 0);
        assert_eq!(cull["images"][0]["type"], "image2D");
        assert_eq!(cull["images"][0]["format"], "rgba16f");
        assert_eq!(cull["images"][0]["writeonly"], true);
        assert_eq!(
            cull["entryPoints"][0]["workgroup_size"],
            serde_json::json!(WORKGROUP)
        );
        let dxil = include_str!("../../../../../shaders/dxil/forward_plus.comp.reflection.txt");
        assert!(dxil.split_whitespace().any(|word| word == "t2"));
        assert!(dxil.split_whitespace().any(|word| word == "u3"));
    }
}
