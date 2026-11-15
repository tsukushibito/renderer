//! Bounded prepared-frame scratch, retained independently of GPU execution slots.

use std::sync::{Arc, Mutex};

use super::{EngineError, MeshHandle, MeshPreparationError, ResidentBuffers};
use crate::graph::BufferHandle;

pub(super) struct FrameScratch {
    pub(super) resolved: Vec<(MeshHandle, (BufferHandle, BufferHandle))>,
    pub(super) pins: Vec<Arc<ResidentBuffers>>,
}

#[derive(Clone)]
pub(super) struct FrameScratchPool(Arc<Mutex<Vec<FrameScratch>>>);

impl FrameScratchPool {
    pub(super) fn new(meshes: usize, slots: usize) -> Result<Self, EngineError> {
        if slots == 0 {
            return Err(EngineError::new("prepared frame capacity must be nonzero"));
        }
        let mut idle = Vec::new();
        idle.try_reserve_exact(slots)
            .map_err(|_| EngineError::new("unable to reserve prepared frame slots"))?;
        for _ in 0..slots {
            let mut resolved = Vec::new();
            let mut pins = Vec::new();
            resolved
                .try_reserve_exact(meshes)
                .map_err(|_| EngineError::new("unable to reserve resolved mesh scratch"))?;
            pins.try_reserve_exact(meshes)
                .map_err(|_| EngineError::new("unable to reserve prepared mesh pins"))?;
            idle.push(FrameScratch { resolved, pins });
        }
        Ok(Self(Arc::new(Mutex::new(idle))))
    }

    pub(super) fn acquire(&self) -> Result<FrameScratchLease, MeshPreparationError> {
        let scratch = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop()
            .ok_or(MeshPreparationError::FrameCapacityExhausted)?;
        Ok(FrameScratchLease {
            pool: self.clone(),
            scratch: Some(scratch),
        })
    }
}

pub(super) struct FrameScratchLease {
    pool: FrameScratchPool,
    scratch: Option<FrameScratch>,
}

impl FrameScratchLease {
    pub(super) fn scratch(&mut self) -> &mut FrameScratch {
        self.scratch
            .as_mut()
            .expect("active prepared frame scratch")
    }
}

impl Drop for FrameScratchLease {
    fn drop(&mut self) {
        if let Some(mut scratch) = self.scratch.take() {
            scratch.resolved.clear();
            scratch.pins.clear();
            // Every lease originated by popping this bounded collection. Its
            // backing survives cancellation, submission and prepared-frame Drop.
            self.pool
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(scratch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ResourceKey;

    #[test]
    fn cancelled_prepared_slots_return_capacity_and_release_mesh_pins() {
        let pool = FrameScratchPool::new(2, 2).unwrap();
        let mut first = pool.acquire().unwrap();
        let second = pool.acquire().unwrap();
        assert!(matches!(
            pool.acquire(),
            Err(MeshPreparationError::FrameCapacityExhausted)
        ));
        let buffers = Arc::new(ResidentBuffers {
            vertices: ResourceKey {
                owner: 1,
                index: 0,
                generation: 1,
            },
            indices: ResourceKey {
                owner: 1,
                index: 1,
                generation: 1,
            },
        });
        first.scratch().pins.push(Arc::clone(&buffers));
        assert_eq!(Arc::strong_count(&buffers), 2);
        drop(first);
        assert_eq!(Arc::strong_count(&buffers), 1);
        let mut reused = pool.acquire().unwrap();
        assert!(reused.scratch().pins.is_empty());
        drop(second);
        assert!(pool.acquire().is_ok());
    }
}
