//! Private staging for explicit working-set reconfiguration.

use super::*;
use crate::backends::vulkan::resources::{ImageCreation, validate_buffer_size};

const PREPARED_RESOURCE_NAME: &str = "prepared graph resource";

#[derive(Clone, Copy)]
pub(super) enum PhysicalPlan {
    Buffer(crate::graph::BufferDesc),
    Image(crate::graph::ImageDesc, ImageCreation),
}

impl PhysicalPlan {
    pub(super) fn prepare(owner: &MemoryOwner, desc: ResourceDesc) -> Result<Self, RuntimeError> {
        match desc {
            ResourceDesc::Buffer(desc) => Ok(Self::Buffer(desc)),
            ResourceDesc::Image(desc) => Ok(Self::Image(desc, DeviceImage::prepare(owner, desc)?)),
        }
    }

    pub(super) fn create(
        self,
        owner: &Arc<MemoryOwner>,
        name: &str,
        scheduled: Snapshot,
    ) -> Result<Arc<Physical>, RuntimeError> {
        let (desc, native) = match self {
            Self::Buffer(desc) => (
                ResourceDesc::Buffer(desc),
                DeviceBuffer::new(Arc::clone(owner), desc.size, name).map(NativeResource::Buffer),
            ),
            Self::Image(desc, creation) => (
                ResourceDesc::Image(desc),
                DeviceImage::create(Arc::clone(owner), creation, name).map(NativeResource::Image),
            ),
        };
        let native = native.map_err(|error| error.context(format_args!("resource {name:?}")))?;
        Ok(Arc::new(Physical {
            owner: Arc::clone(owner),
            desc,
            native,
            state: Mutex::new(State {
                scheduled,
                completed_serial: 0,
            }),
        }))
    }
}

#[derive(Clone, Copy)]
enum ResourceChange {
    Keep,
    Reuse,
    Create(PhysicalPlan),
}

enum PreparedResource {
    Vacant,
    ReleaseIdle,
    Keep(Snapshot),
    Reuse(Snapshot),
    Create(Arc<Physical>),
}

pub(super) struct PreparedResources(Vec<PreparedResource>);

impl PreparedResources {
    pub(super) fn descriptors<'a>(
        &'a self,
        runtime: &'a VulkanRuntime,
    ) -> impl Iterator<Item = ResourceDesc> + 'a {
        self.0
            .iter()
            .enumerate()
            .filter_map(move |(index, resource)| match resource {
                PreparedResource::Vacant | PreparedResource::ReleaseIdle => None,
                PreparedResource::Keep(_) | PreparedResource::Reuse(_) => {
                    Some(runtime.entries[index].physical().desc)
                }
                PreparedResource::Create(physical) => Some(physical.desc),
            })
    }

    pub(super) fn commit(self, runtime: &mut VulkanRuntime) {
        for (index, resource) in self.0.into_iter().enumerate() {
            match resource {
                PreparedResource::Vacant => (),
                PreparedResource::ReleaseIdle => runtime.release_idle_physical(index),
                PreparedResource::Keep(snapshot) => {
                    runtime.entries[index].physical().lock().scheduled = snapshot;
                }
                PreparedResource::Reuse(snapshot) => {
                    let entry = &mut runtime.entries[index];
                    entry.generation += 1;
                    entry.physical().lock().scheduled = snapshot;
                    runtime.resources_reused += 1;
                }
                PreparedResource::Create(physical) => {
                    runtime.install_physical(index, physical);
                    runtime.recycle_entry(index);
                }
            }
        }
    }
}

impl VulkanRuntime {
    fn plan_resource_changes(
        &self,
        resources: &[ResourceClass],
        instances: usize,
    ) -> Result<Vec<ResourceChange>, RuntimeError> {
        let capacity = self.config.resource_capacity;
        let mut changes =
            storage::reserved(self.entries.len().saturating_add(instances).min(capacity))?;
        changes.resize(self.entries.len(), ResourceChange::Keep);
        if resources.is_empty() {
            return Ok(changes);
        }
        let mut available = self
            .available
            .for_preparation()
            .map_err(|error| RuntimeError::with_source(RuntimeErrorKind::Native, error))?;
        let mut vacant = self.vacant.iter().copied().peekable();
        for class in resources {
            let (desc, count) = match *class {
                ResourceClass::Buffer { desc, count } => (ResourceDesc::Buffer(desc), count),
                ResourceClass::Image { desc, count } => (ResourceDesc::Image(desc), count),
            };
            let mut creation = None;
            for _ in 0..count {
                let selected = available
                    .select(
                        desc.into(),
                        changes.len(),
                        capacity,
                        vacant.peek().copied(),
                        |index| self.entries.get(index).is_some_and(Entry::reusable),
                    )
                    .map_err(|()| Self::physical_capacity_exhausted())?;
                if let CacheTarget::Reuse(index) = selected {
                    available.remove(index);
                    changes[index] = ResourceChange::Reuse;
                    continue;
                }
                let creation = match creation {
                    Some(creation) => creation,
                    None => {
                        let plan = PhysicalPlan::prepare(&self.owner, desc).map_err(|error| {
                            error.context(format_args!("resource {PREPARED_RESOURCE_NAME:?}"))
                        })?;
                        creation = Some(plan);
                        plan
                    }
                };
                match selected {
                    CacheTarget::Reuse(_) => unreachable!("handled before creation"),
                    CacheTarget::Create => changes.push(ResourceChange::Create(creation)),
                    CacheTarget::Replace(index) => {
                        available.remove(index);
                        if vacant.peek() == Some(&index) {
                            vacant.next();
                        }
                        changes[index] = ResourceChange::Create(creation);
                    }
                }
            }
        }
        Ok(changes)
    }

    pub(super) fn stage_resources(
        &self,
        capacity: FrameCapacity,
        resources: &[ResourceClass],
        instances: usize,
        replace_idle: bool,
        mut create: impl FnMut(
            PhysicalPlan,
            &Arc<MemoryOwner>,
            &str,
            Snapshot,
        ) -> Result<Arc<Physical>, RuntimeError>,
    ) -> Result<PreparedResources, RuntimeError> {
        // Validate all requested buffer descriptions and plan all replacements
        // before making any native allocations. A matching live image already
        // passed the immutable Device capability checks during its creation.
        for class in resources {
            if let ResourceClass::Buffer { desc, .. } = *class {
                validate_buffer_size(desc.size, self.owner.device.max_buffer_size()).map_err(
                    |error| error.context(format_args!("resource {PREPARED_RESOURCE_NAME:?}")),
                )?;
            }
        }
        let changes = self.plan_resource_changes(resources, instances)?;
        let mut prepared = storage::reserved(changes.len())?;
        for (index, change) in changes.into_iter().enumerate() {
            let resource = match change {
                // Reused and replaced entries have already been selected.
                // Only the common explicit replacement discards unused cached
                // resources. The raw Vulkan preparation keeps its cache.
                ResourceChange::Keep if replace_idle && self.entries[index].idle() => {
                    PreparedResource::ReleaseIdle
                }
                ResourceChange::Keep => match &self.entries[index].physical {
                    Some(physical) => {
                        let mut snapshot = Snapshot::for_working_set(capacity)?;
                        snapshot.copy_from(&physical.lock().scheduled)?;
                        PreparedResource::Keep(snapshot)
                    }
                    None => PreparedResource::Vacant,
                },
                ResourceChange::Reuse => {
                    let mut snapshot = Snapshot::for_working_set(capacity)?;
                    snapshot.copy_native_from(&self.entries[index].physical().lock().scheduled);
                    PreparedResource::Reuse(snapshot)
                }
                ResourceChange::Create(creation) => PreparedResource::Create(create(
                    creation,
                    &self.owner,
                    PREPARED_RESOURCE_NAME,
                    Snapshot::for_working_set(capacity)?,
                )?),
            };
            prepared.push(resource);
        }
        Ok(PreparedResources(prepared))
    }
}
