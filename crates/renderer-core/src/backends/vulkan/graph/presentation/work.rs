//! Retain Graph leases and publish source state around native present commands.

use super::*;

pub(super) struct PresentWork {
    pub(super) source: PhysicalLease,
    pub(super) previous: Option<SourceScope>,
    pub(super) serial: u64,
    pub(super) commands: PresentCommands,
    pub(super) profile: Option<PresentProfile>,
}

pub(super) fn prepare_source(previous: &Snapshot) -> Option<SourceScope> {
    let stage = vk::PipelineStageFlags2::FRAGMENT_SHADER;
    let access = vk::AccessFlags2::SHADER_SAMPLED_READ;
    let layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
    // A prior same-queue barrier exposes the writer to every later command in
    // this exact read scope. New writes/scopes/layouts require a dependency.
    // Keep the last writer in canonical state for those future consumers.
    if previous.layout == layout && previous.has_read_visibility(stage, access) {
        None
    } else {
        Some(SourceScope {
            stage: previous.stage | previous.writer_stage | previous.layout_transition_stage,
            access: previous.access | previous.writer_access,
            layout: previous.layout,
        })
    }
}

fn commit_source(state: &mut Snapshot, serial: u64) {
    // Presentation changes only scheduled native read scope. Mutating these
    // scalars preserves content metadata and its reserved backing allocation.
    let layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
    state.record_read(
        vk::PipelineStageFlags2::FRAGMENT_SHADER,
        vk::AccessFlags2::SHADER_SAMPLED_READ,
        state.layout != layout,
    );
    state.serial = serial;
    state.layout = layout;
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
            if let Some(profile) = &self.profile {
                profile.lease.begin(command);
            }
            self.commands
                .record(command, self.source.image().image, self.previous);
            if let Some(profile) = &self.profile {
                profile.lease.end(command);
            }
        }
    }
    fn submitted(&mut self) {
        self.commands.submitted();
        commit_source(&mut self.source.lock().scheduled, self.serial);
    }
    unsafe fn establish_contents(&mut self) {
        if let Some(profile) = &mut self.profile {
            profile.retire_started = Some(Instant::now());
        }
        let mut state = self.source.lock();
        state.completed_serial = state.completed_serial.max(self.serial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_reuses_only_exact_read_visibility_and_preserves_contents() {
        let mut next = Snapshot {
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
        let first = prepare_source(&next).expect("initial presentation must expose the writer");
        assert_eq!(first.stage, vk::PipelineStageFlags2::COPY);
        assert_eq!(first.access, vk::AccessFlags2::TRANSFER_WRITE);
        assert_eq!(first.layout, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        let initialized_storage = next.initialized.as_ptr();
        commit_source(&mut next, 8);
        assert_eq!(
            next.layout_transition_stage,
            vk::PipelineStageFlags2::FRAGMENT_SHADER
        );
        assert!(prepare_source(&next).is_none());
        commit_source(&mut next, 9);
        assert_eq!(next.initialized.as_ptr(), initialized_storage);
        assert_eq!(next.revision, 7);
        assert_eq!(next.serial, 9);
        assert_eq!(next.initialized.len(), 1);
        assert_eq!(next.initialized[0], 0..1);
        assert_eq!(next.writer_stage, vk::PipelineStageFlags2::COPY);
        assert_eq!(next.writer_access, vk::AccessFlags2::TRANSFER_WRITE);

        for changed in [
            Snapshot {
                stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
                read_stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
                ..next.clone()
            },
            Snapshot {
                access: vk::AccessFlags2::SHADER_STORAGE_READ,
                read_access: vk::AccessFlags2::SHADER_STORAGE_READ,
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
                read_stage: vk::PipelineStageFlags2::NONE,
                read_access: vk::AccessFlags2::NONE,
                ..next
            },
        ] {
            assert!(
                prepare_source(&changed).is_some(),
                "new reader scopes, layouts and writes still need a dependency"
            );
        }
    }

    #[test]
    fn elided_presentation_dependency_keeps_prior_reader_scopes_for_later_writes() {
        let mut next = Snapshot {
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            writer_stage: vk::PipelineStageFlags2::COPY,
            writer_access: vk::AccessFlags2::TRANSFER_WRITE,
            ..Snapshot::default()
        };
        next.record_read(
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ,
            false,
        );
        next.record_read(
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
            false,
        );
        assert!(prepare_source(&next).is_none());
        commit_source(&mut next, 8);
        assert!(next.stage.contains(
            vk::PipelineStageFlags2::COMPUTE_SHADER | vk::PipelineStageFlags2::FRAGMENT_SHADER
        ));
        assert!(next.access.contains(
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_SAMPLED_READ
        ));
        assert_eq!(next.writer_stage, vk::PipelineStageFlags2::COPY);
        assert_eq!(next.writer_access, vk::AccessFlags2::TRANSFER_WRITE);
    }
}
