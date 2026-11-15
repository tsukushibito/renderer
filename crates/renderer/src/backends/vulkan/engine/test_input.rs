//! Shared bounded input setup for native Engine acceptance scenarios.

use std::ops::{Deref, DerefMut};

use crate::{
    engine::{Draw, Engine, EngineCapacity, EngineError, FrameSnapshot, SnapshotPool, View},
    graph::{Graph, GraphCapacity, GraphPool},
};

pub(crate) struct Assets {
    engine: Engine,
    snapshots: SnapshotPool,
}

impl Assets {
    pub(crate) fn new(meshes: usize, materials: usize) -> Result<Self, EngineError> {
        Ok(Self {
            engine: Engine::new(EngineCapacity {
                meshes,
                materials,
                retired_mesh_versions: 32,
            })?,
            snapshots: SnapshotPool::new(256, 32)?,
        })
    }

    pub(crate) fn snapshot(
        &self,
        view: &View,
        draws: &[Draw],
    ) -> Result<FrameSnapshot, EngineError> {
        self.engine.snapshot(&self.snapshots, view, draws)
    }
}

impl Deref for Assets {
    type Target = Engine;

    fn deref(&self) -> &Self::Target {
        &self.engine
    }
}

impl DerefMut for Assets {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.engine
    }
}

pub(crate) fn graph() -> Graph {
    GraphPool::new(GraphCapacity::default(), 1)
        .unwrap()
        .begin()
        .unwrap()
}
