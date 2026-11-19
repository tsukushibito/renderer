//! Bounded native frame storage. Leases move ownership without allocating.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use std::{
    ops::Deref,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    graph::provenance::BufferProvenance,
    program::{ProgramId, RasterPipelineRequest},
};

use super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    raster::{IndexedIndirectSignature, RasterPipeline},
    raster_clear::ClearPrograms,
    raster_storage::{D3D12ImageClass, RasterStorage},
    resources::{DeviceBuffer, MAX_BUFFER_BYTES},
};

/// Native and CPU capacities reserved before normal frame processing begins.
#[derive(Clone, Copy, Debug)]
pub struct D3D12FrameCapacity {
    pub executions: usize,
    pub results: usize,
    pub resources: usize,
    /// Total physical registry entries, including registered resources and idle
    /// prepared instances. Independent of the per-Graph `resources` limit.
    pub resource_capacity: usize,
    pub operations: usize,
    /// Known index-content segments per execution buffer, copy scratch and
    /// canonical registry entry.
    /// Independent of operation/draw capacity; exhaustion returns a capacity error.
    /// Zero is valid for transient buffer-only work without retained provenance,
    /// and for complete immutable mesh index proofs.
    pub provenance_segments_per_resource: usize,
    /// Distinct generic index-count prefix summaries retained per buffer and
    /// snapshot until mutation. Exhaustion returns capacity backpressure without
    /// eviction or rescanning. Zero supports immutable mesh proofs and work
    /// that does not validate generic index contents.
    pub index_prefixes_per_resource: usize,
    /// Canonical initialized intervals and independently retained import tokens.
    pub initialized_ranges_per_resource: usize,
    pub range_snapshots: usize,
    /// Buffer operands retained by one execution, including imported resources.
    pub buffers: usize,
    /// Largest logical buffer accepted by one execution. Explicit working-set
    /// classes allocate their actual sizes independently of this metadata limit.
    pub buffer_bytes: u64,
    pub upload_bytes: u64,
    pub readback_bytes: u64,
    pub scratch_bytes: u64,
}

/// Physical DEFAULT buffers reserved globally across execution slots. A class
/// can satisfy a logical buffer no larger than `size`; no physical allocation
/// is multiplied by the number of concurrent executions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3D12BufferClass {
    pub size: u64,
    pub count: usize,
}

fn validate_buffer_classes(
    capacity: D3D12FrameCapacity,
    classes: &[D3D12BufferClass],
) -> Result<(), D3D12RuntimeError> {
    let mut count = 0usize;
    for class in classes {
        if class.size == 0
            || class.size > capacity.buffer_bytes
            || class.size > MAX_BUFFER_BYTES
            || class.size > isize::MAX as u64
            || class.count == 0
        {
            return Err(D3D12RuntimeError::invalid(
                "buffer class must have a nonzero count and fit the logical buffer byte limit",
            ));
        }
        let required = count.checked_add(class.count);
        count = required
            .filter(|count| *count <= capacity.resource_capacity)
            .ok_or_else(|| {
                D3D12RuntimeError::capacity_with(
                    "prepared buffer classes exceed physical registry capacity",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Resources,
                        CapacityRecovery::Reconfigure,
                        Some(capacity.resource_capacity as u64),
                        required.and_then(|value| u64::try_from(value).ok()),
                    ),
                )
            })?;
    }
    Ok(())
}

impl Default for D3D12FrameCapacity {
    fn default() -> Self {
        Self {
            executions: 3,
            results: 6,
            resources: 1024,
            resource_capacity: 1024,
            operations: 4096,
            provenance_segments_per_resource: 64,
            index_prefixes_per_resource: 64,
            initialized_ranges_per_resource: 64,
            range_snapshots: 1024,
            buffers: 16,
            buffer_bytes: 1024 * 1024,
            upload_bytes: 1024 * 1024,
            readback_bytes: 1024 * 1024,
            scratch_bytes: 1024 * 1024,
        }
    }
}

#[cfg(feature = "memory-profiling")]
fn native(event: crate::measurement::NativeEvent) {
    crate::measurement::record_native(event, 1);
}

/// Counts the owning reference, rather than temporary COM argument references.
pub(super) struct NativeObject<T>(T);
impl<T> Deref for NativeObject<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T> Drop for NativeObject<T> {
    fn drop(&mut self) {
        #[cfg(feature = "memory-profiling")]
        native(crate::measurement::NativeEvent::ObjectDestroy);
    }
}
pub(super) fn object<T>(
    owner: &DeviceOwner,
    operation: &'static str,
    create: impl FnOnce() -> windows_core::Result<T>,
) -> Result<NativeObject<T>, D3D12RuntimeError> {
    #[cfg(feature = "memory-profiling")]
    native(crate::measurement::NativeEvent::ObjectCreate);
    create()
        .map(NativeObject)
        .map_err(|error| owner.native(operation, error))
}

pub(super) struct ExecutionSlot {
    allocator: NativeObject<ID3D12CommandAllocator>,
    pub(super) list: NativeObject<ID3D12GraphicsCommandList>,
    fence: NativeObject<ID3D12Fence>,
    next_value: u64,
    pub(super) buffers: Vec<super::registry::PhysicalRef>,
    pub(super) resource_uses: Vec<Option<super::registry::ResourceUse>>,
    pub(super) resource_count: usize,
    pub(super) upload: DeviceBuffer,
    pub(super) scratch: DeviceBuffer,
    pub(super) states: Vec<D3D12_RESOURCE_STATES>,
    pub(super) common_barriers: super::graph::TransitionBatch,
    pub(super) resource_map: Vec<usize>,
    pub(super) raster_read_promotions: Vec<bool>,
    pub(super) buffers_used: usize,
    pub(super) upload_used: u64,
    pub(super) scratch_state: D3D12_RESOURCE_STATES,
    pub(super) raster: Option<RasterStorage>,
    pub(super) compute_descriptors_used: usize,
    pub(super) buffer_scopes: Vec<D3D12_RESOURCE_STATES>,
    pub(super) provenance: Vec<BufferProvenance>,
    pub(super) provenance_needed: Vec<bool>,
    pub(super) provenance_scratch: BufferProvenance,
    pub(super) timestamps: Option<super::metrics::TimestampPair>,
}
// SAFETY: These free-threaded native objects move as one exclusive lease. Their
// allocator and resources are never reset/reused before actual GPU completion.
unsafe impl Send for ExecutionSlot {}

fn reserved<T>(capacity: usize) -> Result<Vec<T>, D3D12RuntimeError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| {
        D3D12RuntimeError::capacity_with(
            "frame capacity reservation failed",
            RuntimeCapacityError::new(
                RuntimeCapacityKind::HostMemory,
                CapacityRecovery::Reconfigure,
                None,
                None,
            ),
        )
    })?;
    Ok(values)
}
impl ExecutionSlot {
    fn new(owner: &DeviceOwner, capacity: D3D12FrameCapacity) -> Result<Self, D3D12RuntimeError> {
        // SAFETY: The live Device owns a fixed DIRECT allocator and matching list.
        let allocator: NativeObject<ID3D12CommandAllocator> =
            object(owner, "ID3D12Device::CreateCommandAllocator", || unsafe {
                owner
                    .raw
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
            })?;
        // SAFETY: The fresh DIRECT allocator matches the live Device and list.
        let list: NativeObject<ID3D12GraphicsCommandList> =
            object(owner, "ID3D12Device::CreateCommandList", || unsafe {
                owner
                    .raw
                    .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &*allocator, None)
            })?;
        // SAFETY: The new list is open and has never been submitted.
        unsafe { list.Close() }
            .ok()
            .map_err(|error| owner.native("ID3D12GraphicsCommandList::Close", error))?;
        // SAFETY: Initial zero is unfinished; the slot signals increasing values.
        let fence: NativeObject<ID3D12Fence> =
            object(owner, "ID3D12Device::CreateFence", || unsafe {
                owner.raw.CreateFence(0, D3D12_FENCE_FLAG_NONE)
            })?;
        // Physical buffers belong to the global canonical registry. A slot
        // retains only its leased references, including imported resources.
        let mut buffers = reserved(capacity.buffers)?;
        buffers.resize_with(capacity.buffers, Default::default);
        let mut states = reserved(capacity.buffers)?;
        states.resize(capacity.buffers, D3D12_RESOURCE_STATE_COMMON);
        let mut resource_map = reserved(capacity.resources)?;
        resource_map.resize(capacity.resources, usize::MAX);
        let mut upload = DeviceBuffer::new(owner, capacity.upload_bytes, D3D12_HEAP_TYPE_UPLOAD)?;
        upload.map_persistent(owner, capacity.upload_bytes, false)?;
        let mut provenance = reserved(capacity.buffers)?;
        for _ in 0..capacity.buffers {
            provenance.push(BufferProvenance::with_capacity(
                capacity.provenance_segments_per_resource,
                capacity.index_prefixes_per_resource,
            ));
        }
        Ok(Self {
            resource_uses: vec![None; capacity.resources],
            resource_count: 0,
            raster: None,
            compute_descriptors_used: 0,
            buffer_scopes: {
                let mut values = reserved(capacity.buffers)?;
                values.resize(capacity.buffers, D3D12_RESOURCE_STATE_COMMON);
                values
            },
            provenance,
            provenance_needed: {
                let mut v = reserved(capacity.resources)?;
                v.resize(capacity.resources, false);
                v
            },
            timestamps: None,
            provenance_scratch: BufferProvenance::with_capacity(
                capacity.provenance_segments_per_resource,
                capacity.index_prefixes_per_resource,
            ),
            allocator,
            list,
            fence,
            next_value: 1,
            buffers,
            upload,
            scratch: DeviceBuffer::new(owner, capacity.scratch_bytes, D3D12_HEAP_TYPE_DEFAULT)?,
            states,
            common_barriers: super::graph::TransitionBatch::new(super::graph::common_capacity(
                capacity.buffers,
                0,
            )?)?,
            resource_map,
            raster_read_promotions: {
                let mut values = reserved(capacity.resources)?;
                values.resize(capacity.resources, false);
                values
            },
            buffers_used: 0,
            upload_used: 0,
            scratch_state: D3D12_RESOURCE_STATE_COMMON,
        })
    }
    fn reset(&mut self, owner: &DeviceOwner) -> Result<(), D3D12RuntimeError> {
        self.common_barriers.clear();
        // SAFETY: Only Available slots reach reset. Prior GPU use has completed,
        // and cancellation closed the sole recording before returning the slot.
        #[cfg(feature = "memory-profiling")]
        native(crate::measurement::NativeEvent::Reset);
        // SAFETY: Available ownership proves GPU completion and no open list.
        unsafe { self.allocator.Reset() }
            .ok()
            .map_err(|error| owner.native("ID3D12CommandAllocator::Reset", error))?;
        #[cfg(feature = "memory-profiling")]
        native(crate::measurement::NativeEvent::Reset);
        // SAFETY: The closed list and freshly reset allocator belong to this slot.
        unsafe { self.list.Reset(&*self.allocator, None) }
            .ok()
            .map_err(|error| owner.native("ID3D12GraphicsCommandList::Reset", error))?;
        self.states.fill(D3D12_RESOURCE_STATE_COMMON);
        for provenance in &mut self.provenance {
            provenance.clear();
        }
        self.provenance_scratch.clear();
        if let Some(raster) = &mut self.raster {
            raster.reset();
        }
        // Graph requirements initializes the active logical resource map once.
        self.buffers_used = 0;
        self.upload_used = 0;
        self.compute_descriptors_used = 0;
        self.scratch_state = D3D12_RESOURCE_STATE_COMMON;
        Ok(())
    }
    fn completed(&self, owner: &DeviceOwner, target: u64) -> Result<bool, D3D12RuntimeError> {
        // SAFETY: This permanently owned fence tracks only this execution slot.
        let completed = unsafe { self.fence.GetCompletedValue() };
        if completed == u64::MAX {
            return Err(owner.removed());
        }
        Ok(completed >= target)
    }
}

pub(super) struct ReadbackMeta {
    pub(super) pass: usize,
    pub(super) size: usize,
    pub(super) offset: u64,
    pub(super) consumed: bool,
    pub(super) image: Option<ReadbackImage>,
}
#[derive(Clone, Copy)]
pub(super) struct ReadbackImage {
    pub(super) row_bytes: usize,
    pub(super) row_pitch: usize,
    pub(super) rows: usize,
}
pub(super) struct ResultSlot {
    pub(super) buffer: DeviceBuffer,
    pub(super) readbacks: Vec<ReadbackMeta>,
    pub(super) graph: u64,
    pub(super) used: u64,
}
// SAFETY: Result storage is exclusively leased. CPU access starts after its GPU
// writer completed, and the lease prevents another GPU writer until CPU return.
unsafe impl Send for ResultSlot {}
impl ResultSlot {
    fn new(owner: &DeviceOwner, capacity: D3D12FrameCapacity) -> Result<Self, D3D12RuntimeError> {
        let mut buffer =
            DeviceBuffer::new(owner, capacity.readback_bytes, D3D12_HEAP_TYPE_READBACK)?;
        buffer.map_persistent(owner, capacity.readback_bytes, true)?;
        Ok(Self {
            buffer,
            readbacks: reserved(capacity.operations)?,
            graph: 0,
            used: 0,
        })
    }
    fn clear(&mut self) {
        self.readbacks.clear();
        self.used = 0;
        self.graph = 0;
    }
}
struct ResultLease {
    index: usize,
    slot: ResultSlot,
}
struct DetachedWork {
    slot: Box<ExecutionSlot>,
    result: Option<ResultLease>,
    target: u64,
}
enum ExecutionEntry {
    Available(Box<ExecutionSlot>),
    Leased,
    Detached(DetachedWork),
    Poisoned { _slot: Box<ExecutionSlot> },
    Quarantined(DetachedWork),
}
struct PoolState {
    executions: Vec<ExecutionEntry>,
    results: Vec<Option<ResultSlot>>,
}
pub(super) struct FramePool {
    pub(super) registry: Mutex<super::registry::Registry>,
    capacity: D3D12FrameCapacity,
    buffer_classes: Vec<D3D12BufferClass>,
    pub(super) raster_programs: Vec<RasterPipeline>,
    pub(super) clear_programs: Option<ClearPrograms>,
    pub(super) compute_programs: Vec<super::program::ComputePipeline>,
    pub(super) indexed_indirect: Option<IndexedIndirectSignature>,
    state: Mutex<PoolState>,
    profiling: AtomicBool,
    timestamps_prepared: AtomicBool,
    // Declared last so native fields are destroyed while the Device remains live.
    owner: Arc<DeviceOwner>,
}
impl FramePool {
    pub(super) fn new(
        owner: Arc<DeviceOwner>,
        capacity: D3D12FrameCapacity,
    ) -> Result<Arc<Self>, D3D12RuntimeError> {
        let count = capacity
            .buffers
            .checked_mul(capacity.executions)
            .ok_or_else(|| D3D12RuntimeError::invalid("uniform buffer class count overflow"))?;
        Self::with_buffers(
            owner,
            capacity,
            &[D3D12BufferClass {
                size: capacity.buffer_bytes,
                count,
            }],
        )
    }
    fn with_buffers(
        owner: Arc<DeviceOwner>,
        capacity: D3D12FrameCapacity,
        classes: &[D3D12BufferClass],
    ) -> Result<Arc<Self>, D3D12RuntimeError> {
        owner.ensure_active()?;
        if capacity.executions == 0
            || capacity.resources == 0
            || capacity.operations == 0
            || capacity.buffers == 0
        {
            return Err(D3D12RuntimeError::invalid(
                "frame execution and metadata capacities must be nonzero",
            ));
        }
        if [
            capacity.buffer_bytes,
            capacity.upload_bytes,
            capacity.readback_bytes,
            capacity.scratch_bytes,
        ]
        .iter()
        .any(|size| *size == 0 || *size > MAX_BUFFER_BYTES || *size > isize::MAX as u64)
        {
            return Err(D3D12RuntimeError::invalid(
                "frame buffer capacities must be within the supported nonzero range",
            ));
        }
        validate_buffer_classes(capacity, classes)?;
        let mut registry = super::registry::Registry::new(capacity)?;
        registry.prepare_buffers(&owner, classes)?;
        let mut executions = reserved(capacity.executions)?;
        for _ in 0..capacity.executions {
            executions.push(ExecutionEntry::Available(Box::new(ExecutionSlot::new(
                &owner, capacity,
            )?)));
        }
        let mut results = reserved(capacity.results)?;
        for _ in 0..capacity.results {
            results.push(Some(ResultSlot::new(&owner, capacity)?));
        }
        for entry in &mut executions {
            let ExecutionEntry::Available(slot) = entry else {
                unreachable!()
            };
            registry.install(slot)?;
        }
        Ok(Arc::new(Self {
            registry: Mutex::new(registry),
            capacity,
            buffer_classes: classes.to_vec(),
            raster_programs: Vec::new(),
            clear_programs: None,
            compute_programs: Vec::new(),
            indexed_indirect: None,
            profiling: AtomicBool::new(false),
            timestamps_prepared: AtomicBool::new(false),
            state: Mutex::new(PoolState {
                executions,
                results,
            }),
            owner,
        }))
    }
    pub(super) fn profiling_enabled(&self) -> bool {
        self.profiling.load(Ordering::Relaxed)
    }
    pub(super) fn set_profiling(&self, enabled: bool) -> Result<(), D3D12RuntimeError> {
        self.owner.ensure_active()?;
        if enabled && !self.timestamps_prepared.load(Ordering::Relaxed) {
            self.reap()?;
            let mut state = self.state();
            if !state
                .executions
                .iter()
                .all(|entry| matches!(entry, ExecutionEntry::Available(_)))
            {
                return Err(D3D12RuntimeError::capacity_with(
                    "return executions before preparing timestamp storage",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::ExecutionSlots,
                        CapacityRecovery::ReleaseOwners,
                        None,
                        None,
                    ),
                ));
            }
            for entry in &mut state.executions {
                let ExecutionEntry::Available(slot) = entry else {
                    unreachable!()
                };
                if slot.timestamps.is_none() {
                    slot.timestamps = Some(super::metrics::TimestampPair::new(&self.owner)?);
                }
            }
            self.timestamps_prepared.store(true, Ordering::Relaxed);
        }
        self.profiling.store(enabled, Ordering::Relaxed);
        Ok(())
    }
    pub(super) fn is_idle(&self) -> bool {
        let state = self.state();
        state
            .executions
            .iter()
            .all(|entry| matches!(entry, ExecutionEntry::Available(_)))
            && state.results.iter().all(Option::is_some)
    }
    pub(super) fn with_graph(
        owner: Arc<DeviceOwner>,
        capacity: D3D12FrameCapacity,
        buffers: &[D3D12BufferClass],
        classes: &[D3D12ImageClass],
        registered_images: &[crate::graph::ImageDesc],
        programs: &[RasterPipelineRequest],
        compute_programs: &[crate::program::ManagedProgram],
    ) -> Result<Arc<Self>, D3D12RuntimeError> {
        if compute_programs
            .iter()
            .any(|program| program.dxil().is_none())
        {
            return Err(D3D12RuntimeError::unsupported(
                "Compute Program has no DXIL shader",
            ));
        }
        if programs
            .iter()
            .any(|request| request.program.dxil().is_none())
        {
            return Err(D3D12RuntimeError::unsupported(
                "Raster Program has no DXIL shaders",
            ));
        }
        let mut unique_programs = [None::<ProgramId>; crate::program::MAX_RASTER_PROGRAMS];
        let mut program_count = 0;
        for request in programs {
            let program = request.program.identity();
            if unique_programs[..program_count].contains(&Some(program)) {
                continue;
            }
            if program_count == unique_programs.len() {
                return Err(D3D12RuntimeError::capacity_with(
                    "Raster Program working-set capacity exceeded",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::WorkingSet,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                ));
            }
            unique_programs[program_count] = Some(program);
            program_count += 1;
        }
        let mut pool = Self::with_buffers(owner, capacity, buffers)?;
        let pool_mut = Arc::get_mut(&mut pool).expect("new unshared pool");
        let class_count = classes
            .len()
            .checked_add(registered_images.len())
            .ok_or_else(|| {
                D3D12RuntimeError::capacity_with(
                    "image class capacity overflow",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Resources,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?;
        let mut all_classes = reserved(class_count)?;
        all_classes.extend_from_slice(classes);
        all_classes.extend(
            registered_images
                .iter()
                .map(|&desc| D3D12ImageClass { desc, count: 1 }),
        );
        pool_mut.clear_programs = Some(ClearPrograms::new(&pool_mut.owner, &all_classes)?);
        pool_mut
            .raster_programs
            .try_reserve_exact(program_count * 4)
            .map_err(|_| {
                D3D12RuntimeError::capacity_with(
                    "Raster pipeline capacity reservation failed",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::HostMemory,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?;
        let mut raster_images = 0;
        for RasterPipelineRequest {
            program,
            sample_count,
        } in programs
        {
            raster_images = raster_images.max(
                program
                    .bindings()
                    .iter()
                    .filter(|slot| slot.kind == crate::program::BindingType::SampledImage)
                    .count(),
            );
            for depth in [false, true] {
                if program.requires_depth() && !depth {
                    continue;
                }
                if !pool_mut
                    .raster_programs
                    .iter()
                    .any(|pipeline| pipeline.matches(program.identity(), depth, *sample_count))
                {
                    pool_mut.raster_programs.push(RasterPipeline::new(
                        &pool_mut.owner,
                        program,
                        depth,
                        *sample_count,
                    )?);
                }
            }
        }
        pool_mut
            .compute_programs
            .try_reserve_exact(compute_programs.len())
            .map_err(|_| {
                D3D12RuntimeError::capacity_with(
                    "Compute pipeline capacity reservation failed",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::HostMemory,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?;
        let mut compute_images = 0;
        for program in compute_programs {
            if pool_mut.indexed_indirect.is_none()
                && program.same_asset(&crate::program::ManagedProgram::indexed_indirect_encoder())
            {
                pool_mut.indexed_indirect = Some(IndexedIndirectSignature::new(&pool_mut.owner)?);
            }
            if !pool_mut
                .compute_programs
                .iter()
                .any(|native| native.matches(program.identity()))
            {
                pool_mut
                    .compute_programs
                    .push(super::program::ComputePipeline::new(
                        &pool_mut.owner,
                        program,
                        #[cfg(test)]
                        false,
                    )?);
            }
            compute_images = compute_images.max(
                program
                    .bindings()
                    .iter()
                    .filter(|slot| {
                        matches!(
                            slot.kind,
                            crate::program::BindingType::SampledImage
                                | crate::program::BindingType::StorageImageRead(_)
                                | crate::program::BindingType::StorageImageWrite(_)
                        )
                    })
                    .count(),
            );
        }
        let raster_descriptors =
            capacity
                .operations
                .checked_mul(raster_images)
                .ok_or_else(|| {
                    D3D12RuntimeError::capacity_with(
                        "Raster descriptor capacity overflow",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::Descriptors,
                            CapacityRecovery::Reconfigure,
                            None,
                            None,
                        ),
                    )
                })?;
        let compute_descriptors =
            capacity
                .operations
                .checked_mul(compute_images)
                .ok_or_else(|| {
                    D3D12RuntimeError::capacity_with(
                        "Compute descriptor capacity overflow",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::Descriptors,
                            CapacityRecovery::Reconfigure,
                            None,
                            None,
                        ),
                    )
                })?;
        for (slot_index, entry) in pool_mut
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .executions
            .iter_mut()
            .enumerate()
        {
            let ExecutionEntry::Available(slot) = entry else {
                unreachable!()
            };
            slot.raster = Some(RasterStorage::new(
                &pool_mut.owner,
                classes,
                registered_images,
                raster_descriptors,
                compute_descriptors,
                capacity.buffers,
                slot_index == 0,
            )?);
            slot.common_barriers
                .prepare_capacity(super::graph::common_capacity(
                    capacity.buffers,
                    slot.raster.as_ref().unwrap().images.len(),
                )?)?;
            pool_mut
                .registry
                .get_mut()
                .unwrap_or_else(|e| e.into_inner())
                .install(slot)?;
        }
        Ok(pool)
    }
    pub(super) fn capacity(&self) -> D3D12FrameCapacity {
        self.capacity
    }
    pub(super) fn buffer_classes(&self) -> &[D3D12BufferClass] {
        &self.buffer_classes
    }
    pub(super) fn owner(&self) -> &Arc<DeviceOwner> {
        &self.owner
    }
    fn state(&self) -> std::sync::MutexGuard<'_, PoolState> {
        // No user code executes under this lock. Preserve pending ownership even
        // if another thread unwound while it held the bookkeeping lock.
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
    fn return_result(state: &mut PoolState, mut result: ResultLease) {
        result.slot.clear();
        assert!(state.results[result.index].is_none());
        state.results[result.index] = Some(result.slot);
    }
    pub(super) fn reap(&self) -> Result<usize, D3D12RuntimeError> {
        let mut state = self.state();
        let mut count = 0;
        for index in 0..state.executions.len() {
            let ExecutionEntry::Detached(work) = &state.executions[index] else {
                continue;
            };
            let completed = work.slot.completed(&self.owner, work.target);
            match completed {
                Ok(false) => continue,
                Ok(true) => {
                    let ExecutionEntry::Detached(mut work) =
                        std::mem::replace(&mut state.executions[index], ExecutionEntry::Leased)
                    else {
                        unreachable!()
                    };
                    work.slot.release_resources();
                    if let Some(result) = work.result {
                        Self::return_result(&mut state, result);
                    }
                    state.executions[index] = ExecutionEntry::Available(work.slot);
                    count += 1;
                }
                Err(error) => {
                    let entry =
                        std::mem::replace(&mut state.executions[index], ExecutionEntry::Leased);
                    let ExecutionEntry::Detached(work) = entry else {
                        unreachable!()
                    };
                    state.executions[index] = ExecutionEntry::Quarantined(work);
                    return Err(error);
                }
            }
        }
        Ok(count)
    }
    pub(super) fn acquire(
        self: &Arc<Self>,
        needs_result: bool,
    ) -> Result<FrameWork, D3D12RuntimeError> {
        self.owner.ensure_active()?;
        self.reap()?;
        let mut state = self.state();
        let index = state
            .executions
            .iter()
            .position(|entry| matches!(entry, ExecutionEntry::Available(_)))
            .ok_or_else(|| {
                D3D12RuntimeError::capacity_with(
                    "all prepared execution slots are occupied",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::ExecutionSlots,
                        CapacityRecovery::ReleaseOwners,
                        Some(self.capacity.executions as u64),
                        None,
                    ),
                )
            })?;
        let result_index = if needs_result {
            Some(
                state
                    .results
                    .iter()
                    .position(Option::is_some)
                    .ok_or_else(|| {
                        D3D12RuntimeError::capacity_with(
                            "all prepared readback slots are occupied",
                            RuntimeCapacityError::new(
                                RuntimeCapacityKind::ResultSlots,
                                if self.capacity.results == 0 {
                                    CapacityRecovery::Reconfigure
                                } else {
                                    CapacityRecovery::ReleaseResults
                                },
                                Some(self.capacity.results as u64),
                                Some(1),
                            ),
                        )
                    })?,
            )
        } else {
            None
        };
        let ExecutionEntry::Available(slot) =
            std::mem::replace(&mut state.executions[index], ExecutionEntry::Leased)
        else {
            unreachable!()
        };
        let result = result_index.map(|index| ResultLease {
            index,
            slot: state.results[index].take().unwrap(),
        });
        drop(state);
        let mut work = FrameWork {
            pool: Arc::clone(self),
            index,
            slot: Some(slot),
            result,
            graph: 0,
            target: 0,
            phase: Phase::Poisoned,
            plan: None,
            exports: None,
            profile: None,
        };
        if work.slot().next_value == u64::MAX {
            work.phase = Phase::Recorded;
            return Err(D3D12RuntimeError::capacity_with(
                "frame fence timeline requires explicit renewal",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::ExecutionSlots,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            ));
        }
        work.slot.as_mut().unwrap().reset(&self.owner)?;
        work.phase = Phase::Recording;
        work.target = work.slot().next_value;
        work.slot.as_mut().unwrap().next_value += 1;
        Ok(work)
    }
}

impl Drop for FramePool {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut unproven = false;
        for entry in &mut state.executions {
            let pending = match entry {
                ExecutionEntry::Detached(work) | ExecutionEntry::Quarantined(work) => Some(work),
                _ => None,
            };
            let Some(work) = pending else { continue };
            let complete = loop {
                match work.slot.completed(&self.owner, work.target) {
                    Ok(true) => break true,
                    Ok(false) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    _ => break false,
                }
            };
            if !complete {
                self.owner.mark_stopped();
                unproven = true;
                // No normal-frame owner is dropped here. Abnormal teardown must
                // retain the full native chain when actual completion is unknown.
                std::mem::forget(std::mem::replace(entry, ExecutionEntry::Leased));
            }
        }
        if unproven {
            // The intentionally retained pending command lists may reference
            // graphics PSOs/signatures as well as resources. Their pool-level
            // owners must survive the same unproven-completion teardown.
            std::mem::forget(std::mem::take(&mut self.raster_programs));
            std::mem::forget(self.clear_programs.take());
            std::mem::forget(std::mem::take(&mut self.compute_programs));
            std::mem::forget(self.indexed_indirect.take());
            std::mem::forget(Arc::clone(&self.owner));
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Recording,
    Recorded,
    Pending,
    Complete,
    Poisoned,
    Quarantined,
}
pub(super) struct FrameWork {
    pub(super) plan: Option<crate::graph::CompiledGraph>,
    pub(super) exports: Option<super::registry::ExportLease>,
    pool: Arc<FramePool>,
    index: usize,
    slot: Option<Box<ExecutionSlot>>,
    result: Option<ResultLease>,
    graph: u64,
    target: u64,
    phase: Phase,
    profile: Option<super::metrics::WorkProfile>,
}
impl FrameWork {
    pub(super) fn begin_profile(&mut self, started: Option<Instant>) {
        self.profile = started.map(super::metrics::WorkProfile::new);
    }
    pub(super) fn begin_recording_metrics(&mut self) {
        if let Some(profile) = &mut self.profile {
            profile.metrics.cpu_prepare = profile.started.elapsed();
            profile.recording = Some(Instant::now());
            // SAFETY: This execution exclusively owns an open DIRECT list and
            // its prepared pair, which returned only after prior completion.
            unsafe {
                self.slot()
                    .timestamps
                    .as_ref()
                    .unwrap()
                    .begin(&self.slot().list);
            }
        }
    }
    pub(super) fn owner(&self) -> &Arc<DeviceOwner> {
        self.pool.owner()
    }
    pub(super) fn capacity(&self) -> D3D12FrameCapacity {
        self.pool.capacity()
    }
    pub(super) fn slot(&self) -> &ExecutionSlot {
        self.slot.as_ref().unwrap()
    }
    pub(super) fn slot_mut(&mut self) -> &mut ExecutionSlot {
        self.slot.as_mut().unwrap()
    }
    pub(super) fn result(&self) -> Option<&ResultSlot> {
        self.result.as_ref().map(|lease| &lease.slot)
    }
    pub(super) fn result_mut(&mut self) -> Option<&mut ResultSlot> {
        self.result.as_mut().map(|lease| &mut lease.slot)
    }
    pub(super) fn set_graph(&mut self, graph: u64) {
        self.graph = graph;
        if let Some(result) = self.result_mut() {
            result.graph = graph;
        }
    }
    pub(super) fn finish_recording(&mut self) -> Result<(), D3D12RuntimeError> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.d3d12.frame.finish_recording"),
                0,
            )
        });
        if self.phase != Phase::Recording {
            return Err(D3D12RuntimeError::invalid("frame recording is not open"));
        }
        if let Some(result) = self.result_mut() {
            result.readbacks.sort_unstable_by_key(|meta| meta.pass);
        }
        if self.profile.is_some() {
            // SAFETY: The pair's matching begin precedes all Graph commands;
            // pending FrameWork keeps the pair/readback alive through its Fence.
            unsafe {
                self.slot()
                    .timestamps
                    .as_ref()
                    .unwrap()
                    .end(&self.slot().list);
            }
        }
        // A failed Close makes this native command list permanently unresettable.
        self.phase = Phase::Poisoned;
        {
            #[cfg(feature = "tracy")]
            let _span = tracy_client::Client::running().map(|client| {
                client.span(
                    tracy_client::span_location!("renderer.d3d12.frame.close"),
                    0,
                )
            });
            // SAFETY: The sole leased list is recording and all operands are retained.
            unsafe { self.slot().list.Close() }.ok().map_err(|error| {
                self.pool
                    .owner
                    .native("ID3D12GraphicsCommandList::Close", error)
            })?;
        }
        self.phase = Phase::Recorded;
        if let Some(profile) = &mut self.profile {
            profile.metrics.cpu_record = profile.recording.take().unwrap().elapsed();
        }
        Ok(())
    }
    pub(super) fn submit(mut self) -> Result<FrameSubmission, D3D12RuntimeError> {
        let started = self.profile.as_ref().map(|_| Instant::now());
        if self.phase != Phase::Recorded {
            return Err(D3D12RuntimeError::invalid("frame recording is not closed"));
        }
        let owner = Arc::clone(&self.pool.owner);
        let _gate = owner.queue_gate.lock().map_err(|_| {
            owner.mark_stopped();
            D3D12RuntimeError::invalid("D3D12 queue transaction lock was poisoned")
        })?;
        owner.ensure_active()?;
        let pool = Arc::clone(&self.pool);
        let mut registry = pool.registry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(graph) = &self.plan {
            registry.validate(graph)?;
        }
        let list: ID3D12CommandList = self.slot().list.0.clone().into();
        let fence = self.slot().fence.0.clone();
        let target = self.target;
        let signaled = execute_and_signal(
            &mut self.phase,
            |_| {
                // SAFETY: The closed list, allocator and all operands remain
                // owned by this pending lease; the gate serializes Execute/Signal.
                // Keep one list per ExecuteCommandLists: Raster's implicit
                // COMMON reads rely on decay at this execution boundary.
                unsafe {
                    owner.queue.ExecuteCommandLists(&[Some(list)]);
                }
            },
            || {
                // SAFETY: The live slot fence uses a unique increasing target;
                // the queue operation follows the execution covered by that target.
                unsafe { owner.queue.Signal(&fence, target) }.ok()
            },
        );
        if let Err(error) = signaled {
            owner.mark_stopped();
            return Err(owner.native("ID3D12CommandQueue::Signal", error));
        }
        if let Some(graph) = self.plan.take() {
            registry.commit(&graph, self.slot_mut());
        }
        // Canonical provenance has moved into the registry. Release the old
        // backing's content pins while retaining prepared arrays and GPU leases.
        for provenance in &mut self.slot_mut().provenance {
            provenance.clear();
        }
        let exports = self.exports.take();
        if let Some(profile) = &mut self.profile {
            profile.metrics.cpu_submit = started.unwrap().elapsed();
        }
        Ok(FrameSubmission {
            work: Some(self),
            exports,
        })
    }
    fn detached(&mut self) -> DetachedWork {
        DetachedWork {
            slot: self.slot.take().unwrap(),
            result: self.result.take(),
            target: self.target,
        }
    }
    fn complete(mut self) -> Result<FrameResult, D3D12RuntimeError> {
        let started = self.profile.as_ref().map(|_| Instant::now());
        let mut metrics = if let Some(mut profile) = self.profile.take() {
            // SAFETY: FrameSubmission observed the successful execution Fence;
            // this slot cannot return/reuse until after query data is copied.
            profile.metrics.gpu = unsafe {
                self.slot()
                    .timestamps
                    .as_ref()
                    .unwrap()
                    .read(&self.pool.owner, profile.started.elapsed())
            }?;
            Some(profile.metrics)
        } else {
            None
        };
        let returned = self
            .pool
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .result_token();
        let mut slot = self.slot.take().unwrap();
        slot.release_resources();
        self.pool.state().executions[self.index] = ExecutionEntry::Available(slot);
        let result = self.result.take();
        let remaining = result
            .as_ref()
            .map_or(0, |result| result.slot.readbacks.len());
        let mut completed = if remaining == 0 {
            if let Some(result) = result {
                FramePool::return_result(&mut self.pool.state(), result);
            }
            FrameResult {
                graph: self.graph,
                _returned: returned,
                pool: None,
                result: None,
                remaining: 0,
                metrics: None,
            }
        } else {
            FrameResult {
                graph: self.graph,
                _returned: returned,
                pool: Some(Arc::clone(&self.pool)),
                result,
                remaining,
                metrics: None,
            }
        };
        if let Some(metrics) = &mut metrics {
            metrics.cpu_retire = started.unwrap().elapsed();
        }
        completed.metrics = metrics;
        Ok(completed)
    }
}

// Publish uncertain ownership before the void Execute call. A failed Signal or
// unwind cannot prove rejection/completion, so Drop must retain the native chain.
fn execute_and_signal<E>(
    phase: &mut Phase,
    execute: impl FnOnce(&Phase),
    signal: impl FnOnce() -> Result<(), E>,
) -> Result<(), E> {
    *phase = Phase::Pending;
    execute(phase);
    let signaled = signal();
    if signaled.is_err() {
        *phase = Phase::Quarantined;
    }
    signaled
}

impl Drop for FrameWork {
    fn drop(&mut self) {
        if self.slot.is_none() {
            return;
        }
        self.slot_mut().common_barriers.clear();
        if self.phase == Phase::Recording {
            // SAFETY: Cancel owns the sole open list and has never submitted it.
            self.phase = if unsafe { self.slot().list.Close() }.is_ok() {
                Phase::Recorded
            } else {
                Phase::Poisoned
            };
        }
        // CPU provenance only establishes index safety while recording. Even
        // a rejected late draw/cancel must return its Graph pins immediately;
        // the prepared buffers, descriptors and native chain remain retained.
        for provenance in &mut self.slot.as_mut().unwrap().provenance {
            provenance.clear();
        }
        self.slot.as_mut().unwrap().provenance_scratch.clear();
        if matches!(self.phase, Phase::Recorded | Phase::Complete) {
            self.slot_mut().release_resources();
        }
        let work = self.detached();
        let mut state = self.pool.state();
        match self.phase {
            Phase::Pending | Phase::Quarantined => {
                self.pool.owner.mark_stopped();
                state.executions[self.index] = ExecutionEntry::Quarantined(work);
            }
            Phase::Poisoned => {
                if let Some(result) = work.result {
                    FramePool::return_result(&mut state, result);
                }
                state.executions[self.index] = ExecutionEntry::Poisoned { _slot: work.slot };
            }
            _ => {
                if let Some(result) = work.result {
                    FramePool::return_result(&mut state, result);
                }
                state.executions[self.index] = ExecutionEntry::Available(work.slot);
            }
        }
    }
}

pub(super) struct FrameSubmission {
    pub(super) exports: Option<super::registry::ExportLease>,
    work: Option<FrameWork>,
}
impl FrameSubmission {
    pub(super) fn is_pending(&self) -> bool {
        self.work
            .as_ref()
            .is_some_and(|work| work.phase == Phase::Pending)
    }

    pub(super) fn poll(&mut self) -> Result<Option<FrameResult>, D3D12RuntimeError> {
        self.wait(Duration::ZERO)
    }
    pub(super) fn wait(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<FrameResult>, D3D12RuntimeError> {
        let work = self
            .work
            .as_mut()
            .ok_or_else(|| D3D12RuntimeError::invalid("D3D12 submission was already completed"))?;
        let started = Instant::now();
        loop {
            if work.slot().completed(&work.pool.owner, work.target)? {
                break;
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(None);
            }
            std::thread::sleep(remaining.min(Duration::from_millis(1)));
        }
        // Completion permits reclaiming native operands even when another
        // operation stopped the Device; successful data still requires activity.
        work.phase = Phase::Complete;
        work.pool.owner.check_native_active()?;
        Ok(Some(self.work.take().unwrap().complete()?))
    }
}
impl Drop for FrameSubmission {
    fn drop(&mut self) {
        if let Some(mut work) = self.work.take()
            && work.phase == Phase::Pending
        {
            let detached = work.detached();
            work.pool.state().executions[work.index] = ExecutionEntry::Detached(detached);
        }
    }
}

pub(super) struct FrameResult {
    graph: u64,
    _returned: Arc<()>,
    pool: Option<Arc<FramePool>>,
    result: Option<ResultLease>,
    remaining: usize,
    metrics: Option<crate::profiling::SubmissionMetrics>,
}
impl FrameResult {
    pub(super) fn metrics(&self) -> Option<&crate::profiling::SubmissionMetrics> {
        self.metrics.as_ref()
    }
    pub(super) fn graph(&self) -> u64 {
        self.graph
    }
    #[cfg(all(test, feature = "d3d12-diagnostics"))]
    pub(super) fn has_owner(&self) -> bool {
        self.pool.is_some()
    }
    fn find(&self, pass: usize) -> Result<usize, D3D12RuntimeError> {
        self.result
            .as_ref()
            .and_then(|result| {
                result
                    .slot
                    .readbacks
                    .binary_search_by_key(&pass, |meta| meta.pass)
                    .ok()
                    .filter(|index| !result.slot.readbacks[*index].consumed)
            })
            .ok_or_else(|| D3D12RuntimeError::invalid("pass has no unconsumed readback result"))
    }
    pub(super) fn readback_len(&self, pass: usize) -> Result<usize, D3D12RuntimeError> {
        let index = self.find(pass)?;
        Ok(self.result.as_ref().unwrap().slot.readbacks[index].size)
    }
    pub(super) fn readback_into(
        &mut self,
        pass: usize,
        destination: &mut [u8],
    ) -> Result<(), D3D12RuntimeError> {
        let index = self.find(pass)?;
        let result = self.result.as_ref().unwrap();
        let meta = &result.slot.readbacks[index];
        if destination.len() != meta.size {
            return Err(D3D12RuntimeError::invalid(
                "readback destination length does not match the result",
            ));
        }
        let owner = &self.pool.as_ref().unwrap().owner;
        owner.check_native_active()?;
        // SAFETY: This result exists only after normal Fence completion. The
        // readback lease remains exclusive until CPU consumption has finished.
        unsafe {
            if let Some(image) = meta.image {
                for row in 0..image.rows {
                    result.slot.buffer.read_into_at(
                        owner,
                        meta.offset + (row * image.row_pitch) as u64,
                        &mut destination[row * image.row_bytes..(row + 1) * image.row_bytes],
                    )?;
                }
            } else {
                result
                    .slot
                    .buffer
                    .read_into_at(owner, meta.offset, destination)?;
            }
        }
        owner.check_native_active()?;
        self.result.as_mut().unwrap().slot.readbacks[index].consumed = true;
        self.remaining -= 1;
        if self.remaining == 0 {
            self.return_result();
        }
        Ok(())
    }
    fn return_result(&mut self) {
        if let Some(result) = self.result.take() {
            let pool = self.pool.take().unwrap();
            FramePool::return_result(&mut pool.state(), result);
        }
    }
}
impl Drop for FrameResult {
    fn drop(&mut self) {
        self.return_result();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        panic::{AssertUnwindSafe, catch_unwind},
    };

    use super::{
        D3D12BufferClass, D3D12FrameCapacity, Phase, execute_and_signal, validate_buffer_classes,
    };

    #[test]
    fn explicit_buffer_classes_are_global_and_metadata_capacity_is_independent() {
        let capacity = D3D12FrameCapacity {
            executions: 4,
            buffers: 4096,
            resource_capacity: 3,
            buffer_bytes: 4096,
            ..Default::default()
        };
        assert!(
            validate_buffer_classes(
                capacity,
                &[
                    D3D12BufferClass { size: 8, count: 1 },
                    D3D12BufferClass { size: 64, count: 1 },
                    D3D12BufferClass {
                        size: 4096,
                        count: 1
                    },
                ]
            )
            .is_ok()
        );
        assert!(validate_buffer_classes(capacity, &[]).is_ok());
        let failure = validate_buffer_classes(capacity, &[D3D12BufferClass { size: 8, count: 4 }])
            .unwrap_err();
        let payload = failure.capacity().unwrap();
        assert_eq!(payload.limit, Some(3));
        assert_eq!(payload.required, Some(4));
    }

    #[test]
    fn invalid_buffer_classes_fail_before_native_creation() {
        for class in [
            D3D12BufferClass { size: 0, count: 1 },
            D3D12BufferClass { size: 64, count: 0 },
            D3D12BufferClass {
                size: super::MAX_BUFFER_BYTES + 1,
                count: 1,
            },
        ] {
            assert!(validate_buffer_classes(D3D12FrameCapacity::default(), &[class]).is_err());
        }
        let capacity = D3D12FrameCapacity {
            buffer_bytes: 64,
            ..Default::default()
        };
        assert!(
            validate_buffer_classes(capacity, &[D3D12BufferClass { size: 65, count: 1 }]).is_err()
        );
    }

    #[test]
    #[ignore = "requires real D3D12 hardware; root serializes native acceptance"]
    fn d3d12_global_buffer_classes_preserve_capacity_after_smaller_reuse() {
        use crate::graph::{BufferDesc, GraphCapacity, GraphPool};
        use crate::program::{
            ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups, ManagedProgram,
            ManagedRasterProgram, RasterProgramDescriptor, ShaderBinaries,
        };
        use crate::{D3D12Runtime, D3D12RuntimeConfig, D3D12ValidationMode};
        use std::time::Duration;

        static RASTER_METADATA: RasterProgramDescriptor = RasterProgramDescriptor {
            shaders: ShaderBinaries::metadata_only(),
            ..crate::program::MESH_RASTER_DESCRIPTOR
        };
        static COMPUTE_METADATA: ComputeProgramDescriptor = ComputeProgramDescriptor {
            entry: "main",
            workgroup_size: [1; 3],
            bindings: &[],
            dispatch: DispatchContract {
                workgroups: DispatchWorkgroups::Fixed([1; 3]),
                bytes: &[],
            },
            shaders: ShaderBinaries::metadata_only(),
        };

        let strict = cfg!(feature = "d3d12-diagnostics");
        let mut runtime = D3D12Runtime::new_with_working_set(
            D3D12RuntimeConfig {
                validation: if strict {
                    D3D12ValidationMode::Required
                } else {
                    D3D12ValidationMode::Disabled
                },
                capacity: D3D12FrameCapacity {
                    executions: 3,
                    results: 3,
                    resources: 8,
                    operations: 8,
                    buffers: 3,
                    buffer_bytes: 8192,
                    resource_capacity: 3,
                    upload_bytes: 8192,
                    readback_bytes: 8192,
                    scratch_bytes: 4,
                    ..Default::default()
                },
            },
            &[
                D3D12BufferClass { size: 8, count: 1 },
                D3D12BufferClass { size: 64, count: 1 },
                D3D12BufferClass {
                    size: 4096,
                    count: 1,
                },
            ],
            &[],
            &[],
            &[],
        )
        .unwrap();
        // Three physical buffers fit a three-entry registry despite three
        // execution slots; uniform per-slot allocation would require nine.
        let monitor = runtime.validation_monitor();
        // SAFETY: These descriptors carry only immutable CPU Graph metadata.
        let missing_raster =
            unsafe { ManagedRasterProgram::from_trusted_descriptor(&RASTER_METADATA) };
        // SAFETY: The immutable metadata describes a one-workgroup dispatch with no resources.
        let missing_compute = unsafe { ManagedProgram::from_trusted_descriptor(&COMPUTE_METADATA) };
        let before = std::sync::Arc::as_ptr(&runtime.pool);
        let missing_raster_request = missing_raster.into();
        for error in [
            runtime
                .prepare_graph_working_set(None, &[], &[missing_raster_request], &[])
                .unwrap_err(),
            runtime
                .prepare_graph_working_set(None, &[], &[], &[missing_compute])
                .unwrap_err(),
        ] {
            assert_eq!(error.kind(), crate::D3D12RuntimeErrorKind::Unsupported);
            assert!(error.to_string().contains("has no DXIL"), "{error}");
        }
        assert_eq!(std::sync::Arc::as_ptr(&runtime.pool), before);
        for (cycle, sizes) in [[4, 32, 512], [8, 64, 4096], [4, 32, 512], [8, 64, 4096]]
            .into_iter()
            .enumerate()
        {
            let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
            let mut graph = pool.begin().unwrap();
            let mut expected = Vec::new();
            for (ordinal, size) in sizes.into_iter().enumerate() {
                let buffer = graph.buffer("class buffer", BufferDesc { size }).unwrap();
                let bytes = vec![(cycle * 17 + ordinal + 1) as u8; size as usize];
                graph
                    .upload_buffer("initialize", buffer, 0, &bytes)
                    .unwrap();
                let readback = graph.readback_buffer("check", buffer, 0, size).unwrap();
                expected.push((readback, bytes));
            }
            let plan = graph.compile().unwrap();
            let mut submission = runtime.submit(&plan).unwrap();
            let mut result = submission
                .wait(Duration::from_secs(30))
                .unwrap()
                .expect("GPU completion");
            for (readback, bytes) in expected {
                let mut actual = vec![0; bytes.len()];
                result.readback_into(readback, &mut actual).unwrap();
                assert_eq!(actual, bytes);
            }
        }
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let buffer = graph
            .buffer(
                "larger than every prepared class",
                BufferDesc { size: 8192 },
            )
            .unwrap();
        graph
            .upload_buffer("initialize", buffer, 0, &[1; 8192])
            .unwrap();
        graph.readback_buffer("observe", buffer, 0, 8192).unwrap();
        let plan = graph.compile().unwrap();
        let failure = runtime
            .prepare(&plan)
            .err()
            .expect("physical class exhaustion");
        assert!(failure.capacity().is_some());
        drop((plan, pool));
        runtime.shutdown().unwrap();
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:?}");
        if strict {
            assert!(
                report.teardown_checked && report.gpu_based_validation_enabled,
                "{report:?}"
            );
        }
    }

    #[test]
    fn signal_failure_follows_execution_and_quarantines_published_pending_work() {
        let mut phase = Phase::Recorded;
        let stage = Cell::new(0);
        let result = execute_and_signal(
            &mut phase,
            |published| {
                assert!(matches!(published, Phase::Pending));
                assert_eq!(stage.replace(1), 0);
            },
            || {
                assert_eq!(stage.replace(2), 1);
                Err("signal failed")
            },
        );
        assert_eq!(result, Err("signal failed"));
        assert_eq!(stage.get(), 2);
        assert!(matches!(phase, Phase::Quarantined));
    }

    #[test]
    fn unwinding_execution_or_signal_keeps_pending_ownership() {
        for unwind_in_execute in [true, false] {
            let mut phase = Phase::Recorded;
            let signal_called = Cell::new(false);
            let unwound = catch_unwind(AssertUnwindSafe(|| {
                let _: Result<(), ()> = execute_and_signal(
                    &mut phase,
                    |published| {
                        assert!(matches!(published, Phase::Pending));
                        assert!(!unwind_in_execute, "injected Execute unwind");
                    },
                    || {
                        signal_called.set(true);
                        panic!("injected Signal unwind");
                    },
                );
            }));
            assert!(unwound.is_err());
            assert!(matches!(phase, Phase::Pending));
            assert_eq!(signal_called.get(), !unwind_in_execute);
        }
    }

    #[test]
    #[ignore = "requires real D3D12 hardware and requested native diagnostics"]
    fn d3d12_index_provenance_capacity_rejects_and_recovers() {
        use crate::{
            backends::d3d12::{
                D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
                D3D12RuntimeErrorKind, D3D12ValidationMode,
            },
            engine::{MeshData, Vertex},
            graph::{
                Attachment, BufferDesc, CompiledGraph, GraphCapacity, GraphPool, ImageDesc, LoadOp,
                PassHandle, RasterAttachments, RasterDraw, RasterParameters, StoreOp,
            },
            program::ManagedRasterProgram,
        };
        use std::time::Duration;

        fn raster(
            pool: &mut GraphPool,
            mesh: &MeshData,
            arbitrary_split: Option<bool>,
        ) -> (CompiledGraph, PassHandle) {
            let mut graph = pool.begin().unwrap();
            let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
            let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
            mesh.upload_graph_slice(&mut graph, "vertices", vertices, false, 0..84)
                .unwrap();
            if let Some(split) = arbitrary_split {
                let bytes = [0u32, 1, 2].map(u32::to_le_bytes).concat();
                if split {
                    graph
                        .upload_buffer("first index", indices, 0, &bytes[..4])
                        .unwrap();
                    graph
                        .upload_buffer("remaining indices", indices, 4, &bytes[4..])
                        .unwrap();
                } else {
                    graph.upload_buffer("indices", indices, 0, &bytes).unwrap();
                }
            } else {
                mesh.upload_graph_slice(&mut graph, "indices", indices, true, 0..12)
                    .unwrap();
            }
            let color = graph.image("color", ImageDesc::rgba8(1, 1)).unwrap();
            graph
                .raster(
                    "triangle",
                    RasterAttachments {
                        color_resolves: &[],
                        colors: &[Attachment {
                            view: graph.image_view(color, 0, 0).unwrap(),
                            load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
                            store: StoreOp::Store,
                        }],
                        depth: None,
                    },
                    |raster| {
                        raster.draw(
                            &ManagedRasterProgram::mesh(),
                            [RasterDraw {
                                vertices,
                                indices,
                                first_index: 0,
                                index_count: 3,
                                instance_count: 1,
                                bindings: None,
                                parameters: RasterParameters::new(
                                    *RasterParameters::default().transform(),
                                    [1.0, 0.0, 0.0, 1.0],
                                )
                                .unwrap()
                                .into(),
                            }],
                        )
                    },
                )
                .unwrap();
            let readback = graph.readback_image("color bytes", color).unwrap();
            (graph.compile().unwrap(), readback)
        }

        let mesh = MeshData::new(
            &[[-1.0, -1.0, 0.5], [3.0, -1.0, 0.5], [-1.0, 3.0, 0.5]].map(|position| Vertex {
                position,
                color: [1.0; 4],
            }),
            &[0, 1, 2],
        )
        .unwrap();
        let strict = cfg!(feature = "development-validation");
        for (segments, index_prefixes) in [(0, 0), (1, 1), (1, 0)] {
            let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
                validation: if strict {
                    D3D12ValidationMode::Required
                } else {
                    D3D12ValidationMode::Disabled
                },
                capacity: D3D12FrameCapacity {
                    executions: 1,
                    results: 1,
                    resources: 8,
                    resource_capacity: 8,
                    operations: 32,
                    provenance_segments_per_resource: segments,
                    index_prefixes_per_resource: index_prefixes,
                    initialized_ranges_per_resource: 32,
                    range_snapshots: 8,
                    buffers: 2,
                    buffer_bytes: 128,
                    upload_bytes: 512,
                    readback_bytes: 512,
                    scratch_bytes: 128,
                },
            })
            .unwrap();
            let monitor = runtime.validation_monitor();
            let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
            if segments == 0 {
                let mut graph = pool.begin().unwrap();
                let buffer = graph.buffer("buffer only", BufferDesc { size: 4 }).unwrap();
                graph
                    .upload_buffer("bytes", buffer, 0, &[1, 2, 3, 4])
                    .unwrap();
                let readback = graph.readback_buffer("bytes", buffer, 0, 4).unwrap();
                let graph = graph.compile().unwrap();
                let mut submission = runtime.submit(&graph).unwrap();
                let mut result = submission.wait(Duration::from_secs(30)).unwrap().unwrap();
                let mut bytes = [0; 4];
                result.readback_into(readback, &mut bytes).unwrap();
                assert_eq!(bytes, [1, 2, 3, 4]);
                drop((result, submission, graph));
            }
            runtime
                .prepare_graph_working_set(
                    None,
                    &[D3D12ImageClass {
                        desc: ImageDesc::rgba8(1, 1),
                        count: 1,
                    }],
                    &[ManagedRasterProgram::mesh().into()],
                    &[],
                )
                .unwrap();
            let (invalid, _) = raster(&mut pool, &mesh, Some(true));
            assert_eq!(
                runtime.prepare(&invalid).err().unwrap().kind(),
                D3D12RuntimeErrorKind::CapacityExhausted
            );
            drop(invalid);
            assert_eq!(
                pool.leased_slots(),
                0,
                "rejected recording must release every Graph pin"
            );

            if segments != 0 && index_prefixes == 0 {
                let (unprepared_prefix, _) = raster(&mut pool, &mesh, Some(false));
                let error = runtime.prepare(&unprepared_prefix).err().unwrap();
                assert_eq!(error.kind(), D3D12RuntimeErrorKind::CapacityExhausted);
                let capacity = error.capacity().unwrap();
                assert_eq!(
                    capacity.resource,
                    crate::capacity::RuntimeCapacityKind::GraphMetadata
                );
                assert_eq!(
                    capacity.recovery,
                    crate::capacity::CapacityRecovery::Reconfigure
                );
                assert_eq!((capacity.limit, capacity.required), (Some(0), Some(1)));
                drop(unprepared_prefix);
                assert_eq!(pool.leased_slots(), 0);
            }
            // Immutable mesh proofs need neither byte segments nor prefix slots.
            // With both stores prepared, generic bytes recover after the split
            // upload exhausted the segment limit.
            let arbitrary = (segments != 0 && index_prefixes != 0).then_some(false);
            let (valid, readback) = raster(&mut pool, &mesh, arbitrary);
            drop(runtime.prepare(&valid).unwrap());
            let mut submission = runtime.submit(&valid).unwrap();
            let mut result = submission.wait(Duration::from_secs(30)).unwrap().unwrap();
            let mut bytes = [0; 4];
            result.readback_into(readback, &mut bytes).unwrap();
            assert_eq!(bytes, [255, 0, 0, 255]);
            drop((result, submission, valid));
            assert_eq!(pool.leased_slots(), 0);
            drop(pool);
            runtime.shutdown().unwrap();
            let report = monitor.snapshot();
            assert!(report.is_clean(), "{report:?}");
            if strict {
                assert!(
                    report.debug_layer_enabled
                        && report.gpu_based_validation_enabled
                        && report.teardown_checked,
                    "{report:?}"
                );
            }
        }
    }
}
