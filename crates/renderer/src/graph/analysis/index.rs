//! Niche-encoded references into prepared arrays.

use std::num::NonZeroUsize;

/// One-based storage keeps `Option<Index>` one machine word. Callers construct
/// indices only for actual elements of non-ZST prepared arrays, whose byte
/// length is bounded by `isize::MAX`; they can never reach `usize::MAX`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct Index(NonZeroUsize);

impl Index {
    pub fn new(index: usize) -> Self {
        Self(
            NonZeroUsize::new(index.checked_add(1).expect("prepared array index overflow"))
                .expect("one-based prepared index is nonzero"),
        )
    }

    pub fn get(self) -> usize {
        self.0.get() - 1
    }
}
