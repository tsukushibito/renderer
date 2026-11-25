//! Native execution of the validated, single-queue M2 graph.
//!
//! The registry owns canonical physical identities. Payload leases retain every
//! recorded reference, and accepted state is distinct from actual completion.

use crate::ImageFormat;
use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::time::Instant;

use ash::vk;

use super::buffer::BufferRole;
use super::device::VulkanDevice;
use super::diagnostics::{Diagnostics, DiagnosticsHandle};
use super::instance::VulkanInstance;
use super::memory::{MemoryOwner, VulkanAllocator};
use super::program::ShaderPrograms;
use super::transfer::staging::SharedStaging;
use super::transfer::{ExecutionPool, PendingTransfer};
use crate::graph::{
    BufferHandle, CompiledGraph, GraphCapacity, ImageHandle, ImportBuffer, ImportImage,
    ImportRanges, ImportRangesPool, Operation, PassHandle, ResourceDesc, ResourceKey, SampleCount,
};
use crate::runtime::WorkingSet;

mod cache;
mod compute;
#[cfg(feature = "graph-diagnostics")]
use work::diagnostics;
mod environment;
mod exports;
use exports::{ExportLease, ExportPool};
mod frames;
mod interop;
pub use interop::{VulkanExternalDevice, VulkanImageState};
mod metrics;
#[cfg(all(feature = "presentation", windows))]
mod presentation;
mod secondary;
mod snapshots;
mod storage;
use cache::{CacheIndex, CacheTarget, RecycleQueue, ResourceLease};
mod uploads;
mod work;
mod working_set;
use super::resources::{DeviceBuffer, DeviceImage};
use crate::graph::provenance::{self, BufferProvenance};
pub use environment::{DeviceInfo, ValidationMonitor, ValidationReport};
pub use frames::{FrameCompletion, FrameId, FrameSubmission};
pub use metrics::{MemoryHeapStats, MemoryStats, StagingMemoryStats};
#[cfg(all(feature = "presentation", windows))]
pub use presentation::Presenter;
use storage::StoragePool;
use work::GraphWork;
use work::{ResultData, WorkData};
use working_set::PhysicalPlan;

#[cfg(feature = "graph-diagnostics")]
type LoweringCapture = Option<String>;
#[cfg(not(feature = "graph-diagnostics"))]
type LoweringCapture = ();

fn native(owner: &MemoryOwner, operation: &str, result: vk::Result) -> RuntimeError {
    owner.device.observe_result(result);
    RuntimeError::native(operation, owner.device.name(), result)
}

/// Explicit bounds; acquisition never waits or exceeds the configured capacities.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfig {
    pub execution_slots: usize,
    pub resource_capacity: usize,
    /// Requested bytes per staging pool, rounded up once to whole cache atoms.
    pub staging_bytes: u64,
    pub staging_ranges: usize,
    /// Maximum descriptor-bearing operations per Graph. Managed Compute
    /// dispatches count once each, while Raster counts each unique draw binding.
    pub max_shader_bindings: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            execution_slots: 4,
            resource_capacity: 64,
            staging_bytes: 4 * 1024 * 1024,
            max_shader_bindings: 64,
            staging_ranges: 256,
        }
    }
}

/// Backing reserved for changing Graph inputs, independent of native execution
/// slots. Result and metadata capacity also cover callers retaining old frames.
#[derive(Clone, Copy, Debug)]
pub struct FrameCapacity {
    pub graph: GraphCapacity,
    pub work_slots: usize,
    pub result_slots: usize,
    pub metadata_slots: usize,
    pub range_snapshots: usize,
    pub initialized_ranges_per_resource: usize,
    pub provenance_segments_per_resource: usize,
    /// Distinct generic index-count prefix summaries retained per resource and
    /// snapshot until mutation. A new count beyond this limit returns capacity
    /// backpressure without eviction or rescanning. Zero supports immutable
    /// mesh proofs and work that does not validate generic index contents.
    pub index_prefixes_per_resource: usize,
}
impl Default for FrameCapacity {
    fn default() -> Self {
        Self {
            graph: GraphCapacity::default(),
            work_slots: 4,
            result_slots: 8,
            metadata_slots: 16,
            range_snapshots: 1024,
            initialized_ranges_per_resource: 32,
            provenance_segments_per_resource: 64,
            index_prefixes_per_resource: 64,
        }
    }
}

pub use crate::runtime::ResourceClass;

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

struct Snapshot {
    // Changes only when accepted work writes or invalidates logical contents.
    revision: u64,
    // Last accepted use, including read-only native state/layout changes.
    serial: u64,
    initialized: Vec<Range<u64>>,
    layout: vk::ImageLayout,
    // Destination scope of the last native image layout transition.
    layout_transition_stage: vk::PipelineStageFlags2,
    stage: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
    writer_stage: vk::PipelineStageFlags2,
    writer_access: vk::AccessFlags2,
    // One exact read scope exposed since the last write/layout transition.
    // Kept separate from accumulated scopes needed to order later writes.
    read_stage: vk::PipelineStageFlags2,
    read_access: vk::AccessFlags2,
    provenance: BufferProvenance,
}

#[cfg(test)]
impl Clone for Snapshot {
    fn clone(&self) -> Self {
        let mut result = Self::with_capacity(
            self.initialized.capacity(),
            self.provenance.segment_capacity(),
            self.provenance.index_prefix_capacity(),
        )
        .unwrap();
        result.copy_from(self).unwrap();
        result
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            serial: 0,
            initialized: Vec::new(),
            layout: vk::ImageLayout::UNDEFINED,
            layout_transition_stage: vk::PipelineStageFlags2::NONE,
            stage: vk::PipelineStageFlags2::NONE,
            access: vk::AccessFlags2::NONE,
            writer_stage: vk::PipelineStageFlags2::NONE,
            writer_access: vk::AccessFlags2::NONE,
            read_stage: vk::PipelineStageFlags2::NONE,
            read_access: vk::AccessFlags2::NONE,
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
    pending_registration: u64,
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
    Image(ImageFormat, u32, u32, u32, u32),
}
impl From<ResourceDesc> for CacheDescriptor {
    fn from(desc: ResourceDesc) -> Self {
        match desc {
            ResourceDesc::Buffer(desc) => Self::Buffer(desc.size),
            ResourceDesc::Image(desc) => {
                let (width, height, mip_levels, samples) = (
                    desc.width,
                    desc.height,
                    desc.mip_levels,
                    desc.sample_count.as_u32(),
                );
                Self::Image(desc.format, width, height, mip_levels, samples)
            }
        }
    }
}

fn attachment_samples(
    graph: &CompiledGraph,
    colors: &Range<usize>,
    depth: Option<usize>,
) -> SampleCount {
    let attachment = graph
        .raster_attachments(colors)
        .first()
        .or_else(|| depth.map(|index| graph.raster_attachment(index)))
        .expect("Graph rendering has at least one attachment");
    let ResourceDesc::Image(desc) = graph.resources[attachment.view.image().index].desc else {
        unreachable!("validated Graph attachment")
    };
    desc.sample_count
}

type PhysicalLease = ResourceLease<Physical>;

struct Registration<'a> {
    entries: &'a mut [Entry],
    exports: &'a ExportLease,
    logical: &'a [usize],
    serial: u64,
    keep: bool,
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        for &logical in self.logical {
            let key = self
                .exports
                .get(logical)
                .expect("prepared exported key")
                .key();
            let entry = &mut self.entries[key.index as usize];
            if entry.pending_registration == self.serial {
                entry.pending_registration = 0;
                if !self.keep {
                    entry.registered = false;
                }
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
    secondary: Option<secondary::Recorder>,
    upload: SharedStaging,
    readback: SharedStaging,
    programs: Option<Arc<ShaderPrograms>>,
    compute_programs: Option<Arc<compute::ComputePrograms>>,
    workspaces: Option<StoragePool<WorkData>>,
    results: Option<StoragePool<ResultData>>,
    frame_capacity: Option<FrameCapacity>,
    exports: Option<ExportPool>,
    import_ranges: Mutex<ImportRangesPool>,
    entries: Vec<Entry>,
    vacant: Vec<usize>,
    available: CacheIndex<CacheDescriptor>,
    recycle: Arc<RecycleQueue>,
    recycle_pending: Vec<usize>,
    frames: frames::FrameQueue<frames::PendingFrame>,
    timestamp_pool: Option<metrics::TimestampPool>,
    profiling_enabled: bool,
    id: u64,
    serial: u64,
    config: RuntimeConfig,
    resources_created: usize,
    resources_reused: usize,
}

fn validate_raster_shader_formats(
    programs: &[impl Clone + Into<crate::program::RasterPipelineRequest>],
) -> Result<(), RuntimeError> {
    if programs
        .iter()
        .any(|request| request.clone().into().program.spirv().is_none())
    {
        return Err(RuntimeError::unsupported(
            "Raster Program has no SPIR-V shaders",
        ));
    }
    Ok(())
}

fn validate_compute_shader_formats(
    programs: &[crate::program::ManagedProgram],
) -> Result<(), RuntimeError> {
    if programs.iter().any(|program| program.spirv().is_none()) {
        return Err(RuntimeError::unsupported(
            "Compute Program has no SPIR-V shader",
        ));
    }
    Ok(())
}

impl VulkanRuntime {
    pub(crate) fn check_texture_support(
        &self,
        desc: crate::graph::ImageDesc,
        linear: bool,
    ) -> Result<(), RuntimeError> {
        let creation = DeviceImage::prepare(&self.owner, desc)?;
        if linear && !creation.supports_linear_filtering {
            return Err(RuntimeError::unsupported(
                "sampled format lacks linear filtering support",
            ));
        }
        Ok(())
    }

    fn capture_ranges(&self, ranges: &[Range<u64>]) -> Result<ImportRanges, RuntimeError> {
        self.import_ranges
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capture(ranges)
            .map_err(|error| {
                RuntimeError::capacity_with(
                    "prepared import range snapshots exhausted",
                    crate::capacity::import_ranges_capacity(&error),
                )
            })
    }

    /// Prepare native resources and all variable-length frame backing. Normal
    /// submit never creates resources or grows this working set. Graph input,
    /// draw count and pass composition may change within the prepared limits.
    ///
    /// `resources` describes unregistered reusable instances in addition to
    /// explicitly registered inputs. Include recording, GPU and exported uses
    /// in the counts. Return recording, result, and submission metadata leases
    /// before reconfiguration. Registered snapshots remain usable. Changing
    /// the range snapshot pool's own dimensions requires returning its tokens.
    /// Recoverable preparation failures preserve the live working set. During replacement,
    /// the previous and staged native allocations may temporarily coexist.
    pub fn prepare_working_set(
        &mut self,
        capacity: FrameCapacity,
        resources: &[ResourceClass],
    ) -> Result<(), RuntimeError> {
        self.prepare_working_set_with(capacity, resources, None, PhysicalPlan::create)
    }

    pub(crate) fn prepare_graph_working_set(
        &mut self,
        initial_capacity: Option<FrameCapacity>,
        working_set: WorkingSet<'_>,
    ) -> Result<(), RuntimeError> {
        let capacity = initial_capacity.or(self.frame_capacity).ok_or_else(|| {
            RuntimeError::capacity_with(
                "frame working set is not prepared",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::Prepare,
                    None,
                    None,
                ),
            )
        })?;
        validate_raster_shader_formats(working_set.raster)?;
        validate_compute_shader_formats(working_set.compute)?;
        self.prepare_working_set_with(
            capacity,
            working_set.resources,
            Some(working_set),
            PhysicalPlan::create,
        )
    }

    pub(crate) fn grow_graph_working_set(
        &mut self,
        capacity: FrameCapacity,
        settings: crate::runtime::VulkanConfig,
        registry_size: usize,
        working_set: WorkingSet<'_>,
    ) -> Result<(), RuntimeError> {
        let previous = self.config;
        let next = RuntimeConfig {
            execution_slots: previous.execution_slots,
            resource_capacity: registry_size,
            max_shader_bindings: settings.max_shader_bindings,
            staging_bytes: settings.staging_bytes,
            staging_ranges: settings.staging_ranges,
        };
        Self::validate_config(next)?;
        if !self.frame_consumers_returned() {
            return Err(RuntimeError::capacity_with(
                "return frame owners before expanding a working set",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            ));
        }
        let staging = if next.staging_bytes != previous.staging_bytes
            || next.staging_ranges != previous.staging_ranges
        {
            let create = |role| {
                SharedStaging::new(
                    Arc::clone(&self.owner),
                    role,
                    next.staging_bytes,
                    next.staging_ranges,
                )
            };
            Some((create(BufferRole::Upload)?, create(BufferRole::Readback)?))
        } else {
            None
        };
        if registry_size > previous.resource_capacity {
            let reserve_error = |e| RuntimeError::with_source(RuntimeErrorKind::Native, e);
            self.entries
                .try_reserve_exact(registry_size - self.entries.len())
                .map_err(reserve_error)?;
            self.vacant
                .try_reserve_exact(registry_size - self.vacant.len())
                .map_err(reserve_error)?;
            self.recycle_pending
                .try_reserve_exact(registry_size - self.recycle_pending.len())
                .map_err(reserve_error)?;
            let available = self
                .available
                .with_capacity(registry_size)
                .map_err(reserve_error)?;
            self.recycle.grow(registry_size).map_err(reserve_error)?;
            self.available = available;
        }
        // Stage newly added snapshot slots while keeping anchors for live imports.
        let ranges = self
            .import_ranges
            .get_mut()
            .unwrap_or_else(|e| e.into_inner());
        let prior_ranges = ranges
            .expanded_slots(capacity.range_snapshots)
            .map_err(|_| RuntimeError::invalid("unable to grow import snapshots"))?
            .map(|expanded| std::mem::replace(ranges, expanded));
        self.config = next;
        if let Err(error) = self.prepare_graph_working_set(Some(capacity), working_set) {
            self.config = previous;
            if let Some(prior) = prior_ranges {
                *self
                    .import_ranges
                    .get_mut()
                    .unwrap_or_else(|e| e.into_inner()) = prior;
            }
            return Err(error);
        }
        if let Some((upload, readback)) = staging {
            self.upload = upload;
            self.readback = readback;
        }
        Ok(())
    }

    fn prepare_working_set_with(
        &mut self,
        capacity: FrameCapacity,
        resources: &[ResourceClass],
        working_set: Option<WorkingSet<'_>>,
        create: impl FnMut(
            PhysicalPlan,
            &Arc<MemoryOwner>,
            &str,
            Snapshot,
        ) -> Result<Arc<Physical>, RuntimeError>,
    ) -> Result<(), RuntimeError> {
        if resources.iter().any(|class| {
            matches!(class,
            ResourceClass::Image {desc,..} if desc.array_layers != 1)
        }) {
            return Err(RuntimeError::unsupported(
                "native images require one array layer",
            ));
        }
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        self.collect_recycled();
        if !self.frame_consumers_returned() {
            return Err(RuntimeError::capacity_with(
                "working-set preparation requires returned frame/result/export tokens",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            ));
        }
        if capacity.graph.resources == 0
            || capacity.graph.passes == 0
            || capacity.work_slots == 0
            || capacity.result_slots == 0
            || capacity.metadata_slots == 0
            || capacity.range_snapshots == 0
        {
            return Err(RuntimeError::invalid(
                "frame working-set capacities must be nonzero",
            ));
        }
        let ranges = self.stage_import_range_pool(capacity)?;
        let workspaces = StoragePool::new(
            capacity.work_slots,
            || WorkData::new(capacity),
            WorkData::reset,
        )?;
        let results = StoragePool::new(
            capacity.result_slots,
            || ResultData::new(capacity.graph.passes),
            ResultData::reset,
        )?;
        let exports = ExportPool::new(capacity.graph.resources, capacity.metadata_slots)?;
        let instances = resources.iter().try_fold(0usize, |total, class| {
            let count = match class {
                ResourceClass::Buffer { count, .. } | ResourceClass::Image { count, .. } => *count,
            };
            if count == 0 {
                return Err(RuntimeError::invalid(
                    "resource class count must be nonzero",
                ));
            }
            total.checked_add(count).ok_or_else(|| {
                RuntimeError::capacity_with(
                    "resource working-set count overflow",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::WorkingSet,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })
        })?;
        if instances
            > self
                .config
                .resource_capacity
                .saturating_sub(self.entries.iter().filter(|entry| entry.registered).count())
        {
            return Err(RuntimeError::capacity_with(
                "resource working set exceeds registry capacity",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            ));
        }
        let prepared = self.stage_resources(
            capacity,
            resources,
            instances,
            working_set.is_some(),
            create,
        )?;
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let descriptors = prepared.descriptors(self);
        let (raster, compute) = if let Some(working_set) = working_set {
            let depth_samples = Self::depth_sample_counts(descriptors);
            let raster = self.stage_raster_programs(working_set.raster, depth_samples)?;
            let compute = self.stage_compute_programs(working_set.compute)?;
            self.prepare_samplers(working_set.samplers)?;
            (Some(raster), Some(compute))
        } else {
            (self.stage_implicit_raster(descriptors)?, None)
        };
        // Workspaces, registry changes, and explicit programs become visible
        // together after all fallible work finishes.
        prepared.commit(self);
        if let Some(raster) = raster {
            self.programs = Some(raster);
        }
        if let Some(compute) = compute {
            self.compute_programs = Some(compute);
        }
        self.workspaces = Some(workspaces);
        self.results = Some(results);
        self.exports = Some(exports);
        self.frame_capacity = Some(capacity);
        if let Some(ranges) = ranges {
            *self
                .import_ranges
                .get_mut()
                .unwrap_or_else(|e| e.into_inner()) = ranges;
        }
        Ok(())
    }

    fn stage_import_range_pool(
        &mut self,
        capacity: FrameCapacity,
    ) -> Result<Option<ImportRangesPool>, RuntimeError> {
        let pool = self
            .import_ranges
            .get_mut()
            .unwrap_or_else(|e| e.into_inner());
        if pool.has_capacity(
            capacity.initialized_ranges_per_resource,
            capacity.range_snapshots,
        ) {
            return Ok(None);
        }
        if !pool.is_idle() {
            return Err(RuntimeError::capacity_with(
                "return buffer snapshots before resizing their range pool",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::ImportRanges,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            ));
        }
        ImportRangesPool::new(
            capacity.initialized_ranges_per_resource,
            capacity.range_snapshots,
        )
        .map(Some)
        .map_err(|_| {
            RuntimeError::capacity_with(
                "import range preparation failed",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::ImportRanges,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            )
        })
    }

    fn stage_implicit_raster(
        &self,
        resources: impl Iterator<Item = ResourceDesc>,
    ) -> Result<Option<Arc<ShaderPrograms>>, RuntimeError> {
        // Preserve the backend-specific convenience path for prepared RGBA8
        // and D32 resources. The common working set uses explicit requests.
        let mut color = false;
        let mut depth = false;
        for resource in resources {
            if let ResourceDesc::Image(desc) = resource
                && desc.sample_count == SampleCount::X1
            {
                match desc.format {
                    ImageFormat::Rgba8Unorm => color = true,
                    ImageFormat::Depth32Float => depth = true,
                    _ => {}
                }
            }
        }
        if !(color && depth) {
            return Ok(None);
        }
        if let Some(programs) = &self.programs {
            programs.prepare_implicit_raster()?;
            return Ok(None);
        }
        let programs = ShaderPrograms::new(
            Arc::clone(&self.owner),
            self.config.execution_slots,
            self.config.max_shader_bindings,
            true,
        )?;
        programs.prepare_implicit_raster()?;
        Ok(Some(programs))
    }

    fn depth_sample_counts(resources: impl Iterator<Item = ResourceDesc>) -> vk::SampleCountFlags {
        let mut depth_samples = vk::SampleCountFlags::empty();
        for resource in resources {
            if let ResourceDesc::Image(desc) = resource
                && desc.format == ImageFormat::Depth32Float
            {
                depth_samples |= super::resources::sample_count_flags(desc.sample_count);
            }
        }
        depth_samples
    }

    fn stage_raster_programs(
        &self,
        programs: &[impl Clone + Into<crate::program::RasterPipelineRequest>],
        depth_samples: vk::SampleCountFlags,
    ) -> Result<Arc<ShaderPrograms>, RuntimeError> {
        let replacement = ShaderPrograms::new(
            Arc::clone(&self.owner),
            self.config.execution_slots,
            self.config.max_shader_bindings,
            false,
        )?;
        replacement.prepare_rasters(programs, depth_samples)?;
        Ok(replacement)
    }

    /// Replace the complete Raster Program working set before ordinary frames.
    /// Programs prepare the explicitly requested raster sample counts. Color
    /// programs prepare depth-enabled variants when the prepared resource
    /// working set includes depth images of the same sample count.
    /// All recordings, pending commands, results and buffer export/refresh tokens
    /// must be returned before replacing descriptor layouts, as for resource preparation.
    /// The selected set replaces previous explicit Raster selections only after
    /// all native preparation succeeds; failure preserves the previous set.
    pub fn prepare_raster_programs(
        &mut self,
        programs: &[impl Clone + Into<crate::program::RasterPipelineRequest>],
    ) -> Result<(), RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if self.frame_capacity.is_none() {
            return Err(RuntimeError::capacity_with(
                "frame working set is not prepared",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::Prepare,
                    None,
                    None,
                ),
            ));
        }
        self.collect_recycled();
        if !self.frame_consumers_returned() {
            return Err(RuntimeError::capacity_with(
                "Raster preparation requires returned frame/result/export tokens",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            ));
        }
        validate_raster_shader_formats(programs)?;
        let descriptors = self
            .entries
            .iter()
            .filter_map(|entry| entry.physical.as_ref().map(|physical| physical.desc));
        let replacement =
            self.stage_raster_programs(programs, Self::depth_sample_counts(descriptors))?;
        self.programs = Some(replacement);
        Ok(())
    }

    fn frame_consumers_returned(&mut self) -> bool {
        self.execution.stats().leased == 0
            && self.workspaces.as_ref().is_none_or(|pool| pool.is_idle())
            && self.results.as_ref().is_none_or(|pool| pool.is_idle())
            && self.exports.as_mut().is_none_or(|pool| pool.is_idle())
    }

    /// Prepare the complete managed Compute working set before ordinary frames.
    /// Pipelines and descriptor sets are retained and reused; changing this set
    /// requires cancellation or actual retirement of all execution leases.
    pub fn prepare_compute_programs(
        &mut self,
        programs: &[crate::program::ManagedProgram],
    ) -> Result<(), RuntimeError> {
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if self.frame_capacity.is_none() {
            return Err(RuntimeError::capacity_with(
                "frame working set is not prepared",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::WorkingSet,
                    CapacityRecovery::Prepare,
                    None,
                    None,
                ),
            ));
        }
        if self.execution.stats().leased != 0
            || self
                .compute_programs
                .as_ref()
                .is_some_and(|programs| !programs.is_idle())
        {
            return Err(RuntimeError::capacity_with(
                "Compute preparation requires retired execution slots",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::ExecutionSlots,
                    CapacityRecovery::ReleaseOwners,
                    None,
                    None,
                ),
            ));
        }
        validate_compute_shader_formats(programs)?;
        let prepared = self.stage_compute_programs(programs)?;
        self.compute_programs = Some(prepared);
        Ok(())
    }

    fn stage_compute_programs(
        &self,
        programs: &[crate::program::ManagedProgram],
    ) -> Result<Arc<compute::ComputePrograms>, RuntimeError> {
        compute::ComputePrograms::new(
            Arc::clone(&self.owner),
            programs,
            self.config.execution_slots,
            self.config.max_shader_bindings,
        )
    }

    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        Self::new_with_validation(config, super::instance::ValidationMode::default())
    }

    /// Open a dedicated compute runtime with acceleration structures, buffer addresses
    /// and inline ray queries enabled. No supported adapter returns None.
    pub fn try_new_ray_query(
        config: RuntimeConfig,
        validation: super::instance::ValidationMode,
    ) -> Result<Option<Self>, RuntimeError> {
        Self::validate_config(config)?;
        let diagnostics = Diagnostics::new();
        let instance = VulkanInstance::with_validation(diagnostics.clone(), validation)?;
        let Some(device) = VulkanDevice::try_ray_queries(instance)? else {
            return Ok(None);
        };
        Self::from_device(config, diagnostics, device).map(Some)
    }

    /// Create Renderer-owned VMA, execution and staging resources on an existing
    /// device. The host configures native validation and observes its own callback.
    /// The Renderer monitor does not receive host validation messages.
    ///
    /// # Safety
    /// The dispatch tables, physical device, logical device and unprotected
    /// graphics/compute queue must belong to the same live single-device Vulkan
    /// configuration. The handles and callback state must stay valid while
    /// `host.lifetime` is retained. Each claimed feature and the extensions
    /// required by nonzero `presentation_slots` must actually be enabled.
    /// Include the requested presentation capacity in `config.execution_slots`.
    /// Use the supplied mutex for all host access
    /// requiring external synchronization on this queue (including device-wide
    /// waits). Never enter Renderer queue operations while holding that mutex.
    /// The mutex serializes host access. The host separately synchronizes GPU
    /// access and ownership when sharing native resources.
    ///
    /// Reserve `sampler_budget` of the device's available sampler slots for
    /// this runtime. Provision headroom for its VMA allocations at creation
    /// and every working-set change, including simultaneous old/new pools.
    /// All allocators on this device together must obey maxMemoryAllocationCount.
    pub unsafe fn from_host_device(
        config: RuntimeConfig,
        host: super::VulkanHostDevice,
    ) -> Result<Self, RuntimeError> {
        Self::validate_config(config)?;
        let diagnostics = Diagnostics::new();
        // SAFETY: The caller guarantees the documented Vulkan adoption contract.
        let device = unsafe { VulkanDevice::from_host(host, diagnostics.clone()) }?;
        Self::from_device(config, diagnostics, device)
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
            || config.staging_ranges == 0
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
        let upload = SharedStaging::new(
            Arc::clone(&owner),
            BufferRole::Upload,
            config.staging_bytes,
            config.staging_ranges,
        )?;
        let readback = SharedStaging::new(
            Arc::clone(&owner),
            BufferRole::Readback,
            config.staging_bytes,
            config.staging_ranges,
        )?;
        let id = crate::graph::next_id();
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(config.resource_capacity)
            .map_err(|error| RuntimeError::with_source(RuntimeErrorKind::Native, error))?;
        let mut vacant = Vec::new();
        vacant
            .try_reserve_exact(config.resource_capacity)
            .map_err(|error| RuntimeError::with_source(RuntimeErrorKind::Native, error))?;
        let mut recycle_pending = Vec::new();
        recycle_pending
            .try_reserve_exact(config.resource_capacity)
            .map_err(|error| RuntimeError::with_source(RuntimeErrorKind::Native, error))?;
        let available = CacheIndex::new(config.resource_capacity)
            .map_err(|error| RuntimeError::with_source(RuntimeErrorKind::Native, error))?;
        let recycle = Arc::new(
            RecycleQueue::new(config.resource_capacity)
                .map_err(|error| RuntimeError::with_source(RuntimeErrorKind::Native, error))?,
        );
        Ok(Self {
            owner,
            #[cfg(any(test, feature = "vulkan-diagnostics"))]
            diagnostics: _diagnostics,
            execution,
            upload,
            readback,
            programs: None,
            compute_programs: None,
            workspaces: None,
            results: None,
            frame_capacity: None,
            exports: None,
            import_ranges: Mutex::new(ImportRangesPool::new(0, config.resource_capacity).map_err(
                |_| {
                    RuntimeError::capacity_with(
                        "import range preparation failed",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::ImportRanges,
                            CapacityRecovery::Reconfigure,
                            None,
                            None,
                        ),
                    )
                },
            )?),
            entries,
            vacant,
            available,
            recycle,
            recycle_pending,
            frames: frames::FrameQueue::new(config.execution_slots).map_err(|error| {
                RuntimeError::with_source(RuntimeErrorKind::Native, error)
                    .context("reserving bounded frame bookkeeping")
            })?,
            timestamp_pool: None,
            secondary: None,
            profiling_enabled: false,
            id,
            serial: 0,
            config,
            resources_created: 0,
            resources_reused: 0,
        })
    }

    /// Prepare immutable samplers once on this Device, outside frame recording.
    /// Previously prepared configurations remain available after failure.
    pub fn prepare_samplers(
        &mut self,
        descriptors: &[crate::program::SamplerDescriptor],
    ) -> Result<(), RuntimeError> {
        self.owner.device.prepare_samplers(descriptors)
    }

    pub fn device_name(&self) -> &str {
        self.owner.device.name()
    }

    pub fn stats(&self) -> RuntimeStats {
        let execution = self.execution.stats();
        let shader = self.programs.as_ref().map(|programs| programs.stats());
        let (compute_arenas, compute_reuses) = self
            .compute_programs
            .as_ref()
            .map_or((0, 0), |programs| programs.arena_stats());
        RuntimeStats {
            resources_created: self.resources_created,
            resources_reused: self.resources_reused,
            registered_resources: self.entries.iter().filter(|entry| entry.registered).count(),
            execution_slots_created: execution.native_creates,
            execution_slots_reused: execution.reuses,
            upload_live_ranges: self.upload.stats().live_ranges,
            readback_live_ranges: self.readback.stats().live_ranges,
            descriptor_arenas_created: compute_arenas
                + shader.map_or(0, |stats| stats.descriptor_arenas_created),
            descriptor_arenas_reused: compute_reuses
                + shader.map_or(0, |stats| stats.descriptor_arenas_reused),
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
        ImportBuffer::new(
            key,
            state.scheduled.revision,
            desc,
            self.capture_ranges(&state.scheduled.initialized)?,
        )
        .map_err(|error| RuntimeError::invalid(error.to_string()))
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
        ImportImage::new(
            key,
            state.scheduled.revision,
            desc,
            &state.scheduled.initialized,
        )
        .map_err(|error| RuntimeError::invalid(error.to_string()))
    }

    /// Invalidate the logical handle immediately. Recorded/pending references
    /// keep the allocation alive, and prevent its acquisition for another owner.
    pub fn delete(&mut self, key: ResourceKey) -> Result<(), RuntimeError> {
        if matches!(&self.entry(key)?.physical().native, NativeResource::Image(image) if image.is_external())
        {
            return Err(RuntimeError::invalid(
                "release an external image with release_external_image",
            ));
        }
        let index = key.index as usize;
        self.entries[index].registered = false;
        // The key can no longer prepare another CPU index consumer. Already
        // recorded work owns its native buffers, staging and Graph operands,
        // so logical provenance need not pin a builder slot until a later poll.
        // Preserve native scopes, layouts and revision until physical reuse.
        self.entries[index]
            .physical()
            .lock()
            .scheduled
            .clear_contents();
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
        for index in 0..self.entries.len() {
            if !self.entries[index].idle() {
                continue;
            }
            self.release_idle_physical(index);
            released += 1;
        }
        released
    }

    fn release_idle_physical(&mut self, index: usize) {
        let entry = &mut self.entries[index];
        debug_assert!(entry.idle());
        self.available.remove(index);
        // The registry is the sole owner. Dropping its Arc destroys the
        // native allocation and any content metadata, without a GPU wait.
        entry.physical = None;
        if entry.generation < u32::MAX
            && let Err(position) = self.vacant.binary_search(&index)
        {
            self.vacant.insert(position, index);
        }
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
            let serial = {
                let mut state = entry.physical().lock();
                // No public key or recording/GPU lease can read these logical
                // contents again. Release provenance pins before the next Graph
                // builder checkout; retain actual native layout and last writer.
                state.scheduled.clear_contents();
                state.scheduled.serial
            };
            self.available
                .insert(index, entry.physical().desc.into(), serial);
        } else {
            self.available.remove(index);
        }
    }

    fn collect_recycled(&mut self) {
        // Drop the queue lock before acquiring physical state locks. Keep the
        // prepared scratch backing through every collection and lease return.
        let mut pending = std::mem::take(&mut self.recycle_pending);
        self.recycle.drain_into(&mut pending);
        for &index in &pending {
            self.recycle_entry(index);
        }
        pending.clear();
        self.recycle_pending = pending;
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

    fn resource_capacity_context(
        &self,
        error: RuntimeError,
        desc: ResourceDesc,
        acquired: &[Option<ResourceKey>],
    ) -> RuntimeError {
        if error
            .capacity()
            .is_none_or(|capacity| capacity.resource != RuntimeCapacityKind::Resources)
        {
            return error;
        }
        // Failure-only diagnosis. The success path uses the prepared candidate
        // index; do not scan every registry entry on normal acquisition.
        let mut limit = 0u64;
        let mut used = 0u64;
        let mut retained_elsewhere = false;
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.registered || entry.generation == u32::MAX {
                continue;
            }
            let Some(physical) = &entry.physical else {
                continue;
            };
            if physical.desc != desc {
                continue;
            }
            limit += 1;
            if acquired
                .iter()
                .flatten()
                .any(|key| key.index as usize == index)
            {
                used += 1;
            } else if Arc::strong_count(physical) > 1 {
                retained_elsewhere = true;
            }
        }
        RuntimeError::capacity_with(
            error.message,
            RuntimeCapacityError::new(
                RuntimeCapacityKind::Resources,
                if retained_elsewhere {
                    CapacityRecovery::ReleaseOwners
                } else {
                    CapacityRecovery::Reconfigure
                },
                Some(limit),
                Some(used + 1),
            ),
        )
    }

    fn acquire(
        &mut self,
        desc: ResourceDesc,
        name: &str,
    ) -> Result<(ResourceKey, PhysicalLease), RuntimeError> {
        self.acquire_inner(desc, name, false)
    }

    fn physical_capacity_exhausted() -> RuntimeError {
        RuntimeError::capacity_with(
            "graph physical resource capacity exhausted",
            RuntimeCapacityError::new(
                RuntimeCapacityKind::Resources,
                CapacityRecovery::Reconfigure,
                None,
                None,
            ),
        )
    }

    fn acquire_inner(
        &mut self,
        desc: ResourceDesc,
        name: &str,
        allow_create: bool,
    ) -> Result<(ResourceKey, PhysicalLease), RuntimeError> {
        if matches!(desc, ResourceDesc::Image(desc) if desc.array_layers != 1) {
            return Err(RuntimeError::unsupported(
                "native images require one array layer",
            ));
        }
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
            .map_err(|()| Self::physical_capacity_exhausted())?;
        if !allow_create && !matches!(target, CacheTarget::Reuse(_)) {
            return Err(RuntimeError::capacity_with(
                "resource class has no prepared free instance",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Resources,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            ));
        }
        if let CacheTarget::Reuse(index) = target {
            self.available.remove(index);
            let entry = &mut self.entries[index];
            entry.generation += 1;
            let mut state = entry.physical().lock();
            // Preserve actual native synchronization, never old logical contents.
            state.scheduled.initialized.clear();
            state.scheduled.provenance.clear();
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
        let creation = PhysicalPlan::prepare(&self.owner, desc)
            .map_err(|error| error.context(format_args!("resource {name:?}")))?;
        let scheduled = self
            .frame_capacity
            .map_or_else(|| Ok(Snapshot::default()), Snapshot::for_working_set)?;
        let physical = creation.create(&self.owner, name, scheduled)?;
        let index = replace.unwrap_or(self.entries.len());
        let key = self.install_physical(index, Arc::clone(&physical));
        Ok((
            key,
            PhysicalLease::new(physical, index, Arc::clone(&self.recycle)),
        ))
    }

    /// Install a successfully staged allocation in a reserved, eligible slot.
    fn install_physical(&mut self, index: usize, physical: Arc<Physical>) -> ResourceKey {
        let created =
            !matches!(&physical.native, NativeResource::Image(image) if image.is_external());
        let replace = index < self.entries.len();
        let generation = if replace {
            self.entries[index].generation + 1
        } else {
            1
        };
        let entry = Entry {
            generation,
            registered: false,
            pending_registration: 0,
            physical: Some(physical),
        };
        if replace {
            self.available.remove(index);
            if let Ok(position) = self.vacant.binary_search(&index) {
                self.vacant.remove(position);
            }
            self.entries[index] = entry;
        } else {
            self.entries.push(entry);
        }
        self.resources_created += usize::from(created);
        ResourceKey {
            owner: self.id,
            index: index as u32,
            generation,
        }
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
        #[cfg(feature = "tracy")]
        let prepare_prelude = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.vulkan.graph.prepare_prelude"),
                0,
            )
        });
        self.collect_recycled();
        let profile_started = self.profiling_enabled.then(Instant::now);
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        let serial = self.serial.checked_add(1).ok_or_else(|| {
            RuntimeError::capacity_with(
                "submission serial exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Identity,
                    CapacityRecovery::Recreate,
                    None,
                    None,
                ),
            )
        })?;
        // Reserve native capacity before accepting any other bounded resources.
        let reservation = self.execution.reserve()?;
        let timestamp = if self.profiling_enabled {
            Some(self.timestamp_pool.as_ref().unwrap().acquire()?)
        } else {
            None
        };
        let mut work = GraphWork::acquire(
            Arc::clone(&self.owner),
            self.workspaces.as_ref().ok_or_else(|| {
                RuntimeError::capacity_with(
                    "frame working set is not prepared",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::WorkingSet,
                        CapacityRecovery::Prepare,
                        None,
                        None,
                    ),
                )
            })?,
            self.results.as_ref().ok_or_else(|| {
                RuntimeError::capacity_with(
                    "frame result working set is not prepared",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::WorkingSet,
                        CapacityRecovery::Prepare,
                        None,
                        None,
                    ),
                )
            })?,
            graph,
            serial,
        )?;
        let max_color_attachments = crate::program::MAX_RASTER_COLOR_ATTACHMENTS
            .min(self.owner.device.limits().max_color_attachments as usize);
        for &pass in &graph.order {
            match &graph.passes[pass].operation {
                Operation::Raster { colors, .. } | Operation::AttachmentPass { colors, .. }
                    if colors.len() > max_color_attachments =>
                {
                    return Err(RuntimeError::unsupported(
                        "rendering scope exceeds Device color attachment count",
                    ));
                }
                Operation::Raster {
                    batches,
                    colors,
                    depth,
                    ..
                } => {
                    let programs = self.programs.as_ref().ok_or_else(|| {
                        RuntimeError::capacity_with(
                            "Raster programs are not prepared",
                            RuntimeCapacityError::new(
                                RuntimeCapacityKind::Programs,
                                CapacityRecovery::Prepare,
                                None,
                                None,
                            ),
                        )
                    })?;
                    let sample_count = attachment_samples(graph, colors, *depth);
                    for batch in graph.raster_batches(batches) {
                        if let Some(indirect) = batch.indirect
                            && indirect.command_count > self.owner.device.max_draw_indirect_count()
                        {
                            return Err(RuntimeError::unsupported(
                                "indirect batch exceeds the Device's enabled draw count limit",
                            ));
                        }
                        let slot = programs.select_raster_program(
                            batch.program,
                            depth.is_some(),
                            sample_count,
                        )?;
                        // Every owned batch has at least one draw. Work
                        // acquisition already verified the total draw bound.
                        work.raster_programs.push(slot);
                    }
                }
                _ => {}
            }
            if let Operation::Dispatch { program, .. } = &graph.passes[pass].operation
                && !self
                    .compute_programs
                    .as_ref()
                    .is_some_and(|programs| programs.supports(*program))
            {
                return Err(RuntimeError::capacity_with(
                    "Compute Program is not prepared",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Programs,
                        CapacityRecovery::Prepare,
                        None,
                        None,
                    ),
                ));
            }
            if let Operation::Declared(kind) = &graph.passes[pass].operation {
                return Err(RuntimeError::unsupported(format!(
                    "pass {} is declaration-only {kind:?}; native execution unsupported",
                    graph.pass_name(pass)
                )));
            }
            for access in graph.accesses(pass) {
                if matches!(
                    access.purpose,
                    crate::graph::AccessPurpose::ColorAttachment
                        | crate::graph::AccessPurpose::DepthAttachment
                        | crate::graph::AccessPurpose::ColorResolve
                ) && (access.range != (0..1)
                    || matches!(graph.resources[access.resource].desc, ResourceDesc::Image(desc) if desc.mip_levels != 1))
                {
                    return Err(RuntimeError::unsupported(
                        "native attachments require a single-mip single-layer image",
                    ));
                }
                if matches!(graph.resources[access.resource].desc, ResourceDesc::Image(desc) if desc.array_layers != 1)
                {
                    return Err(RuntimeError::unsupported(
                        "native images require one array layer",
                    ));
                }
                work.needed[access.resource] = true;
            }
        }
        for &resource in &graph.exports {
            work.needed[resource] = true;
        }
        // Validate logical contents, including culled imports, before allocation.
        // Read-only uses may have advanced native scopes/layouts since compile;
        // prepare rebases those from the latest canonical scheduled state.
        #[cfg(feature = "tracy")]
        let import_check = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.vulkan.graph.import_revisions"),
                0,
            )
        });
        for (resource_index, resource) in graph.resources.iter().enumerate() {
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
                    || state.scheduled.initialized != graph.initial_contents(resource_index);
                if inconsistent {
                    return Err(RuntimeError::stale(format!(
                        "stale or inconsistent import snapshot for {}; refresh and recompile",
                        graph.resource_name(resource_index)
                    )));
                }
            }
        }
        #[cfg(feature = "tracy")]
        drop(import_check);
        #[cfg(feature = "tracy")]
        let resource_leases = tracy_client::Client::running().map(|client| {
            client.span(
                tracy_client::span_location!("renderer.vulkan.graph.resource_leases"),
                0,
            )
        });
        for (index, resource) in graph.resources.iter().enumerate() {
            if !work.needed[index] {
                continue;
            }
            let (key, value) = if let Some(key) = resource.import {
                (key, self.lease(key)?)
            } else {
                self.acquire(resource.desc, graph.resource_name(index))
                    .map_err(|error| {
                        self.resource_capacity_context(error, resource.desc, &work.keys[..index])
                    })?
            };
            work.keys[index] = Some(key);
            work.physical[index] = Some(value);
        }
        #[cfg(feature = "tracy")]
        drop(resource_leases);
        let mut uses_raster = false;
        let mut binding_count = 0usize;
        let mut textured_raster = false;
        for &pass in &graph.order {
            let count = match &graph.passes[pass].operation {
                Operation::Raster { batches, depth, .. } => {
                    uses_raster = true;
                    let count = graph
                        .raster_batches(batches)
                        .iter()
                        .map(|batch| graph.raster_draws(&batch.draws))
                        // A batch contains at least one draw and uses one Program ABI.
                        .filter(|draws| !draws[0].bindings.is_empty())
                        .flatten()
                        .filter(|draw| {
                            work.raster_bindings
                                .require(&draw.bindings, depth.is_some())
                        })
                        .count();
                    textured_raster |= count != 0;
                    count
                }
                Operation::Dispatch { .. } => 1,
                _ => 0,
            };
            binding_count = binding_count
                .checked_add(count)
                .filter(|&count| count <= self.config.max_shader_bindings as usize)
                .ok_or_else(|| {
                    RuntimeError::capacity_with(
                        "Graph descriptor binding capacity exhausted",
                        RuntimeCapacityError::new(
                            RuntimeCapacityKind::Descriptors,
                            CapacityRecovery::Reconfigure,
                            None,
                            None,
                        ),
                    )
                })?;
        }
        let programs = if uses_raster {
            Some(Arc::clone(self.programs.as_ref().ok_or_else(|| {
                RuntimeError::capacity_with(
                    "shader working set is not prepared",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::WorkingSet,
                        CapacityRecovery::Prepare,
                        None,
                        None,
                    ),
                )
            })?))
        } else {
            None
        };
        let bindings = if textured_raster {
            Some(programs.as_ref().unwrap().acquire_bindings()?)
        } else {
            None
        };
        let compute = if graph
            .order
            .iter()
            .any(|&pass| matches!(graph.passes[pass].operation, Operation::Dispatch { .. }))
        {
            Some(self.compute_programs.as_ref().unwrap().acquire()?)
        } else {
            None
        };
        #[cfg(feature = "graph-diagnostics")]
        let mut diagnostic = capture_diagnostics.then(|| {
            diagnostics::begin(
                graph,
                serial,
                self.owner.device.queue_family(),
                &work.keys[..graph.resources.len()],
                &work.physical[..graph.resources.len()],
            )
        });
        #[cfg(not(feature = "graph-diagnostics"))]
        let diagnostic = ();
        #[cfg(feature = "tracy")]
        drop(prepare_prelude);
        let mut work = work.prepare(
            graph,
            programs,
            bindings,
            compute,
            &self.upload,
            &self.readback,
            #[cfg(feature = "graph-diagnostics")]
            diagnostic.as_mut(),
        )?;
        work.profile = timestamp.map(|lease| {
            metrics::WorkProfile::new(
                lease,
                profile_started.expect("profiling enabled"),
                Arc::clone(&work.result.cpu_pending),
            )
        });
        let cpu_pending = work
            .profile
            .as_ref()
            .map(|profile| Arc::clone(&profile.cpu_pending));
        let mut exports = self
            .exports
            .as_mut()
            .ok_or_else(|| {
                RuntimeError::capacity_with(
                    "frame metadata working set is not prepared",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::WorkingSet,
                        CapacityRecovery::Prepare,
                        None,
                        None,
                    ),
                )
            })?
            .acquire()?;
        for &index in &graph.exports {
            let key = work.keys[index].unwrap();
            let snapshot = work.final_states[index].as_ref().unwrap();
            exports.insert(
                index,
                export_snapshot(self, key, graph.resources[index].desc, snapshot).map_err(
                    |error| {
                        if let Some(capacity) = error.capacity().filter(|capacity| {
                            capacity.resource == RuntimeCapacityKind::ImportRanges
                        }) {
                            let required = graph
                                .exports
                                .iter()
                                .filter(|&&resource| {
                                    matches!(
                                        graph.resources[resource].desc,
                                        ResourceDesc::Buffer(_)
                                    )
                                })
                                .count() as u64;
                            error.with_capacity_details(capacity.simultaneous_slots(required))
                        } else {
                            error
                        }
                    },
                )?,
            )?;
        }
        // Slots become externally named only for an actual export. The local
        // payload leases already prevent reuse throughout this transaction.
        let record_started = cpu_pending.as_ref().map(|_| Instant::now());
        let mut work = work;
        if let Some(recorder) = &mut self.secondary {
            work.record_secondaries(recorder)?;
        }
        let recorded = reservation.record(work)?;
        if let (Some(cpu), Some(started)) = (&cpu_pending, record_started) {
            cpu.lock().unwrap_or_else(|error| error.into_inner())[0] = started.elapsed();
        }
        for &logical in &graph.exports {
            let key = exports.get(logical).expect("prepared exported key").key();
            let index = key.index as usize;
            if !self.entries[index].registered {
                self.entries[index].pending_registration = serial;
                self.register(index);
            }
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
            exports: &exports,
            logical: &graph.exports,
            serial,
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
impl Export {
    fn key(&self) -> ResourceKey {
        match self {
            Self::Buffer(value) => value.key(),
            Self::Image(value) => value.key(),
        }
    }
}
fn export_snapshot(
    runtime: &VulkanRuntime,
    key: ResourceKey,
    desc: ResourceDesc,
    snapshot: &Snapshot,
) -> Result<Export, RuntimeError> {
    Ok(match desc {
        ResourceDesc::Buffer(desc) => Export::Buffer(
            ImportBuffer::new(
                key,
                snapshot.revision,
                desc,
                runtime.capture_ranges(&snapshot.initialized)?,
            )
            .map_err(|error| RuntimeError::invalid(error.to_string()))?,
        ),
        ResourceDesc::Image(desc) => Export::Image(
            ImportImage::new(key, snapshot.revision, desc, &snapshot.initialized)
                .map_err(|error| RuntimeError::invalid(error.to_string()))?,
        ),
    })
}

/// Owns a pending submission, never a CPU-readiness promise. Its bounded Drop
/// cleanup follows the existing transfer retirement contract.
pub struct Submission {
    id: FrameId,
    graph: u64,
    pending: PendingTransfer<GraphWork>,
    exports: ExportLease,
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
            .get(handle.index)
            .and_then(|value| match value {
                Export::Buffer(value) => Some(value),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::invalid("buffer was not exported"))
    }
    pub fn export_image(&self, handle: ImageHandle) -> Result<ImportImage, RuntimeError> {
        if handle.graph != self.graph {
            return Err(RuntimeError::wrong_owner("foreign graph export handle"));
        }
        self.exports
            .get(handle.index)
            .and_then(|value| match value {
                Export::Image(value) => Some(value),
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
    fn readback_index(&self, pass: PassHandle) -> Result<usize, RuntimeError> {
        if pass.graph != self.graph {
            return Err(RuntimeError::wrong_owner("foreign graph readback handle"));
        }
        self.work
            .result
            .readbacks
            .binary_search_by_key(&pass.index, |(index, _)| *index)
            .map_err(|_| RuntimeError::invalid("pass has no readback result"))
    }

    /// Exact size of an unconsumed readback. No native mapping or allocation.
    pub fn readback_len(&self, pass: PassHandle) -> Result<usize, RuntimeError> {
        let index = self.readback_index(pass)?;
        let value = self.work.result.readbacks[index]
            .1
            .as_ref()
            .ok_or_else(|| RuntimeError::invalid("readback was already consumed"))?;
        usize::try_from(value.size())
            .map_err(|_| RuntimeError::unsupported("readback exceeds CPU address space"))
    }

    /// Copy into caller-owned prepared storage and return this readback range.
    /// A length mismatch or failed read preserves the lease for retry. No native
    /// pointer or mapped memory escapes, and no output backing is allocated.
    pub fn readback_into(
        &mut self,
        pass: PassHandle,
        output: &mut [u8],
    ) -> Result<(), RuntimeError> {
        let index = self.readback_index(pass)?;
        let (_, range) = &mut self.work.result.readbacks[index];
        let value = range
            .as_mut()
            .ok_or_else(|| RuntimeError::invalid("readback was already consumed"))?;
        value.read(output)?;
        range.take();
        Ok(())
    }

    // Content-focused unit/GPU acceptance uses an owned output for assertions.
    // Allocation acceptance and every production consumer use readback_into.
    #[cfg(test)]
    pub(super) fn test_readback(&mut self, pass: PassHandle) -> Result<Vec<u8>, RuntimeError> {
        let mut bytes = vec![0; self.readback_len(pass)?];
        self.readback_into(pass, &mut bytes)?;
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

#[cfg(test)]
mod registration_tests {
    use super::*;

    #[test]
    fn rejected_exports_rollback_only_new_names_and_acceptance_keeps_them() {
        let mut pool = ExportPool::new(2, 1).unwrap();
        let mut exports = pool.acquire().unwrap();
        for index in 0..2 {
            exports
                .insert(
                    index,
                    Export::Image(ImportImage::test_initialized(
                        ResourceKey {
                            owner: 1,
                            index: index as u32,
                            generation: 1,
                        },
                        41,
                        crate::graph::ImageDesc::rgba8(1, 1),
                    )),
                )
                .unwrap();
        }
        let mut entries = [
            Entry {
                generation: 1,
                registered: true,
                pending_registration: 41,
                physical: None,
            },
            Entry {
                generation: 1,
                registered: true,
                pending_registration: 0,
                physical: None,
            },
        ];
        drop(Registration {
            entries: &mut entries,
            exports: &exports,
            logical: &[0, 1],
            serial: 41,
            keep: false,
        });
        assert!(!entries[0].registered);
        assert!(
            entries[1].registered,
            "an existing import remains named on rejected work"
        );
        entries[0].registered = true;
        entries[0].pending_registration = 42;
        drop(Registration {
            entries: &mut entries,
            exports: &exports,
            logical: &[0, 1],
            serial: 42,
            keep: true,
        });
        assert!(entries.iter().all(|entry| entry.registered));
        assert!(entries.iter().all(|entry| entry.pending_registration == 0));
    }
}
