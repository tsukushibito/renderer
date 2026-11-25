use std::{collections::HashMap, sync::Arc};

use super::proof::{IndexBounds, Proof};
use super::{Error, Kind, capacity, format, unsupported};
use crate::capacity::{
    CapacityRecovery as Recovery, ResourceClass, RuntimeCapacityKind as Capacity,
};
use crate::graph::{
    BufferDesc, CompiledGraph, ImageDesc, ImageView, ImportBuffer, ImportImage, ImportRangesPool,
    ResourceDesc, ResourceKey, SampleCount,
};
use crate::program::{AddressMode, FilterMode, MipFilter, SamplerDescriptor};
use crate::runtime::RuntimeLimits;

#[derive(Clone)]
pub(super) enum Snapshot {
    Buffer(ImportBuffer),
    Image(ImportImage),
}

impl Snapshot {
    fn revision(&self) -> u64 {
        match self {
            Self::Buffer(v) => v.revision(),
            Self::Image(v) => v.revision(),
        }
    }
}

enum Physical {
    Vacant,
    Buffer(wgpu::Buffer),
    Image {
        texture: wgpu::Texture,
        views: Vec<wgpu::TextureView>,
    },
}

struct Entry {
    physical: Physical,
    class: ResourceDesc,
    registered: Option<Snapshot>,
    generation: u32,
    presentation: Arc<()>,
    last_use: u64,
    selected: u64,
}

impl Entry {
    fn fits(&self, desc: ResourceDesc) -> bool {
        match (self.class, desc) {
            (ResourceDesc::Buffer(prepared), ResourceDesc::Buffer(needed)) => {
                prepared.size >= needed.size
            }
            _ => self.class == desc,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Usage {
    slot: Option<usize>,
    changed: bool,
    live: bool,
}

/// Graph-local operands bound to prepared physical slots for this submission.
pub(in crate::webgpu) struct Resources<'a> {
    entries: &'a [Entry],
    uses: &'a [Usage],
    samplers: &'a HashMap<SamplerDescriptor, wgpu::Sampler>,
}

impl Resources<'_> {
    fn entry(&self, index: usize) -> Result<&Entry, Error> {
        self.uses
            .get(index)
            .and_then(|u| u.slot)
            .and_then(|i| self.entries.get(i))
            .ok_or_else(|| Error::invalid("unbound WebGPU Graph resource"))
    }

    pub(in crate::webgpu) fn buffer(&self, index: usize) -> Result<&wgpu::Buffer, Error> {
        match &self.entry(index)?.physical {
            Physical::Buffer(buffer) => Ok(buffer),
            _ => Err(Error::invalid("WebGPU operand requires a buffer")),
        }
    }

    pub(in crate::webgpu) fn texture(&self, index: usize) -> Result<&wgpu::Texture, Error> {
        match &self.entry(index)?.physical {
            Physical::Image { texture, .. } => Ok(texture),
            _ => Err(Error::invalid("WebGPU operand requires a texture")),
        }
    }

    pub(in crate::webgpu) fn view(&self, view: ImageView) -> Result<&wgpu::TextureView, Error> {
        let entry = self.entry(view.image().index)?;
        let (ResourceDesc::Image(desc), Physical::Image { views, .. }) =
            (entry.class, &entry.physical)
        else {
            return Err(Error::invalid("WebGPU operand requires an image view"));
        };
        // For each layer, enumerate (first mip, consecutive count) in lexical order.
        let levels = desc.mip_levels;
        let mip = view.mip_level();
        let start = mip * (2 * levels + 1 - mip) / 2;
        let per_layer = levels * (levels + 1) / 2;
        let index = view.array_layer() as usize * per_layer as usize
            + (start + view.mip_level_count() - 1) as usize;
        views
            .get(index)
            .ok_or_else(|| Error::invalid("WebGPU view exceeds prepared texture"))
    }

    pub(in crate::webgpu) fn sampler(
        &self,
        descriptor: SamplerDescriptor,
    ) -> Result<&wgpu::Sampler, Error> {
        self.samplers
            .get(&descriptor)
            .ok_or_else(|| unsupported("sampler outside prepared working set"))
    }
}

fn image_usage(desc: ImageDesc) -> wgpu::TextureUsages {
    use wgpu::TextureUsages as U;
    let mut usage = U::TEXTURE_BINDING;
    if desc.format.supports_attachment() {
        usage |= U::RENDER_ATTACHMENT;
    }
    if desc.sample_count == SampleCount::X1 {
        usage |= U::COPY_SRC | U::COPY_DST;
        if desc.format.supports_storage() {
            usage |= U::STORAGE_BINDING;
        }
    }
    usage
}

pub(super) fn check_image(
    device: &wgpu::Device,
    desc: ImageDesc,
    linear: bool,
) -> Result<(), Error> {
    if desc.byte_len().is_none() {
        return Err(Error::invalid("invalid WebGPU image extent or mip count"));
    }
    let limits = device.limits();
    if desc.width.max(desc.height) > limits.max_texture_dimension_2d
        || desc.array_layers > limits.max_texture_array_layers
    {
        return Err(unsupported("scene texture exceeds WebGPU device limits"));
    }
    if desc.format.is_block_compressed()
        && (!device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
            || !desc.width.is_multiple_of(4)
            || !desc.height.is_multiple_of(4))
    {
        return Err(unsupported(
            "BC texture requires BC support and a block-aligned base level",
        ));
    }
    let caps = format(desc.format).guaranteed_format_features(device.features());
    if !caps.allowed_usages.contains(image_usage(desc))
        || !caps
            .flags
            .sample_count_supported(desc.sample_count.as_u32())
    {
        return Err(unsupported(
            "WebGPU texture lacks required usage or sample count",
        ));
    }
    if linear
        && !caps
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::FILTERABLE)
    {
        return Err(unsupported("WebGPU texture format is not filterable"));
    }
    if desc.sample_count != SampleCount::X1 && desc.array_layers != 1 {
        return Err(unsupported("multisampled WebGPU textures need one layer"));
    }
    Ok(())
}

fn create(device: &wgpu::Device, desc: ResourceDesc) -> Result<Physical, Error> {
    match desc {
        ResourceDesc::Buffer(desc) => {
            let size = desc
                .size
                .checked_next_multiple_of(4)
                .ok_or_else(|| unsupported("buffer size overflow"))?;
            if size == 0 || size > device.limits().max_buffer_size {
                return Err(unsupported("scene buffer exceeds WebGPU device limits"));
            }
            use wgpu::BufferUsages as U;
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size,
                usage: U::COPY_SRC
                    | U::COPY_DST
                    | U::UNIFORM
                    | U::STORAGE
                    | U::VERTEX
                    | U::INDEX
                    | U::INDIRECT,
                mapped_at_creation: false,
            });
            Ok(Physical::Buffer(buffer))
        }
        ResourceDesc::Image(desc) => {
            check_image(device, desc, false)?;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: desc.width,
                    height: desc.height,
                    depth_or_array_layers: desc.array_layers,
                },
                mip_level_count: desc.mip_levels,
                sample_count: desc.sample_count.as_u32(),
                dimension: wgpu::TextureDimension::D2,
                format: format(desc.format),
                usage: image_usage(desc),
                view_formats: &[],
            });
            let mut views = Vec::new();
            for layer in 0..desc.array_layers {
                for mip in 0..desc.mip_levels {
                    for count in 1..=desc.mip_levels - mip {
                        views.push(texture.create_view(&wgpu::TextureViewDescriptor {
                            dimension: Some(wgpu::TextureViewDimension::D2),
                            base_mip_level: mip,
                            mip_level_count: Some(count),
                            base_array_layer: layer,
                            array_layer_count: Some(1),
                            ..Default::default()
                        }));
                    }
                }
            }
            Ok(Physical::Image { texture, views })
        }
    }
}

fn address(mode: AddressMode) -> wgpu::AddressMode {
    match mode {
        AddressMode::Repeat => wgpu::AddressMode::Repeat,
        AddressMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
        AddressMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
    }
}
fn filter(mode: FilterMode) -> wgpu::FilterMode {
    match mode {
        FilterMode::Nearest => wgpu::FilterMode::Nearest,
        FilterMode::Linear => wgpu::FilterMode::Linear,
    }
}

pub(super) struct Registry {
    pub(super) proofs: Vec<Proof>,
    owner: u64,
    stamp: u64,
    retired: Vec<(Arc<()>, Physical)>,
    entries: Vec<Entry>,
    samplers: HashMap<SamplerDescriptor, wgpu::Sampler>,
    uses: Vec<Usage>,
    // Additional leases keep the metadata separate from registration ownership.
    ranges: ImportRangesPool,
    limits: RuntimeLimits,
}

impl Registry {
    pub(super) fn new(owner: u64, limits: RuntimeLimits) -> Result<Self, Error> {
        let ranges = ImportRangesPool::new(
            limits.initialized_ranges_per_resource,
            limits.range_snapshots,
        )
        .map_err(|_| Error::invalid("cannot reserve WebGPU range snapshots"))?;
        Ok(Self {
            proofs: Vec::new(),
            owner,
            stamp: 0,
            retired: Vec::new(),
            entries: Vec::new(),
            samplers: HashMap::new(),
            uses: Vec::with_capacity(limits.graph.resources),
            ranges,
            limits,
        })
    }

    pub(super) fn prepare(
        &mut self,
        device: &wgpu::Device,
        limits: RuntimeLimits,
        classes: &[ResourceClass],
    ) -> Result<(), Error> {
        // Preserve every registered slot and its identity. Plan replacements before
        // publishing any change, so a later unsupported format leaves the old set usable.
        let mut replace = Vec::new();
        let mut wanted = Vec::new();
        for class in classes {
            let (desc, count) = match *class {
                ResourceClass::Buffer { desc, count } => (ResourceDesc::Buffer(desc), count),
                ResourceClass::Image { desc, count } => (ResourceDesc::Image(desc), count),
            };
            if count > limits.resource_capacity.saturating_sub(wanted.len()) {
                return Err(capacity(
                    Capacity::Resources,
                    Recovery::Reconfigure,
                    limits.resource_capacity,
                    wanted.len().saturating_add(count),
                ));
            }
            wanted.extend(std::iter::repeat_n(desc, count));
        }
        // An unchanged physical slot, including a registered one, can cover its class.
        let mut covered = vec![false; self.entries.len()];
        for desc in wanted {
            if let Some(index) = self.entries.iter().enumerate().position(|(i, e)| {
                !covered[i] && !matches!(e.physical, Physical::Vacant) && e.class == desc
            }) {
                covered[index] = true;
            } else {
                replace.push(desc);
            }
        }
        let mut available = (0..self.entries.len())
            .filter(|&i| !covered[i] && self.entries[i].registered.is_none());
        let mut appended = 0usize;
        let mut changes = Vec::new();
        for desc in replace {
            let index = available.next().unwrap_or_else(|| {
                let i = self.entries.len() + appended;
                appended += 1;
                i
            });
            if index >= limits.resource_capacity || index > u32::MAX as usize {
                return Err(capacity(
                    Capacity::Resources,
                    Recovery::Reconfigure,
                    limits.resource_capacity,
                    index + 1,
                ));
            }
            let generation = self.entries.get(index).map_or(0, |e| e.generation);
            changes.push((
                index,
                Entry {
                    physical: create(device, desc)?,
                    class: desc,
                    registered: None,
                    generation,
                    presentation: Arc::new(()),
                    last_use: 0,
                    selected: 0,
                },
            ));
        }
        let ranges = self
            .ranges
            .expanded_slots(limits.range_snapshots)
            .map_err(|_| Error::invalid("cannot expand WebGPU import snapshots"))?;
        self.uses
            .try_reserve(limits.graph.resources.saturating_sub(self.uses.len()))
            .map_err(|_| Error::invalid("cannot expand WebGPU resource mapping"))?;
        self.entries
            .try_reserve(appended)
            .map_err(|_| Error::invalid("cannot reserve WebGPU registry"))?;
        self.retired.retain(|(pin, _)| Arc::strong_count(pin) > 1);
        // Never renumber entries. Old tokens keep identifying the prior generation.
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if !covered[index] && entry.registered.is_none() {
                let old = std::mem::replace(&mut entry.physical, Physical::Vacant);
                if Arc::strong_count(&entry.presentation) > 1 && !matches!(old, Physical::Vacant) {
                    self.retired.push((entry.presentation.clone(), old));
                }
            }
        }
        for (index, entry) in changes {
            if index == self.entries.len() {
                self.entries.push(entry);
                self.proofs.push(Proof::new(limits));
            } else {
                self.entries[index] = entry;
                self.proofs[index] = Proof::new(limits);
            }
        }
        if let Some(ranges) = ranges {
            self.ranges = ranges;
        }
        self.limits = limits;
        Ok(())
    }

    pub(super) fn prepare_samplers(
        &mut self,
        device: &wgpu::Device,
        samplers: &[SamplerDescriptor],
    ) {
        for &s in samplers {
            self.samplers.entry(s).or_insert_with(|| {
                device.create_sampler(&wgpu::SamplerDescriptor {
                    address_mode_u: address(s.wrap_u),
                    address_mode_v: address(s.wrap_v),
                    mag_filter: filter(s.mag_filter),
                    min_filter: filter(s.min_filter),
                    mipmap_filter: if s.mip_filter == MipFilter::Linear {
                        wgpu::MipmapFilterMode::Linear
                    } else {
                        wgpu::MipmapFilterMode::Nearest
                    },
                    lod_max_clamp: if s.mip_filter == MipFilter::None {
                        0.0
                    } else {
                        32.0
                    },
                    ..Default::default()
                })
            });
        }
    }

    fn lookup(&self, key: ResourceKey) -> Result<(&Entry, &Snapshot), Error> {
        if key.owner != self.owner {
            return Err(Error::webgpu(Kind::WrongOwner, "foreign WebGPU resource"));
        }
        self.entries
            .get(key.index as usize)
            .filter(|e| e.generation == key.generation)
            .and_then(|e| e.registered.as_ref().map(|s| (e, s)))
            .ok_or_else(stale)
    }
    fn pick(&mut self, desc: ResourceDesc, completed: u64) -> Result<usize, Error> {
        let i = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.registered.is_none()
                    && !matches!(e.physical, Physical::Vacant)
                    && Arc::strong_count(&e.presentation) == 1
                    && e.last_use <= completed
                    && e.selected != self.stamp
                    && e.generation < u32::MAX
                    && e.fits(desc)
            })
            .min_by_key(|(_, e)| e.class.units())
            .map(|(i, _)| i)
            .ok_or_else(|| {
                capacity(
                    Capacity::Resources,
                    Recovery::Prepare,
                    self.entries.len(),
                    self.entries.len() + 1,
                )
            })?;
        self.entries[i].selected = self.stamp;
        Ok(i)
    }
    fn begin(&mut self) -> Result<(), Error> {
        self.stamp = self
            .stamp
            .checked_add(1)
            .ok_or_else(|| unsupported("WebGPU resource reservation identity exhausted"))?;
        Ok(())
    }
    pub(super) fn register_buffer(
        &mut self,
        desc: BufferDesc,
        completed: u64,
    ) -> Result<ResourceKey, Error> {
        self.begin()?;
        let i = self.pick(ResourceDesc::Buffer(desc), completed)?;
        let e = &mut self.entries[i];
        let key = ResourceKey {
            owner: self.owner,
            index: i as u32,
            generation: e.generation + 1,
        };
        let ranges = self.ranges.capture(&[]).map_err(|_| {
            capacity(
                Capacity::ImportRanges,
                Recovery::ReleaseOwners,
                self.limits.range_snapshots,
                self.limits.range_snapshots + 1,
            )
        })?;
        let snapshot = ImportBuffer::new(key, 0, desc, ranges)
            .map_err(|_| Error::invalid("invalid WebGPU buffer snapshot"))?;
        e.registered = Some(Snapshot::Buffer(snapshot));
        e.generation = key.generation;
        Ok(key)
    }
    pub(super) fn refresh_buffer(&self, key: ResourceKey) -> Result<ImportBuffer, Error> {
        match self.lookup(key)?.1 {
            Snapshot::Buffer(v) => Ok(v.clone()),
            _ => Err(Error::invalid("resource is an image")),
        }
    }
    pub(super) fn refresh_image(&self, key: ResourceKey) -> Result<ImportImage, Error> {
        match self.lookup(key)?.1 {
            Snapshot::Image(v) => Ok(v.clone()),
            _ => Err(Error::invalid("resource is a buffer")),
        }
    }
    pub(super) fn texture(&self, image: &ImportImage) -> Result<&wgpu::Texture, Error> {
        let (entry, current) = self.lookup(image.key())?;
        if current.revision() != image.revision() {
            return Err(stale());
        }
        if !image.initialized() {
            return Err(Error::invalid("WebGPU presentation image is undefined"));
        }
        match &entry.physical {
            Physical::Image { texture, .. } => Ok(texture),
            _ => Err(Error::invalid("resource is a buffer")),
        }
    }
    pub(super) fn mark_presented(
        &self,
        queue: &wgpu::Queue,
        image: &ImportImage,
    ) -> Result<(), Error> {
        let (entry, current) = self.lookup(image.key())?;
        if current.revision() != image.revision() {
            return Err(stale());
        }
        let lease = Arc::clone(&entry.presentation);
        queue.on_submitted_work_done(move || drop(lease));
        Ok(())
    }
    pub(super) fn delete(&mut self, key: ResourceKey) -> Result<(), Error> {
        self.lookup(key)?;
        self.entries[key.index as usize].registered = None;
        self.proofs[key.index as usize].discard();
        Ok(())
    }

    pub(super) fn bind(&mut self, graph: &CompiledGraph, completed: u64) -> Result<(), Error> {
        self.begin()?;
        if graph.resources.len() > self.limits.graph.resources {
            return Err(capacity(
                Capacity::Resources,
                Recovery::Reconfigure,
                self.limits.graph.resources,
                graph.resources.len(),
            ));
        }
        self.uses.clear();
        self.uses.resize(graph.resources.len(), Usage::default());
        for &p in &graph.order {
            for access in graph.accesses(p) {
                let u = &mut self.uses[access.resource];
                u.live = true;
                u.changed |= access.write || access.discard_after;
            }
        }
        for &i in &graph.exports {
            self.uses[i].live = true;
        }
        for (i, r) in graph.resources.iter().enumerate() {
            let slot = if let Some(key) = r.import {
                let (_, snapshot) = self.lookup(key)?;
                if snapshot.revision() != r.import_revision {
                    return Err(stale());
                }
                if self.uses[i].changed && r.import_revision == u64::MAX {
                    return Err(unsupported("WebGPU resource revision exhausted"));
                }
                key.index as usize
            } else if self.uses[i].live {
                self.pick(r.desc, completed)?
            } else {
                continue;
            };
            if self.uses[i].live {
                self.uses[i].slot = Some(slot);
            }
        }
        Ok(())
    }
    pub(super) fn prove(
        &mut self,
        graph: &CompiledGraph,
        bounds: &mut IndexBounds,
    ) -> Result<(), Error> {
        bounds.prepare(graph, |i| self.uses[i].slot);
        bounds.validate(graph, &mut self.proofs)
    }
    pub(super) fn resources(&self) -> Resources<'_> {
        Resources {
            entries: &self.entries,
            uses: &self.uses,
            samplers: &self.samplers,
        }
    }

    pub(super) fn snapshots(
        &mut self,
        graph: &CompiledGraph,
        updates: &mut Vec<(usize, Snapshot)>,
    ) -> Result<(), Error> {
        updates.clear();
        for (i, u) in self.uses.iter().enumerate() {
            let Some(physical) = u.slot else { continue };
            let r = &graph.resources[i];
            let export = graph.has_export(i);
            if !export && !(r.import.is_some() && u.changed) {
                continue;
            }
            let e = &self.entries[physical];
            if !u.changed
                && let Some(s) = e.registered.as_ref()
            {
                updates.push((i, s.clone()));
                continue;
            }
            let key = ResourceKey {
                owner: self.owner,
                index: physical as u32,
                generation: if r.import.is_some() {
                    e.generation
                } else {
                    e.generation + 1
                },
            };
            let revision = r.import_revision + u64::from(u.changed);
            let initialized = graph.final_contents(i);
            let snapshot = match r.desc {
                ResourceDesc::Buffer(desc) => {
                    let ranges = self.ranges.capture(initialized).map_err(|_| {
                        capacity(
                            Capacity::ImportRanges,
                            Recovery::ReleaseOwners,
                            self.limits.range_snapshots,
                            self.limits.range_snapshots + 1,
                        )
                    })?;
                    Snapshot::Buffer(
                        ImportBuffer::new(key, revision, desc, ranges)
                            .map_err(|_| Error::invalid("WebGPU buffer export ranges"))?,
                    )
                }
                ResourceDesc::Image(desc) => Snapshot::Image(
                    ImportImage::new(key, revision, desc, initialized)
                        .map_err(|_| Error::invalid("WebGPU image export ranges"))?,
                ),
            };
            updates.push((i, snapshot));
        }
        Ok(())
    }
    pub(super) fn commit(&mut self, serial: u64, updates: &[(usize, Snapshot)]) {
        for u in &self.uses {
            if let Some(p) = u.slot {
                self.entries[p].last_use = serial;
            }
        }
        for (i, s) in updates {
            let e = &mut self.entries[self.uses[*i].slot.expect("prepared resource")];
            let key = match s {
                Snapshot::Buffer(s) => s.key(),
                Snapshot::Image(s) => s.key(),
            };
            e.generation = key.generation;
            e.registered = Some(s.clone());
        }
    }
}
fn stale() -> Error {
    Error::webgpu(Kind::StaleResource, "stale WebGPU resource snapshot")
}
