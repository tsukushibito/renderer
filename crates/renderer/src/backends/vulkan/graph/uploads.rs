//! Explicit resource registration and advisory upload staging bounds.

use crate::graph::{BufferDesc, ImportBuffer};

use super::{ResourceDesc, RuntimeError, VulkanRuntime};
use crate::backends::vulkan::resources::validate_buffer_size;

impl VulkanRuntime {
    /// Physical registry bound including meshes and view targets.
    pub fn resource_capacity(&self) -> usize {
        self.config.resource_capacity
    }

    /// Register a device buffer without uploading data or submitting work.
    ///
    /// The returned snapshot has no initialized bytes. Upload to it through
    /// ordinary Graph operations before reading, then refresh the snapshot after
    /// each accepted submission. Registration lasts until `delete`; dropping the
    /// snapshot neither deletes the buffer nor waits for the GPU.
    pub fn create_buffer(
        &mut self,
        name: impl AsRef<str>,
        desc: BufferDesc,
    ) -> Result<ImportBuffer, RuntimeError> {
        self.register_buffer(name.as_ref(), desc, true)
    }

    pub(crate) fn register_buffer(
        &mut self,
        name: &str,
        desc: BufferDesc,
        allow_creation: bool,
    ) -> Result<ImportBuffer, RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        // C-string validity belongs to optional native debug naming. Logical
        // Graph/resource names remain arbitrary Rust strings in shipping.
        // Validate even when acquisition can reuse an existing allocation.
        #[cfg(feature = "vulkan-diagnostics")]
        if name.as_bytes().contains(&0) {
            return Err(RuntimeError::invalid(
                "graph buffer debug name contains a NUL byte",
            ));
        }
        validate_buffer_size(desc.size, self.owner.device.max_buffer_size())?;
        let (key, physical) =
            self.acquire_inner(ResourceDesc::Buffer(desc), name, allow_creation)?;
        let revision = physical.lock().scheduled.revision;
        // A poisoned resource state stops the Device. Do not publish a new
        // registration after acquisition has observed that terminal condition.
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let initialized = self.capture_ranges(&[])?;
        let snapshot = ImportBuffer::new(key, revision, desc, initialized)
            .map_err(|error| RuntimeError::invalid(error.to_string()))?;
        self.register(key.index as usize);
        Ok(snapshot)
    }

    /// Fixed capacity of the shared upload staging buffer, in bytes.
    /// This does not include readback staging or device resource allocations.
    pub fn upload_capacity(&self) -> u64 {
        self.upload.stats().capacity
    }

    /// Alignment of upload reservations, including cache atom isolation.
    /// Each upload's reserved length is rounded up to a multiple of this value.
    pub fn upload_alignment(&self) -> u64 {
        self.upload.stats().alignment
    }

    /// An advisory upper bound on additional upload reservation bytes.
    ///
    /// This is capacity minus currently reserved bytes. It does not reserve
    /// space or guarantee one contiguous range: fragmentation or another live
    /// staging owner can still cause a later allocation to report exhaustion.
    pub fn available_upload_bytes(&self) -> u64 {
        let stats = self.upload.stats();
        stats.capacity.saturating_sub(stats.reserved_bytes)
    }
}
