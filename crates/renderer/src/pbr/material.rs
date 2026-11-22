//! Typed uploads for the standard PBR material ABI.

use renderer_core::{
    graph::ResourceKey,
    runtime::{ExecutionError, FrameSubmission, Runtime},
};
use renderer_core::{
    graph::{
        Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphError, ImportBuffer,
        PassHandle, RasterBindings,
    },
    program::{BindingType, MAX_RASTER_BINDINGS, ManagedRasterProgram},
};

use crate::passes::pbr::MATERIAL_BYTES;

const SIZE: u64 = MATERIAL_BYTES as u64;

use crate::material::AlphaMode;

/// Material constants encoded at asset creation or update. Pass them to
/// [`super::MaterialUniform::upload`] before using the material in a recipe.
/// The default is a white dielectric with unit normal and AO strengths.
#[derive(Clone, Copy, Debug)]
pub struct MaterialFactors {
    pub base_color: [f32; 4],
    pub emissive: [f32; 3],
    pub normal_scale: f32,
    pub metallic: f32,
    pub roughness: f32,
    pub occlusion_strength: f32,
    pub alpha_cutoff: f32,
    pub alpha_mode: AlphaMode,
    pub double_sided: bool,
    /// First baked triangle chart for this static placement and LOD. Lightmapped
    /// mesh vertices are expanded in triangle order; UV0 remains the material UV.
    pub lightmap: Option<LightmapBinding>,
}

/// Bind a static placement and LOD to its consecutive triangle charts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightmapBinding {
    pub first_chart: u32,
    /// Start of the expanded receiver vertices within the bound vertex buffer.
    pub first_vertex: u32,
}

impl Default for MaterialFactors {
    fn default() -> Self {
        Self {
            base_color: [1.0; 4],
            emissive: [0.0; 3],
            normal_scale: 1.0,
            metallic: 0.0,
            roughness: 1.0,
            occlusion_strength: 1.0,
            alpha_cutoff: 0.5,
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
            lightmap: None,
        }
    }
}

impl MaterialFactors {
    /// Decode a cooked standard material at an asset import or change boundary.
    pub fn decode(bytes: &[u8; MATERIAL_BYTES]) -> Result<Self, GraphError> {
        let block = bytes.as_chunks::<4>().0;
        let alpha_mode = match u32::from_le_bytes(block[12]) {
            0 => AlphaMode::Opaque,
            1 => AlphaMode::Mask,
            2 => AlphaMode::Blend,
            _ => return Err(GraphError::invalid_input("invalid PBR material alpha mode")),
        };
        let floats = |index: usize| f32::from_le_bytes(block[index]);
        let factors = MaterialFactors {
            base_color: std::array::from_fn(floats),
            emissive: std::array::from_fn(|index| floats(4 + index)),
            normal_scale: floats(7),
            metallic: floats(8),
            roughness: floats(9),
            occlusion_strength: floats(10),
            alpha_cutoff: floats(11),
            alpha_mode,
            double_sided: u32::from_le_bytes(block[13]) != 0,
            lightmap: u32::from_le_bytes(block[14])
                .checked_sub(1)
                .map(|first_chart| LightmapBinding {
                    first_chart,
                    first_vertex: u32::from_le_bytes(block[15]),
                }),
        };
        if factors.encode()? != *bytes {
            return Err(GraphError::invalid_input(
                "invalid PBR material flags or padding",
            ));
        }
        Ok(factors)
    }

    /// Encode the standard shader ABI for a custom Graph or cooked asset.
    pub fn encode(self) -> Result<[u8; MATERIAL_BYTES], GraphError> {
        if self
            .base_color
            .into_iter()
            .chain([self.metallic, self.roughness, self.occlusion_strength])
            .any(|value| !(0.0..=1.0).contains(&value))
            || self
                .emissive
                .into_iter()
                .any(|value| !value.is_finite() || value < 0.0)
            || !self.normal_scale.is_finite()
            || !self.alpha_cutoff.is_finite()
            || self.alpha_cutoff < 0.0
        {
            return Err(GraphError::invalid_input("invalid PBR material factors"));
        }
        let chart = self
            .lightmap
            .map_or(Some(0), |b| b.first_chart.checked_add(1))
            .ok_or_else(|| GraphError::invalid_input("lightmap chart index overflow"))?;
        let floats = self.base_color.into_iter().chain(self.emissive).chain([
            self.normal_scale,
            self.metallic,
            self.roughness,
            self.occlusion_strength,
            self.alpha_cutoff,
        ]);
        let mode: u32 = match self.alpha_mode {
            AlphaMode::Opaque => 0,
            AlphaMode::Mask => 1,
            AlphaMode::Blend => 2,
        };
        let mut bytes = [0; MATERIAL_BYTES];
        let words = floats.map(f32::to_le_bytes).chain(
            [
                mode,
                u32::from(self.double_sided),
                chart,
                self.lightmap.map_or(0, |b| b.first_vertex),
            ]
            .map(u32::to_le_bytes),
        );
        for (out, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *out = value;
        }
        Ok(bytes)
    }
}

/// PBR material constants uploaded into the current Graph. The buffer and its
/// alpha classification originate from the same checked CPU input.
#[derive(Clone, Copy, Debug)]
pub struct MaterialUniform {
    buffer: BufferHandle,
    graph: u64,
    alpha_mode: AlphaMode,
}

impl MaterialUniform {
    /// Graph-local buffer containing the encoded PBR factors.
    pub const fn buffer(self) -> BufferHandle {
        self.buffer
    }

    /// Buffer class used for material uploads and resident registrations.
    pub const DESC: BufferDesc = BufferDesc { size: SIZE };

    /// Upload factors into a new buffer dedicated to this material.
    /// Reserve one Graph resource, pass and access, `DESC.size` upload bytes,
    /// and room for two copies of the name.
    pub fn upload(
        graph: &mut Graph,
        name: &str,
        factors: MaterialFactors,
    ) -> Result<Self, GraphError> {
        Self::append(graph, name, &factors.encode()?, factors.alpha_mode)
    }

    /// Import a cooked standard material block without changing its bytes.
    /// Check the same field constraints as a typed upload before declaring work.
    pub fn upload_encoded(
        graph: &mut Graph,
        name: &str,
        bytes: &[u8; MATERIAL_BYTES],
    ) -> Result<Self, GraphError> {
        let factors = MaterialFactors::decode(bytes)?;
        Self::append(graph, name, bytes, factors.alpha_mode)
    }

    pub(crate) fn append(
        graph: &mut Graph,
        name: &str,
        bytes: &[u8; MATERIAL_BYTES],
        alpha_mode: AlphaMode,
    ) -> Result<Self, GraphError> {
        let buffer = graph.buffer(name, Self::DESC)?;
        graph.upload_buffer(name, buffer, 0, bytes)?;
        Ok(Self {
            buffer,
            graph: graph.graph_id(),
            alpha_mode,
        })
    }

    pub(crate) fn import_snapshot(
        graph: &mut Graph,
        name: &str,
        snapshot: &ImportBuffer,
        alpha_mode: AlphaMode,
    ) -> Result<Self, GraphError> {
        Ok(Self {
            buffer: graph.import_buffer(name, snapshot)?,
            graph: graph.graph_id(),
            alpha_mode,
        })
    }
    pub const fn alpha_mode(self) -> AlphaMode {
        self.alpha_mode
    }

    /// Keep the uploaded data available to later Graphs.
    pub fn export(self, graph: &mut Graph) -> Result<(), GraphError> {
        self.check_graph(graph)?;
        graph.export_buffer(self.buffer)
    }

    /// Acquire the export from the matching accepted submission.
    pub fn accept(
        self,
        submission: &FrameSubmission,
    ) -> Result<ResidentMaterialUniform, ExecutionError> {
        let snapshot = submission.export_buffer(self.buffer)?;
        Ok(ResidentMaterialUniform {
            key: snapshot.key(),
            snapshot: Some(snapshot),
            alpha_mode: self.alpha_mode,
        })
    }

    /// Request a CPU copy while keeping the Graph buffer private.
    pub fn readback(self, graph: &mut Graph, name: &str) -> Result<PassHandle, GraphError> {
        self.check_graph(graph)?;
        graph.readback_buffer(name, self.buffer, 0, SIZE)
    }

    /// Construct read-only Raster bindings with this uniform at group 0, slot
    /// 1. Supply the Program's remaining operands in `other`.
    pub fn raster_bindings(
        self,
        graph: &mut Graph,
        program: &ManagedRasterProgram,
        other: &[Binding],
    ) -> Result<RasterBindings, GraphError> {
        self.check_graph(graph)?;
        let factors = program
            .bindings()
            .iter()
            .zip(program.binding_bytes())
            .find(|(layout, _)| (layout.group, layout.binding) == (0, 1));
        if !factors.is_some_and(|(layout, &bytes)| {
            layout.kind == BindingType::Uniform && layout.min_size == SIZE && bytes == SIZE
        }) || other.len() >= MAX_RASTER_BINDINGS
        {
            return Err(GraphError::invalid_input(
                "PBR material requires a 64-byte uniform at group 0, slot 1",
            ));
        }
        let mut bindings = [self.binding(); MAX_RASTER_BINDINGS];
        let count = 1 + other.len();
        bindings[1..count].copy_from_slice(other);
        graph.raster_bindings(program, &bindings[..count])
    }

    pub(super) fn binding(self) -> Binding {
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::Buffer {
                buffer: self.buffer,
                offset: 0,
                size: SIZE,
            },
        }
    }

    pub(super) fn check_graph(self, graph: &Graph) -> Result<(), GraphError> {
        if self.graph == graph.graph_id() {
            Ok(())
        } else {
            Err(GraphError::invalid_input(
                "PBR material uniform belongs to another Graph",
            ))
        }
    }
}

/// Accepted immutable material data. Replace it by uploading and accepting a
/// new material, then release the old registration.
#[derive(Debug)]
#[must_use = "release the registration when the material is no longer needed"]
pub struct ResidentMaterialUniform {
    key: ResourceKey,
    snapshot: Option<ImportBuffer>,
    alpha_mode: AlphaMode,
}

impl ResidentMaterialUniform {
    pub const fn alpha_mode(&self) -> AlphaMode {
        self.alpha_mode
    }

    /// Import the scheduled contents of this material without waiting for GPU
    /// completion. The Runtime retains them through every accepted consumer.
    pub fn import(&self, graph: &mut Graph, name: &str) -> Result<MaterialUniform, GraphError> {
        MaterialUniform::import_snapshot(graph, name, self.snapshot()?, self.alpha_mode)
    }
    pub(crate) fn snapshot(&self) -> Result<&ImportBuffer, GraphError> {
        self.snapshot
            .as_ref()
            .ok_or_else(|| GraphError::invalid_input("PBR material snapshot is not available"))
    }

    /// Unregister the material. Previously accepted work keeps its leases.
    /// On failure, the registration remains available for a retry.
    pub fn release(&mut self, runtime: &mut Runtime) -> Result<(), ExecutionError> {
        runtime.delete(self.key)?;
        self.snapshot = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_packing_matches_float_and_flag_slots() {
        let values = MaterialFactors {
            base_color: [0.1, 0.2, 0.3, 0.4],
            emissive: [1.0, 2.0, 3.0],
            normal_scale: -1.0,
            metallic: 0.5,
            roughness: 0.6,
            occlusion_strength: 0.9,
            alpha_cutoff: 2.0,
            alpha_mode: AlphaMode::Mask,
            double_sided: true,
            lightmap: Some(LightmapBinding {
                first_chart: 15,
                first_vertex: 7,
            }),
        };
        let bytes = values.encode().unwrap();
        let words = bytes.as_chunks::<4>().0;
        assert_eq!(
            words[..12],
            [
                0.1f32, 0.2, 0.3, 0.4, 1.0, 2.0, 3.0, -1.0, 0.5, 0.6, 0.9, 2.0
            ]
            .map(f32::to_le_bytes)
        );
        assert_eq!(words[12..], [1u32, 1, 16, 7].map(u32::to_le_bytes));
        assert!(
            MaterialFactors {
                roughness: f32::NAN,
                ..values
            }
            .encode()
            .is_err()
        );
    }
}
