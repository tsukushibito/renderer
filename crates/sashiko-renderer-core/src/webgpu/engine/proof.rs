//! Trial versions of index metadata, committed only with their GPU writes.
use super::{Capacity, Error, Recovery, capacity, unsupported};
use crate::graph::{
    CompiledGraph, MeshUpload, Operation, ResourceDesc,
    provenance::{BufferProvenance, IndexError, ProvenanceCapacityError},
};
use crate::program::{ProgramId, RasterPipelineRequest};
use crate::runtime::RuntimeLimits;

fn exceeded(e: ProvenanceCapacityError) -> Error {
    capacity(
        Capacity::GraphMetadata,
        Recovery::Reconfigure,
        e.capacity,
        e.required,
    )
}
fn storage(limits: RuntimeLimits) -> BufferProvenance {
    BufferProvenance::with_capacity(
        limits.provenance_segments_per_resource,
        limits.index_prefixes_per_resource,
    )
}

pub(super) struct Proof {
    current: BufferProvenance,
    trial: BufferProvenance,
}

impl Proof {
    pub(super) fn new(limits: RuntimeLimits) -> Self {
        Self {
            current: storage(limits),
            trial: storage(limits),
        }
    }
    pub(super) fn begin(&mut self, registered: bool) -> Result<(), Error> {
        self.trial.clear();
        if registered {
            self.trial.copy_from(&self.current).map_err(exceeded)?;
        }
        Ok(())
    }
    pub(super) fn discard(&mut self) {
        self.current.clear();
        self.trial.clear();
    }
    pub(super) fn commit(&mut self) {
        std::mem::swap(&mut self.current, &mut self.trial);
        self.trial.clear();
    }
}

/// The client stores physical slot numbers in a pre-reserved graph-local map.
pub(super) struct IndexBounds {
    slots: Vec<Option<usize>>,
    strides: Vec<(ProgramId, u32, u32)>,
    scratch: BufferProvenance,
}
impl IndexBounds {
    pub(super) fn new(limits: RuntimeLimits, requests: &[RasterPipelineRequest]) -> Self {
        let strides = requests
            .iter()
            .map(|req| {
                let program = &req.program;
                (
                    program.identity(),
                    program.vertex_stride(),
                    program.descriptor().secondary_vertex_stride,
                )
            })
            .collect();
        Self {
            slots: Vec::with_capacity(limits.graph.resources),
            strides,
            scratch: storage(limits),
        }
    }
    pub(super) fn prepare(
        &mut self,
        graph: &CompiledGraph,
        lookup: impl Fn(usize) -> Option<usize>,
    ) {
        self.slots.clear();
        self.slots.resize(graph.resources.len(), None);
        for (i, r) in graph.resources.iter().enumerate() {
            if matches!(r.desc, ResourceDesc::Buffer(_))
                && (r.import.is_some() || graph.has_export(i))
            {
                self.slots[i] = lookup(i);
            }
        }
        for &p in graph.order.iter().rev() {
            match &graph.passes[p].operation {
                Operation::Raster { draws, .. } => {
                    for d in graph
                        .raster_draws(draws)
                        .iter()
                        .filter_map(|d| d.indexed.as_ref())
                    {
                        self.slots[d.indices] = lookup(d.indices);
                    }
                }
                Operation::CopyBuffer { src, dst, .. } if self.slots[*dst].is_some() => {
                    self.slots[*src] = lookup(*src);
                }
                _ => {}
            }
        }
    }

    pub(super) fn validate(
        &mut self,
        graph: &CompiledGraph,
        proofs: &mut [Proof],
    ) -> Result<(), Error> {
        for (i, v) in self.slots.iter().enumerate() {
            if let Some(slot) = v {
                proofs[*slot].begin(graph.resources[i].import.is_some())?;
            }
        }
        for &p in &graph.order {
            match &graph.passes[p].operation {
                Operation::UploadBuffer {
                    dst, offset, mesh, ..
                } => {
                    if let Some(slot) = self.slots[*dst] {
                        let proof = &mut proofs[slot].trial;
                        let size = graph.upload_data(&graph.passes[p].operation).len();
                        let range = *offset..offset + size as u64;
                        match mesh {
                            Some(MeshUpload::Indices { byte_len, maximum }) => {
                                proof.write_mesh_indices(range, *byte_len, *maximum)
                            }
                            Some(MeshUpload::Vertices) => proof.clear(),
                            None => proof
                                .write_graph_slice(range, graph, p, 0..size)
                                .map_err(exceeded)?,
                        }
                    }
                }
                Operation::CopyBuffer {
                    src,
                    dst,
                    src_offset,
                    dst_offset,
                    size,
                } => {
                    if let Some(dst) = self.slots[*dst] {
                        let src = self.slots[*src]
                            .ok_or_else(|| Error::invalid("missing WebGPU copy index source"))?;
                        proofs[src]
                            .trial
                            .copy_into(
                                *src_offset..src_offset + size,
                                *dst_offset,
                                &mut self.scratch,
                            )
                            .map_err(exceeded)?;
                        proofs[dst]
                            .trial
                            .replace(*dst_offset..dst_offset + size, &self.scratch)
                            .map_err(exceeded)?;
                        self.scratch.clear();
                    }
                }
                Operation::Dispatch { .. } => {
                    for access in graph.accesses(p) {
                        if access.write
                            && let Some(slot) = self.slots[access.resource]
                        {
                            proofs[slot]
                                .trial
                                .clear_range(access.range.clone())
                                .map_err(exceeded)?;
                        }
                    }
                }
                Operation::Raster { batches, .. } => {
                    for batch in graph.raster_batches(batches) {
                        let &(_, stride, secondary) = self
                            .strides
                            .iter()
                            .find(|s| s.0 == batch.program)
                            .ok_or_else(|| unsupported("WebGPU raster program is not prepared"))?;
                        for d in graph
                            .raster_draws(&batch.draws)
                            .iter()
                            .filter_map(|d| d.indexed.as_ref())
                        {
                            let mut count = graph.resources[d.vertices]
                                .desc
                                .units()
                                .checked_div(u64::from(stride))
                                .ok_or_else(|| {
                                    Error::invalid("indexed draw requires a vertex stride")
                                })?;
                            if let Some(stream) = d.secondary_vertices {
                                let limit = graph.resources[stream]
                                    .desc
                                    .units()
                                    .checked_div(u64::from(secondary))
                                    .ok_or_else(|| {
                                        Error::invalid("secondary vertex stream requires a stride")
                                    })?;
                                count = count.min(limit);
                            }
                            let slot = self.slots[d.indices].ok_or_else(|| {
                                Error::invalid("missing WebGPU draw index buffer")
                            })?;
                            proofs[slot]
                                .trial
                                .validate_index_range(d.first_index, d.index_count, count, u32::MAX)
                                .map_err(|e| match e {
                                    IndexError::Capacity(e) => exceeded(e),
                                    IndexError::UnknownContents => {
                                        unsupported("indexed draw lacks known index contents")
                                    }
                                    _ => Error::invalid("WebGPU index exceeds vertex range"),
                                })?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub(super) fn commit(&self, graph: &CompiledGraph, proofs: &mut [Proof]) {
        for (i, slot) in self.slots.iter().enumerate() {
            if let Some(slot) = slot {
                if graph.resources[i].import.is_some() || graph.has_export(i) {
                    proofs[*slot].commit();
                } else {
                    proofs[*slot].discard();
                }
            }
        }
    }
    pub(super) fn cancel(&mut self, proofs: &mut [Proof]) {
        self.scratch.clear();
        for slot in self.slots.iter().flatten() {
            proofs[*slot].trial.clear();
        }
    }
}
