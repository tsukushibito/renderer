//! Native execution of the validated, single-queue M2 graph.
//!
//! The registry owns canonical physical identities. Payload leases retain every
//! recorded reference, and accepted state is distinct from actual completion.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::time::Instant;

use ash::vk;

use super::buffer::BufferRole;
use super::device::VulkanDevice;
use super::diagnostics::{Diagnostics, DiagnosticsHandle};
use super::instance::VulkanInstance;
use super::memory::{MemoryOwner, VulkanAllocator};
use super::program::{ShaderNeeds, ShaderPrograms};
use super::transfer::staging::SharedStaging;
use super::transfer::{ExecutionPool, PendingTransfer};
use crate::graph::{
    BufferHandle, CompiledGraph, ImageFormat, ImageHandle, ImportBuffer, ImportImage, Operation,
    PassHandle, ResourceDesc, ResourceKey,
};

mod cache;
#[cfg(feature = "graph-diagnostics")]
use work::diagnostics;
mod environment;
mod frames;
mod metrics;
#[cfg(all(feature = "presentation", windows))]
mod presentation;
mod provenance;
use cache::{CacheIndex, CacheTarget, RecycleQueue, ResourceLease};
mod uploads;
mod work;
use super::resources::{DeviceBuffer, DeviceImage};
pub use environment::{DeviceInfo, ValidationMonitor, ValidationReport};
pub use frames::{FrameCompletion, FrameId, FrameSubmission};
pub use metrics::{GpuTiming, MemoryStats, StagingMemoryStats, SubmissionMetrics};
#[cfg(all(feature = "presentation", windows))]
pub use presentation::{PresentOutcome, PresentationConfig, Presenter};
use provenance::BufferProvenance;
use work::GraphWork;

static NEXT_RUNTIME: AtomicU64 = AtomicU64::new(1);

#[cfg(feature = "graph-diagnostics")]
type LoweringCapture = Option<String>;
#[cfg(not(feature = "graph-diagnostics"))]
type LoweringCapture = ();

/// Explicit bounds; acquisition never waits or exceeds the configured capacities.
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

pub use super::error::{RuntimeError, RuntimeErrorKind};

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

#[derive(Clone)]
struct Snapshot {
    // Changes only when accepted work writes or invalidates logical contents.
    revision: u64,
    // Last accepted use, including read-only native state/layout changes.
    serial: u64,
    initialized: Vec<Range<u64>>,
    layout: vk::ImageLayout,
    stage: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
    writer_stage: vk::PipelineStageFlags2,
    writer_access: vk::AccessFlags2,
    provenance: BufferProvenance,
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
            provenance: BufferProvenance::default(),
        }
    }
}

struct State {
    scheduled: Snapshot,
    // Completion observations need a serial only. Keeping a second content
    // snapshot here would pin obsolete upload inputs after full replacement,
    // especially when later submissions retire through Drop rather than wait.
    completed_serial: u64,
}
enum NativeResource {
    Buffer(DeviceBuffer),
    Image(DeviceImage),
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
    fn buffer(&self) -> &DeviceBuffer {
        match &self.native {
            NativeResource::Buffer(value) => value,
            _ => unreachable!("compiled buffer kind"),
        }
    }
    fn image(&self) -> &DeviceImage {
        match &self.native {
            NativeResource::Image(value) => value,
            _ => unreachable!("compiled image kind"),
        }
    }
}

struct Entry {
    generation: u32,
    registered: bool,
    // Trimming leaves the generation in place: a later allocation must never
    // resurrect an old public ResourceKey, even when the native object is gone.
    physical: Option<Arc<Physical>>,
}

impl Entry {
    fn physical(&self) -> &Arc<Physical> {
        self.physical.as_ref().expect("live resource entry")
    }

    fn idle(&self) -> bool {
        !self.registered
            && self
                .physical
                .as_ref()
                .is_some_and(|physical| Arc::strong_count(physical) == 1)
    }

    fn reusable(&self) -> bool {
        self.idle() && self.generation < u32::MAX
    }
}

/// Cache buckets encode the complete currently supported resource descriptor.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
enum CacheDescriptor {
    Buffer(u64),
    Rgba8(u32, u32),
    Depth32(u32, u32),
}
impl From<ResourceDesc> for CacheDescriptor {
    fn from(desc: ResourceDesc) -> Self {
        match desc {
            ResourceDesc::Buffer(desc) => Self::Buffer(desc.size),
            ResourceDesc::Image(desc) => match desc.format {
                ImageFormat::Rgba8Unorm => Self::Rgba8(desc.width, desc.height),
                ImageFormat::Depth32Float => Self::Depth32(desc.width, desc.height),
            },
        }
    }
}

type PhysicalLease = ResourceLease<Physical>;

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
    #[cfg(any(test, feature = "vulkan-diagnostics"))]
    diagnostics: DiagnosticsHandle,
    execution: ExecutionPool,
    upload: SharedStaging,
    readback: SharedStaging,
    programs: Option<Arc<ShaderPrograms>>,
    entries: Vec<Entry>,
    vacant: BTreeSet<usize>,
    available: CacheIndex<CacheDescriptor>,
    recycle: Arc<RecycleQueue>,
    frames: frames::FrameQueue<frames::PendingFrame>,
    timestamp_pool: Option<metrics::TimestampPool>,
    profiling_enabled: bool,
    id: u64,
    serial: u64,
    config: RuntimeConfig,
    resources_created: usize,
    resources_reused: usize,
}

impl VulkanRuntime {
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        Self::new_with_validation(config, super::instance::ValidationMode::default())
    }

    /// Select native validation independently of optimization and Renderer checks.
    /// Required needs `vulkan-diagnostics`; `development-validation` selects it by default.
    pub fn new_with_validation(
        config: RuntimeConfig,
        validation: super::instance::ValidationMode,
    ) -> Result<Self, RuntimeError> {
        Self::validate_config(config)?;
        let diagnostics = Diagnostics::new();
        let instance = VulkanInstance::with_validation(diagnostics.clone(), validation)?;
        let device = VulkanDevice::new(instance)?;
        Self::from_device(config, diagnostics, device)
    }

    fn validate_config(config: RuntimeConfig) -> Result<(), RuntimeError> {
        if config.execution_slots == 0
            || config.resource_capacity == 0
            || config.resource_capacity > u32::MAX as usize
            || config.max_shader_bindings == 0
        {
            return Err(RuntimeError::invalid(
                "runtime capacities must be nonzero and resource capacity fit u32",
            ));
        }
        Ok(())
    }

    fn from_device(
        config: RuntimeConfig,
        _diagnostics: DiagnosticsHandle,
        device: VulkanDevice,
    ) -> Result<Self, RuntimeError> {
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
            #[cfg(any(test, feature = "vulkan-diagnostics"))]
            diagnostics: _diagnostics,
            execution,
            upload,
            readback,
            programs: None,
            entries: Vec::new(),
            vacant: BTreeSet::new(),
            available: CacheIndex::default(),
            recycle: Arc::new(RecycleQueue::default()),
            frames: frames::FrameQueue::new(config.execution_slots)?,
            timestamp_pool: None,
            profiling_enabled: false,
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
        let physical = self.entry(key)?.physical();
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
        let physical = self.entry(key)?.physical();
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
        let index = key.index as usize;
        self.entries[index].registered = false;
        self.recycle_entry(index);
        Ok(())
    }

    /// Release all unregistered physical resources without escaped leases.
    ///
    /// This is an explicit working-set boundary, for example after resizing or
    /// leaving a level. It does not wait for or poll GPU completion, unregister
    /// live resources, or release resources retained by recorded/pending work.
    /// Call again after those owners retire to release the newly idle resources.
    ///
    /// Returns the number of native resource allocations released. VMA may keep
    /// empty memory blocks for reuse; this is not a process VRAM byte guarantee.
    pub fn trim_idle_resources(&mut self) -> usize {
        self.collect_recycled();
        let mut released = 0;
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if !entry.idle() {
                continue;
            }
            self.available.remove(index);
            // The registry is the sole owner. Dropping its Arc destroys the
            // native allocation and any content metadata, without a GPU wait.
            entry.physical = None;
            if entry.generation < u32::MAX {
                self.vacant.insert(index);
            }
            released += 1;
        }
        released
    }

    /// Exposing a logical resource name removes it from every reuse index.
    fn register(&mut self, index: usize) {
        self.available.remove(index);
        self.entries[index].registered = true;
    }

    fn recycle_entry(&mut self, index: usize) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        if entry.reusable() {
            self.available.insert(
                index,
                entry.physical().desc.into(),
                entry.physical().lock().scheduled.serial,
            );
        } else {
            self.available.remove(index);
        }
    }

    fn collect_recycled(&mut self) {
        for index in self.recycle.take() {
            self.recycle_entry(index);
        }
    }

    fn lease(&self, key: ResourceKey) -> Result<PhysicalLease, RuntimeError> {
        Ok(PhysicalLease::new(
            Arc::clone(self.entry(key)?.physical()),
            key.index as usize,
            Arc::clone(&self.recycle),
        ))
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
    ) -> Result<(ResourceKey, PhysicalLease), RuntimeError> {
        self.collect_recycled();
        let target = self
            .available
            .select(
                desc.into(),
                self.entries.len(),
                self.config.resource_capacity,
                self.vacant.first().copied(),
                |index| self.entries.get(index).is_some_and(Entry::reusable),
            )
            .map_err(|()| RuntimeError::capacity("graph physical resource capacity exhausted"))?;
        if let CacheTarget::Reuse(index) = target {
            self.available.remove(index);
            let entry = &mut self.entries[index];
            entry.generation += 1;
            let mut state = entry.physical().lock();
            // Preserve actual native synchronization, never old logical contents.
            state.scheduled.initialized.clear();
            state.scheduled.provenance = BufferProvenance::default();
            drop(state);
            self.resources_reused += 1;
            return Ok((
                ResourceKey {
                    owner: self.id,
                    index: index as u32,
                    generation: entry.generation,
                },
                PhysicalLease::new(
                    Arc::clone(entry.physical()),
                    index,
                    Arc::clone(&self.recycle),
                ),
            ));
        }
        let replace = match target {
            CacheTarget::Replace(index) => Some(index),
            CacheTarget::Create => None,
            CacheTarget::Reuse(_) => unreachable!("reuse returned above"),
        };
        // Complete native creation and naming before replacing an idle entry.
        // Failure must retain its allocation, generation and accepted state.
        let native = match desc {
            ResourceDesc::Buffer(desc) => NativeResource::Buffer(
                DeviceBuffer::new(Arc::clone(&self.owner), desc.size, name)
                    .map_err(|error| error.context(format!("resource {name:?}")))?,
            ),
            ResourceDesc::Image(desc) => NativeResource::Image(
                DeviceImage::new(
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
                completed_serial: 0,
            }),
        });
        let index = replace.unwrap_or(self.entries.len());
        let generation = replace.map_or(1, |index| self.entries[index].generation + 1);
        let entry = Entry {
            generation,
            registered: false,
            physical: Some(Arc::clone(&physical)),
        };
        if replace.is_some() {
            self.available.remove(index);
            self.vacant.remove(&index);
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
            PhysicalLease::new(physical, index, Arc::clone(&self.recycle)),
        ))
    }

    /// Records one compiled graph and submits it once. It does not wait for any
    /// prior graph. Imported snapshots must still match canonical accepted state.
    pub fn submit(&mut self, graph: &CompiledGraph) -> Result<Submission, RuntimeError> {
        self.submit_inner(
            graph,
            #[cfg(feature = "graph-diagnostics")]
            false,
        )
        .map(|(submission, _)| submission)
    }

    /// Submit with an owned diagnostic dump of the actual native lowering.
    /// The dump includes physical identities, accepted import states and the
    /// barriers recorded for each pass. It is built before native recording and
    /// returned only after successful submission; it is not a completion token.
    /// Ordinary `submit` does not build diagnostic strings or retain a trace.
    #[cfg(feature = "graph-diagnostics")]
    pub fn submit_with_diagnostics(
        &mut self,
        graph: &CompiledGraph,
    ) -> Result<(Submission, String), RuntimeError> {
        let (submission, diagnostic) = self.submit_inner(graph, true)?;
        Ok((
            submission,
            diagnostic.expect("requested lowering diagnostic"),
        ))
    }

    fn submit_inner(
        &mut self,
        graph: &CompiledGraph,
        #[cfg(feature = "graph-diagnostics")] capture_diagnostics: bool,
    ) -> Result<(Submission, LoweringCapture), RuntimeError> {
        self.collect_recycled();
        let profile_started = self.profiling_enabled.then(Instant::now);
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
        let timestamp = if self.profiling_enabled {
            Some(self.timestamp_pool.as_ref().unwrap().acquire()?)
        } else {
            None
        };
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
        // Validate logical contents, including culled imports, before allocation.
        // Read-only uses may have advanced native scopes/layouts since compile;
        // prepare rebases those from the latest canonical scheduled state.
        for resource in &graph.resources {
            if let Some(key) = resource.import {
                let physical = self.entry(key)?.physical();
                let state = physical.lock();
                // Public imports retain private immutable descriptors/contents.
                // Owner+generation select that descriptor, and every accepted
                // write/invalidation changes revision. Recheck the live revision
                // in shipping; deep metadata equality is an internal diagnostic.
                let inconsistent = state.scheduled.revision != resource.import_revision;
                #[cfg(feature = "development-validation")]
                let inconsistent = inconsistent
                    || physical.desc != resource.desc
                    || state.scheduled.initialized != resource.initial;
                if inconsistent {
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
                (key, self.lease(key)?)
            } else {
                self.acquire(resource.desc, &resource.name)?
            };
            keys[index] = Some(key);
            physical[index] = Some(value);
        }
        let mut shader_needs = ShaderNeeds::default();
        for &pass in &graph.order {
            match graph.passes[pass].operation {
                Operation::Compute { .. } => shader_needs.compute = true,
                Operation::Raster { .. } | Operation::RasterBatch { .. } => {
                    shader_needs.raster = true
                }
                Operation::Composite { .. } => shader_needs.composite = true,
                _ => {}
            }
        }
        if shader_needs.any() && self.programs.is_none() {
            self.programs = Some(ShaderPrograms::new(
                Arc::clone(&self.owner),
                self.config.execution_slots,
                self.config.max_shader_bindings,
            )?);
        }
        let programs = shader_needs
            .any()
            .then(|| Arc::clone(self.programs.as_ref().unwrap()));
        if let Some(programs) = &programs {
            programs.ensure(shader_needs)?;
        }
        let bindings = if shader_needs.compute || shader_needs.composite {
            Some(programs.as_ref().unwrap().acquire_bindings()?)
        } else {
            None
        };
        #[cfg(feature = "graph-diagnostics")]
        let mut diagnostic = capture_diagnostics.then(|| {
            diagnostics::begin(
                graph,
                serial,
                self.owner.device.queue_family(),
                &keys,
                &physical,
            )
        });
        #[cfg(not(feature = "graph-diagnostics"))]
        let diagnostic = ();
        let mut work = GraphWork::prepare(
            Arc::clone(&self.owner),
            graph,
            serial,
            physical,
            programs,
            bindings,
            &self.upload,
            &self.readback,
            #[cfg(feature = "graph-diagnostics")]
            diagnostic.as_mut(),
        )?;
        work.profile = timestamp.map(|lease| {
            metrics::WorkProfile::new(lease, profile_started.expect("profiling enabled"))
        });
        let cpu_pending = work
            .profile
            .as_ref()
            .map(|profile| Arc::clone(&profile.cpu_pending));
        let mut exports = BTreeMap::new();
        for &index in &graph.exports {
            let key = keys[index].unwrap();
            let snapshot = work.final_states[index].as_ref().unwrap();
            exports.insert(
                index,
                export_snapshot(key, graph.resources[index].desc, snapshot),
            );
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
        let record_started = cpu_pending.as_ref().map(|_| Instant::now());
        let recorded = reservation.record(work)?;
        if let (Some(cpu), Some(started)) = (&cpu_pending, record_started) {
            cpu.lock().unwrap_or_else(|error| error.into_inner())[0] = started.elapsed();
        }
        for &index in &new_exports {
            self.register(index);
        }
        // Consume the serial before submit/logging: even if an application
        // subscriber panics after acceptance, a later write cannot reuse its
        // content revision and accidentally accept an obsolete snapshot.
        self.serial = serial;
        // Rejected submit or an application logging unwind must not leave
        // unreachable exported names occupying the bounded registry forever.
        // This guard changes registration only, never accepted physical state.
        let mut registration = Registration {
            entries: &mut self.entries,
            indices: new_exports,
            keep: false,
        };
        let submit_started = cpu_pending.as_ref().map(|_| Instant::now());
        let pending = recorded.submit()?;
        registration.keep = true;
        drop(registration);
        if let (Some(cpu), Some(started)) = (&cpu_pending, submit_started) {
            cpu.lock().unwrap_or_else(|error| error.into_inner())[1] = started.elapsed();
        }
        Ok((
            Submission {
                id: FrameId::new(self.id, serial),
                graph: graph.id,
                pending,
                exports,
            },
            diagnostic,
        ))
    }
}

#[derive(Clone, Debug)]
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
    id: FrameId,
    graph: u64,
    pending: PendingTransfer<GraphWork>,
    exports: BTreeMap<usize, Export>,
}

impl Submission {
    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_pending()
    }
    pub fn export_buffer(&self, handle: BufferHandle) -> Result<ImportBuffer, RuntimeError> {
        if handle.graph != self.graph {
            return Err(RuntimeError::wrong_owner("foreign graph export handle"));
        }
        self.exports
            .get(&handle.index)
            .and_then(|value| match value {
                Export::Buffer(value) => Some(value.clone()),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::invalid("buffer was not exported"))
    }
    pub fn export_image(&self, handle: ImageHandle) -> Result<ImportImage, RuntimeError> {
        if handle.graph != self.graph {
            return Err(RuntimeError::wrong_owner("foreign graph export handle"));
        }
        self.exports
            .get(&handle.index)
            .and_then(|value| match value {
                Export::Image(value) => Some(value.clone()),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::invalid("image was not exported"))
    }
    /// Timeout retains all pending resources. Only a real successful Fence wait
    /// on an active Device yields readable output.
    pub fn wait(&mut self, timeout: Duration) -> Result<Option<SubmissionResult>, RuntimeError> {
        self.pending
            .wait(timeout)?
            .map(|work| SubmissionResult::completed(self.graph, work))
            .transpose()
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
        let index = self
            .work
            .readbacks
            .binary_search_by_key(&pass.index, |(index, _)| *index)
            .map_err(|_| RuntimeError::invalid("pass has no readback result"))?;
        let (_, range) = &mut self.work.readbacks[index];
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cache_tests;

#[cfg(test)]
mod content_revision_tests;

#[cfg(test)]
mod render_tests;

#[cfg(test)]
mod engine_tests;

#[cfg(test)]
mod frame_scenarios;

#[cfg(test)]
mod frame_tests;
