//! Retain Graph leases and publish source state around native present commands.

use super::*;

pub(super) struct PresentWork {
    pub(super) source: PhysicalLease,
    pub(super) previous: Snapshot,
    pub(super) next: Option<Snapshot>,
    pub(super) serial: u64,
    pub(super) commands: PresentCommands,
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
            self.commands.record(
                command,
                self.source.image().image,
                SourceScope {
                    stage: self.previous.stage | self.previous.writer_stage,
                    access: self.previous.access | self.previous.writer_access,
                    layout: self.previous.layout,
                },
            );
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
