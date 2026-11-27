use super::{Bounds, SceneInput, classification::Visibility, history::CasterChanges};
use crate::{
    passes::virtual_shadow::{ATLAS_DESC, CacheTargets, SLOT_BYTES},
    pbr::{MapFrame, MapHistory, MapUpdates, ShadowHistoryError, ShadowKey, ShadowSettings},
};
use sashiko_renderer_core::{
    graph::{BufferDesc, BufferHandle, Graph, GraphError, ImportBuffer},
    runtime::{FrameSubmission, Runtime},
};

pub(super) struct History {
    pub maps: MapHistory<1>,
    owners: Option<ImportBuffer>,
    casters: CasterChanges,
}
pub(super) struct Frame<'a> {
    map: MapFrame<1>,
    pub targets: CacheTargets<'a>,
}
pub(super) struct Updates {
    pub map: MapUpdates<1>,
    owners: BufferHandle,
}
impl Frame<'_> {
    pub fn export(self, graph: &mut Graph) -> Result<Updates, GraphError> {
        graph.export_buffer(self.targets.owners)?;
        Ok(Updates {
            map: self.map.export(graph)?,
            owners: self.targets.owners,
        })
    }
}
impl History {
    pub fn new(max: usize) -> Self {
        Self {
            maps: MapHistory::default(),
            owners: None,
            casters: CasterChanges::new(max),
        }
    }
    pub fn begin(
        &mut self,
        graph: &mut Graph,
        input: SceneInput<'_>,
        results: &[Visibility],
        bounds: Bounds,
        settings: ShadowSettings,
    ) -> Result<Frame<'_>, GraphError> {
        self.maps.require_idle()?;
        self.casters.observe(graph, input, results)?;
        let mut projection = [[0.; 4]; 4];
        // These exact inputs determine the light axes and D32 normalisation.
        // Camera movement only changes virtual addresses within each world grid.
        projection[0][..3].copy_from_slice(&input.sun.direction());
        projection[0][3] = settings.depth_padding;
        projection[1][..3].copy_from_slice(&bounds.min());
        projection[2][..3].copy_from_slice(&bounds.max());
        let mut map = self.maps.begin_maps(
            graph,
            ATLAS_DESC,
            ["directional virtual shadow atlas"],
            [ShadowKey::new(projection, 0)],
        )?;
        let reuse = map.is_reused(0)
            && self.owners.is_some()
            && !self.casters.changed().iter().any(Option::is_none);
        let owners = match &self.owners {
            Some(old) => graph.import_buffer("persistent virtual shadow owners", old)?,
            None => graph.buffer(
                "persistent virtual shadow owners",
                BufferDesc {
                    size: SLOT_BYTES as u64,
                },
            )?,
        };
        let targets = CacheTargets {
            atlas: map.target(0),
            owners,
            reuse,
            changed: self.casters.changed(),
        };
        map.mark_written(0);
        Ok(Frame { map, targets })
    }
    pub fn accept(
        &mut self,
        receipt: &FrameSubmission,
        updates: &mut Updates,
    ) -> Result<(), ShadowHistoryError> {
        self.maps.check_updates(receipt.graph_id(), &updates.map)?;
        let owners = receipt
            .export_buffer(updates.owners)
            .map_err(ShadowHistoryError::Execution)?;
        self.maps.accept(receipt, &mut updates.map)?;
        self.owners = Some(owners);
        self.casters.accept();
        Ok(())
    }
    pub fn clear(&mut self, runtime: &mut Runtime) -> Result<(), ShadowHistoryError> {
        self.maps.clear(runtime)?;
        if let Some(owners) = &self.owners {
            runtime
                .delete(owners.key())
                .map_err(ShadowHistoryError::Execution)?;
            self.owners = None;
        }
        Ok(())
    }
}
