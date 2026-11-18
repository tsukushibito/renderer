//! Reusable canonical and per-workspace content state. Publishing swaps prepared
//! snapshots; copying and resetting never replace their backing allocations.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use std::ops::Range;

use ash::vk;

use super::{BufferProvenance, FrameCapacity, RuntimeError, RuntimeErrorKind, Snapshot};

impl Snapshot {
    pub(super) fn for_working_set(capacity: FrameCapacity) -> Result<Self, RuntimeError> {
        Self::with_capacity(
            capacity.initialized_ranges_per_resource,
            capacity.provenance_segments_per_resource,
            capacity.index_prefixes_per_resource,
        )
    }

    /// Explicit preparation boundary for both variable-length content stores.
    pub(super) fn with_capacity(
        initialized_ranges: usize,
        provenance_segments: usize,
        index_prefixes: usize,
    ) -> Result<Self, RuntimeError> {
        let mut initialized = Vec::new();
        initialized
            .try_reserve_exact(initialized_ranges)
            .map_err(|error| RuntimeError::with_source(RuntimeErrorKind::Native, error))?;
        Ok(Self {
            initialized,
            provenance: BufferProvenance::with_capacity(provenance_segments, index_prefixes),
            ..Self::default()
        })
    }

    /// Copy into prepared backing, preserving the old version on either content
    /// store's capacity failure. Immutable Graph pins preserve source contents.
    pub(super) fn copy_from(&mut self, source: &Self) -> Result<(), RuntimeError> {
        self.require_initialized_capacity(source.initialized.len())?;
        // This check-and-copy is transactional. After it succeeds no remaining
        // operation can fail or require a larger backing allocation.
        self.provenance.copy_from(&source.provenance).map_err(|_| {
            RuntimeError::capacity_with(
                "snapshot provenance metadata capacity exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::GraphMetadata,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            )
        })?;
        self.initialized.clear();
        self.initialized.extend_from_slice(&source.initialized);
        self.copy_native_from(source);
        Ok(())
    }

    /// Copy the native history into a fresh snapshot for a reused allocation.
    pub(super) fn copy_native_from(&mut self, source: &Self) {
        self.revision = source.revision;
        self.serial = source.serial;
        self.layout = source.layout;
        self.stage = source.stage;
        self.access = source.access;
        self.writer_stage = source.writer_stage;
        self.writer_access = source.writer_access;
        self.read_stage = source.read_stage;
        self.read_access = source.read_access;
    }

    /// Graph's initialized-range guarantee is already established. This update
    /// only enforces the prepared capacity and retains the old value on failure.
    pub(super) fn set_initialized(&mut self, ranges: &[Range<u64>]) -> Result<(), RuntimeError> {
        self.require_initialized_capacity(ranges.len())?;
        self.initialized.clear();
        self.initialized.extend_from_slice(ranges);
        Ok(())
    }

    fn require_initialized_capacity(&self, required: usize) -> Result<(), RuntimeError> {
        if required > self.initialized.capacity() {
            return Err(RuntimeError::capacity_with(
                "snapshot initialized range capacity exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::GraphMetadata,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            ));
        }
        Ok(())
    }

    /// Invalidate logical contents while retaining the last native scope and
    /// revision. The owning GraphPool outlives every released provenance pin.
    pub(super) fn clear_contents(&mut self) {
        self.initialized.clear();
        self.provenance.clear();
    }

    /// Return a workspace to its initial state without replacing its backing.
    pub(super) fn reset(&mut self) {
        self.clear_contents();
        self.revision = 0;
        self.serial = 0;
        self.layout = vk::ImageLayout::UNDEFINED;
        self.stage = vk::PipelineStageFlags2::NONE;
        self.access = vk::AccessFlags2::NONE;
        self.writer_stage = vk::PipelineStageFlags2::NONE;
        self.writer_access = vk::AccessFlags2::NONE;
        self.read_stage = vk::PipelineStageFlags2::NONE;
        self.read_access = vk::AccessFlags2::NONE;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn set_native_state(snapshot: &mut Snapshot) {
        snapshot.revision = 13;
        snapshot.serial = 17;
        snapshot.layout = vk::ImageLayout::GENERAL;
        snapshot.stage = vk::PipelineStageFlags2::COPY;
        snapshot.access = vk::AccessFlags2::TRANSFER_READ;
        snapshot.writer_stage = vk::PipelineStageFlags2::COMPUTE_SHADER;
        snapshot.writer_access = vk::AccessFlags2::SHADER_WRITE;
        snapshot.read_stage = vk::PipelineStageFlags2::COPY;
        snapshot.read_access = vk::AccessFlags2::TRANSFER_READ;
    }

    fn assert_native_state(snapshot: &Snapshot) {
        assert_eq!(snapshot.revision, 13);
        assert_eq!(snapshot.serial, 17);
        assert_eq!(snapshot.layout, vk::ImageLayout::GENERAL);
        assert_eq!(snapshot.stage, vk::PipelineStageFlags2::COPY);
        assert_eq!(snapshot.access, vk::AccessFlags2::TRANSFER_READ);
        assert_eq!(
            snapshot.writer_stage,
            vk::PipelineStageFlags2::COMPUTE_SHADER
        );
        assert_eq!(snapshot.writer_access, vk::AccessFlags2::SHADER_WRITE);
        assert!(snapshot.has_read_visibility(
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ
        ));
    }

    #[test]
    fn copying_rejects_either_capacity_before_changing_the_destination() {
        let bytes: Arc<[u8]> = Arc::from([0u8; 12]);
        let mut source = Snapshot::with_capacity(2, 2, 4).unwrap();
        source.set_initialized(&[0..4, 8..12]).unwrap();
        source.provenance.write_slice(0..4, &bytes, 0..4).unwrap();
        source.provenance.write_slice(8..12, &bytes, 8..12).unwrap();
        for (range_capacity, segment_capacity) in [(1, 2), (2, 1)] {
            let mut target = Snapshot::with_capacity(range_capacity, segment_capacity, 4).unwrap();
            set_native_state(&mut target);
            target
                .set_initialized(std::slice::from_ref(&(0..12)))
                .unwrap();
            target.provenance.write_mesh_indices(0..12, 12, 2);
            assert_eq!(
                target.copy_from(&source).unwrap_err().kind(),
                RuntimeErrorKind::CapacityExhausted
            );
            assert_native_state(&target);
            assert_eq!(
                target.initialized.as_slice(),
                std::slice::from_ref(&(0..12))
            );
            assert_eq!(target.provenance.validate_index_range(0, 3, 3, 2), Ok(()));
        }
    }

    #[test]
    fn initialized_capacity_failure_keeps_existing_contents_and_native_state() {
        let mut state = Snapshot::with_capacity(1, 0, 4).unwrap();
        set_native_state(&mut state);
        state
            .set_initialized(std::slice::from_ref(&(0..12)))
            .unwrap();
        state.provenance.write_mesh_indices(0..12, 12, 2);
        assert_eq!(
            state.set_initialized(&[0..4, 8..12]).unwrap_err().kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(state.initialized.as_slice(), std::slice::from_ref(&(0..12)));
        assert_native_state(&state);
        assert_eq!(state.provenance.validate_index_range(0, 3, 3, 2), Ok(()));
    }

    #[test]
    fn copying_preserves_independent_contents_and_reuses_the_destination_backing() {
        let bytes: Arc<[u8]> = Arc::from([0u8; 12]);
        let patch: Arc<[u8]> = Arc::from([2u8]);
        let mut source = Snapshot::with_capacity(2, 3, 4).unwrap();
        let mut target = Snapshot::with_capacity(2, 3, 4).unwrap();
        let initialized_pointer = target.initialized.as_ptr();
        let initialized_capacity = target.initialized.capacity();
        let provenance_capacity = target.provenance.segment_capacity();
        set_native_state(&mut source);
        source
            .set_initialized(std::slice::from_ref(&(0..12)))
            .unwrap();
        source.provenance.write_slice(0..12, &bytes, 0..12).unwrap();
        assert_eq!(source.provenance.validate_index_range(0, 3, 1, 0), Ok(()));
        target.copy_from(&source).unwrap();
        assert_native_state(&target);
        assert_eq!(
            target.initialized.as_slice(),
            std::slice::from_ref(&(0..12))
        );
        source.set_initialized(&[]).unwrap();
        source.provenance.write_slice(0..1, &patch, 0..1).unwrap();
        assert_eq!(target.provenance.validate_index_range(0, 3, 1, 0), Ok(()));
        assert_eq!(target.provenance.test_evidence(), (1, 1));
        assert_eq!(target.initialized.as_ptr(), initialized_pointer);
        assert_eq!(target.initialized.capacity(), initialized_capacity);
        assert_eq!(target.provenance.segment_capacity(), provenance_capacity);
    }

    #[test]
    fn clearing_and_resetting_reuse_both_stores_and_keep_the_required_native_state() {
        let bytes: Arc<[u8]> = Arc::from([0u8; 12]);
        let mut state = Snapshot::with_capacity(2, 2, 4).unwrap();
        let initialized_pointer = state.initialized.as_ptr();
        let initialized_capacity = state.initialized.capacity();
        let provenance_capacity = state.provenance.segment_capacity();
        for _ in 0..100 {
            set_native_state(&mut state);
            state
                .set_initialized(std::slice::from_ref(&(0..12)))
                .unwrap();
            state.provenance.write_slice(0..12, &bytes, 0..12).unwrap();
            state.clear_contents();
            assert_native_state(&state);
            assert!(state.initialized.is_empty());
            assert_eq!(state.provenance.test_evidence(), (0, 0));
            state.reset();
            assert_eq!(state.revision, 0);
            assert_eq!(state.serial, 0);
            assert_eq!(state.layout, vk::ImageLayout::UNDEFINED);
            assert_eq!(state.stage, vk::PipelineStageFlags2::NONE);
            assert_eq!(state.access, vk::AccessFlags2::NONE);
            assert_eq!(state.writer_stage, vk::PipelineStageFlags2::NONE);
            assert_eq!(state.writer_access, vk::AccessFlags2::NONE);
            assert_eq!(state.read_stage, vk::PipelineStageFlags2::NONE);
            assert_eq!(state.read_access, vk::AccessFlags2::NONE);
            assert_eq!(state.initialized.as_ptr(), initialized_pointer);
            assert_eq!(state.initialized.capacity(), initialized_capacity);
            assert_eq!(state.provenance.segment_capacity(), provenance_capacity);
        }
        assert_eq!(Arc::strong_count(&bytes), 1);
    }

    #[test]
    fn impossible_initialized_capacity_is_a_preparation_error() {
        assert_eq!(
            Snapshot::with_capacity(usize::MAX, 0, 4)
                .err()
                .unwrap()
                .kind(),
            RuntimeErrorKind::Native
        );
    }
}
