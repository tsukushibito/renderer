//! Optional text diagnostics, built from the same barriers that are recorded.
//! No native state or resource ownership escapes with the returned string.

use std::fmt::Write;

use ash::vk;

use super::{NativeResource, PhysicalLease, Snapshot, Use};
use crate::graph::{CompiledGraph, Pass, ResourceKey};

pub(super) fn begin(
    graph: &CompiledGraph,
    serial: u64,
    queue_family: u32,
    keys: &[Option<ResourceKey>],
    physical: &[Option<PhysicalLease>],
) -> String {
    let mut output = format!(
        "Vulkan Graph lowering graph={} submission={serial} queue_family={queue_family} queue_index=0\n",
        graph.id
    );
    for (index, resource) in graph.resources.iter().enumerate() {
        let Some(physical) = &physical[index] else {
            continue;
        };
        let key = keys[index].expect("materialized resource key");
        writeln!(
            output,
            "resource {index} {:?} physical_key owner={} index={} generation={} desc={:?} imported={} import_revision={}",
            resource.name, key.owner, key.index, key.generation, resource.desc,
            resource.import.is_some(), resource.import_revision,
        )
        .expect("writing String");
        match &physical.native {
            NativeResource::Buffer(buffer) => writeln!(
                output,
                "  native_buffer={:?} bytes={}",
                buffer.handle, buffer.size,
            ),
            NativeResource::Image(image) => writeln!(
                output,
                "  native_image={:?} native_view={:?} aspect={:?}",
                image.image, image.view, image.aspect,
            ),
        }
        .expect("writing String");
        state(&mut output, "initial_scheduled", &physical.lock().scheduled);
    }
    output
}

fn state(output: &mut String, label: &str, snapshot: &Snapshot) {
    writeln!(
        output,
        "  {label} revision={} initialized={:?} stage={:?} access={:?} layout={:?} writer_stage={:?} writer_access={:?}",
        snapshot.revision, snapshot.initialized, snapshot.stage, snapshot.access,
        snapshot.layout, snapshot.writer_stage, snapshot.writer_access,
    )
    .expect("writing String");
}

pub(super) fn pass(output: &mut String, index: usize, pass: &Pass) {
    writeln!(output, "pass {index} {:?}", pass.name).expect("writing String");
    for access in &pass.accesses {
        writeln!(
            output,
            "  use resource={} range={:?} purpose={:?} read={} write={}",
            access.resource, access.range, access.purpose, access.read, access.write,
        )
        .expect("writing String");
    }
}

fn reason(previous: &Snapshot, current: Use, layout_change: bool, emitted: bool) -> &'static str {
    if !emitted {
        "no prior writer or layout change requiring a dependency"
    } else if layout_change
        && previous.layout == vk::ImageLayout::UNDEFINED
        && (previous.stage | previous.writer_stage).is_empty()
    {
        "initial image layout; prior COPY execution ordering workaround renderer-j17"
    } else if layout_change {
        "prior accesses and writer before image layout transition"
    } else if current.writes {
        "prior accesses and writer before write"
    } else {
        "last writer visibility to this reader"
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn dependency(
    output: &mut String,
    resource: usize,
    previous: &Snapshot,
    current: Use,
    layout_change: bool,
    buffer: Option<&vk::BufferMemoryBarrier2<'_>>,
    image: Option<&vk::ImageMemoryBarrier2<'_>>,
) {
    let emitted = buffer.is_some() || image.is_some();
    writeln!(
        output,
        "  dependency resource={resource} emitted={emitted} reason={}",
        reason(previous, current, layout_change, emitted),
    )
    .expect("writing String");
    if let Some(barrier) = buffer {
        writeln!(
            output,
            "    buffer={:?} offset={} size={} src_stage={:?} src_access={:?} dst_stage={:?} dst_access={:?} src_family={} dst_family={}",
            barrier.buffer, barrier.offset, barrier.size, barrier.src_stage_mask,
            barrier.src_access_mask, barrier.dst_stage_mask, barrier.dst_access_mask,
            barrier.src_queue_family_index, barrier.dst_queue_family_index,
        )
        .expect("writing String");
    }
    if let Some(barrier) = image {
        writeln!(
            output,
            "    image={:?} src_stage={:?} src_access={:?} dst_stage={:?} dst_access={:?} old_layout={:?} new_layout={:?} src_family={} dst_family={} subresource={:?}",
            barrier.image, barrier.src_stage_mask, barrier.src_access_mask,
            barrier.dst_stage_mask, barrier.dst_access_mask, barrier.old_layout,
            barrier.new_layout, barrier.src_queue_family_index,
            barrier.dst_queue_family_index, barrier.subresource_range,
        )
        .expect("writing String");
    }
}

pub(super) fn finish(
    output: &mut String,
    graph: &CompiledGraph,
    states: &[Option<Snapshot>],
    has_readbacks: bool,
) {
    if has_readbacks {
        let barrier = super::super::transfer::host_read_barrier();
        writeln!(
            output,
            "epilogue reason=readback host visibility src_stage={:?} src_access={:?} dst_stage={:?} dst_access={:?}",
            barrier.src_stage_mask, barrier.src_access_mask,
            barrier.dst_stage_mask, barrier.dst_access_mask,
        )
        .expect("writing String");
    }
    for (index, snapshot) in states.iter().enumerate() {
        if let Some(snapshot) = snapshot {
            writeln!(
                output,
                "final resource={index} exported={}",
                graph.has_export(index),
            )
            .expect("writing String");
            state(output, "accepted_on_submit", snapshot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{AccessPurpose, BufferDesc, Graph};

    #[test]
    fn dump_reports_actual_barrier_fields_and_writer_reason() {
        let mut output = String::new();
        let previous = Snapshot {
            writer_stage: vk::PipelineStageFlags2::COPY,
            writer_access: vk::AccessFlags2::TRANSFER_WRITE,
            ..Snapshot::default()
        };
        let current = super::super::usage(AccessPurpose::Index);
        // Deliberately supply a scope different from the planner's normal choice:
        // the diagnostic must describe its actual operand, not regenerate one.
        let barrier = vk::BufferMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(current.stage)
            .dst_access_mask(current.access)
            .offset(16)
            .size(32);
        dependency(
            &mut output,
            3,
            &previous,
            current,
            false,
            Some(&barrier),
            None,
        );
        assert!(output.contains("resource=3 emitted=true"));
        assert!(output.contains("last writer visibility to this reader"));
        assert!(
            output.contains("offset=16 size=32 src_stage=ALL_COMMANDS src_access=MEMORY_WRITE")
        );
        assert!(output.contains("dst_stage=INDEX_INPUT dst_access=INDEX_READ"));
    }

    #[test]
    fn layout_and_no_dependency_reasons_are_distinct() {
        let mut output = String::new();
        let current = super::super::usage(AccessPurpose::CopyDestination);
        let barrier = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_stage_mask(current.stage)
            .dst_access_mask(current.access)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(current.layout);
        dependency(
            &mut output,
            0,
            &Snapshot::default(),
            current,
            true,
            None,
            Some(&barrier),
        );
        assert!(output.contains("workaround renderer-j17"));
        assert!(output.contains("old_layout=UNDEFINED new_layout=TRANSFER_DST_OPTIMAL"));
        output.clear();
        dependency(
            &mut output,
            0,
            &Snapshot::default(),
            current,
            false,
            None,
            None,
        );
        assert!(output.contains("emitted=false reason=no prior writer"));
        assert!(!output.contains("src_stage="));
    }

    #[test]
    fn pass_and_epilogue_include_uses_and_accepted_state_without_payload() {
        let mut graph = Graph::new();
        let buffer = graph.buffer("payload", BufferDesc { size: 4 }).unwrap();
        graph
            .upload_buffer("upload", buffer, 0, [0x81, 0x82, 0x83, 0x84])
            .unwrap();
        graph.readback_buffer("read", buffer, 0, 4).unwrap();
        let graph = graph.compile().unwrap();
        let mut output = String::new();
        pass(&mut output, 0, &graph.passes[0]);
        assert!(output.contains("purpose=CopyDestination read=false write=true"));
        finish(&mut output, &graph, &[Some(Snapshot::default())], true);
        assert!(output.contains("epilogue reason=readback host visibility"));
        assert!(output.contains(
            "src_stage=COPY src_access=TRANSFER_WRITE dst_stage=HOST dst_access=HOST_READ"
        ));
        assert!(output.contains("accepted_on_submit revision=0"));
        assert!(!output.contains("129, 130, 131, 132"));
        let mut no_readback = String::new();
        finish(&mut no_readback, &graph, &[], false);
        assert!(!no_readback.contains("HOST_READ"));
    }
}
