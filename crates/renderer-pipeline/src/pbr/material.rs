//! Typed uploads for the standard PBR material ABI.

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::{
    graph::ResourceKey,
    runtime::{ExecutionError, FrameSubmission, Runtime},
};
use renderer::{
    graph::{
        Binding, BindingResource, BufferDesc, BufferHandle, Graph, GraphError, ImportBuffer,
        PassHandle, RasterBindings,
    },
    program::{BindingType, MAX_RASTER_BINDINGS, ManagedRasterProgram},
};

use crate::passes::pbr::MATERIAL_BYTES;

use super::{AlphaMode, MaterialFactors};

const SIZE: u64 = MATERIAL_BYTES as u64;

/// PBR material constants uploaded into the current Graph. The buffer and its
/// alpha classification originate from the same checked CPU input.
#[derive(Clone, Copy, Debug)]
pub struct MaterialUniform {
    buffer: BufferHandle,
    graph: u64,
    alpha_mode: AlphaMode,
}

impl MaterialUniform {
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
        };
        if factors.encode()? != *bytes {
            return Err(GraphError::invalid_input(
                "invalid PBR material flags or padding",
            ));
        }
        Self::append(graph, name, bytes, alpha_mode)
    }

    fn append(
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

    pub const fn alpha_mode(self) -> AlphaMode {
        self.alpha_mode
    }

    /// Keep the uploaded data available to later Graphs.
    pub fn export(self, graph: &mut Graph) -> Result<(), GraphError> {
        self.check_graph(graph)?;
        graph.export_buffer(self.buffer)
    }

    /// Acquire the export from the matching accepted submission.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn accept(self, submission: &FrameSubmission) -> Result<ResidentMaterial, ExecutionError> {
        let snapshot = submission.export_buffer(self.buffer)?;
        Ok(ResidentMaterial {
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
pub struct ResidentMaterial {
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    key: ResourceKey,
    snapshot: Option<ImportBuffer>,
    alpha_mode: AlphaMode,
}

impl ResidentMaterial {
    pub const fn alpha_mode(&self) -> AlphaMode {
        self.alpha_mode
    }

    /// Import the scheduled contents of this material without waiting for GPU
    /// completion. The Runtime retains them through every accepted consumer.
    pub fn import(&self, graph: &mut Graph, name: &str) -> Result<MaterialUniform, GraphError> {
        let snapshot = self
            .snapshot
            .as_ref()
            .ok_or_else(|| GraphError::invalid_input("PBR material snapshot is not available"))?;
        let buffer = graph.import_buffer(name, snapshot)?;
        Ok(MaterialUniform {
            buffer,
            graph: graph.graph_id(),
            alpha_mode: self.alpha_mode,
        })
    }

    /// Return the range snapshot before an explicit Runtime reprepare. The
    /// private registration and its immutable contents remain unchanged.
    pub fn release_snapshot(&mut self) {
        self.snapshot = None;
    }

    /// Reacquire the snapshot after reprepare. Use the same Runtime.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn refresh_snapshot(&mut self, runtime: &Runtime) -> Result<(), ExecutionError> {
        self.snapshot = Some(runtime.refresh_buffer(self.key)?);
        Ok(())
    }

    /// Unregister the material. Previously accepted work keeps its leases.
    /// On failure, the registration remains available for a retry.
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn release(&mut self, runtime: &mut Runtime) -> Result<(), ExecutionError> {
        runtime.delete(self.key)?;
        self.release_snapshot();
        Ok(())
    }
}
