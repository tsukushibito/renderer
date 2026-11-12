//! Explicit resource registration and advisory upload staging bounds.

use crate::graph::{BufferDesc, ImportBuffer};

use super::{ResourceDesc, RuntimeError, VulkanRuntime};

impl VulkanRuntime {
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
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let name = name.as_ref();
        // Validate names even when acquisition can reuse an existing allocation.
        if name.as_bytes().contains(&0) {
            return Err(RuntimeError::invalid(
                "graph buffer debug name contains a NUL byte",
            ));
        }
        let maximum = self.owner.device.max_buffer_size();
        if desc.size == 0 || desc.size > maximum {
            return Err(RuntimeError::invalid(format!(
                "graph buffer size {} must be nonzero and not exceed Device maxBufferSize {maximum}",
                desc.size,
            )));
        }
        let (key, physical) = self.acquire(ResourceDesc::Buffer(desc), name)?;
        let revision = physical.lock().scheduled.revision;
        // A poisoned resource state stops the Device. Do not publish a new
        // registration after acquisition has observed that terminal condition.
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        self.register(key.index as usize);
        Ok(ImportBuffer {
            key,
            revision,
            desc,
            initialized: Vec::new(),
        })
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
