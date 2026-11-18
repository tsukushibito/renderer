/// Identity of one accepted submission, scoped to its originating runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct FrameId {
    pub(crate) owner: u64,
    serial: u64,
}

impl FrameId {
    pub(crate) fn new(owner: u64, serial: u64) -> Self {
        Self { owner, serial }
    }
}
