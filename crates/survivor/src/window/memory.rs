//! Explicit snapshots at frame boundaries. Native budget estimates are not residency.
use renderer::{WindowRenderer, pipeline::PipelineResult};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};
use survivor::Game;

pub(super) struct Snapshots<'a> {
    path: Option<&'a Path>,
    entries: Vec<Value>,
}

impl<'a> Snapshots<'a> {
    pub fn start(
        path: Option<&'a Path>,
        renderer: &mut WindowRenderer,
        game: &Game,
    ) -> PipelineResult<Self> {
        let mut entries = Self {
            path,
            entries: Vec::new(),
        };
        entries.observe("prepared", renderer, game)?;
        Ok(entries)
    }

    pub fn observe(
        &mut self,
        stage: &str,
        renderer: &mut WindowRenderer,
        game: &Game,
    ) -> PipelineResult<()> {
        if self.path.is_none() {
            return Ok(());
        }
        let runtime = renderer.runtime_info();
        let limits = runtime.limits();
        let mut native = Value::Null;
        #[cfg(feature = "vulkan")]
        if let Some(vk) = runtime.as_vulkan() {
            let m = vk.memory_stats()?;
            native = json!({
                "backend":"vulkan", "registered_bytes":m.registered_logical_bytes,
                "retained_bytes":m.retained_logical_bytes, "cached_bytes":m.cached_logical_bytes,
                "vma_allocations":m.vma_allocation_count, "vma_allocation_bytes":m.vma_allocation_bytes,
                "vma_blocks":m.vma_block_count, "vma_block_bytes":m.vma_block_bytes,
                "device_local_process_usage_bytes":m.process_usage_bytes, "device_local_budget_bytes":m.process_budget_bytes,
                "upload_capacity_bytes":m.upload.capacity_bytes, "upload_used_bytes":m.upload.used_bytes,
                "upload_peak_reserved_bytes":m.upload.peak_reserved_bytes, "readback_capacity_bytes":m.readback.capacity_bytes,
                "readback_used_bytes":m.readback.used_bytes, "readback_peak_reserved_bytes":m.readback.peak_reserved_bytes
            });
        }
        #[cfg(feature = "d3d12")]
        if let Some(dx) = runtime.as_d3d12() {
            let m = dx.memory_stats()?;
            native = json!({
                "backend":"d3d12", "registered_bytes":m.registered_logical_bytes,
                "retained_bytes":m.retained_logical_bytes, "cached_bytes":m.cached_logical_bytes,
                "device_local_process_usage_bytes":m.process_local.map(|h|h.current_usage_bytes),
                "device_local_budget_bytes":m.process_local.map(|h|h.budget_bytes),
                "upload_capacity_bytes":m.upload.capacity_bytes, "readback_capacity_bytes":m.readback.capacity_bytes
            });
        }
        self.entries.push(json!({
            "stage":stage, "tick":game.tick, "native":native,
            "prepared": {"frame_slots":limits.frames,"result_slots":limits.results,"resource_slots":limits.resource_capacity,
                "graph_resources": limits.graph.resources, "graph_passes": limits.graph.passes,
                "graph_draws":limits.graph.draws, "graph_accesses":limits.graph.accesses},
            "game": {"enemies":[game.enemies.len(),game.enemies.capacity()],
                "shots":[game.shots.len(),game.shots.capacity()], "drops":[game.drops.len(),game.drops.capacity()]}
        }));
        Ok(())
    }

    pub fn save(self) -> PipelineResult<()> {
        if let Some(path) = self.path {
            let mut writer = BufWriter::new(File::create(path)?);
            serde_json::to_writer_pretty(&mut writer, &self.entries)?;
            writer.flush()?;
        }
        Ok(())
    }
}
