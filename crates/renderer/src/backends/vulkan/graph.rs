//! Native execution of the validated, single-queue M2 graph.
//!
//! The registry owns canonical physical identities. Payload leases retain every
//! recorded reference, and accepted state is distinct from actual completion.

use std::ffi::{CStr, CString};
use std::fmt;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ash::vk;

use super::buffer::{BufferError, BufferRole};
use super::device::{DeviceInitError, DeviceState, VulkanDevice};
use super::diagnostics::Diagnostics;
use super::instance::{InitError, PrerequisiteFailure, VulkanInstance};
use super::memory::{AllocatorInitError, MemoryOwner, VulkanAllocator};
use super::transfer::staging::{SharedStaging, StagingError, StagingRange};
use super::transfer::{ExecutionPool, PendingTransfer, TransferError, TransferPayload};
use crate::graph::{
    AccessPurpose, BufferHandle, CompiledGraph, ComputeTransform, ImageFormat, ImageHandle,
    ImportBuffer, ImportImage, LoadOp, Operation, PassHandle, ResourceDesc, ResourceKey, StoreOp,
};

mod resources;
mod shaders;
use resources::GraphBuffer;
use shaders::{GraphImage, ShaderBindings, ShaderPrograms};

static NEXT_RUNTIME: AtomicU64 = AtomicU64::new(1);

/// Explicit bounds; acquisition reports exhaustion instead of waiting/growing.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfig {
    pub execution_slots: usize,
    pub resource_capacity: usize,
    pub staging_bytes: u64,
    pub max_shader_bindings: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            execution_slots: 4,
            resource_capacity: 64,
            staging_bytes: 4 * 1024 * 1024,
            max_shader_bindings: 64,
        }
    }
}

/// Stable, native-free categories for handling an execution failure.
/// Native allocation OOM is `Native`; bounded Renderer pool exhaustion is
/// `CapacityExhausted`, so applications can distinguish these recovery choices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuntimeErrorKind {
    InvalidInput,
    Unsupported,
    CapacityExhausted,
    StaleResource,
    WrongOwner,
    DeviceLost,
    DeviceUnavailable,
    Native,
}

/// Context and its original error chain, with native API types erased.
#[derive(Debug)]
pub struct RuntimeError {
    pub message: String,
    kind: RuntimeErrorKind,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl RuntimeError {
    pub fn kind(&self) -> RuntimeErrorKind {
        self.kind
    }

    fn context(mut self, context: impl fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    fn classified(kind: RuntimeErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
            source: None,
        }
    }
    pub(super) fn invalid(message: impl Into<String>) -> Self {
        Self::classified(RuntimeErrorKind::InvalidInput, message)
    }
    pub(super) fn unsupported(message: impl Into<String>) -> Self {
        Self::classified(RuntimeErrorKind::Unsupported, message)
    }
    pub(super) fn capacity(message: impl Into<String>) -> Self {
        Self::classified(RuntimeErrorKind::CapacityExhausted, message)
    }
    fn stale(message: impl Into<String>) -> Self {
        Self::classified(RuntimeErrorKind::StaleResource, message)
    }
    fn wrong_owner(message: impl Into<String>) -> Self {
        Self::classified(RuntimeErrorKind::WrongOwner, message)
    }
    fn with_source<E: std::error::Error + Send + Sync + 'static>(
        kind: RuntimeErrorKind,
        source: E,
    ) -> Self {
        Self {
            message: source.to_string(),
            kind,
            source: Some(Box::new(source)),
        }
    }
    pub(super) fn device_unavailable(state: DeviceState) -> Self {
        Self::classified(
            device_kind(state),
            format!("runtime Device is {state:?}; normal use is stopped"),
        )
    }
    pub(super) fn native(
        operation: impl Into<String>,
        device: impl Into<String>,
        result: vk::Result,
    ) -> Self {
        Self::with_source(
            native_kind(result),
            NativeRuntimeCause {
                operation: operation.into(),
                device: device.into(),
                result,
            },
        )
    }
}
impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn std::error::Error + 'static))
    }
}

#[derive(Debug)]
struct NativeRuntimeCause {
    operation: String,
    device: String,
    result: vk::Result,
}
impl fmt::Display for NativeRuntimeCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} on {} failed: {:?}",
            self.operation, self.device, self.result
        )
    }
}
impl std::error::Error for NativeRuntimeCause {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.result)
    }
}

fn native_kind(result: vk::Result) -> RuntimeErrorKind {
    match result {
        vk::Result::ERROR_DEVICE_LOST => RuntimeErrorKind::DeviceLost,
        vk::Result::ERROR_FORMAT_NOT_SUPPORTED
        | vk::Result::ERROR_FEATURE_NOT_PRESENT
        | vk::Result::ERROR_EXTENSION_NOT_PRESENT
        | vk::Result::ERROR_LAYER_NOT_PRESENT
        | vk::Result::ERROR_INCOMPATIBLE_DRIVER => RuntimeErrorKind::Unsupported,
        _ => RuntimeErrorKind::Native,
    }
}
fn device_kind(state: DeviceState) -> RuntimeErrorKind {
    if state == DeviceState::Lost {
        RuntimeErrorKind::DeviceLost
    } else {
        RuntimeErrorKind::DeviceUnavailable
    }
}
fn buffer_error_kind(error: &BufferError) -> RuntimeErrorKind {
    match error {
        BufferError::DeviceUnavailable(state) => device_kind(*state),
        BufferError::Vulkan { result, .. } => native_kind(*result),
        _ => RuntimeErrorKind::InvalidInput,
    }
}
impl From<TransferError> for RuntimeError {
    fn from(error: TransferError) -> Self {
        let kind = match &error {
            TransferError::CapacityExhausted { .. } => RuntimeErrorKind::CapacityExhausted,
            TransferError::DeviceUnavailable(state) => device_kind(*state),
            TransferError::Vulkan { result, .. } => native_kind(*result),
            _ => RuntimeErrorKind::InvalidInput,
        };
        Self::with_source(kind, error)
    }
}
impl From<StagingError> for RuntimeError {
    fn from(error: StagingError) -> Self {
        let kind = match &error {
            StagingError::CapacityExhausted { .. } => RuntimeErrorKind::CapacityExhausted,
            StagingError::DeviceUnavailable(state) => device_kind(*state),
            StagingError::Buffer(error) => buffer_error_kind(error),
            StagingError::Vulkan { result, .. } => native_kind(*result),
            _ => RuntimeErrorKind::InvalidInput,
        };
        Self::with_source(kind, error)
    }
}
impl From<InitError> for RuntimeError {
    fn from(error: InitError) -> Self {
        let kind = match &error {
            InitError::Loader(_) => RuntimeErrorKind::DeviceUnavailable,
            InitError::Prerequisites(failures) => failures
                .iter()
                .find_map(|failure| match failure {
                    PrerequisiteFailure::Query { result, .. } => Some(native_kind(*result)),
                    _ => None,
                })
                .unwrap_or(RuntimeErrorKind::Unsupported),
            InitError::MissingEntryPoint(_) => RuntimeErrorKind::Unsupported,
            InitError::Vulkan { result, .. } => native_kind(*result),
        };
        Self::with_source(kind, error)
    }
}
impl From<DeviceInitError> for RuntimeError {
    fn from(error: DeviceInitError) -> Self {
        let kind = match &error {
            DeviceInitError::NoSuitableDevice(_) => RuntimeErrorKind::Unsupported,
            DeviceInitError::Vulkan { result, .. } => native_kind(*result),
        };
        Self::with_source(kind, error)
    }
}
impl From<AllocatorInitError> for RuntimeError {
    fn from(error: AllocatorInitError) -> Self {
        // The allocator's structured source is its VkResult; preserve both
        // layers rather than replacing its operation with a formatted string.
        let kind = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<vk::Result>())
            .map_or(RuntimeErrorKind::Native, |result| native_kind(*result));
        Self::with_source(kind, error)
    }
}

/// Counters describe bounded reuse, not performance claims.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeStats {
    pub resources_created: usize,
    pub resources_reused: usize,
    pub registered_resources: usize,
    pub execution_slots_created: usize,
    pub execution_slots_reused: usize,
    pub upload_live_ranges: usize,
    pub readback_live_ranges: usize,
    pub descriptor_arenas_created: usize,
    pub descriptor_arenas_reused: usize,
}

#[derive(Clone, Default)]
struct Shadow(Vec<ShadowSegment>);
#[derive(Clone)]
struct ShadowSegment {
    start: u64,
    bytes: Arc<[u8]>,
    range: Range<usize>,
}

impl ShadowSegment {
    fn end(&self) -> u64 {
        self.start + self.range.len() as u64
    }
}

impl Shadow {
    fn write(&mut self, target: Range<u64>, bytes: Option<Arc<[u8]>>) {
        let mut next = Vec::new();
        for segment in self.0.drain(..) {
            if segment.end() <= target.start || segment.start >= target.end {
                next.push(segment);
                continue;
            }
            if segment.start < target.start {
                let count = (target.start - segment.start) as usize;
                next.push(ShadowSegment {
                    start: segment.start,
                    bytes: Arc::clone(&segment.bytes),
                    range: segment.range.start..segment.range.start + count,
                });
            }
            if segment.end() > target.end {
                let skip = (target.end - segment.start) as usize;
                next.push(ShadowSegment {
                    start: target.end,
                    bytes: segment.bytes,
                    range: segment.range.start + skip..segment.range.end,
                });
            }
        }
        if let Some(bytes) = bytes {
            let length = bytes.len();
            next.push(ShadowSegment {
                start: target.start,
                bytes,
                range: 0..length,
            });
        }
        next.sort_by_key(|segment| segment.start);
        self.0 = next;
    }

    fn read(&self, target: Range<u64>) -> Option<Vec<u8>> {
        let length = usize::try_from(target.end.checked_sub(target.start)?).ok()?;
        if length > isize::MAX as usize {
            return None;
        }
        let mut bytes = Vec::with_capacity(length);
        let mut cursor = target.start;
        for segment in &self.0 {
            if segment.end() <= cursor {
                continue;
            }
            if segment.start > cursor {
                return None;
            }
            let end = target.end.min(segment.end());
            let begin = segment.range.start + usize::try_from(cursor - segment.start).ok()?;
            let count = usize::try_from(end - cursor).ok()?;
            bytes.extend_from_slice(&segment.bytes[begin..begin + count]);
            cursor = end;
            if cursor == target.end {
                return Some(bytes);
            }
        }
        (cursor == target.end).then_some(bytes)
    }
}

#[derive(Clone)]
struct Snapshot {
    revision: u64,
    serial: u64,
    initialized: Vec<Range<u64>>,
    layout: vk::ImageLayout,
    stage: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
    writer_stage: vk::PipelineStageFlags2,
    writer_access: vk::AccessFlags2,
    shadow: Shadow,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            serial: 0,
            initialized: Vec::new(),
            layout: vk::ImageLayout::UNDEFINED,
            stage: vk::PipelineStageFlags2::NONE,
            access: vk::AccessFlags2::NONE,
            writer_stage: vk::PipelineStageFlags2::NONE,
            writer_access: vk::AccessFlags2::NONE,
            shadow: Shadow::default(),
        }
    }
}

struct State {
    scheduled: Snapshot,
    completed: Snapshot,
}
enum NativeResource {
    Buffer(GraphBuffer),
    Image(GraphImage),
}
struct Physical {
    owner: Arc<MemoryOwner>,
    desc: ResourceDesc,
    native: NativeResource,
    state: Mutex<State>,
}

impl Physical {
    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                self.owner.device.stop();
                self.state.clear_poison();
                poisoned.into_inner()
            }
        }
    }
    fn buffer(&self) -> &GraphBuffer {
        match &self.native {
            NativeResource::Buffer(value) => value,
            _ => unreachable!("compiled buffer kind"),
        }
    }
    fn image(&self) -> &GraphImage {
        match &self.native {
            NativeResource::Image(value) => value,
            _ => unreachable!("compiled image kind"),
        }
    }
}

struct Entry {
    generation: u32,
    registered: bool,
    physical: Arc<Physical>,
}

struct Registration<'a> {
    entries: &'a mut [Entry],
    indices: Vec<usize>,
    keep: bool,
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if !self.keep {
            for &index in &self.indices {
                self.entries[index].registered = false;
            }
        }
    }
}

/// A validation-required headless Vulkan 1.3 runtime. Graphs and imports contain
/// no Vulkan handles. One mutable submit transaction serializes state planning.
pub struct VulkanRuntime {
    owner: Arc<MemoryOwner>,
    diagnostics: Arc<Diagnostics>,
    execution: ExecutionPool,
    upload: SharedStaging,
    readback: SharedStaging,
    programs: Option<Arc<ShaderPrograms>>,
    entries: Vec<Entry>,
    id: u64,
    serial: u64,
    config: RuntimeConfig,
    resources_created: usize,
    resources_reused: usize,
}

impl VulkanRuntime {
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        if config.execution_slots == 0
            || config.resource_capacity == 0
            || config.resource_capacity > u32::MAX as usize
            || config.max_shader_bindings == 0
        {
            return Err(RuntimeError::invalid(
                "runtime capacities must be nonzero and resource capacity fit u32",
            ));
        }
        let diagnostics = Diagnostics::new();
        let instance = VulkanInstance::new(Arc::clone(&diagnostics))?;
        let device = VulkanDevice::new(instance)?;
        let allocator = VulkanAllocator::new(device)?;
        let owner = allocator.owner;
        let execution = ExecutionPool::new(Arc::clone(&owner), config.execution_slots)?;
        let upload =
            SharedStaging::new(Arc::clone(&owner), BufferRole::Upload, config.staging_bytes)?;
        let readback = SharedStaging::new(
            Arc::clone(&owner),
            BufferRole::Readback,
            config.staging_bytes,
        )?;
        let id = NEXT_RUNTIME
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| RuntimeError::capacity("runtime identity exhausted"))?;
        Ok(Self {
            owner,
            diagnostics,
            execution,
            upload,
            readback,
            programs: None,
            entries: Vec::new(),
            id,
            serial: 0,
            config,
            resources_created: 0,
            resources_reused: 0,
        })
    }

    pub fn device_name(&self) -> &str {
        self.owner.device.name()
    }

    pub fn stats(&self) -> RuntimeStats {
        let execution = self.execution.stats();
        let shader = self.programs.as_ref().map(|programs| programs.stats());
        RuntimeStats {
            resources_created: self.resources_created,
            resources_reused: self.resources_reused,
            registered_resources: self.entries.iter().filter(|entry| entry.registered).count(),
            execution_slots_created: execution.native_creates,
            execution_slots_reused: execution.reuses,
            upload_live_ranges: self.upload.stats().live_ranges,
            readback_live_ranges: self.readback.stats().live_ranges,
            descriptor_arenas_created: shader.map_or(0, |stats| stats.descriptor_arenas_created),
            descriptor_arenas_reused: shader.map_or(0, |stats| stats.descriptor_arenas_reused),
        }
    }

    /// Fresh scheduled snapshot. This may refer to accepted, still-pending work.
    pub fn refresh_buffer(&self, key: ResourceKey) -> Result<ImportBuffer, RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let physical = &self.entry(key)?.physical;
        let ResourceDesc::Buffer(desc) = physical.desc else {
            return Err(RuntimeError::invalid("resource is not a buffer"));
        };
        let state = physical.lock();
        Ok(ImportBuffer {
            key,
            revision: state.scheduled.revision,
            desc,
            initialized: state.scheduled.initialized.clone(),
        })
    }
    pub fn refresh_image(&self, key: ResourceKey) -> Result<ImportImage, RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let physical = &self.entry(key)?.physical;
        let ResourceDesc::Image(desc) = physical.desc else {
            return Err(RuntimeError::invalid("resource is not an image"));
        };
        let state = physical.lock();
        Ok(ImportImage {
            key,
            revision: state.scheduled.revision,
            desc,
            initialized: state.scheduled.initialized.len() == 1
                && state.scheduled.initialized[0] == (0..1),
        })
    }

    /// Invalidate the logical handle immediately. Recorded/pending references
    /// keep the allocation alive, and prevent its acquisition for another owner.
    pub fn delete(&mut self, key: ResourceKey) -> Result<(), RuntimeError> {
        self.entry(key)?;
        self.entries[key.index as usize].registered = false;
        Ok(())
    }

    fn entry(&self, key: ResourceKey) -> Result<&Entry, RuntimeError> {
        if key.owner != self.id {
            return Err(RuntimeError::wrong_owner(
                "resource belongs to another runtime/Device",
            ));
        }
        self.entries
            .get(key.index as usize)
            .filter(|entry| entry.registered && entry.generation == key.generation)
            .ok_or_else(|| RuntimeError::stale("deleted or stale resource generation"))
    }

    fn acquire(
        &mut self,
        desc: ResourceDesc,
        name: &str,
    ) -> Result<(ResourceKey, Arc<Physical>), RuntimeError> {
        if let Some((index, entry)) = self.entries.iter_mut().enumerate().find(|(_, entry)| {
            !entry.registered
                && Arc::strong_count(&entry.physical) == 1
                && entry.physical.desc == desc
                && entry.generation < u32::MAX
        }) {
            entry.generation += 1;
            let mut state = entry.physical.lock();
            // Preserve actual native layout/access, but never inherit old bytes
            // as initialized content of a new logical resource.
            state.scheduled.initialized.clear();
            state.scheduled.shadow = Shadow::default();
            state.completed.initialized.clear();
            state.completed.shadow = Shadow::default();
            drop(state);
            self.resources_reused += 1;
            return Ok((
                ResourceKey {
                    owner: self.id,
                    index: index as u32,
                    generation: entry.generation,
                },
                Arc::clone(&entry.physical),
            ));
        }
        let replace = self.entries.iter().position(|entry| {
            !entry.registered
                && Arc::strong_count(&entry.physical) == 1
                && entry.generation < u32::MAX
        });
        if replace.is_none() && self.entries.len() >= self.config.resource_capacity {
            return Err(RuntimeError::capacity(
                "graph physical resource capacity exhausted",
            ));
        }
        let native = match desc {
            ResourceDesc::Buffer(desc) => NativeResource::Buffer(
                GraphBuffer::new(Arc::clone(&self.owner), desc.size, name)
                    .map_err(|error| error.context(format!("resource {name:?}")))?,
            ),
            ResourceDesc::Image(desc) => NativeResource::Image(
                GraphImage::new(
                    Arc::clone(&self.owner),
                    desc.width,
                    desc.height,
                    match desc.format {
                        ImageFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
                        ImageFormat::Depth32Float => vk::Format::D32_SFLOAT,
                    },
                    name,
                )
                .map_err(|error| error.context(format!("resource {name:?}")))?,
            ),
        };
        let physical = Arc::new(Physical {
            owner: Arc::clone(&self.owner),
            desc,
            native,
            state: Mutex::new(State {
                scheduled: Snapshot::default(),
                completed: Snapshot::default(),
            }),
        });
        let index = replace.unwrap_or(self.entries.len());
        let generation = replace.map_or(1, |index| self.entries[index].generation + 1);
        let entry = Entry {
            generation,
            registered: false,
            physical: Arc::clone(&physical),
        };
        if replace.is_some() {
            self.entries[index] = entry;
        } else {
            self.entries.push(entry);
        }
        self.resources_created += 1;
        Ok((
            ResourceKey {
                owner: self.id,
                index: index as u32,
                generation,
            },
            physical,
        ))
    }

    /// Records one compiled graph and submits it once. It does not wait for any
    /// prior graph. Imported snapshots must still match canonical accepted state.
    pub fn submit(&mut self, graph: &CompiledGraph) -> Result<Submission, RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let serial = self
            .serial
            .checked_add(1)
            .ok_or_else(|| RuntimeError::capacity("submission serial exhausted"))?;
        // Reserve native capacity before accepting any other bounded resources.
        let reservation = self.execution.reserve()?;
        let mut needed = vec![false; graph.resources.len()];
        for &pass in &graph.order {
            if let Operation::Declared(kind) = &graph.passes[pass].operation {
                return Err(RuntimeError::unsupported(format!(
                    "pass {} is declaration-only {kind:?}; native execution unsupported",
                    graph.passes[pass].name
                )));
            }
            for access in &graph.passes[pass].accesses {
                needed[access.resource] = true;
            }
        }
        for &resource in &graph.exports {
            needed[resource] = true;
        }
        // Validate all imports, including culled declarations, before allocation.
        for resource in &graph.resources {
            if let Some(key) = resource.import {
                let physical = &self.entry(key)?.physical;
                let state = physical.lock();
                if physical.desc != resource.desc
                    || state.scheduled.revision != resource.import_revision
                    || state.scheduled.initialized != resource.initial
                {
                    return Err(RuntimeError::stale(format!(
                        "stale or inconsistent import snapshot for {}; refresh and recompile",
                        resource.name
                    )));
                }
            }
        }
        let mut physical = vec![None; graph.resources.len()];
        let mut keys = vec![None; graph.resources.len()];
        for (index, resource) in graph.resources.iter().enumerate() {
            if !needed[index] {
                continue;
            }
            let (key, value) = if let Some(key) = resource.import {
                (key, Arc::clone(&self.entry(key)?.physical))
            } else {
                self.acquire(resource.desc, &resource.name)?
            };
            keys[index] = Some(key);
            physical[index] = Some(value);
        }
        let uses_shaders = graph.order.iter().any(|&pass| {
            matches!(
                graph.passes[pass].operation,
                Operation::Compute { .. } | Operation::Raster { .. } | Operation::Composite { .. }
            )
        });
        if uses_shaders && self.programs.is_none() {
            self.programs = Some(ShaderPrograms::new(
                Arc::clone(&self.owner),
                self.config.execution_slots,
                self.config.max_shader_bindings,
            )?);
        }
        let programs = uses_shaders.then(|| Arc::clone(self.programs.as_ref().unwrap()));
        let bindings = programs
            .as_ref()
            .map(|programs| programs.acquire_bindings())
            .transpose()?;
        let work = GraphWork::prepare(
            Arc::clone(&self.owner),
            graph,
            serial,
            physical,
            programs,
            bindings,
            &self.upload,
            &self.readback,
        )?;
        let mut exports = Vec::new();
        for &index in &graph.exports {
            let key = keys[index].unwrap();
            let snapshot = work.final_states[index].as_ref().unwrap();
            exports.push((
                index,
                export_snapshot(key, graph.resources[index].desc, snapshot),
            ));
        }
        // Slots become externally named only for an actual export. The local
        // payload leases already prevent reuse throughout this transaction.
        let new_exports = graph
            .exports
            .iter()
            .filter_map(|&index| {
                let key = keys[index].unwrap();
                (!self.entries[key.index as usize].registered).then_some(key.index as usize)
            })
            .collect::<Vec<_>>();
        let recorded = reservation.record(work)?;
        for &index in &new_exports {
            self.entries[index].registered = true;
        }
        // Consume the serial before submit/logging: even if an application
        // subscriber panics after acceptance, a subsequent transaction cannot
        // reuse this revision and accidentally accept an obsolete snapshot.
        self.serial = serial;
        // Rejected submit or an application logging unwind must not leave
        // unreachable exported names occupying the bounded registry forever.
        // This guard changes registration only, never accepted physical state.
        let mut registration = Registration {
            entries: &mut self.entries,
            indices: new_exports,
            keep: false,
        };
        let pending = recorded.submit()?;
        registration.keep = true;
        drop(registration);
        Ok(Submission {
            graph: graph.id,
            pending,
            exports,
        })
    }
}

enum Export {
    Buffer(ImportBuffer),
    Image(ImportImage),
}
fn export_snapshot(key: ResourceKey, desc: ResourceDesc, snapshot: &Snapshot) -> Export {
    match desc {
        ResourceDesc::Buffer(desc) => Export::Buffer(ImportBuffer {
            key,
            revision: snapshot.revision,
            desc,
            initialized: snapshot.initialized.clone(),
        }),
        ResourceDesc::Image(desc) => Export::Image(ImportImage {
            key,
            revision: snapshot.revision,
            desc,
            initialized: snapshot.initialized.len() == 1 && snapshot.initialized[0] == (0..1),
        }),
    }
}

/// Owns a pending submission, never a CPU-readiness promise. Its bounded Drop
/// cleanup follows the existing transfer retirement contract.
pub struct Submission {
    graph: u64,
    pending: PendingTransfer<GraphWork>,
    exports: Vec<(usize, Export)>,
}

impl Submission {
    pub fn export_buffer(&self, handle: BufferHandle) -> Result<ImportBuffer, RuntimeError> {
        if handle.graph != self.graph {
            return Err(RuntimeError::wrong_owner("foreign graph export handle"));
        }
        self.exports
            .iter()
            .find_map(|(index, value)| match value {
                Export::Buffer(value) if *index == handle.index => Some(value.clone()),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::invalid("buffer was not exported"))
    }
    pub fn export_image(&self, handle: ImageHandle) -> Result<ImportImage, RuntimeError> {
        if handle.graph != self.graph {
            return Err(RuntimeError::wrong_owner("foreign graph export handle"));
        }
        self.exports
            .iter()
            .find_map(|(index, value)| match value {
                Export::Image(value) if *index == handle.index => Some(value.clone()),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::invalid("image was not exported"))
    }
    /// Timeout retains all pending resources. Only a real successful Fence wait
    /// on an active Device yields readable output.
    pub fn wait(&mut self, timeout: Duration) -> Result<Option<SubmissionResult>, RuntimeError> {
        Ok(self.pending.wait(timeout)?.map(|work| SubmissionResult {
            graph: self.graph,
            work,
        }))
    }
}

pub struct SubmissionResult {
    graph: u64,
    work: GraphWork,
}
impl SubmissionResult {
    /// Copies and releases this pass's readback lease. A failed read preserves
    /// the lease for retry. No native pointer or mapped memory escapes.
    pub fn readback(&mut self, pass: PassHandle) -> Result<Vec<u8>, RuntimeError> {
        if pass.graph != self.graph {
            return Err(RuntimeError::wrong_owner("foreign graph readback handle"));
        }
        let (_, range) = self
            .work
            .readbacks
            .iter_mut()
            .find(|(index, _)| *index == pass.index)
            .ok_or_else(|| RuntimeError::invalid("pass has no readback result"))?;
        let value = range
            .as_mut()
            .ok_or_else(|| RuntimeError::invalid("readback was already consumed"))?;
        let length = usize::try_from(value.size())
            .map_err(|_| RuntimeError::unsupported("readback exceeds CPU address space"))?;
        let mut bytes = vec![0; length];
        value.read(&mut bytes)?;
        range.take();
        Ok(bytes)
    }
}

struct PreparedPass {
    name: CString,
    operation: Operation,
    buffers: Vec<vk::BufferMemoryBarrier2<'static>>,
    images: Vec<vk::ImageMemoryBarrier2<'static>>,
    staging: Option<usize>,
    descriptor: Option<vk::DescriptorSet>,
}

struct GraphWork {
    owner: Arc<MemoryOwner>,
    physical: Vec<Option<Arc<Physical>>>,
    final_states: Vec<Option<Snapshot>>,
    completed_states: Vec<Option<Snapshot>>,
    passes: Vec<PreparedPass>,
    uploads: Vec<StagingRange>,
    readbacks: Vec<(usize, Option<StagingRange>)>,
    programs: Option<Arc<ShaderPrograms>>,
    bindings: Option<ShaderBindings>,
}

impl GraphWork {
    #[allow(clippy::too_many_arguments)]
    fn prepare(
        owner: Arc<MemoryOwner>,
        graph: &CompiledGraph,
        serial: u64,
        physical: Vec<Option<Arc<Physical>>>,
        programs: Option<Arc<ShaderPrograms>>,
        mut bindings: Option<ShaderBindings>,
        upload: &SharedStaging,
        readback: &SharedStaging,
    ) -> Result<Self, RuntimeError> {
        let mut states = physical
            .iter()
            .map(|value| value.as_ref().map(|value| value.lock().scheduled.clone()))
            .collect::<Vec<_>>();
        let mut passes = Vec::new();
        let mut uploads = Vec::new();
        let mut readbacks = Vec::new();
        for &index in &graph.order {
            let pass = &graph.passes[index];
            let mut prepared = PreparedPass {
                name: CString::new(pass.name.as_str())
                    .map_err(|_| RuntimeError::invalid("pass debug name contains a NUL byte"))?,
                operation: pass.operation.clone(),
                buffers: Vec::new(),
                images: Vec::new(),
                staging: None,
                descriptor: None,
            };
            // Aggregate one pass's uses before modifying previous-access state;
            // disjoint self-copies must not appear to depend on themselves.
            let mut uses = vec![None; physical.len()];
            for access in &pass.accesses {
                let current = usage(access.purpose);
                let target = uses[access.resource].get_or_insert(Use::default());
                target.stage |= current.stage;
                target.access |= current.access;
                target.writes |= access.write;
                if current.layout != vk::ImageLayout::UNDEFINED {
                    target.layout = current.layout;
                }
            }
            for (resource, current) in uses
                .into_iter()
                .enumerate()
                .filter_map(|(index, value)| value.map(|value| (index, value)))
            {
                let native = physical[resource].as_ref().unwrap();
                let previous = states[resource].as_mut().unwrap();
                let layout_change = matches!(native.native, NativeResource::Image(_))
                    && previous.layout != current.layout;
                let (source_stage, source_access) = source_scope(previous, current, layout_change);
                let dependency = !source_stage.is_empty();
                match &native.native {
                    NativeResource::Buffer(buffer) if dependency => {
                        prepared.buffers.push(
                            vk::BufferMemoryBarrier2::default()
                                .src_stage_mask(source_stage)
                                .src_access_mask(source_access)
                                .dst_stage_mask(current.stage)
                                .dst_access_mask(current.access)
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .buffer(buffer.handle)
                                .offset(0)
                                .size(buffer.size),
                        );
                    }
                    NativeResource::Image(image) if dependency || layout_change => {
                        prepared.images.push(
                            vk::ImageMemoryBarrier2::default()
                                .src_stage_mask(source_stage)
                                .src_access_mask(source_access)
                                .dst_stage_mask(current.stage)
                                .dst_access_mask(current.access)
                                .old_layout(previous.layout)
                                .new_layout(current.layout)
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .image(image.image)
                                .subresource_range(
                                    vk::ImageSubresourceRange::default()
                                        .aspect_mask(image.aspect)
                                        .level_count(1)
                                        .layer_count(1),
                                ),
                        );
                    }
                    _ => {}
                }
                if !current.writes && !layout_change {
                    previous.stage |= current.stage;
                    previous.access |= current.access;
                } else {
                    previous.stage = current.stage;
                    previous.access = current.access;
                }
                if current.writes {
                    previous.writer_stage = current.stage;
                    previous.writer_access = current.access;
                }
                if matches!(native.native, NativeResource::Image(_)) {
                    previous.layout = current.layout;
                }
            }
            (|| -> Result<(), RuntimeError> {
                match &pass.operation {
                    Operation::UploadBuffer { dst, offset, bytes } => {
                        let mut range = upload.allocate(bytes.len() as u64)?;
                        range.write(bytes)?;
                        prepared.staging = Some(uploads.len());
                        uploads.push(range);
                        states[*dst].as_mut().unwrap().shadow.write(
                            *offset..*offset + bytes.len() as u64,
                            Some(Arc::from(bytes.clone())),
                        );
                    }
                    Operation::CopyBuffer {
                        src,
                        dst,
                        src_offset,
                        dst_offset,
                        size,
                    } => {
                        let bytes = states[*src]
                            .as_ref()
                            .unwrap()
                            .shadow
                            .read(*src_offset..*src_offset + *size)
                            .map(Arc::from);
                        states[*dst]
                            .as_mut()
                            .unwrap()
                            .shadow
                            .write(*dst_offset..*dst_offset + *size, bytes);
                    }
                    Operation::UploadImage { bytes, .. } => {
                        let mut range = upload.allocate(bytes.len() as u64)?;
                        range.write(bytes)?;
                        prepared.staging = Some(uploads.len());
                        uploads.push(range);
                    }
                    Operation::ReadbackBuffer { size, .. } => {
                        prepared.staging = Some(readbacks.len());
                        readbacks.push((index, Some(readback.allocate(*size)?)));
                    }
                    Operation::ReadbackImage { src } => {
                        let ResourceDesc::Image(desc) = graph.resources[*src].desc else {
                            unreachable!()
                        };
                        prepared.staging = Some(readbacks.len());
                        readbacks.push((index, Some(readback.allocate(desc.byte_len().unwrap())?)));
                    }
                    Operation::Compute { src, dst, .. } => {
                        prepared.descriptor = Some(bindings.as_mut().unwrap().compute(
                            physical[*src].as_ref().unwrap().image(),
                            physical[*dst].as_ref().unwrap().image(),
                        )?);
                    }
                    Operation::Composite { src, .. } => {
                        prepared.descriptor = Some(
                            bindings
                                .as_mut()
                                .unwrap()
                                .composite(physical[*src].as_ref().unwrap().image())?,
                        );
                    }
                    Operation::Raster {
                        vertices,
                        indices,
                        index_count,
                        ..
                    } => {
                        let bytes = states[*indices]
                            .as_ref()
                            .unwrap()
                            .shadow
                            .read(0..u64::from(*index_count) * 4)
                            .ok_or_else(|| {
                                RuntimeError::unsupported(
                                    "raster indices must have CPU-known upload/copy contents",
                                )
                            })?;
                        let vertex_count = graph.resources[*vertices].desc.units() / 28;
                        if bytes
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .any(|bytes| u64::from(u32::from_le_bytes(*bytes)) >= vertex_count)
                        {
                            return Err(RuntimeError::invalid(
                                "raster index exceeds declared vertex count",
                            ));
                        }
                    }
                    Operation::CopyImage { .. } => {}
                    Operation::Declared(_) => {
                        return Err(RuntimeError::unsupported(
                            "declaration-only pass is not executable",
                        ));
                    }
                }
                Ok(())
            })()
            .map_err(|error| error.context(format!("pass {:?}", pass.name)))?;
            passes.push(prepared);
        }
        for (index, state) in states.iter_mut().enumerate() {
            if let Some(state) = state {
                state.revision = serial;
                state.serial = serial;
                state.initialized = graph.final_contents[index].clone();
            }
        }
        let completed_states = states.clone();
        Ok(Self {
            owner,
            physical,
            final_states: states,
            completed_states,
            passes,
            uploads,
            readbacks,
            programs,
            bindings,
        })
    }

    fn resource(&self, index: usize) -> &Physical {
        self.physical[index].as_ref().unwrap()
    }
}

#[derive(Clone, Copy)]
struct Use {
    stage: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
    layout: vk::ImageLayout,
    writes: bool,
}
impl Default for Use {
    fn default() -> Self {
        Self {
            stage: vk::PipelineStageFlags2::NONE,
            access: vk::AccessFlags2::NONE,
            layout: vk::ImageLayout::UNDEFINED,
            writes: false,
        }
    }
}

fn usage(purpose: AccessPurpose) -> Use {
    use AccessPurpose::*;
    let (stage, access, layout) = match purpose {
        CopySource => (
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        ),
        CopyDestination => (
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_WRITE,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        ),
        StorageRead => (
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ,
            vk::ImageLayout::GENERAL,
        ),
        StorageWrite => (
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::ImageLayout::GENERAL,
        ),
        StorageReadWrite => (
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::ImageLayout::GENERAL,
        ),
        Sampled => (
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        ),
        Vertex => (
            vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
            vk::AccessFlags2::VERTEX_ATTRIBUTE_READ,
            vk::ImageLayout::UNDEFINED,
        ),
        Index => (
            vk::PipelineStageFlags2::INDEX_INPUT,
            vk::AccessFlags2::INDEX_READ,
            vk::ImageLayout::UNDEFINED,
        ),
        ColorAttachment => (
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_READ | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        ),
        DepthAttachment => (
            vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
            vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_READ
                | vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
            vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
        ),
    };
    Use {
        stage,
        access,
        layout,
        writes: false,
    }
}

fn source_scope(
    previous: &Snapshot,
    current: Use,
    layout_change: bool,
) -> (vk::PipelineStageFlags2, vk::AccessFlags2) {
    if current.writes || layout_change {
        (
            previous.stage | previous.writer_stage,
            previous.access | previous.writer_access,
        )
    } else {
        // A COPY read does not by itself make an older COPY write visible to
        // vertex/index/shader reads. Preserve the last writer across readers,
        // including imports, and expose it to each actual destination access.
        (previous.writer_stage, previous.writer_access)
    }
}

impl TransferPayload for GraphWork {
    fn validate(&self) -> Result<(), TransferError> {
        self.owner
            .device
            .ensure_active()
            .map_err(TransferError::DeviceUnavailable)
    }
    fn owner(&self) -> &Arc<MemoryOwner> {
        &self.owner
    }
    fn bytes(&self) -> u64 {
        self.uploads
            .iter()
            .chain(
                self.readbacks
                    .iter()
                    .filter_map(|(_, range)| range.as_ref()),
            )
            .map(StagingRange::size)
            .sum()
    }
    fn label(&self) -> &'static CStr {
        c"renderer.graph"
    }

    unsafe fn record(&self, command: vk::CommandBuffer) {
        let device = &self.owner.device;
        for pass in &self.passes {
            // SAFETY: Every command operand is validated and retained in this
            // payload. Pass boundaries lie outside all dynamic rendering scopes.
            unsafe {
                device.debug_utils().cmd_begin_debug_utils_label(
                    command,
                    &vk::DebugUtilsLabelEXT::default().label_name(&pass.name),
                );
                if !pass.buffers.is_empty() || !pass.images.is_empty() {
                    device.raw().cmd_pipeline_barrier2(
                        command,
                        &vk::DependencyInfo::default()
                            .buffer_memory_barriers(&pass.buffers)
                            .image_memory_barriers(&pass.images),
                    );
                }
                match &pass.operation {
                    Operation::UploadBuffer { dst, offset, .. } => {
                        let range = &self.uploads[pass.staging.unwrap()];
                        device.raw().cmd_copy_buffer(
                            command,
                            range.transfer_handle(),
                            self.resource(*dst).buffer().handle,
                            &[vk::BufferCopy::default()
                                .src_offset(range.offset())
                                .dst_offset(*offset)
                                .size(range.size())],
                        );
                    }
                    Operation::CopyBuffer {
                        src,
                        dst,
                        src_offset,
                        dst_offset,
                        size,
                    } => device.raw().cmd_copy_buffer(
                        command,
                        self.resource(*src).buffer().handle,
                        self.resource(*dst).buffer().handle,
                        &[vk::BufferCopy::default()
                            .src_offset(*src_offset)
                            .dst_offset(*dst_offset)
                            .size(*size)],
                    ),
                    Operation::ReadbackBuffer { src, offset, size } => {
                        let range = self.readbacks[pass.staging.unwrap()].1.as_ref().unwrap();
                        device.raw().cmd_copy_buffer(
                            command,
                            self.resource(*src).buffer().handle,
                            range.transfer_handle(),
                            &[vk::BufferCopy::default()
                                .src_offset(*offset)
                                .dst_offset(range.offset())
                                .size(*size)],
                        );
                    }
                    Operation::UploadImage { dst, .. } => {
                        let range = &self.uploads[pass.staging.unwrap()];
                        let image = self.resource(*dst).image();
                        device.raw().cmd_copy_buffer_to_image(
                            command,
                            range.transfer_handle(),
                            image.image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[image_copy(image, range.offset())],
                        );
                    }
                    Operation::CopyImage { src, dst } => {
                        let source = self.resource(*src).image();
                        let target = self.resource(*dst).image();
                        device.raw().cmd_copy_image(
                            command,
                            source.image,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            target.image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[vk::ImageCopy::default()
                                .src_subresource(layers(source))
                                .dst_subresource(layers(target))
                                .extent(vk::Extent3D {
                                    width: source.extent.width,
                                    height: source.extent.height,
                                    depth: 1,
                                })],
                        );
                    }
                    Operation::ReadbackImage { src } => {
                        let range = self.readbacks[pass.staging.unwrap()].1.as_ref().unwrap();
                        let image = self.resource(*src).image();
                        device.raw().cmd_copy_image_to_buffer(
                            command,
                            image.image,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            range.transfer_handle(),
                            &[image_copy(image, range.offset())],
                        );
                    }
                    Operation::Compute { dst, transform, .. } => {
                        self.programs.as_ref().unwrap().record_compute(
                            command,
                            pass.descriptor.unwrap(),
                            self.resource(*dst).image(),
                            match transform {
                                ComputeTransform::Invert => 0,
                                ComputeTransform::SwapRedBlue => 1,
                            },
                        )
                    }
                    Operation::Raster {
                        vertices,
                        indices,
                        index_count,
                        color,
                        depth,
                        color_ops,
                        depth_ops,
                        clear_color,
                        clear_depth,
                    } => self.programs.as_ref().unwrap().record_raster(
                        command,
                        self.resource(*vertices).buffer().handle,
                        self.resource(*indices).buffer().handle,
                        *index_count,
                        self.resource(*color).image(),
                        self.resource(*depth).image(),
                        load(color_ops.load),
                        store(color_ops.store),
                        *clear_color,
                        load(depth_ops.load),
                        store(depth_ops.store),
                        *clear_depth,
                    ),
                    Operation::Composite { dst, .. } => {
                        self.programs.as_ref().unwrap().record_composite(
                            command,
                            pass.descriptor.unwrap(),
                            self.resource(*dst).image(),
                        )
                    }
                    Operation::Declared(_) => {
                        unreachable!("unsupported operation rejected before recording")
                    }
                }
                device.debug_utils().cmd_end_debug_utils_label(command);
            }
        }
        if !self.readbacks.is_empty() {
            // SAFETY: Only retained copy-written readback ranges are host-read.
            // CPU access additionally requires successful Fence wait/invalidate.
            unsafe {
                super::transfer::record_host_read(device, command);
            }
        }
    }

    fn submitted(&mut self) {
        // No allocation, logging or application callbacks at this commit point.
        for (index, physical) in self.physical.iter().enumerate() {
            if let Some(physical) = physical {
                physical.lock().scheduled = self.final_states[index].take().unwrap();
            }
        }
    }

    unsafe fn establish_contents(&mut self) {
        for (index, physical) in self.physical.iter().enumerate() {
            if let Some(physical) = physical {
                let completed = self.completed_states[index].take().unwrap();
                let mut state = physical.lock();
                // Waiting A after B must never overwrite B's accepted state, or
                // a later completion already observed by another submission.
                if state.completed.serial <= completed.serial {
                    state.completed = completed;
                }
            }
        }
        for (_, range) in &mut self.readbacks {
            // SAFETY: Caller obtained actual completion of this whole payload.
            unsafe {
                range.as_mut().unwrap().establish_contents();
            }
        }
        self.uploads.clear();
        self.physical.clear();
        self.bindings.take();
        self.programs.take();
    }
}

fn layers(image: &GraphImage) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers::default()
        .aspect_mask(image.aspect)
        .layer_count(1)
}
fn image_copy(image: &GraphImage, offset: u64) -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .buffer_offset(offset)
        .image_subresource(layers(image))
        .image_extent(vk::Extent3D {
            width: image.extent.width,
            height: image.extent.height,
            depth: 1,
        })
}
fn load(value: LoadOp) -> vk::AttachmentLoadOp {
    match value {
        LoadOp::Clear => vk::AttachmentLoadOp::CLEAR,
        LoadOp::Load => vk::AttachmentLoadOp::LOAD,
        LoadOp::Discard => vk::AttachmentLoadOp::DONT_CARE,
    }
}
fn store(value: StoreOp) -> vk::AttachmentStoreOp {
    match value {
        StoreOp::Store => vk::AttachmentStoreOp::STORE,
        StoreOp::Discard => vk::AttachmentStoreOp::DONT_CARE,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod render_tests;

#[cfg(test)]
mod error_tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn transfer_native_failures_preserve_operation_device_result_and_classification() {
        for (result, expected) in [
            (
                vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
                RuntimeErrorKind::Native,
            ),
            (
                vk::Result::ERROR_OUT_OF_HOST_MEMORY,
                RuntimeErrorKind::Native,
            ),
            (vk::Result::ERROR_DEVICE_LOST, RuntimeErrorKind::DeviceLost),
        ] {
            let error = RuntimeError::from(TransferError::Vulkan {
                operation: "vkQueueSubmit2",
                device: "test adapter".to_owned(),
                result,
            });
            assert_eq!(error.kind(), expected);
            assert!(error.message.contains("vkQueueSubmit2"));
            assert!(error.message.contains("test adapter"));
            let source = error.source().unwrap();
            assert!(source.downcast_ref::<TransferError>().is_some());
            assert_eq!(
                source.source().unwrap().downcast_ref::<vk::Result>(),
                Some(&result)
            );
        }
    }

    #[test]
    fn native_helper_keeps_original_result_and_support_failure_category() {
        let error = RuntimeError::native(
            "vkCreateImageView",
            "test adapter",
            vk::Result::ERROR_DEVICE_LOST,
        );
        assert_eq!(error.kind(), RuntimeErrorKind::DeviceLost);
        assert_eq!(
            error
                .source()
                .unwrap()
                .source()
                .unwrap()
                .downcast_ref::<vk::Result>(),
            Some(&vk::Result::ERROR_DEVICE_LOST)
        );
        let error = RuntimeError::native(
            "vkGetPhysicalDeviceImageFormatProperties2",
            "test adapter",
            vk::Result::ERROR_FORMAT_NOT_SUPPORTED,
        );
        assert_eq!(error.kind(), RuntimeErrorKind::Unsupported);
        assert!(
            error
                .message
                .contains("vkGetPhysicalDeviceImageFormatProperties2")
        );
    }

    #[test]
    fn bounded_capacity_and_terminal_device_states_have_distinct_kinds() {
        assert_eq!(
            RuntimeError::from(TransferError::CapacityExhausted { capacity: 2 }).kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(
            RuntimeError::from(StagingError::CapacityExhausted {
                capacity: 16,
                requested: 12,
                reserved: 8
            })
            .kind(),
            RuntimeErrorKind::CapacityExhausted
        );
        assert_eq!(
            RuntimeError::from(TransferError::DeviceUnavailable(DeviceState::Lost)).kind(),
            RuntimeErrorKind::DeviceLost
        );
        assert_eq!(
            RuntimeError::from(StagingError::DeviceUnavailable(DeviceState::Stopped)).kind(),
            RuntimeErrorKind::DeviceUnavailable
        );
        assert_eq!(
            RuntimeError::from(StagingError::Buffer(BufferError::DeviceUnavailable(
                DeviceState::Lost
            )))
            .kind(),
            RuntimeErrorKind::DeviceLost
        );
        // Words in a diagnostic never determine its category.
        assert_eq!(
            RuntimeError::invalid("capacity unsupported lost").kind(),
            RuntimeErrorKind::InvalidInput
        );
        assert_eq!(
            RuntimeError::stale("snapshot changed").kind(),
            RuntimeErrorKind::StaleResource
        );
        assert_eq!(
            RuntimeError::wrong_owner("different runtime").kind(),
            RuntimeErrorKind::WrongOwner
        );
    }
}
