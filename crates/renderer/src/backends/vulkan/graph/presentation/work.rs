//! Retain Graph leases and publish source state around native present commands.

use super::*;

pub(super) struct PresentWork {
    pub(super) source: PhysicalLease,
    pub(super) previous: Option<SourceScope>,
    pub(super) next: Option<Snapshot>,
    pub(super) serial: u64,
    pub(super) commands: PresentCommands,
}

pub(super) fn prepare_source(mut next: Snapshot, serial: u64) -> (Option<SourceScope>, Snapshot) {
    let stage = vk::PipelineStageFlags2::FRAGMENT_SHADER;
    let access = vk::AccessFlags2::SHADER_SAMPLED_READ;
    let layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
    // A prior same-queue barrier exposes the writer to every later command in
    // this exact read scope. New writes/scopes/layouts require a dependency.
    // Keep the last writer in the moved Snapshot for those future consumers.
    let previous = if next.layout == layout && next.has_read_visibility(stage, access) {
        None
    } else {
        Some(SourceScope {
            stage: next.stage | next.writer_stage,
            access: next.access | next.writer_access,
            layout: next.layout,
        })
    };
    next.serial = serial;
    next.stage = stage;
    next.access = access;
    next.layout = layout;
    (previous, next)
}
impl TransferPayload for PresentWork {
    fn owner(&self) -> &Arc<MemoryOwner> {
        self.commands.owner()
    }
    fn validate(&self) -> Result<(), TransferError> {
        Ok(())
    }
    fn bytes(&self) -> u64 {
        0
    }
    fn label(&self) -> &'static std::ffi::CStr {
        c"Graph linear output -> sRGB presentation"
    }
    unsafe fn record(&self, command: vk::CommandBuffer) {
        // SAFETY: This payload retains the physical source lease and native
        // commands throughout recorded/pending work. Canonical state was read
        // under the runtime transaction before this recording was prepared.
        unsafe {
            self.commands
                .record(command, self.source.image().image, self.previous);
        }
    }
    fn submitted(&mut self) {
        self.commands.submitted();
        if let Some(next) = self.next.take() {
            self.source.lock().scheduled = next;
        }
    }
    unsafe fn establish_contents(&mut self) {
        let mut state = self.source.lock();
        state.completed_serial = state.completed_serial.max(self.serial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_reuses_only_exact_read_visibility_and_preserves_contents() {
        let initial = Snapshot {
            revision: 7,
            serial: 7,
            initialized: std::iter::once(0..1).collect(),
            layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            stage: vk::PipelineStageFlags2::COPY,
            access: vk::AccessFlags2::TRANSFER_WRITE,
            writer_stage: vk::PipelineStageFlags2::COPY,
            writer_access: vk::AccessFlags2::TRANSFER_WRITE,
            ..Snapshot::default()
        };
        let (first, next) = prepare_source(initial, 8);
        let first = first.expect("initial presentation must expose the writer");
        assert_eq!(first.stage, vk::PipelineStageFlags2::COPY);
        assert_eq!(first.access, vk::AccessFlags2::TRANSFER_WRITE);
        assert_eq!(first.layout, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        let (repeated, next) = prepare_source(next, 9);
        assert!(repeated.is_none());
        assert_eq!(next.revision, 7);
        assert_eq!(next.serial, 9);
        assert_eq!(next.initialized.len(), 1);
        assert_eq!(next.initialized[0], 0..1);
        assert_eq!(next.writer_stage, vk::PipelineStageFlags2::COPY);
        assert_eq!(next.writer_access, vk::AccessFlags2::TRANSFER_WRITE);

        for changed in [
            Snapshot {
                stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
                ..next.clone()
            },
            Snapshot {
                access: vk::AccessFlags2::SHADER_STORAGE_READ,
                ..next.clone()
            },
            Snapshot {
                layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                ..next.clone()
            },
            Snapshot {
                revision: 10,
                stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
                access: vk::AccessFlags2::SHADER_STORAGE_WRITE,
                writer_stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
                writer_access: vk::AccessFlags2::SHADER_STORAGE_WRITE,
                ..next
            },
        ] {
            assert!(
                prepare_source(changed, 11).0.is_some(),
                "new reader scopes, layouts and writes still need a dependency"
            );
        }
    }
}
