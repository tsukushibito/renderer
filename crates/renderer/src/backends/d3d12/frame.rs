//! Bounded native frame storage. Leases move ownership without allocating.

use std::{
    ops::Deref,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    resources::{DeviceBuffer, MAX_BUFFER_BYTES},
};

/// Native and CPU capacities reserved before normal frame processing begins.
#[derive(Clone, Copy, Debug)]
pub struct D3D12FrameCapacity {
    pub executions: usize,
    pub results: usize,
    pub resources: usize,
    pub operations: usize,
    pub buffers: usize,
    pub buffer_bytes: u64,
    pub upload_bytes: u64,
    pub readback_bytes: u64,
    pub scratch_bytes: u64,
}

impl Default for D3D12FrameCapacity {
    fn default() -> Self {
        Self {
            executions: 3,
            results: 6,
            resources: 1024,
            operations: 4096,
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
    pub(super) buffers: Vec<DeviceBuffer>,
    pub(super) upload: DeviceBuffer,
    pub(super) scratch: DeviceBuffer,
    pub(super) states: Vec<D3D12_RESOURCE_STATES>,
    pub(super) resource_map: Vec<usize>,
    pub(super) buffers_used: usize,
    pub(super) upload_used: u64,
    pub(super) scratch_state: D3D12_RESOURCE_STATES,
}
// SAFETY: These free-threaded native objects move as one exclusive lease. Their
// allocator and resources are never reset/reused before actual GPU completion.
unsafe impl Send for ExecutionSlot {}

fn reserved<T>(capacity: usize) -> Result<Vec<T>, D3D12RuntimeError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| D3D12RuntimeError::capacity("frame capacity reservation failed"))?;
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
        let mut buffers = reserved(capacity.buffers)?;
        for _ in 0..capacity.buffers {
            buffers.push(DeviceBuffer::new(
                owner,
                capacity.buffer_bytes,
                D3D12_HEAP_TYPE_DEFAULT,
            )?);
        }
        let mut states = reserved(capacity.buffers)?;
        states.resize(capacity.buffers, D3D12_RESOURCE_STATE_COMMON);
        let mut resource_map = reserved(capacity.resources)?;
        resource_map.resize(capacity.resources, usize::MAX);
        let mut upload = DeviceBuffer::new(owner, capacity.upload_bytes, D3D12_HEAP_TYPE_UPLOAD)?;
        upload.map_persistent(owner, capacity.upload_bytes, false)?;
        Ok(Self {
            allocator,
            list,
            fence,
            next_value: 1,
            buffers,
            upload,
            scratch: DeviceBuffer::new(owner, capacity.scratch_bytes, D3D12_HEAP_TYPE_DEFAULT)?,
            states,
            resource_map,
            buffers_used: 0,
            upload_used: 0,
            scratch_state: D3D12_RESOURCE_STATE_COMMON,
        })
    }
    fn reset(&mut self, owner: &DeviceOwner) -> Result<(), D3D12RuntimeError> {
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
        // Graph requirements initializes the active logical resource map once.
        self.buffers_used = 0;
        self.upload_used = 0;
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
    slot: ExecutionSlot,
    result: Option<ResultLease>,
    target: u64,
}
enum ExecutionEntry {
    Available(ExecutionSlot),
    Leased,
    Detached(DetachedWork),
    Poisoned { _slot: ExecutionSlot },
    Quarantined(DetachedWork),
}
struct PoolState {
    executions: Vec<ExecutionEntry>,
    results: Vec<Option<ResultSlot>>,
}
pub(super) struct FramePool {
    capacity: D3D12FrameCapacity,
    state: Mutex<PoolState>,
    // Declared last so native fields are destroyed while the Device remains live.
    owner: Arc<DeviceOwner>,
}
impl FramePool {
    pub(super) fn new(
        owner: Arc<DeviceOwner>,
        capacity: D3D12FrameCapacity,
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
        let mut executions = reserved(capacity.executions)?;
        for _ in 0..capacity.executions {
            executions.push(ExecutionEntry::Available(ExecutionSlot::new(
                &owner, capacity,
            )?));
        }
        let mut results = reserved(capacity.results)?;
        for _ in 0..capacity.results {
            results.push(Some(ResultSlot::new(&owner, capacity)?));
        }
        Ok(Arc::new(Self {
            capacity,
            state: Mutex::new(PoolState {
                executions,
                results,
            }),
            owner,
        }))
    }
    pub(super) fn capacity(&self) -> D3D12FrameCapacity {
        self.capacity
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
                    let ExecutionEntry::Detached(work) =
                        std::mem::replace(&mut state.executions[index], ExecutionEntry::Leased)
                    else {
                        unreachable!()
                    };
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
                D3D12RuntimeError::capacity("all prepared execution slots are occupied")
            })?;
        let result_index = if needs_result {
            Some(
                state
                    .results
                    .iter()
                    .position(Option::is_some)
                    .ok_or_else(|| {
                        D3D12RuntimeError::capacity("all prepared readback slots are occupied")
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
        };
        if work.slot().next_value == u64::MAX {
            work.phase = Phase::Recorded;
            return Err(D3D12RuntimeError::capacity(
                "frame fence timeline requires explicit renewal",
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
    pool: Arc<FramePool>,
    index: usize,
    slot: Option<ExecutionSlot>,
    result: Option<ResultLease>,
    graph: u64,
    target: u64,
    phase: Phase,
}
impl FrameWork {
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
        if self.phase != Phase::Recording {
            return Err(D3D12RuntimeError::invalid("frame recording is not open"));
        }
        if let Some(result) = self.result_mut() {
            result.readbacks.sort_unstable_by_key(|meta| meta.pass);
        }
        // A failed Close makes this native command list permanently unresettable.
        self.phase = Phase::Poisoned;
        // SAFETY: The sole leased list is recording and all operands are retained.
        unsafe { self.slot().list.Close() }.ok().map_err(|error| {
            self.pool
                .owner
                .native("ID3D12GraphicsCommandList::Close", error)
        })?;
        self.phase = Phase::Recorded;
        Ok(())
    }
    pub(super) fn submit(mut self) -> Result<FrameSubmission, D3D12RuntimeError> {
        if self.phase != Phase::Recorded {
            return Err(D3D12RuntimeError::invalid("frame recording is not closed"));
        }
        let owner = Arc::clone(&self.pool.owner);
        let _gate = owner.queue_gate.lock().map_err(|_| {
            owner.mark_stopped();
            D3D12RuntimeError::invalid("D3D12 queue transaction lock was poisoned")
        })?;
        owner.ensure_active()?;
        let list: ID3D12CommandList = self.slot().list.0.clone().into();
        let fence = self.slot().fence.0.clone();
        let target = self.target;
        let signaled = execute_and_signal(
            &mut self.phase,
            |_| {
                // SAFETY: The closed list, allocator and all operands remain
                // owned by this pending lease; the gate serializes Execute/Signal.
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
        Ok(FrameSubmission { work: Some(self) })
    }
    fn detached(&mut self) -> DetachedWork {
        DetachedWork {
            slot: self.slot.take().unwrap(),
            result: self.result.take(),
            target: self.target,
        }
    }
    fn complete(mut self) -> FrameResult {
        let slot = self.slot.take().unwrap();
        self.pool.state().executions[self.index] = ExecutionEntry::Available(slot);
        let result = self.result.take();
        let remaining = result
            .as_ref()
            .map_or(0, |result| result.slot.readbacks.len());
        if remaining == 0 {
            if let Some(result) = result {
                FramePool::return_result(&mut self.pool.state(), result);
            }
            FrameResult {
                graph: self.graph,
                pool: None,
                result: None,
                remaining: 0,
            }
        } else {
            FrameResult {
                graph: self.graph,
                pool: Some(Arc::clone(&self.pool)),
                result,
                remaining,
            }
        }
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
        if self.phase == Phase::Recording {
            // SAFETY: Cancel owns the sole open list and has never submitted it.
            self.phase = if unsafe { self.slot().list.Close() }.is_ok() {
                Phase::Recorded
            } else {
                Phase::Poisoned
            };
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
    work: Option<FrameWork>,
}
impl FrameSubmission {
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
        Ok(Some(self.work.take().unwrap().complete()))
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
    pool: Option<Arc<FramePool>>,
    result: Option<ResultLease>,
    remaining: usize,
}
impl FrameResult {
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
            result
                .slot
                .buffer
                .read_into_at(owner, meta.offset, destination)?;
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

    use super::{Phase, execute_and_signal};

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
}
