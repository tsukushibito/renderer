//! Native bundle recording for direct draws with immutable root-buffer operands.
use super::super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    frame::{ExecutionSlot, NativeObject, object},
    raster::RasterPipeline,
    registry::PhysicalRef,
};
use crate::jobs::RecordingTeam;
use crate::{
    graph::{CompiledGraph, Operation, RasterBatchOperands},
    program::BindingType,
};
use std::num::NonZeroUsize;

pub(in crate::backends::d3d12) struct Bundle {
    pub(super) list: NativeObject<ID3D12GraphicsCommandList>,
    allocator: NativeObject<ID3D12CommandAllocator>,
    pub(super) pipeline: Option<usize>,
    error: Option<D3D12RuntimeError>,
}
// SAFETY: Command lists/allocators can move between threads. Only one job
// owns each pair while recording. The slot retains it through GPU completion.
unsafe impl Send for Bundle {}

impl Bundle {
    pub(in crate::backends::d3d12) fn new(owner: &DeviceOwner) -> Result<Self, D3D12RuntimeError> {
        // SAFETY: The fresh BUNDLE allocator and list have the same device and type.
        let allocator: NativeObject<ID3D12CommandAllocator> =
            object(owner, "CreateCommandAllocator(bundle)", || unsafe {
                owner
                    .raw
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_BUNDLE)
            })?;
        // SAFETY: The allocator is exclusively owned and has no associated list yet.
        let list: NativeObject<ID3D12GraphicsCommandList> =
            object(owner, "CreateCommandList(bundle)", || unsafe {
                owner
                    .raw
                    .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_BUNDLE, &*allocator, None)
            })?;
        // SAFETY: The newly created list is open and has never been executed.
        unsafe { list.Close() }
            .ok()
            .map_err(|e| owner.native("Close(bundle)", e))?;
        Ok(Self {
            allocator,
            list,
            pipeline: None,
            error: None,
        })
    }

    fn record(
        &mut self,
        owner: &DeviceOwner,
        pipeline: &RasterPipeline,
        buffers: &[PhysicalRef],
        map: &[usize],
        graph: &CompiledGraph,
        batch: &RasterBatchOperands,
    ) -> Result<(), D3D12RuntimeError> {
        // SAFETY: Only cancelled or fence-retired slots can reach recording. No
        // other job touches this allocator/list. Each bundle establishes its PSO,
        // root signature and topology; the parent supplies viewport and targets.
        unsafe {
            #[cfg(feature = "memory-profiling")]
            crate::measurement::record_native(crate::measurement::NativeEvent::Reset, 2);
            self.allocator
                .Reset()
                .ok()
                .map_err(|e| owner.native("Reset(bundle allocator)", e))?;
            self.list
                .Reset(&*self.allocator, None)
                .ok()
                .map_err(|e| owner.native("Reset(bundle list)", e))?;
            self.list.SetPipelineState(&*pipeline.pipeline);
            self.list.IASetPrimitiveTopology(D3D12_PRIMITIVE_TOPOLOGY(
                D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            ));
            self.list.SetGraphicsRootSignature(&*pipeline.signature);
        }
        // All resource views have been bounded by graph preflight. Index contents
        // are proved in pass order on the parent before it can execute this bundle.
        let result = super::raster::record_draws(
            &self.list,
            buffers,
            map,
            owner,
            graph,
            batch,
            &pipeline.managed,
            None,
            None,
            |_, _, _, _| Ok(()),
        );
        // SAFETY: This job exclusively owns the open bundle, including on error.
        unsafe { self.list.Close() }
            .ok()
            .map_err(|e| owner.native("Close(bundle)", e))?;
        result
    }
}

pub(super) fn record(
    slot: &mut ExecutionSlot,
    owner: &DeviceOwner,
    graph: &CompiledGraph,
    programs: &[RasterPipeline],
    team: &RecordingTeam,
) -> Result<(), D3D12RuntimeError> {
    let storage = graph.data();
    for bundle in &mut slot.bundles {
        bundle.pipeline = None;
    }
    let mut end = 0;
    for &pass in &storage.order {
        let Operation::Raster {
            batches,
            colors,
            depth,
            ..
        } = &storage.passes[pass].operation
        else {
            continue;
        };
        let samples = super::raster::target_samples(graph, colors, *depth);
        for (index, batch) in batches.clone().zip(storage.raster_batches(batches)) {
            let Some(bundle) = slot.bundles.get_mut(index) else {
                break;
            };
            let pipeline = programs
                .iter()
                .position(|p| p.matches(batch.program, depth.is_some(), samples))
                .expect("preflight resolved raster pipeline");
            if batch.indirect.is_none()
                && programs[pipeline]
                    .managed
                    .bindings()
                    .iter()
                    .all(|b| matches!(b.kind, BindingType::Uniform | BindingType::StorageRead))
            {
                bundle.pipeline = Some(pipeline);
                end = end.max(index + 1);
            }
        }
    }
    if !team.large_enough(
        16,
        slot.bundles[..end]
            .iter()
            .enumerate()
            .filter(|(_, b)| b.pipeline.is_some())
            .map(|(i, _)| storage.raster_batches(&(i..i + 1))[0].draws.len()),
    ) {
        for bundle in &mut slot.bundles[..end] {
            bundle.pipeline = None;
        }
        return Ok(());
    }
    if end == 0 {
        return Ok(());
    }
    let buffers = &slot.buffers;
    let map = &slot.resource_map;
    let bundles = &mut slot.bundles[..end];
    team.jobs.with(|team| {
        team.scope(|scope| {
            scope.for_each_chunk_mut(bundles, NonZeroUsize::MIN, |index, chunk| {
                let bundle = &mut chunk[0];
                if let Some(pipeline) = bundle.pipeline {
                    let batch = &storage.raster_batches(&(index..index + 1))[0];
                    bundle.error = bundle
                        .record(owner, &programs[pipeline], buffers, map, graph, batch)
                        .err();
                }
            })
        })
    });
    for bundle in bundles {
        if let Some(error) = bundle.error.take() {
            return Err(error);
        }
    }
    Ok(())
}
