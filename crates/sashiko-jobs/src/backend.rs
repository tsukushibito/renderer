#[cfg(not(target_arch = "wasm32"))]
use super::native;
#[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
use super::scheduler;
#[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
use super::web;
use std::{marker::PhantomData, num::NonZeroUsize};

pub(super) enum Backend {
    Inline,
    #[cfg(not(target_arch = "wasm32"))]
    Workers(native::Workers),
    #[cfg(all(target_arch = "wasm32", feature = "web-threads"))]
    Workers(web::Workers),
    #[cfg(all(not(target_arch = "wasm32"), feature = "experimental-rayon"))]
    Rayon(rayon::ThreadPool),
    #[cfg(all(
        target_arch = "wasm32",
        feature = "web-threads",
        feature = "experimental-rayon"
    ))]
    Rayon(web::RayonWorkers),
}

#[derive(Clone, Copy)]
pub(super) enum Team<'env> {
    Inline(PhantomData<&'env ()>),
    #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
    Workers(&'env scheduler::Scheduler),
    #[cfg(all(
        feature = "experimental-rayon",
        any(not(target_arch = "wasm32"), feature = "web-threads")
    ))]
    Rayon(&'env rayon::ThreadPool),
}

impl Backend {
    pub(super) fn team(&self) -> Team<'_> {
        match self {
            Self::Inline => Team::Inline(PhantomData),
            #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
            Self::Workers(workers) => Team::Workers(&workers.scheduler),
            #[cfg(all(not(target_arch = "wasm32"), feature = "experimental-rayon"))]
            Self::Rayon(pool) => Team::Rayon(pool),
            #[cfg(all(
                target_arch = "wasm32",
                feature = "web-threads",
                feature = "experimental-rayon"
            ))]
            Self::Rayon(workers) => Team::Rayon(&workers.pool),
        }
    }
}

impl Team<'_> {
    pub(super) fn parallelism(self) -> NonZeroUsize {
        match self {
            Self::Inline(_) => NonZeroUsize::MIN,
            #[cfg(any(not(target_arch = "wasm32"), feature = "web-threads"))]
            Self::Workers(pool) => NonZeroUsize::new(pool.helpers + 1).unwrap(),
            #[cfg(all(
                feature = "experimental-rayon",
                any(not(target_arch = "wasm32"), feature = "web-threads")
            ))]
            Self::Rayon(pool) => NonZeroUsize::new(pool.current_num_threads() + 1).unwrap(),
        }
    }
}
