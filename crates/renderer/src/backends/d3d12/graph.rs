//! The first D3D12 lowering: transient buffers, one DIRECT queue, no aliasing.
//!
//! The shared compiled graph owns logical validity and scheduling. Preparation
//! checks backend support and capacity, creates private native operands, and
//! records once. Submission does not revalidate immutable graph contents.

use std::{
    mem::ManuallyDrop,
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(test)]
use windows_core::{Error, HRESULT};

use crate::graph::{CompiledGraph, Operation, PassHandle, ResourceDesc};

use super::resources::{DeviceBuffer, MAX_BUFFER_BYTES};
use super::{D3D12Runtime, D3D12RuntimeError, bindings::*, device::DeviceOwner};

const DROP_TIMEOUT: Duration = Duration::from_secs(5);

struct Plan<'a> {
    graph: u64,
    buffers: Vec<Option<u64>>,
    operations: Vec<PlannedOperation<'a>>,
}

enum PlannedOperation<'a> {
    #[cfg(test)]
    ManagedFixture {
        binding: &'a super::program::D3D12Bindings,
        initialize: Option<super::program::ImageInitialization<'a>>,
    },
    Upload {
        dst: usize,
        offset: u64,
        bytes: &'a [u8],
    },
    Copy {
        src: usize,
        dst: usize,
        src_offset: u64,
        dst_offset: u64,
        size: u64,
    },
    Readback {
        pass: usize,
        src: usize,
        offset: u64,
        size: usize,
    },
}

impl<'a> Plan<'a> {
    fn new(graph: &'a CompiledGraph) -> Result<Self, D3D12RuntimeError> {
        if !graph.exports.is_empty() {
            return Err(D3D12RuntimeError::unsupported(
                "D3D12 Graph export is not implemented at M4-04",
            ));
        }
        let mut plan = Self {
            graph: graph.id,
            buffers: vec![None; graph.resources.len()],
            operations: Vec::with_capacity(graph.order.len()),
        };
        // Only retained operations need native support. An unused image or
        // culled shader operation must not make a buffer-only graph unsupported.
        for &pass in &graph.order {
            let operation = match &graph.passes[pass].operation {
                Operation::UploadBuffer {
                    dst,
                    offset,
                    bytes,
                    source,
                    ..
                } => {
                    plan.use_buffer(graph, *dst)?;
                    PlannedOperation::Upload {
                        dst: *dst,
                        offset: *offset,
                        bytes: &bytes[source.clone()],
                    }
                }
                Operation::CopyBuffer {
                    src,
                    dst,
                    src_offset,
                    dst_offset,
                    size,
                } => {
                    plan.use_buffer(graph, *src)?;
                    plan.use_buffer(graph, *dst)?;
                    PlannedOperation::Copy {
                        src: *src,
                        dst: *dst,
                        src_offset: *src_offset,
                        dst_offset: *dst_offset,
                        size: *size,
                    }
                }
                Operation::ReadbackBuffer { src, offset, size } => {
                    plan.use_buffer(graph, *src)?;
                    let size = usize::try_from(*size).map_err(|_| {
                        D3D12RuntimeError::unsupported("readback exceeds CPU address space")
                    })?;
                    if size > isize::MAX as usize {
                        return Err(D3D12RuntimeError::unsupported(
                            "readback exceeds Rust's maximum allocation length",
                        ));
                    }
                    PlannedOperation::Readback {
                        pass,
                        src: *src,
                        offset: *offset,
                        size,
                    }
                }
                _ => {
                    return Err(D3D12RuntimeError::unsupported(
                        "D3D12 M4-04 supports only live buffer upload, copy and readback operations",
                    ));
                }
            };
            plan.operations.push(operation);
        }
        Ok(plan)
    }

    fn use_buffer(&mut self, graph: &CompiledGraph, index: usize) -> Result<(), D3D12RuntimeError> {
        if self.buffers[index].is_some() {
            return Ok(());
        }
        let resource = &graph.resources[index];
        if resource.import.is_some() {
            return Err(D3D12RuntimeError::unsupported(
                "D3D12 Graph import is not implemented at M4-04",
            ));
        }
        let ResourceDesc::Buffer(desc) = resource.desc else {
            return Err(D3D12RuntimeError::unsupported(
                "D3D12 image resources require M4-05",
            ));
        };
        if desc.size > MAX_BUFFER_BYTES {
            return Err(D3D12RuntimeError::unsupported(
                "buffer exceeds the D3D12 M4-04 supported 2 GiB per-buffer limit",
            ));
        }
        self.buffers[index] = Some(desc.size);
        Ok(())
    }
}

struct Readback {
    pass: usize,
    size: usize,
    buffer: Option<DeviceBuffer>,
}

struct Work {
    graph: u64,
    #[cfg(test)]
    managed_bindings: Vec<super::program::D3D12Bindings>,
    #[cfg(test)]
    resource_leases: Vec<super::program::ExecutionLease>,
    #[cfg(test)]
    depth_targets: Vec<super::program::DepthTarget>,
    buffers: Vec<Option<DeviceBuffer>>,
    staging: Vec<DeviceBuffer>,
    scratch: Vec<DeviceBuffer>,
    readbacks: Vec<Readback>,
    _allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    owner: Arc<DeviceOwner>,
}

impl Work {
    fn prepare(
        owner: Arc<DeviceOwner>,
        plan: Plan<'_>,
        #[cfg(test)] fail_after_first_buffer: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        owner.ensure_active()?;
        let mut buffers = Vec::with_capacity(plan.buffers.len());
        for size in plan.buffers {
            let buffer = size
                .map(|size| DeviceBuffer::new(&owner, size, D3D12_HEAP_TYPE_DEFAULT))
                .transpose()?;
            #[cfg(test)]
            let created = buffer.is_some();
            buffers.push(buffer);
            #[cfg(test)]
            if fail_after_first_buffer && created {
                // A narrow native-creation boundary fault: the first real
                // committed resource has succeeded, later creation fails.
                return Err(owner.native(
                    "injected next committed resource creation",
                    Error::from_hresult(HRESULT(0x8007_000Eu32 as i32)),
                ));
            }
        }
        // SAFETY: The live device and fixed DIRECT type are valid, and every
        // returned COM object immediately enters an owned Rust value.
        let allocator: ID3D12CommandAllocator = unsafe {
            owner
                .raw
                .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
        }
        .map_err(|error| owner.native("ID3D12Device::CreateCommandAllocator", error))?;
        // SAFETY: The matching DIRECT allocator is newly created and private;
        // node zero selects the single-device baseline, with no initial PSO.
        let list: ID3D12GraphicsCommandList = unsafe {
            owner
                .raw
                .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
        }
        .map_err(|error| owner.native("ID3D12Device::CreateCommandList", error))?;
        // SAFETY: Initial zero means unfinished; private work signals exactly
        // value one on this fence after its ExecuteCommandLists call.
        let fence: ID3D12Fence = unsafe { owner.raw.CreateFence(0, D3D12_FENCE_FLAG_NONE) }
            .map_err(|error| owner.native("ID3D12Device::CreateFence", error))?;
        let mut work = Self {
            graph: plan.graph,
            #[cfg(test)]
            managed_bindings: Vec::new(),
            #[cfg(test)]
            resource_leases: Vec::new(),
            #[cfg(test)]
            depth_targets: Vec::new(),
            buffers,
            staging: Vec::new(),
            scratch: Vec::new(),
            readbacks: Vec::new(),
            _allocator: allocator,
            list,
            fence,
            owner,
        };
        let mut states = vec![D3D12_RESOURCE_STATE_COMMON; work.buffers.len()];
        for operation in plan.operations {
            match operation {
                #[cfg(test)]
                PlannedOperation::ManagedFixture {
                    binding,
                    initialize,
                } => {
                    let recording = binding.record_fixture(&work.list, initialize)?;
                    work.resource_leases.extend(recording.leases);
                    work.staging.extend(recording.staging);
                    work.depth_targets.extend(recording.depth_targets);
                    work.readbacks.push(Readback {
                        pass: 0,
                        size: recording.size,
                        buffer: Some(recording.readback),
                    });
                    work.managed_bindings.push(binding.clone());
                }
                PlannedOperation::Upload { dst, offset, bytes } => {
                    let upload =
                        DeviceBuffer::new(&work.owner, bytes.len() as u64, D3D12_HEAP_TYPE_UPLOAD)?;
                    // SAFETY: This exact-sized upload is newly owned here and
                    // no native command can access it before queue submission.
                    // Owned CPU input cannot overlap this new native allocation.
                    unsafe {
                        upload.write(&work.owner, bytes)?;
                    }
                    transition(
                        &work.list,
                        work.buffers[dst].as_ref().unwrap().raw(),
                        &mut states[dst],
                        D3D12_RESOURCE_STATE_COPY_DEST,
                    );
                    // SAFETY: The immutable compiled graph establishes ranges,
                    // and the newly allocated upload has exactly bytes.len().
                    // Both buffers remain owned by work through GPU completion.
                    unsafe {
                        work.list.CopyBufferRegion(
                            work.buffers[dst].as_ref().unwrap().raw(),
                            offset,
                            upload.raw(),
                            0,
                            bytes.len() as u64,
                        );
                    }
                    work.staging.push(upload);
                }
                PlannedOperation::Copy {
                    src,
                    dst,
                    src_offset,
                    dst_offset,
                    size,
                } => {
                    transition(
                        &work.list,
                        work.buffers[src].as_ref().unwrap().raw(),
                        &mut states[src],
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    );
                    if src == dst {
                        // Legacy barriers cannot make one resource both source
                        // and destination. Preserve legal nonoverlapping Graph
                        // self-copy semantics with an independent scratch buffer.
                        let scratch =
                            DeviceBuffer::new(&work.owner, size, D3D12_HEAP_TYPE_DEFAULT)?;
                        let mut state = D3D12_RESOURCE_STATE_COMMON;
                        transition(
                            &work.list,
                            scratch.raw(),
                            &mut state,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        );
                        // SAFETY: Graph guarantees the source range, scratch is
                        // exactly size bytes, and both resources are retained.
                        unsafe {
                            work.list.CopyBufferRegion(
                                scratch.raw(),
                                0,
                                work.buffers[src].as_ref().unwrap().raw(),
                                src_offset,
                                size,
                            );
                        }
                        transition(
                            &work.list,
                            scratch.raw(),
                            &mut state,
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                        );
                        transition(
                            &work.list,
                            work.buffers[dst].as_ref().unwrap().raw(),
                            &mut states[dst],
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        );
                        // SAFETY: Independent source and destination now have
                        // matching native states and checked logical ranges.
                        unsafe {
                            work.list.CopyBufferRegion(
                                work.buffers[dst].as_ref().unwrap().raw(),
                                dst_offset,
                                scratch.raw(),
                                0,
                                size,
                            );
                        }
                        work.scratch.push(scratch);
                    } else {
                        transition(
                            &work.list,
                            work.buffers[dst].as_ref().unwrap().raw(),
                            &mut states[dst],
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        );
                        // SAFETY: Distinct private buffers have the required
                        // states and immutable Graph-validated copy ranges.
                        unsafe {
                            work.list.CopyBufferRegion(
                                work.buffers[dst].as_ref().unwrap().raw(),
                                dst_offset,
                                work.buffers[src].as_ref().unwrap().raw(),
                                src_offset,
                                size,
                            );
                        }
                    }
                }
                PlannedOperation::Readback {
                    pass,
                    src,
                    offset,
                    size,
                } => {
                    let readback =
                        DeviceBuffer::new(&work.owner, size as u64, D3D12_HEAP_TYPE_READBACK)?;
                    transition(
                        &work.list,
                        work.buffers[src].as_ref().unwrap().raw(),
                        &mut states[src],
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    );
                    // SAFETY: The source range is Graph-validated; the private
                    // readback destination is exactly size bytes in COPY_DEST.
                    unsafe {
                        work.list.CopyBufferRegion(
                            readback.raw(),
                            0,
                            work.buffers[src].as_ref().unwrap().raw(),
                            offset,
                            size as u64,
                        );
                    }
                    work.readbacks.push(Readback {
                        pass,
                        size,
                        buffer: Some(readback),
                    });
                }
            }
        }
        // SAFETY: This uniquely owned command list is open and fully recorded.
        unsafe { work.list.Close() }
            .ok()
            .map_err(|error| work.owner.native("ID3D12GraphicsCommandList::Close", error))?;
        Ok(work)
    }

    fn completed(&self) -> Result<bool, D3D12RuntimeError> {
        // SAFETY: The private fence is live and its owner retains the device.
        let value = unsafe { self.fence.GetCompletedValue() };
        if value == u64::MAX {
            return Err(self.owner.removed());
        }
        Ok(value >= 1)
    }

    fn wait(&self, timeout: Duration) -> Result<bool, D3D12RuntimeError> {
        let started = Instant::now();
        loop {
            if self.completed()? {
                return Ok(true);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(false);
            }
            // M4's small baseline uses fence polling without an OS event or
            // hidden queue-idle wait. Native event-backed pooling belongs to .6.
            std::thread::sleep(remaining.min(Duration::from_millis(1)));
        }
    }
}

pub(super) fn transition(
    list: &ID3D12GraphicsCommandList,
    resource: &ID3D12Resource,
    before: &mut D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) {
    if *before == after {
        return;
    }
    let mut barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: *before,
                StateAfter: after,
            }),
        },
    };
    // SAFETY: The tagged union contains Transition, this private buffer is live,
    // and before records its last state within this single command list.
    unsafe {
        list.ResourceBarrier(std::slice::from_ref(&barrier));
        // The generated union deliberately suppresses field Drop. Balance the
        // temporary cloned resource reference after the native call returns.
        ManuallyDrop::drop(&mut (*barrier.Anonymous.Transition).pResource);
    }
    *before = after;
}

/// Recorded but not submitted work. Dropping this value cancels immediately;
/// every native object remains private and has never been used by the GPU.
pub struct D3D12PreparedGraph {
    work: Work,
}

impl D3D12Runtime {
    #[cfg(test)]
    pub(super) fn prepare_managed_fixture(
        &self,
        binding: &super::program::D3D12Bindings,
    ) -> Result<D3D12PreparedGraph, D3D12RuntimeError> {
        self.prepare_fixture(binding, None)
    }

    #[cfg(test)]
    pub(super) fn prepare_sampled_fixture(
        &self,
        binding: &super::program::D3D12Bindings,
        pixels: Option<&[u8]>,
    ) -> Result<D3D12PreparedGraph, D3D12RuntimeError> {
        self.prepare_fixture(
            binding,
            pixels.map(super::program::ImageInitialization::Upload),
        )
    }

    #[cfg(test)]
    pub(super) fn prepare_depth_fixture(
        &self,
        binding: &super::program::D3D12Bindings,
        clear: Option<&super::image::D3D12ImageView>,
    ) -> Result<D3D12PreparedGraph, D3D12RuntimeError> {
        self.prepare_fixture(
            binding,
            clear.map(|view| super::program::ImageInitialization::DepthClear { view }),
        )
    }

    #[cfg(test)]
    fn prepare_fixture(
        &self,
        binding: &super::program::D3D12Bindings,
        initialize: Option<super::program::ImageInitialization<'_>>,
    ) -> Result<D3D12PreparedGraph, D3D12RuntimeError> {
        binding.check_owner(&self.device)?;
        let work = Work::prepare(
            Arc::clone(&self.device),
            Plan {
                graph: 0,
                buffers: vec![],
                operations: vec![PlannedOperation::ManagedFixture {
                    binding,
                    initialize,
                }],
            },
            false,
        )?;
        Ok(D3D12PreparedGraph { work })
    }

    /// Records retained buffer operations without submitting or waiting. Input
    /// validity and ordering come from the shared immutable compiled graph.
    pub fn prepare(&self, graph: &CompiledGraph) -> Result<D3D12PreparedGraph, D3D12RuntimeError> {
        let plan = Plan::new(graph)?;
        let work = Work::prepare(
            Arc::clone(&self.device),
            plan,
            #[cfg(test)]
            false,
        )?;
        Ok(D3D12PreparedGraph { work })
    }

    /// Convenience preparation and asynchronous submission; no GPU wait occurs.
    pub fn submit(&mut self, graph: &CompiledGraph) -> Result<D3D12Submission, D3D12RuntimeError> {
        let prepared = self.prepare(graph)?;
        self.submit_prepared(prepared)
    }

    /// Submits an owned recording. A recording from another Device owner is
    /// rejected before any queue command. Handles sharing the same native
    /// Device owner can submit each other's recordings. Execution and its
    /// completion signal are serialized across those handles.
    pub fn submit_prepared(
        &mut self,
        prepared: D3D12PreparedGraph,
    ) -> Result<D3D12Submission, D3D12RuntimeError> {
        let mut retained = Retained::recorded(prepared.work);
        let work = retained.value().unwrap();
        if !Arc::ptr_eq(&self.device, &work.owner) {
            return Err(D3D12RuntimeError::wrong_owner(
                "prepared Graph belongs to another D3D12 device",
            ));
        }
        self.device.ensure_active()?;
        // The generated interface hierarchy guarantees this parent conversion;
        // no native QueryInterface is needed for an immutable recording.
        let list: ID3D12CommandList = work.list.clone().into();
        let owner = Arc::clone(&work.owner);
        let queue_guard = owner.queue_gate.lock().map_err(|_| {
            owner.mark_stopped();
            D3D12RuntimeError::invalid("D3D12 queue transaction lock was poisoned")
        })?;
        owner.ensure_active()?;
        let signaled = execute_and_signal(
            &mut retained,
            |_| {
                // SAFETY: The list is closed and never previously executed;
                // its matching allocator and resources remain in retained.
                // The queue gate keeps execution and signal indivisible.
                unsafe {
                    owner.queue.ExecuteCommandLists(&[Some(list)]);
                }
            },
            |work| {
                // SAFETY: A private fence starts at zero; this queue signals
                // one after the only execution using these private resources.
                unsafe { owner.queue.Signal(&work.fence, 1) }.ok()
            },
        );
        drop(queue_guard);
        if let Err(error) = signaled {
            let error = owner.native("ID3D12CommandQueue::Signal", error);
            owner.mark_stopped();
            // ExecuteCommandLists returned void. A failed later Signal is not
            // proof of rejection or completion, so never rollback/free this work.
            // Retaining the entire chain also retains diagnostics and the device.
            return Err(error);
        }
        Ok(D3D12Submission { retained })
    }
}

/// Owns all GPU references until its private fence proves completion. Dropping
/// pending work waits up to five seconds for that submission, then stops and
/// retains unproven native work. Normal prepare/submit never waits.
pub struct D3D12Submission {
    retained: Retained<Work>,
}

impl D3D12Submission {
    pub fn poll(&mut self) -> Result<Option<D3D12SubmissionResult>, D3D12RuntimeError> {
        self.wait(Duration::ZERO)
    }

    /// A timeout preserves the complete pending owner for another wait. Device
    /// removal never returns readable data, including UINT64_MAX fence values.
    pub fn wait(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<D3D12SubmissionResult>, D3D12RuntimeError> {
        let work = self
            .retained
            .value()
            .ok_or_else(|| D3D12RuntimeError::invalid("D3D12 submission was already completed"))?;
        if !work.wait(timeout)? {
            return Ok(None);
        }
        // Real fence completion permits teardown even if a different operation
        // stopped the shared owner; it does not permit returning successful data.
        self.retained.complete();
        let work = self.retained.value().unwrap();
        work.owner.check_native_active()?;
        let work = self.retained.take_retired().unwrap();
        let Work {
            graph,
            mut readbacks,
            #[cfg(test)]
            managed_bindings,
            #[cfg(test)]
            resource_leases,
            #[cfg(test)]
            depth_targets,
            owner,
            buffers,
            staging,
            scratch,
            _allocator,
            list,
            fence,
        } = work;
        // Release every completed command/resource reference before possibly
        // releasing the last owner. The owner keeps final-release diagnostics
        // active; a partial pattern with `..` would defer these fields too far.
        #[cfg(test)]
        drop(depth_targets);
        drop((buffers, staging, scratch, list, _allocator, fence));
        #[cfg(test)]
        {
            for lease in &resource_leases {
                lease.complete();
            }
            drop((managed_bindings, resource_leases));
        }
        // Recording order is the Graph's execution order, which need not match
        // pass indices. No command operands refer to these entries after the
        // completed work has retired; index them once for result consumption.
        readbacks.sort_unstable_by_key(|readback| readback.pass);
        let remaining_readbacks = readbacks.len();
        let owner = if readbacks.is_empty() {
            None
        } else {
            Some(owner)
        };
        Ok(Some(D3D12SubmissionResult {
            graph,
            readbacks,
            remaining_readbacks,
            owner,
        }))
    }
}

impl Drop for D3D12Submission {
    fn drop(&mut self) {
        if !self.retained.pending {
            return;
        }
        let Some(work) = self.retained.value() else {
            return;
        };
        match work.wait(DROP_TIMEOUT) {
            Ok(true) => self.retained.complete(),
            _ => {
                work.owner.mark_stopped();
                // Timeout or an unproven native failure must not free resources
                // that the GPU may still access, even during panic unwinding.
            }
        }
    }
}

/// Own the complete native dependency chain before the void Execute call. An
/// unproven Signal/wait failure or unwind must not free that chain.
struct Retained<T> {
    value: Option<T>,
    pending: bool,
}
impl<T> Retained<T> {
    fn recorded(value: T) -> Self {
        Self {
            value: Some(value),
            pending: false,
        }
    }
    fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }
    fn complete(&mut self) {
        self.pending = false;
    }
    fn take_retired(&mut self) -> Option<T> {
        if self.pending {
            None
        } else {
            self.value.take()
        }
    }
}
impl<T> Drop for Retained<T> {
    fn drop(&mut self) {
        if self.pending
            && let Some(value) = self.value.take()
        {
            std::mem::forget(value);
        }
    }
}

// The small boundary is shared with failure-path tests. Neither a failed
// separate Signal nor unwinding after Execute proves commands were rejected.
fn execute_and_signal<T, E>(
    retained: &mut Retained<T>,
    execute: impl FnOnce(&T),
    signal: impl FnOnce(&T) -> Result<(), E>,
) -> Result<(), E> {
    retained.pending = true;
    let work = retained.value().unwrap();
    execute(work);
    signal(work)
}

/// Readback leases exposed only after real fence completion on an active device.
pub struct D3D12SubmissionResult {
    graph: u64,
    readbacks: Vec<Readback>,
    remaining_readbacks: usize,
    owner: Option<Arc<DeviceOwner>>,
}
impl D3D12SubmissionResult {
    /// Copies and consumes a pass's private native readback. Wrong handles and
    /// repeated consumption fail; a Map/allocation failure preserves the lease.
    pub fn readback(&mut self, pass: PassHandle) -> Result<Vec<u8>, D3D12RuntimeError> {
        if pass.graph != self.graph {
            return Err(D3D12RuntimeError::wrong_owner(
                "foreign Graph readback handle",
            ));
        }
        let index = self
            .readbacks
            .binary_search_by_key(&pass.index, |readback| readback.pass)
            .map_err(|_| D3D12RuntimeError::invalid("pass has no readback result"))?;
        let readback = &mut self.readbacks[index];
        let buffer = readback
            .buffer
            .as_ref()
            .ok_or_else(|| D3D12RuntimeError::invalid("readback was already consumed"))?;
        let owner = self.owner.as_ref().unwrap();
        owner.check_native_active()?;
        // SAFETY: Normal Fence completion retired the GPU copy before this
        // result existed. The plan checked this exact readback length against
        // the allocation and Rust slice limits; owner remains retained here.
        let bytes = unsafe { buffer.read(owner, readback.size)? };
        // Removal may occur while a previously completed readback is held or
        // copied. Do not publish bytes as a valid result after native failure.
        owner.check_native_active()?;
        readback.buffer.take();
        self.remaining_readbacks -= 1;
        if self.remaining_readbacks == 0 {
            self.owner.take();
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{BufferDesc, Graph, ImageDesc};
    use std::{cell::Cell, rc::Rc};

    #[cfg(feature = "d3d12-diagnostics")]
    fn copy_graph() -> (CompiledGraph, [PassHandle; 3], PassHandle) {
        let mut graph = Graph::new();
        let a = graph.buffer("a", BufferDesc { size: 32 }).unwrap();
        let b = graph.buffer("b", BufferDesc { size: 32 }).unwrap();
        let upload = graph
            .upload_buffer("upload", a, 4, (0u8..24).collect::<Vec<_>>())
            .unwrap();
        graph.copy_buffer("copy", a, 8, b, 12, 12).unwrap();
        let copied = graph.readback_buffer("copied", b, 12, 12).unwrap();
        let source = graph.readback_buffer("source", a, 4, 8).unwrap();
        let tail = graph.readback_buffer("tail", b, 16, 8).unwrap();
        // Ensure native recording order differs from pass-index order; result
        // lookup must work after completed entries are indexed independently.
        graph.depends_on(copied, tail).unwrap();
        (graph.compile().unwrap(), [copied, source, tail], upload)
    }

    #[test]
    fn plan_uses_shared_culled_order_and_ignores_unused_resources() {
        let mut graph = Graph::new();
        let image = graph.image("unused image", ImageDesc::rgba8(1, 1)).unwrap();
        graph
            .upload_image("culled image upload", image, vec![0; 4])
            .unwrap();
        let buffer = graph.buffer("buffer", BufferDesc { size: 4 }).unwrap();
        graph
            .upload_buffer("upload", buffer, 0, vec![1; 4])
            .unwrap();
        graph.readback_buffer("readback", buffer, 0, 4).unwrap();
        let graph = graph.compile().unwrap();
        let plan = Plan::new(&graph).unwrap();
        assert_eq!(plan.operations.len(), graph.pass_count());
        assert_eq!(plan.buffers, [None, Some(4)]);
    }

    #[test]
    fn plan_rejects_exports_and_backend_capacity_before_native_allocation() {
        let mut graph = Graph::new();
        let buffer = graph.buffer("export", BufferDesc { size: 4 }).unwrap();
        graph
            .upload_buffer("upload", buffer, 0, vec![1; 4])
            .unwrap();
        graph.export_buffer(buffer).unwrap();
        assert!(Plan::new(&graph.compile().unwrap()).is_err());

        let mut graph = Graph::new();
        let buffer = graph
            .buffer(
                "large",
                BufferDesc {
                    size: MAX_BUFFER_BYTES + 4,
                },
            )
            .unwrap();
        graph
            .upload_buffer("upload", buffer, 0, vec![1; 4])
            .unwrap();
        graph.readback_buffer("readback", buffer, 0, 4).unwrap();
        assert!(Plan::new(&graph.compile().unwrap()).is_err());
    }

    #[test]
    fn plan_rejects_live_image_work() {
        let mut graph = Graph::new();
        let image = graph.image("image", ImageDesc::rgba8(1, 1)).unwrap();
        graph
            .upload_image("upload image", image, vec![0; 4])
            .unwrap();
        graph.readback_image("readback image", image).unwrap();
        assert!(Plan::new(&graph.compile().unwrap()).is_err());

        let source = crate::graph::ImportBuffer {
            key: crate::graph::ResourceKey {
                owner: 1,
                index: 0,
                generation: 0,
            },
            revision: 0,
            desc: BufferDesc { size: 4 },
            initialized: std::iter::once(0..4).collect(),
        };
        let mut graph = Graph::new();
        let imported = graph.import_buffer("external buffer", &source).unwrap();
        graph
            .readback_buffer("readback external", imported, 0, 4)
            .unwrap();
        assert!(Plan::new(&graph.compile().unwrap()).is_err());
    }

    struct DropProbe(Rc<Cell<usize>>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn cancellation_and_signal_failure_have_distinct_retirement_contracts() {
        let drops = Rc::new(Cell::new(0));
        drop(Retained::recorded(DropProbe(Rc::clone(&drops))));
        assert_eq!(drops.get(), 1, "unsubmitted work cancels immediately");

        let mut retained = Retained::recorded(DropProbe(Rc::clone(&drops)));
        let executed = Cell::new(false);
        let signal_error = execute_and_signal(
            &mut retained,
            |_| executed.set(true),
            |_| {
                assert!(executed.get());
                Err("Signal failed after Execute")
            },
        );
        assert_eq!(signal_error, Err("Signal failed after Execute"));
        assert!(
            retained.take_retired().is_none(),
            "separate signal failure is no rollback proof"
        );
        assert_eq!(drops.get(), 1);
        // Only subsequent real completion may authorize native destruction.
        retained.complete();
        drop(retained.take_retired());
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn unwinding_after_execute_retains_the_complete_owner_chain() {
        let drops = Rc::new(Cell::new(0));
        let retained_owner = Rc::clone(&drops);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut retained = Retained::recorded(DropProbe(retained_owner));
            let _: Result<(), ()> = execute_and_signal(
                &mut retained,
                |_| panic!("native boundary interrupted after acceptance"),
                |_| Ok(()),
            );
        }));
        assert!(panic.is_err());
        assert_eq!(
            drops.get(),
            0,
            "uncertain native ownership must survive unwind"
        );
        assert_eq!(Rc::strong_count(&drops), 2, "entire owner was retained");
    }

    #[test]
    #[ignore = "requires real D3D12 hardware, Debug Layer and GBV"]
    #[cfg(feature = "d3d12-diagnostics")]
    fn buffer_graph_lifetimes_partial_creation_and_pending_drop() {
        use super::super::{D3D12RuntimeConfig, D3D12RuntimeErrorKind, D3D12ValidationMode};
        let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: D3D12ValidationMode::Required,
        })
        .expect("real D3D12 hardware and strict validation prerequisites");
        let monitor = runtime.validation_monitor();
        let weak = Arc::downgrade(&runtime.device);
        let (graph, [copied, source, tail], upload) = copy_graph();
        let baseline = Arc::strong_count(&runtime.device);

        let canceled = runtime.prepare(&graph).expect("recorded work");
        assert_eq!(Arc::strong_count(&runtime.device), baseline + 1);
        drop(canceled);
        assert_eq!(
            Arc::strong_count(&runtime.device),
            baseline,
            "unsubmitted owner released"
        );

        let error = Work::prepare(
            Arc::clone(&runtime.device),
            Plan::new(&graph).unwrap(),
            true,
        )
        .err()
        .expect("injected failure after first real committed buffer");
        assert_eq!(error.kind(), D3D12RuntimeErrorKind::Native);
        assert!(std::error::Error::source(&error).is_some());
        assert_eq!(
            Arc::strong_count(&runtime.device),
            baseline,
            "partial creation released owner"
        );

        let first = runtime.submit(&graph).expect("first async submit");
        let mut second = runtime.submit(&graph).expect("second async submit");
        let deferred = runtime.shutdown().expect("runtime handle shutdown");
        assert!(
            !deferred.teardown_checked,
            "pending work retains native device and diagnostics"
        );
        // Waiting the second before the first does not release the first's
        // private command allocator, resources, Fence or Device owner.
        let mut result = second
            .wait(Duration::from_secs(30))
            .unwrap()
            .expect("second completion");
        let mut foreign = Graph::new();
        let buffer = foreign.buffer("foreign", BufferDesc { size: 4 }).unwrap();
        foreign
            .upload_buffer("upload", buffer, 0, vec![0; 4])
            .unwrap();
        let foreign_readback = foreign.readback_buffer("readback", buffer, 0, 4).unwrap();
        assert_eq!(
            result.readback(foreign_readback).unwrap_err().kind(),
            D3D12RuntimeErrorKind::WrongOwner
        );
        assert_eq!(
            result.readback(upload).unwrap_err().kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        // Consume independently of declaration order. Failed API calls must
        // neither consume another readback nor release its native owner.
        assert_eq!(
            result.readback(source).unwrap(),
            (0u8..8).collect::<Vec<_>>()
        );
        assert_eq!(
            result.readback(source).unwrap_err().kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        assert!(
            result.owner.is_some(),
            "unconsumed readbacks retain the owner"
        );
        assert_eq!(
            result.readback(copied).unwrap(),
            (4u8..16).collect::<Vec<_>>()
        );
        assert_eq!(
            result.readback(tail).unwrap(),
            (8u8..16).collect::<Vec<_>>()
        );
        assert!(result.owner.is_none(), "last readback releases the owner");
        assert_eq!(
            result.readback(copied).unwrap_err().kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        drop(result);
        drop(second);
        assert!(
            weak.upgrade().is_some(),
            "first submission still owns native dependencies"
        );
        drop(first); // Bounded wait for this submission, then exact retirement.
        assert!(
            weak.upgrade().is_none(),
            "last pending owner released after real completion"
        );
        let report = monitor.snapshot();
        assert!(report.debug_layer_enabled && report.gpu_based_validation_enabled);
        assert!(report.teardown_checked && report.is_clean(), "{report:?}");

        // An empty completed result owns no native readback. Its work's child
        // COM references must be released before the last diagnostic owner.
        let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: D3D12ValidationMode::Required,
        })
        .unwrap();
        let monitor = runtime.validation_monitor();
        let mut pending = runtime.submit(&Graph::new().compile().unwrap()).unwrap();
        assert!(!runtime.shutdown().unwrap().teardown_checked);
        let empty_result = pending.wait(Duration::from_secs(30)).unwrap().unwrap();
        let report = monitor.snapshot();
        assert!(report.teardown_checked && report.is_clean(), "{report:?}");
        drop(empty_result);
    }
}
