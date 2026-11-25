/// Identity of one accepted submission, scoped to its originating runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct FrameId {
    pub(crate) owner: u64,
    serial: u64,
}

#[cfg(sashiko_gpu_backend)]
impl FrameId {
    pub(crate) fn new(owner: u64, serial: u64) -> Self {
        Self { owner, serial }
    }
}
