//! Explicitly prepared Graph images and descriptor storage for one frame slot.

use crate::ImageFormat;
use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};
use crate::graph::ImageDesc;
use windows_core::Interface;

use super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    frame::{NativeObject, object},
    resources::DeviceImage,
};

/// One exact image description and the unregistered physical instances reserved
/// globally across execution slots, in addition to retained registered images.
/// Each execution reserves its own reference/View metadata; native instances
/// are not multiplied by the number of simultaneous executions.
#[derive(Clone, Copy, Debug)]
pub struct D3D12ImageClass {
    pub desc: ImageDesc,
    pub count: usize,
}

pub(super) struct MipCopyLayout {
    pub(super) footprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    pub(super) row_bytes: usize,
    pub(super) rows: usize,
    pub(super) packed_offset: usize,
}

pub(super) struct GraphImage {
    pub(super) desc: ImageDesc,
    pub(super) native: super::registry::PhysicalRef,
    pub(super) state: D3D12_RESOURCE_STATES,
    pub(super) uav_write: bool,
    pub(super) used: bool,
    pub(super) rtv: Option<D3D12_CPU_DESCRIPTOR_HANDLE>,
    pub(super) dsv: Option<D3D12_CPU_DESCRIPTOR_HANDLE>,
    pub(super) footprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    pub(super) total_bytes: u64,
    pub(super) row_bytes: usize,
    pub(super) rows: usize,
    pub(super) mips: Vec<MipCopyLayout>,
}

pub(super) struct DescriptorHeap {
    pub(super) raw: NativeObject<ID3D12DescriptorHeap>,
    #[cfg(feature = "memory-profiling")]
    count: u64,
}

impl DescriptorHeap {
    pub(super) fn new(
        owner: &DeviceOwner,
        count: usize,
        kind: D3D12_DESCRIPTOR_HEAP_TYPE,
        visible: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        let count = u32::try_from(count)
            .ok()
            .filter(|count| *count != 0)
            .ok_or_else(|| D3D12RuntimeError::invalid("invalid prepared descriptor count"))?;
        let desc = D3D12_DESCRIPTOR_HEAP_DESC {
            Type: kind,
            NumDescriptors: count,
            Flags: if visible {
                D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE
            } else {
                D3D12_DESCRIPTOR_HEAP_FLAG_NONE
            },
            NodeMask: 0,
        };
        // SAFETY: Counts are nonzero and fit UINT; each caller supplies a valid
        // heap type and its permitted visibility at the preparation boundary.
        let raw = object(
            owner,
            "ID3D12Device::CreateDescriptorHeap(Graph)",
            || unsafe { owner.raw.CreateDescriptorHeap(&desc) },
        )?;
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(
            crate::measurement::NativeEvent::DescriptorAllocate,
            u64::from(count),
        );
        Ok(Self {
            raw,
            #[cfg(feature = "memory-profiling")]
            count: u64::from(count),
        })
    }
}

impl Drop for DescriptorHeap {
    fn drop(&mut self) {
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(
            crate::measurement::NativeEvent::DescriptorFree,
            self.count,
        );
    }
}

pub(super) struct RasterStorage {
    // Release descriptor owners before the images they reference. The containing
    // FramePool keeps the Device alive through every field's final destruction.
    _rtv_heap: Option<DescriptorHeap>,
    _dsv_heap: Option<DescriptorHeap>,
    srv: DescriptorHeap,
    sampler: DescriptorHeap,
    pub(super) images: Vec<GraphImage>,
    image_slots: ImageSlots,
    target_views: TargetViews,
    texture_cpu_start: D3D12_CPU_DESCRIPTOR_HANDLE,
    texture_gpu_start: D3D12_GPU_DESCRIPTOR_HANDLE,
    sampler_gpu_start: D3D12_GPU_DESCRIPTOR_HANDLE,
    texture_step: u32,
    sampler_step: u32,
    descriptor_capacity: usize,
    pub(super) raster_descriptor_capacity: usize,
    pub(super) compute_descriptor_start: usize,
    pub(super) compute_descriptor_capacity: usize,
    pub(super) scopes: PassScopes,
    raster_srvs: RasterSrvCache,
    #[cfg(test)]
    pub(super) srv_creations: usize,
    #[cfg(test)]
    pub(super) target_view_creations: [usize; 2],
}

type ImageClassKey = (u32, u32, u32, u32, u32);

fn image_class_key(desc: ImageDesc) -> ImageClassKey {
    (
        desc.width,
        desc.height,
        desc.format as u32,
        desc.mip_levels,
        desc.array_layers,
    )
}

struct ImageSlotClass {
    key: ImageClassKey,
    slots: std::ops::Range<usize>,
    next: usize,
}

/// Immutable description classes and their available local slots. Native
/// resources are still selected separately by the canonical registry. A class
/// lookup is O(log C), and taking its next slot is O(1), even when many logical
/// images have the same description or prepared classes repeat that description.
struct ImageSlots {
    classes: Vec<ImageSlotClass>,
    candidates: Vec<usize>,
    touched_classes: Vec<usize>,
}

impl ImageSlots {
    fn new(
        descriptions: impl ExactSizeIterator<Item = ImageDesc>,
    ) -> Result<Self, D3D12RuntimeError> {
        let total = descriptions.len();
        let mut sorted = reserved(total)?;
        sorted.extend(
            descriptions
                .enumerate()
                .map(|(index, desc)| (image_class_key(desc), index)),
        );
        sorted.sort_unstable();
        let class_count = usize::from(!sorted.is_empty())
            + sorted
                .windows(2)
                .filter(|pair| pair[0].0 != pair[1].0)
                .count();
        let mut classes: Vec<ImageSlotClass> = reserved(class_count)?;
        let mut candidates = reserved(total)?;
        for (key, index) in sorted {
            let position = candidates.len();
            if classes.last().is_none_or(|class| class.key != key) {
                classes.push(ImageSlotClass {
                    key,
                    slots: position..position,
                    next: position,
                });
            }
            candidates.push(index);
            classes.last_mut().unwrap().slots.end += 1;
        }
        Ok(Self {
            classes,
            candidates,
            touched_classes: reserved(class_count)?,
        })
    }

    fn take(&mut self, desc: ImageDesc) -> Result<usize, D3D12RuntimeError> {
        let class = self
            .classes
            .binary_search_by_key(&image_class_key(desc), |class| class.key);
        if let Ok(index) = class {
            let class = &mut self.classes[index];
            if class.next < class.slots.end {
                if class.next == class.slots.start {
                    // A class is appended once per exclusive lease, so its
                    // prepared count also bounds this reset list exactly.
                    self.touched_classes.push(index);
                }
                let image = self.candidates[class.next];
                class.next += 1;
                return Ok(image);
            }
        }
        Err(D3D12RuntimeError::capacity_with(
            "prepared Raster image class exhausted",
            RuntimeCapacityError::new(
                RuntimeCapacityKind::Resources,
                CapacityRecovery::Reconfigure,
                None,
                None,
            ),
        ))
    }

    fn reset(&mut self) {
        for index in self.touched_classes.drain(..) {
            let class = &mut self.classes[index];
            class.next = class.slots.start;
        }
    }
}

/// Only attachment accesses require CPU target descriptors. Deduplicate their
/// local slots so sampled/copy/storage-only images never receive unused views.
struct TargetViews {
    marked: Vec<bool>,
    indices: Vec<usize>,
}

impl TargetViews {
    fn new(images: usize) -> Result<Self, D3D12RuntimeError> {
        let mut marked = reserved(images)?;
        marked.resize(images, false);
        Ok(Self {
            marked,
            indices: reserved(images)?,
        })
    }

    fn require(&mut self, image: usize) {
        if !self.marked[image] {
            self.marked[image] = true;
            self.indices.push(image);
        }
    }

    fn reset(&mut self) {
        for image in self.indices.drain(..) {
            self.marked[image] = false;
        }
    }
}

/// Prepared per-pass state aggregation. The two native index namespaces remain
/// separate, and each touched list preserves its resource's first access order.
pub(super) struct PassScopes {
    buffers: Vec<D3D12_RESOURCE_STATES>,
    images: Vec<D3D12_RESOURCE_STATES>,
    touched_buffers: Vec<usize>,
    touched_images: Vec<usize>,
}

fn reserved<T>(capacity: usize) -> Result<Vec<T>, D3D12RuntimeError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| {
        D3D12RuntimeError::capacity_with(
            "Raster recording metadata reservation failed",
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

impl PassScopes {
    pub(super) fn new(buffers: usize, images: usize) -> Result<Self, D3D12RuntimeError> {
        let mut buffer_states = reserved(buffers)?;
        buffer_states.resize(buffers, D3D12_RESOURCE_STATE_COMMON);
        let mut image_states = reserved(images)?;
        image_states.resize(images, D3D12_RESOURCE_STATE_COMMON);
        Ok(Self {
            buffers: buffer_states,
            images: image_states,
            touched_buffers: reserved(buffers)?,
            touched_images: reserved(images)?,
        })
    }

    pub(super) fn buffer(&mut self, index: usize, scope: D3D12_RESOURCE_STATES) {
        if self.buffers[index] == D3D12_RESOURCE_STATE_COMMON {
            // Every trusted Raster use has a nonempty state. Only the first use
            // appends, so the prepared buffer count bounds this list exactly.
            self.touched_buffers.push(index);
        }
        self.buffers[index] |= scope;
    }

    pub(super) fn image(&mut self, index: usize, scope: D3D12_RESOURCE_STATES) {
        if self.images[index] == D3D12_RESOURCE_STATE_COMMON {
            self.touched_images.push(index);
        }
        self.images[index] |= scope;
    }

    pub(super) fn drain_buffers(&mut self, mut apply: impl FnMut(usize, D3D12_RESOURCE_STATES)) {
        // Retain the touched list until all callbacks finish. An unwind leaves
        // the remaining indices available to reset at the next exclusive lease.
        for &index in &self.touched_buffers {
            let scope = std::mem::replace(&mut self.buffers[index], D3D12_RESOURCE_STATE_COMMON);
            apply(index, scope);
        }
        self.touched_buffers.clear();
    }

    pub(super) fn drain_images(&mut self, mut apply: impl FnMut(usize, D3D12_RESOURCE_STATES)) {
        for &index in &self.touched_images {
            let scope = std::mem::replace(&mut self.images[index], D3D12_RESOURCE_STATE_COMMON);
            apply(index, scope);
        }
        self.touched_images.clear();
    }

    fn reset(&mut self) {
        self.drain_buffers(|_, _| {});
        self.drain_images(|_, _| {});
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RasterSrvKey {
    resource: usize,
    format: DXGI_FORMAT,
    first_mip: u32,
    mip_count: u32,
}

#[derive(Clone, Copy)]
struct RasterSrvEntry {
    key: RasterSrvKey,
    descriptor: usize,
}

/// One exclusive execution's immutable SRVs. Keys do not retain native owners:
/// the slot's pinned physical images already outlive all recorded/GPU uses.
struct RasterSrvCache {
    entries: Vec<Option<RasterSrvEntry>>,
    occupied: Vec<usize>,
    limit: usize,
}

impl RasterSrvCache {
    fn new(limit: usize) -> Result<Self, D3D12RuntimeError> {
        let size = limit
            .checked_mul(2)
            .and_then(|size| size.max(1).checked_next_power_of_two())
            .ok_or_else(|| Self::capacity_error(limit, None))?;
        let mut entries = reserved(size)?;
        entries.resize(size, None);
        Ok(Self {
            entries,
            occupied: reserved(limit)?,
            limit,
        })
    }

    fn capacity_error(limit: usize, required: Option<u64>) -> D3D12RuntimeError {
        D3D12RuntimeError::capacity_with(
            "prepared Raster SRV capacity exhausted",
            RuntimeCapacityError::new(
                RuntimeCapacityKind::Descriptors,
                CapacityRecovery::Reconfigure,
                Some(limit as u64),
                required,
            ),
        )
    }

    fn bucket(&self, key: RasterSrvKey) -> usize {
        // Full key equality resolves collisions; hashing is only an index into
        // this bounded table. At most half its buckets can become occupied.
        let hash = (key.resource as u64).rotate_left(21)
            ^ (u64::from(key.format as u32) << 32)
            ^ u64::from(key.first_mip).rotate_left(11)
            ^ u64::from(key.mip_count);
        let hash = hash.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (hash ^ (hash >> 32)) as usize & (self.entries.len() - 1)
    }

    fn get_or_insert(&mut self, key: RasterSrvKey) -> Result<(usize, bool), D3D12RuntimeError> {
        let mut bucket = self.bucket(key);
        loop {
            match self.entries[bucket] {
                Some(entry) if entry.key == key => return Ok((entry.descriptor, false)),
                Some(_) => bucket = (bucket + 1) & (self.entries.len() - 1),
                None => {
                    let descriptor = self.occupied.len();
                    if descriptor == self.limit {
                        return Err(Self::capacity_error(
                            self.limit,
                            (descriptor as u64).checked_add(1),
                        ));
                    }
                    self.entries[bucket] = Some(RasterSrvEntry { key, descriptor });
                    self.occupied.push(bucket);
                    return Ok((descriptor, true));
                }
            }
        }
    }

    fn reset(&mut self) {
        // Clear only entries used by this execution, never the full prepared
        // table. Available-slot ownership proves no recorded/pending user.
        for bucket in self.occupied.drain(..) {
            self.entries[bucket] = None;
        }
    }
}

fn validate_class(class: D3D12ImageClass) -> Result<(), D3D12RuntimeError> {
    if class.count == 0 || class.desc.byte_len().is_none() {
        return Err(D3D12RuntimeError::invalid(
            "prepared image classes require a nonzero count and valid extent",
        ));
    }
    if class.desc.array_layers != 1
        || class.desc.width > D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION as u32
        || class.desc.height > D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION as u32
    {
        return Err(D3D12RuntimeError::unsupported(
            "D3D12 Graph images require a supported single-layer extent",
        ));
    }
    Ok(())
}

pub(super) fn require_format(
    owner: &DeviceOwner,
    format: DXGI_FORMAT,
    required: D3D12_FORMAT_SUPPORT1,
) -> Result<(), D3D12RuntimeError> {
    let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
        Format: format,
        ..Default::default()
    };
    // SAFETY: The feature enum, initialized output structure and its byte count
    // match. The Device remains owned throughout this synchronous query.
    unsafe {
        owner.raw.CheckFeatureSupport(
            D3D12_FEATURE_FORMAT_SUPPORT,
            std::ptr::from_mut(&mut support).cast(),
            size_of_val(&support) as u32,
        )
    }
    .ok()
    .map_err(|error| owner.native("CheckFeatureSupport(Graph image)", error))?;
    if support.Support1 & required != required {
        return Err(D3D12RuntimeError::unsupported(
            "prepared Graph image format lacks required native capabilities",
        ));
    }
    Ok(())
}

fn require_image_format(owner: &DeviceOwner, format: ImageFormat) -> Result<(), D3D12RuntimeError> {
    match format {
        ImageFormat::Rgba8Srgb => require_format(
            owner,
            DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
            D3D12_FORMAT_SUPPORT1_TEXTURE2D
                | D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE
                | D3D12_FORMAT_SUPPORT1_MIP,
        ),
        ImageFormat::Depth32Float => {
            require_format(
                owner,
                DXGI_FORMAT_R32_TYPELESS,
                D3D12_FORMAT_SUPPORT1_TEXTURE2D,
            )?;
            require_format(
                owner,
                DXGI_FORMAT_D32_FLOAT,
                D3D12_FORMAT_SUPPORT1_TEXTURE2D | D3D12_FORMAT_SUPPORT1_DEPTH_STENCIL,
            )?;
            require_format(
                owner,
                DXGI_FORMAT_R32_FLOAT,
                D3D12_FORMAT_SUPPORT1_TEXTURE2D | D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE,
            )
        }
        ImageFormat::Rgba8Unorm | ImageFormat::Rgba16Float => require_format(
            owner,
            match format {
                ImageFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
                ImageFormat::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
                ImageFormat::Depth32Float | ImageFormat::Rgba8Srgb => unreachable!(),
            },
            D3D12_FORMAT_SUPPORT1_TEXTURE2D
                | D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE
                | D3D12_FORMAT_SUPPORT1_RENDER_TARGET
                | D3D12_FORMAT_SUPPORT1_TYPED_UNORDERED_ACCESS_VIEW,
        ),
    }
}

fn checked_handle_capacity(
    cpu: D3D12_CPU_DESCRIPTOR_HANDLE,
    gpu: Option<D3D12_GPU_DESCRIPTOR_HANDLE>,
    step: u32,
    count: usize,
) -> Result<(), D3D12RuntimeError> {
    let last = count.saturating_sub(1);
    if step == 0
        || last
            .checked_mul(step as usize)
            .and_then(|offset| cpu.ptr.checked_add(offset))
            .is_none()
        || gpu.is_some_and(|gpu| {
            (last as u64)
                .checked_mul(u64::from(step))
                .and_then(|offset| gpu.ptr.checked_add(offset))
                .is_none()
        })
    {
        return Err(D3D12RuntimeError::unsupported(
            "prepared descriptor handles exceed the native address range",
        ));
    }
    Ok(())
}

fn copy_layout(
    owner: &DeviceOwner,
    desc: ImageDesc,
) -> Result<(Vec<MipCopyLayout>, u64), D3D12RuntimeError> {
    // A supported 2D extent has at most 15 mips. The fixed arrays accommodate
    // every u32 2D extent and avoid temporary per-level native allocations.
    let mut footprints = [D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); 32];
    let mut rows = [0u32; 32];
    let mut row_bytes = [0u64; 32];
    let mut total_bytes = 0u64;
    // SAFETY: Creation validated the description and one layer. Each output
    // array contains at least mip_levels entries, matching NumSubresources.
    unsafe {
        owner.raw.GetCopyableFootprints(
            &DeviceImage::graph_resource_desc(desc),
            0,
            desc.mip_levels,
            0,
            Some(footprints.as_mut_ptr()),
            Some(rows.as_mut_ptr()),
            Some(row_bytes.as_mut_ptr()),
            Some(&mut total_bytes),
        );
    }
    if total_bytes == u64::MAX || total_bytes > isize::MAX as u64 {
        return Err(D3D12RuntimeError::unsupported(
            "invalid native image copy footprint",
        ));
    }
    let mut layouts = Vec::with_capacity(desc.mip_levels as usize);
    let mut packed_offset = 0usize;
    let mut previous_end = 0u64;
    for mip in 0..desc.mip_levels as usize {
        let footprint = footprints[mip];
        let width = (desc.width >> mip).max(1);
        let height = (desc.height >> mip).max(1);
        let expected_row = u64::from(width) * desc.format.texel_bytes();
        let accessed_end = u64::from(rows[mip].saturating_sub(1))
            .checked_mul(u64::from(footprint.Footprint.RowPitch))
            .and_then(|stride| footprint.Offset.checked_add(stride))
            .and_then(|start| start.checked_add(row_bytes[mip]));
        if rows[mip] != height
            || row_bytes[mip] != expected_row
            || u64::from(footprint.Footprint.RowPitch) < expected_row
            || footprint.Footprint.RowPitch % D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as u32 != 0
            || footprint.Offset % D3D12_TEXTURE_DATA_PLACEMENT_ALIGNMENT as u64 != 0
            || footprint.Offset < previous_end
            || footprint.Footprint.Width != width
            || footprint.Footprint.Height != height
            || footprint.Footprint.Depth != 1
            || accessed_end.is_none_or(|end| end > total_bytes)
        {
            return Err(D3D12RuntimeError::unsupported(
                "native Graph mip copy footprint is outside the supported layout",
            ));
        }
        previous_end = accessed_end.unwrap();
        layouts.push(MipCopyLayout {
            footprint,
            row_bytes: expected_row as usize,
            rows: height as usize,
            packed_offset,
        });
        packed_offset += expected_row as usize * height as usize;
    }
    Ok((layouts, total_bytes))
}

impl RasterStorage {
    pub(super) fn raster_srv(
        &mut self,
        owner: &DeviceOwner,
        image: usize,
        first_mip: u32,
        mip_count: u32,
    ) -> Result<usize, D3D12RuntimeError> {
        let image = &self.images[image];
        let key = RasterSrvKey {
            resource: image.native.raw().as_raw() as usize,
            format: super::resources::sampled_format(image.desc.format),
            first_mip,
            mip_count,
        };
        let (descriptor, create) = self.raster_srvs.get_or_insert(key)?;
        if create {
            // The current contract fixes dimension, mapping, plane and LOD
            // clamp. Extend the key if any of those become caller-selectable.
            let srv = D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: key.format,
                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING as u32,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_SRV {
                        MostDetailedMip: first_mip,
                        MipLevels: mip_count,
                        PlaneSlice: 0,
                        ResourceMinLODClamp: 0.0,
                    },
                },
            };
            let resource = image.native.raw();
            let target = self.texture_cpu(descriptor);
            {
                #[cfg(feature = "tracy")]
                let _span = tracy_client::Client::running().map(|client| {
                    client.span(
                        tracy_client::span_location!("renderer.d3d12.raster.create_srv"),
                        0,
                    )
                });
                // SAFETY: Preflight established the view contract and bounded
                // all binding occurrences. A miss owns a new in-bounds
                // descriptor; a hit never overwrites it. The slot pins the
                // image and heap until completion or unsubmitted cancellation.
                unsafe {
                    owner
                        .raw
                        .CreateShaderResourceView(resource, Some(&srv), target);
                }
            }
            #[cfg(test)]
            {
                self.srv_creations += 1;
            }
        }
        Ok(descriptor)
    }

    pub(super) fn use_image(&mut self, desc: ImageDesc) -> Result<usize, D3D12RuntimeError> {
        let index = self.image_slots.take(desc)?;
        self.images[index].used = true;
        Ok(index)
    }

    pub(super) fn require_target_view(&mut self, image: usize) {
        self.target_views.require(image);
    }

    pub(super) fn bind_views(&mut self, owner: &DeviceOwner) {
        for &index in &self.target_views.indices {
            let image = &self.images[index];
            if let Some(handle) = image.rtv {
                // SAFETY: This exclusive execution slot owns the CPU descriptor;
                // the leased native image matches its prepared color class.
                unsafe {
                    owner
                        .raw
                        .CreateRenderTargetView(image.native.raw(), None, handle)
                };
                #[cfg(test)]
                {
                    self.target_view_creations[0] += 1;
                }
            }
            if let Some(handle) = image.dsv {
                let view = D3D12_DEPTH_STENCIL_VIEW_DESC {
                    Format: DXGI_FORMAT_D32_FLOAT,
                    ViewDimension: D3D12_DSV_DIMENSION_TEXTURE2D,
                    Flags: D3D12_DSV_FLAG_NONE,
                    Anonymous: D3D12_DEPTH_STENCIL_VIEW_DESC_0 {
                        Texture2D: D3D12_TEX2D_DSV { MipSlice: 0 },
                    },
                };
                // SAFETY: The exclusive descriptor and retained single-mip/layer
                // typeless depth resource match the concrete D32 view.
                unsafe {
                    owner
                        .raw
                        .CreateDepthStencilView(image.native.raw(), Some(&view), handle)
                };
                #[cfg(test)]
                {
                    self.target_view_creations[1] += 1;
                }
            }
        }
    }
    pub(super) fn new(
        owner: &DeviceOwner,
        classes: &[D3D12ImageClass],
        registered_images: &[ImageDesc],
        raster_descriptor_capacity: usize,
        compute_descriptor_capacity: usize,
        buffer_capacity: usize,
        allocate_images: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        owner.ensure_active()?;
        let descriptor_capacity = raster_descriptor_capacity
            .checked_add(compute_descriptor_capacity)
            .ok_or_else(|| {
                D3D12RuntimeError::capacity_with(
                    "prepared descriptor capacity overflow",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Descriptors,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?
            .max(1);
        // One million is the portable shader-visible CBV/SRV/UAV heap bound
        // at the supported baseline; samplers need only their single fixed slot.
        if !(1..=1_000_000).contains(&descriptor_capacity) {
            return Err(D3D12RuntimeError::invalid(
                "prepared Graph descriptor capacity must be within 1..=1000000",
            ));
        }
        let classes = classes.iter().copied().map(|class| (class, true)).chain(
            registered_images
                .iter()
                .map(|&desc| (D3D12ImageClass { desc, count: 1 }, false)),
        );
        let mut total = 0usize;
        let mut color_count = 0usize;
        let mut depth_count = 0usize;
        for (class, _) in classes.clone() {
            validate_class(class)?;
            require_image_format(owner, class.desc.format)?;
            if class.desc.mip_levels > 1 {
                require_format(
                    owner,
                    super::resources::sampled_format(class.desc.format),
                    D3D12_FORMAT_SUPPORT1_MIP,
                )?;
            }
            total = total.checked_add(class.count).ok_or_else(|| {
                D3D12RuntimeError::capacity_with(
                    "prepared image count overflows",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Resources,
                        CapacityRecovery::Reconfigure,
                        None,
                        None,
                    ),
                )
            })?;
            if class.desc.format == ImageFormat::Depth32Float {
                depth_count += class.count;
            } else if class.desc.format != ImageFormat::Rgba8Srgb {
                color_count += class.count;
            }
        }
        let mut images = Vec::new();
        images.try_reserve_exact(total).map_err(|_| {
            D3D12RuntimeError::capacity_with(
                "prepared image metadata allocation failed",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Resources,
                    CapacityRecovery::Reconfigure,
                    None,
                    None,
                ),
            )
        })?;
        let srv = DescriptorHeap::new(
            owner,
            descriptor_capacity,
            D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            true,
        )?;
        let sampler = DescriptorHeap::new(owner, 4, D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER, true)?;
        let rtv_heap = (color_count != 0)
            .then(|| DescriptorHeap::new(owner, color_count, D3D12_DESCRIPTOR_HEAP_TYPE_RTV, false))
            .transpose()?;
        let dsv_heap = (depth_count != 0)
            .then(|| DescriptorHeap::new(owner, depth_count, D3D12_DESCRIPTOR_HEAP_TYPE_DSV, false))
            .transpose()?;
        // SAFETY: Every heap is live and owned, each handle's CPU/GPU visibility
        // matches the getter, and increments are immutable Device properties.
        let (
            texture_cpu_start,
            texture_gpu_start,
            sampler_gpu_start,
            rtv_start,
            dsv_start,
            rtv_step,
            dsv_step,
        ) = unsafe {
            (
                srv.raw.GetCPUDescriptorHandleForHeapStart(),
                srv.raw.GetGPUDescriptorHandleForHeapStart(),
                sampler.raw.GetGPUDescriptorHandleForHeapStart(),
                rtv_heap
                    .as_ref()
                    .map(|heap| heap.raw.GetCPUDescriptorHandleForHeapStart()),
                dsv_heap
                    .as_ref()
                    .map(|heap| heap.raw.GetCPUDescriptorHandleForHeapStart()),
                owner
                    .raw
                    .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV),
                owner
                    .raw
                    .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_DSV),
            )
        };
        checked_handle_capacity(
            texture_cpu_start,
            Some(texture_gpu_start),
            owner.descriptor_steps[0],
            descriptor_capacity,
        )?;
        if let Some(start) = rtv_start {
            checked_handle_capacity(start, None, rtv_step, color_count)?;
        }
        if let Some(start) = dsv_start {
            checked_handle_capacity(start, None, dsv_step, depth_count)?;
        }
        let mut color_index = 0usize;
        let mut depth_index = 0usize;
        for (class, allocate) in classes {
            for _ in 0..class.count {
                let native = if allocate && allocate_images {
                    super::registry::PhysicalRef::image(DeviceImage::graph(owner, class.desc)?)
                } else {
                    super::registry::PhysicalRef::default()
                };
                let (mips, total_bytes) = copy_layout(owner, class.desc)?;
                let footprint = mips[0].footprint;
                let row_bytes = mips[0].row_bytes;
                let rows = mips[0].rows;
                let (rtv, dsv) = if class.desc.format == ImageFormat::Depth32Float {
                    let handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                        ptr: dsv_start.unwrap().ptr + depth_index * dsv_step as usize,
                    };
                    let view = D3D12_DEPTH_STENCIL_VIEW_DESC {
                        Format: DXGI_FORMAT_D32_FLOAT,
                        ViewDimension: D3D12_DSV_DIMENSION_TEXTURE2D,
                        Flags: D3D12_DSV_FLAG_NONE,
                        Anonymous: D3D12_DEPTH_STENCIL_VIEW_DESC_0 {
                            Texture2D: D3D12_TEX2D_DSV { MipSlice: 0 },
                        },
                    };
                    if allocate && allocate_images {
                        // SAFETY: The checked single-subresource typeless image
                        // is depth-capable. Its concrete D32 view occupies this
                        // slot's in-bounds CPU-only descriptor.
                        unsafe {
                            owner
                                .raw
                                .CreateDepthStencilView(native.raw(), Some(&view), handle)
                        };
                    }
                    depth_index += 1;
                    (None, Some(handle))
                } else if class.desc.format == ImageFormat::Rgba8Srgb {
                    (None, None)
                } else {
                    let handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                        ptr: rtv_start.unwrap().ptr + color_index * rtv_step as usize,
                    };
                    if allocate && allocate_images {
                        // SAFETY: The checked color resource permits render
                        // targets. The sole mip/layer and this slot's in-bounds
                        // descriptor match the default view.
                        unsafe { owner.raw.CreateRenderTargetView(native.raw(), None, handle) };
                    }
                    color_index += 1;
                    (Some(handle), None)
                };
                images.push(GraphImage {
                    desc: class.desc,
                    native,
                    state: D3D12_RESOURCE_STATE_COMMON,
                    uav_write: false,
                    used: false,
                    rtv,
                    dsv,
                    footprint,
                    total_bytes,
                    row_bytes,
                    rows,
                    mips,
                });
            }
        }
        // Prepare all immutable policies once. A Graph binding selects its
        // actual policy without editing a live descriptor or allocating.
        // SAFETY: Live shader-visible heap holds all four in-bounds descriptors.
        let sampler_cpu = unsafe { sampler.raw.GetCPUDescriptorHandleForHeapStart() };
        checked_handle_capacity(
            sampler_cpu,
            Some(sampler_gpu_start),
            owner.descriptor_steps[1],
            4,
        )?;
        for (index, policy) in [
            crate::program::SamplerDescriptor::NearestClamp,
            crate::program::SamplerDescriptor::LinearRepeat,
            crate::program::SamplerDescriptor::LinearClamp,
            crate::program::SamplerDescriptor::LinearRepeatUClampV,
        ]
        .into_iter()
        .enumerate()
        {
            let desc = super::image::sampler_desc(policy);
            // SAFETY: Each prepared descriptor has its own bounded heap slot.
            unsafe {
                owner.raw.CreateSampler(
                    &desc,
                    D3D12_CPU_DESCRIPTOR_HANDLE {
                        ptr: sampler_cpu.ptr + index * owner.descriptor_steps[1] as usize,
                    },
                );
            }
        }
        let image_slots = ImageSlots::new(images.iter().map(|image| image.desc))?;
        Ok(Self {
            _rtv_heap: rtv_heap,
            _dsv_heap: dsv_heap,
            srv,
            sampler,
            images,
            image_slots,
            target_views: TargetViews::new(total)?,
            texture_cpu_start,
            texture_gpu_start,
            sampler_gpu_start,
            texture_step: owner.descriptor_steps[0],
            sampler_step: owner.descriptor_steps[1],
            descriptor_capacity,
            raster_descriptor_capacity,
            compute_descriptor_start: raster_descriptor_capacity,
            compute_descriptor_capacity,
            scopes: PassScopes::new(buffer_capacity, total)?,
            raster_srvs: RasterSrvCache::new(raster_descriptor_capacity)?,
            #[cfg(test)]
            srv_creations: 0,
            #[cfg(test)]
            target_view_creations: [0; 2],
        })
    }

    pub(super) fn reset(&mut self) {
        // Only an exclusive available frame lease may reset this storage. The
        // recorder restored actual resources to COMMON before successful Close;
        // cancelled recording was never executed and cannot change their state.
        self.scopes.reset();
        self.raster_srvs.reset();
        self.image_slots.reset();
        self.target_views.reset();
        #[cfg(test)]
        {
            self.srv_creations = 0;
            self.target_view_creations = [0; 2];
        }
        for image in &mut self.images {
            image.state = D3D12_RESOURCE_STATE_COMMON;
            image.used = false;
            image.uav_write = false;
        }
    }

    pub(super) fn texture_cpu(&self, index: usize) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        assert!(
            index < self.descriptor_capacity,
            "prepared Raster descriptor index"
        );
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: self.texture_cpu_start.ptr + index * self.texture_step as usize,
        }
    }

    pub(super) fn texture_gpu(&self, index: usize) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        assert!(
            index < self.descriptor_capacity,
            "prepared Raster descriptor index"
        );
        D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: self.texture_gpu_start.ptr + index as u64 * u64::from(self.texture_step),
        }
    }

    pub(super) fn sampler_gpu(
        &self,
        policy: crate::program::SamplerDescriptor,
    ) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        let index = match policy {
            crate::program::SamplerDescriptor::NearestClamp => 0u64,
            crate::program::SamplerDescriptor::LinearRepeat => 1u64,
            crate::program::SamplerDescriptor::LinearClamp => 2u64,
            crate::program::SamplerDescriptor::LinearRepeatUClampV => 3u64,
        };
        D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: self.sampler_gpu_start.ptr + index * u64::from(self.sampler_step),
        }
    }

    pub(super) fn srv_heap(&self) -> &ID3D12DescriptorHeap {
        &self.srv.raw
    }

    pub(super) fn sampler_heap(&self) -> &ID3D12DescriptorHeap {
        &self.sampler.raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_slots_preserve_lowest_index_across_repeated_classes_and_reset() {
        let small = ImageDesc::rgba8(1, 1);
        let large = ImageDesc::rgba8(128, 64);
        let depth = ImageDesc::depth32(1, 1);
        let mut mipped = small;
        mipped.mip_levels = 2;
        // Registered images may repeat any earlier working-set class, and the
        // caller's preparation order need not be sorted by description.
        let descriptions = [large, small, depth, small, mipped, large, small];
        let mut slots = ImageSlots::new(descriptions.into_iter()).unwrap();
        let capacity = (slots.classes.capacity(), slots.candidates.capacity());
        for _ in 0..3 {
            assert_eq!(slots.take(small).unwrap(), 1);
            assert_eq!(slots.take(large).unwrap(), 0);
            assert_eq!(slots.take(small).unwrap(), 3);
            assert_eq!(slots.take(depth).unwrap(), 2);
            assert_eq!(slots.take(mipped).unwrap(), 4);
            assert_eq!(slots.take(large).unwrap(), 5);
            assert!(slots.take(large).is_err());
            // A failed request must not consume a different class's last slot.
            assert_eq!(slots.take(small).unwrap(), 6);
            assert!(slots.take(small).is_err());
            assert!(slots.take(ImageDesc::rgba16_float(1, 1)).is_err());
            slots.reset();
        }
        assert_eq!(
            capacity,
            (slots.classes.capacity(), slots.candidates.capacity())
        );
        let mut empty = ImageSlots::new(std::iter::empty()).unwrap();
        assert!(empty.take(small).is_err());
        empty.reset();
    }

    #[test]
    fn image_slots_consume_large_classes_once_without_metadata_growth() {
        let desc = ImageDesc::rgba8(1, 1);
        let mut slots = ImageSlots::new(std::iter::repeat_n(desc, 4096)).unwrap();
        let capacity = (slots.classes.capacity(), slots.candidates.capacity());
        for _ in 0..2 {
            for expected in 0..4096 {
                assert_eq!(slots.take(desc).unwrap(), expected);
            }
            assert!(slots.take(desc).is_err());
            slots.reset();
        }
        assert_eq!(
            capacity,
            (slots.classes.capacity(), slots.candidates.capacity())
        );
    }

    #[test]
    fn image_slots_reset_only_classes_used_by_the_current_lease() {
        let descriptions = (1..1025).map(|width| ImageDesc::rgba8(width, 1));
        let mut slots = ImageSlots::new(descriptions).unwrap();
        let capacity = slots.touched_classes.capacity();
        for _ in 0..3 {
            assert_eq!(slots.take(ImageDesc::rgba8(900, 1)).unwrap(), 899);
            assert!(slots.take(ImageDesc::rgba8(900, 1)).is_err());
            assert_eq!(slots.take(ImageDesc::rgba8(2, 1)).unwrap(), 1);
            assert!(slots.take(ImageDesc::rgba8(2048, 1)).is_err());
            assert_eq!(slots.touched_classes, [899, 1]);
            slots.reset();
            assert!(slots.touched_classes.is_empty());
        }
        assert_eq!(capacity, slots.touched_classes.capacity());
    }

    #[test]
    fn target_views_keep_only_unique_attachments_and_reset_cancelled_requirements() {
        let mut views = TargetViews::new(4).unwrap();
        let capacity = (views.marked.capacity(), views.indices.capacity());
        // Slots 0 and 2 represent live sampled/storage-only images: merely
        // assigning a local slot does not require an RTV or DSV for them.
        views.require(3);
        views.require(1);
        views.require(3);
        assert_eq!(views.indices, [3, 1]);
        views.reset();
        assert!(views.marked.iter().all(|&marked| !marked));
        // The next exclusive lease may use a former sampled slot as a target.
        views.require(0);
        views.require(0);
        assert_eq!(views.indices, [0]);
        assert_eq!(
            capacity,
            (views.marked.capacity(), views.indices.capacity())
        );
        views.reset();
        TargetViews::new(0).unwrap().reset();
    }

    #[test]
    fn pass_scopes_keep_unique_first_use_order_and_recover_from_partial_drain() {
        let mut scopes = PassScopes::new(2, 2).unwrap();
        let capacities = (
            scopes.touched_buffers.capacity(),
            scopes.touched_images.capacity(),
        );
        scopes.image(1, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE);
        scopes.buffer(1, D3D12_RESOURCE_STATE_INDEX_BUFFER);
        scopes.image(0, D3D12_RESOURCE_STATE_RENDER_TARGET);
        scopes.buffer(0, D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER);
        scopes.buffer(1, D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER);
        scopes.image(1, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
        let mut applied = Vec::new();
        scopes.drain_buffers(|index, state| applied.push((false, index, state)));
        scopes.drain_images(|index, state| applied.push((true, index, state)));
        assert_eq!(
            applied,
            [
                (
                    false,
                    1,
                    D3D12_RESOURCE_STATE_INDEX_BUFFER
                        | D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER
                ),
                (false, 0, D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER),
                (
                    true,
                    1,
                    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE
                        | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE
                ),
                (true, 0, D3D12_RESOURCE_STATE_RENDER_TARGET),
            ]
        );
        assert!(
            scopes
                .buffers
                .iter()
                .chain(&scopes.images)
                .all(|&state| state == D3D12_RESOURCE_STATE_COMMON)
        );
        scopes.buffer(0, D3D12_RESOURCE_STATE_INDEX_BUFFER);
        scopes.buffer(1, D3D12_RESOURCE_STATE_INDEX_BUFFER);
        scopes.image(0, D3D12_RESOURCE_STATE_RENDER_TARGET);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scopes.drain_buffers(|_, _| panic!("native recording failed"));
        }));
        assert!(failed.is_err());
        scopes.reset();
        assert!(
            scopes
                .buffers
                .iter()
                .chain(&scopes.images)
                .all(|&state| state == D3D12_RESOURCE_STATE_COMMON)
        );
        // An image-only pass must not retain either buffer from the failed pass.
        scopes.image(1, D3D12_RESOURCE_STATE_DEPTH_WRITE);
        scopes.drain_buffers(|_, _| panic!("stale buffer scope"));
        scopes.drain_images(|index, state| {
            assert_eq!((index, state), (1, D3D12_RESOURCE_STATE_DEPTH_WRITE))
        });
        assert_eq!(
            capacities,
            (
                scopes.touched_buffers.capacity(),
                scopes.touched_images.capacity()
            )
        );
        let mut empty = PassScopes::new(0, 0).unwrap();
        empty.reset();
    }

    #[test]
    fn srv_cache_reuses_views_and_preserves_full_capacity_hits_until_reset() {
        let mut cache = RasterSrvCache::new(5).unwrap();
        let base = RasterSrvKey {
            resource: 0x1000,
            format: DXGI_FORMAT_R8G8B8A8_UNORM,
            first_mip: 0,
            mip_count: 1,
        };
        let keys = [
            base,
            RasterSrvKey {
                resource: 0x2000,
                ..base
            },
            RasterSrvKey {
                format: DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
                ..base
            },
            RasterSrvKey {
                first_mip: 1,
                ..base
            },
            RasterSrvKey {
                mip_count: 2,
                ..base
            },
        ];
        let capacity = (cache.entries.capacity(), cache.occupied.capacity());
        for (index, key) in keys.into_iter().enumerate() {
            assert_eq!(cache.get_or_insert(key).unwrap(), (index, true));
            assert_eq!(cache.get_or_insert(key).unwrap(), (index, false));
        }
        let extra = RasterSrvKey {
            resource: 0x3000,
            ..base
        };
        let error = cache.get_or_insert(extra).unwrap_err();
        assert_eq!(
            error.capacity().unwrap().resource,
            RuntimeCapacityKind::Descriptors
        );
        for (index, key) in keys.into_iter().enumerate() {
            assert_eq!(cache.get_or_insert(key).unwrap(), (index, false));
        }
        cache.reset();
        assert!(cache.entries.iter().all(Option::is_none));
        // Equal pointer values in a later lease cannot reuse the old descriptor.
        assert_eq!(cache.get_or_insert(base).unwrap(), (0, true));
        assert_eq!(
            capacity,
            (cache.entries.capacity(), cache.occupied.capacity())
        );
        let mut empty = RasterSrvCache::new(0).unwrap();
        assert!(empty.get_or_insert(base).is_err());
        assert!(RasterSrvCache::new(usize::MAX).is_err());
    }

    #[test]
    fn srv_cache_resolves_collisions_across_the_table_boundary() {
        let mut cache = RasterSrvCache::new(3).unwrap();
        let keys = (1..1000)
            .map(|resource| RasterSrvKey {
                resource,
                format: DXGI_FORMAT_R8G8B8A8_UNORM,
                first_mip: 0,
                mip_count: 1,
            })
            .filter(|&key| cache.bucket(key) == cache.entries.len() - 1)
            .take(3)
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 3);
        for (index, &key) in keys.iter().enumerate() {
            assert_eq!(cache.get_or_insert(key).unwrap(), (index, true));
        }
        for (index, &key) in keys.iter().enumerate() {
            assert_eq!(cache.get_or_insert(key).unwrap(), (index, false));
        }
    }

    #[test]
    fn prepared_classes_reject_empty_and_unsupported_image_descriptions() {
        assert!(
            validate_class(D3D12ImageClass {
                desc: ImageDesc::rgba8(2, 2),
                count: 1
            })
            .is_ok()
        );
        assert!(
            validate_class(D3D12ImageClass {
                desc: ImageDesc::rgba8(2, 2),
                count: 0
            })
            .is_err()
        );
        assert!(
            validate_class(D3D12ImageClass {
                desc: ImageDesc::rgba8(0, 2),
                count: 1
            })
            .is_err()
        );
        let mut desc = ImageDesc::rgba8(2, 2);
        desc.mip_levels = 2;
        assert!(validate_class(D3D12ImageClass { desc, count: 1 }).is_ok());
        desc.array_layers = 2;
        assert!(validate_class(D3D12ImageClass { desc, count: 1 }).is_err());
    }
}
