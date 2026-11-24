//! On-demand telemetry for presented games. Memory queries may allocate and belong at explicit observation boundaries.
use renderer::{
    WindowRenderer,
    core::{
        profiling::{GpuTiming, SubmissionMetrics},
        runtime::Runtime,
    },
    pipeline::PipelineResult,
};
use serde_json::{Value, json};

/// Logical capacity, native allocation and OS budget observations for the active backend.
pub fn renderer_memory(runtime: &Runtime) -> PipelineResult<Value> {
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
    Ok(json!({"native":native,
        "prepared":{"frame_slots":limits.frames,"result_slots":limits.results,"resource_slots":limits.resource_capacity,
           "graph_resources":limits.graph.resources,"graph_passes":limits.graph.passes,
           "graph_draws":limits.graph.draws,"graph_accesses":limits.graph.accesses}}))
}

/// Current and lifetime peak Windows process working set at the instant of this call.
pub fn working_set() -> std::io::Result<Value> {
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    };
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: The current process pseudo-handle is always valid. The output size
    // matches the writable native struct for this synchronous call.
    if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(
        json!({"working_set_bytes":counters.WorkingSetSize, "peak_working_set_bytes":counters.PeakWorkingSetSize}),
    )
}

/// Matched graph and presentation profiles from a fixed-length, successfully presented sequence.
pub struct WindowTimings {
    graph: Vec<SubmissionMetrics>,
    presentation: Vec<GpuTiming>,
}

impl WindowTimings {
    pub fn new(total_frames: usize) -> Self {
        Self {
            graph: Vec::with_capacity(total_frames),
            presentation: Vec::with_capacity(total_frames),
        }
    }

    pub fn poll(&mut self, renderer: &mut WindowRenderer) {
        if let Some(graph) = renderer.take_graph_profile() {
            self.graph.push(graph);
        }
        while let Some(present) = renderer.take_presentation_profile() {
            self.presentation.push(present.gpu);
        }
    }

    /// Call after draining both the renderer and presenter.
    pub fn finish(
        self,
        presented: usize,
    ) -> PipelineResult<impl Iterator<Item = (SubmissionMetrics, GpuTiming)>> {
        if self.graph.len() != presented || self.presentation.len() != presented {
            return Err(format!("incomplete timestamp sequence: {} graphs, {} presentations, {presented} accepted frames",
                self.graph.len(), self.presentation.len()).into());
        }
        Ok(self.graph.into_iter().zip(self.presentation))
    }
}
