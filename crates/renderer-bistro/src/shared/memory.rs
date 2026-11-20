//! Explicit diagnostic snapshots; never called inside the normal frame window.

use super::Error;
use serde_json::{Value, json};

#[cfg(feature = "vulkan")]
pub(crate) fn vulkan(runtime: &renderer::VulkanRuntime) -> Result<Value, Error> {
    let stats = runtime.memory_stats()?;
    let staging = |s: renderer::StagingMemoryStats| {
        json!({"capacity_bytes":s.capacity_bytes,"used_bytes":s.used_bytes,
            "reserved_bytes":s.reserved_bytes,"peak_reserved_bytes":s.peak_reserved_bytes,
            "live_ranges":s.live_ranges})
    };
    let heaps = stats.heaps[..stats.heap_count]
        .iter()
        .enumerate()
        .map(|(index, heap)| {
            json!({"index":index,"capacity_bytes":heap.capacity_bytes,"device_local":heap.device_local,
                "allocation_count":heap.vma_allocation_count,"allocation_bytes":heap.vma_allocation_bytes,
                "block_count":heap.vma_block_count,"block_bytes":heap.vma_block_bytes,
                "process_usage_bytes":heap.process_usage_bytes,"process_budget_bytes":heap.process_budget_bytes})
        })
        .collect::<Vec<_>>();
    Ok(json!({"api":"vulkan",
        "logical":{"registered_bytes":stats.registered_logical_bytes,"retained_bytes":stats.retained_logical_bytes,
            "cached_bytes":stats.cached_logical_bytes},
        "native":{"allocation_count":stats.vma_allocation_count,"allocation_bytes":stats.vma_allocation_bytes,
            "block_count":stats.vma_block_count,"block_bytes":stats.vma_block_bytes},
        "upload":staging(stats.upload),"readback":staging(stats.readback),"heaps":heaps,
        "memory_budget_extension_enabled":stats.memory_budget_extension_enabled,
        "process_usage_bytes":stats.process_usage_bytes,"process_budget_bytes":stats.process_budget_bytes,
        "granularity":{"buffer_image_bytes":stats.buffer_image_granularity,
            "non_coherent_atom_bytes":stats.non_coherent_atom_size},
        "scope":"VMA allocations/blocks include this Runtime allocator; packed logical bytes exclude tiling/alignment; EXT process usage/budget are driver estimates, not guaranteed physical page residency or future allocation success"}))
}

#[cfg(all(windows, feature = "d3d12"))]
pub(crate) fn d3d12(runtime: &renderer::D3D12Runtime) -> Result<Value, Error> {
    let stats = runtime.memory_stats()?;
    let heap = |s: renderer::D3D12CommittedHeapStats| {
        json!({"resource_count":s.resource_count,"allocation_bytes":s.allocation_bytes,
            "alignment_4k_count":s.alignment_4k_count,"alignment_64k_count":s.alignment_64k_count,
            "alignment_4m_count":s.alignment_4m_count,"alignment_other_count":s.alignment_other_count})
    };
    let native = if let Some(s) = stats.committed {
        if s.overflowed {
            return Err("D3D12 native memory counters overflowed".into());
        }
        let count = s.default_heap.resource_count
            + s.upload_heap.resource_count
            + s.readback_heap.resource_count;
        let bytes = s.default_heap.allocation_bytes
            + s.upload_heap.allocation_bytes
            + s.readback_heap.allocation_bytes;
        json!({"allocation_count":count,"allocation_bytes":bytes,"block_count":count,"block_bytes":bytes,
            "default_heap":heap(s.default_heap),"upload_heap":heap(s.upload_heap),"readback_heap":heap(s.readback_heap)})
    } else {
        Value::Null
    };
    let process = |s: renderer::D3D12VideoMemoryInfo| {
        json!({"process_usage_bytes":s.current_usage_bytes,"process_budget_bytes":s.budget_bytes,
            "available_for_reservation_bytes":s.available_for_reservation_bytes,"current_reservation_bytes":s.current_reservation_bytes})
    };
    let staging = |s: renderer::D3D12StagingMemoryStats| {
        json!({"capacity_bytes":s.capacity_bytes,"slots":s.slots,"bytes_per_slot":s.bytes_per_slot,
            "used_bytes":s.used_bytes,"peak_used_bytes":s.peak_used_bytes})
    };
    Ok(json!({"api":"d3d12","native":native,
        "logical":{"registered_bytes":stats.registered_logical_bytes,"retained_bytes":stats.retained_logical_bytes,
            "cached_bytes":stats.cached_logical_bytes},
        "upload":staging(stats.upload),"readback":staging(stats.readback),
        "process_local":stats.process_local.map(process),"process_non_local":stats.process_non_local.map(process),
        "process_usage_bytes":stats.process_local.map(|s|s.current_usage_bytes),
        "process_budget_bytes":stats.process_local.map(|s|s.budget_bytes),
        "scope":"native committed resource requirement bytes include one implicit heap per resource on this Device; exclude descriptor/pipeline/driver allocations; DXGI process segment usage/budget are independent OS accounting, not guaranteed physical page residency; live staging byte high-water is unavailable"}))
}

/// Compare owned allocation sizes after equivalent drained change boundaries.
/// Dynamic OS usage/budget and staging high-water are deliberately independent.
pub(crate) fn plateau(snapshots: &[Value]) -> Result<Value, Error> {
    let first = snapshots.first().ok_or("missing memory plateau samples")?;
    if snapshots.len() < 3 {
        return Err("memory plateau needs at least three completed replacement cycles".into());
    }
    let fields = [
        "allocation_count",
        "allocation_bytes",
        "block_count",
        "block_bytes",
    ];
    for snapshot in snapshots {
        if snapshot["api"] != first["api"] {
            return Err("memory plateau mixed backend identities".into());
        }
        for field in fields {
            let value = snapshot["native"][field]
                .as_u64()
                .ok_or("native memory accounting unavailable; build with memory-profiling")?;
            if value == 0 || snapshot["native"][field] != first["native"][field] {
                return Err(
                    format!("native {field} changed across drained replacement cycles").into(),
                );
            }
        }
        for field in ["registered_bytes", "retained_bytes", "cached_bytes"] {
            if snapshot["logical"][field].as_u64().is_none()
                || snapshot["logical"][field] != first["logical"][field]
            {
                return Err(
                    format!("logical {field} changed across drained replacement cycles").into(),
                );
            }
        }
    }
    Ok(
        json!({"passed":true,"samples":snapshots.len(),"native":first["native"],
        "logical":first["logical"],
        "scope":"equal native allocation/block counts and bytes, plus registered/retained/cached logical bytes after each drained replacement cycle; OS budget/usage may change independently"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({"api":"vulkan","native":{"allocation_count":3,"allocation_bytes":400,
            "block_count":2,"block_bytes":512},"logical":{"registered_bytes":300,"retained_bytes":0,
            "cached_bytes":20},"process_usage_bytes":800})
    }

    #[test]
    fn detects_retained_or_native_growth_without_treating_os_usage_as_a_leak() {
        let mut samples = vec![sample(); 3];
        samples[2]["process_usage_bytes"] = json!(900);
        assert!(plateau(&samples).is_ok());
        samples[2]["native"]["allocation_bytes"] = json!(401);
        assert!(plateau(&samples).is_err());
        samples[2] = sample();
        samples[2]["logical"]["retained_bytes"] = json!(4);
        assert!(plateau(&samples).is_err());
        samples[2] = sample();
        samples[2]["native"] = Value::Null;
        assert!(plateau(&samples).is_err());
        assert!(plateau(&samples[..1]).is_err());
    }
}
