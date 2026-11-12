//! Single-queue Windows presentation. Swapchain ownership stays outside Graph
//! analysis; source images use the same canonical state and resource leases.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ash::vk;
use winit::window::Window;

use super::{MemoryOwner, Physical, RuntimeConfig, RuntimeError, Snapshot, VulkanRuntime};
use crate::backends::vulkan::device::VulkanDevice;
use crate::backends::vulkan::diagnostics::Diagnostics;
use crate::backends::vulkan::instance::ValidationMode;
use crate::backends::vulkan::presentation::WindowSurface;
use crate::backends::vulkan::transfer::{PendingTransfer, TransferError, TransferPayload};
use crate::graph::{ImageFormat, ImportImage, ResourceDesc};

mod program;
use program::{Binding, Program};

const CLEANUP_BUDGET: Duration = Duration::from_secs(10);

fn native(owner: &MemoryOwner, operation: &str, result: vk::Result) -> RuntimeError {
    owner.device.observe_result(result);
    RuntimeError::native(operation, owner.device.name(), result)
}
fn timeout_ns(timeout: Duration) -> u64 {
    timeout.as_nanos().min((u64::MAX - 1) as u128) as u64
}

/// Explicit bounds for WSI resources. They are independent of the actual
/// Swapchain image count and the runtime's native execution-slot capacity.
#[derive(Clone, Copy, Debug)]
pub struct PresentationConfig {
    pub max_frames: usize,
    pub max_retired_swapchains: usize,
    pub desired_images: u32,
    /// FIFO when true; IMMEDIATE when false. Unsupported requests fail rather
    /// than silently falling back to a synchronized presentation mode.
    pub vsync: bool,
}
impl Default for PresentationConfig {
    fn default() -> Self {
        Self {
            max_frames: 2,
            max_retired_swapchains: 2,
            desired_images: 3,
            vsync: true,
        }
    }
}
fn validate_config(config: PresentationConfig) -> Result<(), RuntimeError> {
    if config.max_frames == 0
        || config.max_frames > u32::MAX as usize
        || config.max_retired_swapchains == 0
        || config.max_retired_swapchains > u32::MAX as usize
        || config.desired_images == 0
    {
        return Err(RuntimeError::invalid(
            "presentation capacities must be nonzero and fit u32",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentOutcome {
    Presented {
        suboptimal: bool,
    },
    /// No image or WSI slot is currently available; no CPU/GPU wait was added.
    NotReady,
    /// The output extent may have changed. Rebuild for `Presenter::extent()`.
    Recreated,
    /// The requested window extent is zero; no acquire is attempted.
    Suspended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentResult {
    Enqueued { recreate: bool, lost: bool },
    Unchanged,
    Uncertain,
}
fn classify_present(result: vk::Result) -> PresentResult {
    match result {
        vk::Result::SUCCESS => PresentResult::Enqueued {
            recreate: false,
            lost: false,
        },
        vk::Result::SUBOPTIMAL_KHR | vk::Result::ERROR_OUT_OF_DATE_KHR => PresentResult::Enqueued {
            recreate: true,
            lost: false,
        },
        vk::Result::ERROR_SURFACE_LOST_KHR => PresentResult::Enqueued {
            recreate: false,
            lost: true,
        },
        vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => {
            PresentResult::Unchanged
        }
        _ => PresentResult::Uncertain,
    }
}

fn choose_format(formats: &[vk::SurfaceFormatKHR]) -> Result<vk::SurfaceFormatKHR, RuntimeError> {
    [vk::Format::B8G8R8A8_SRGB, vk::Format::R8G8B8A8_SRGB]
        .into_iter()
        .find_map(|format| {
            formats
                .iter()
                .find(|entry| {
                    entry.format == format && entry.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
                })
                .copied()
        })
        .ok_or_else(|| {
            RuntimeError::unsupported(
                "presentation requires BGRA8_SRGB or RGBA8_SRGB with SRGB_NONLINEAR",
            )
        })
}
fn choose_extent(caps: &vk::SurfaceCapabilitiesKHR, desired: (u32, u32)) -> Option<vk::Extent2D> {
    if desired.0 == 0
        || desired.1 == 0
        || caps.max_image_extent.width == 0
        || caps.max_image_extent.height == 0
    {
        return None;
    }
    let value = if caps.current_extent.width != u32::MAX {
        caps.current_extent
    } else {
        vk::Extent2D {
            width: desired
                .0
                .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
            height: desired
                .1
                .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
        }
    };
    (value.width != 0 && value.height != 0).then_some(value)
}
fn image_count(caps: &vk::SurfaceCapabilitiesKHR, requested: u32) -> u32 {
    let value = requested.max(caps.min_image_count);
    if caps.max_image_count == 0 {
        value
    } else {
        value.min(caps.max_image_count)
    }
}

fn choose_present_mode(
    supported: &[vk::PresentModeKHR],
    vsync: bool,
) -> Result<vk::PresentModeKHR, RuntimeError> {
    let mode = if vsync {
        vk::PresentModeKHR::FIFO
    } else {
        vk::PresentModeKHR::IMMEDIATE
    };
    if supported.contains(&mode) {
        Ok(mode)
    } else {
        Err(RuntimeError::unsupported(format!(
            "Surface does not support requested VSync {} ({mode:?})",
            if vsync { "on" } else { "off" }
        )))
    }
}

struct Swapchain {
    owner: Arc<MemoryOwner>,
    _surface: Arc<WindowSurface>,
    loader: ash::khr::swapchain::Device,
    handle: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    extent: vk::Extent2D,
    format: vk::Format,
}
struct SwapchainPlan {
    extent: vk::Extent2D,
    format: vk::SurfaceFormatKHR,
    count: u32,
    alpha: vk::CompositeAlphaFlagsKHR,
    transform: vk::SurfaceTransformFlagsKHR,
    present_mode: vk::PresentModeKHR,
}
impl Swapchain {
    fn new(
        owner: Arc<MemoryOwner>,
        surface: Arc<WindowSurface>,
        plan: SwapchainPlan,
        old: vk::SwapchainKHR,
    ) -> Result<Arc<Self>, RuntimeError> {
        let SwapchainPlan {
            extent,
            format,
            count,
            alpha,
            transform,
            present_mode,
        } = plan;
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface.handle)
            .min_image_count(count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(transform)
            .composite_alpha(alpha)
            .present_mode(present_mode)
            .clipped(true)
            .old_swapchain(old);
        let loader =
            ash::khr::swapchain::Device::new(owner.device.instance_raw(), owner.device.raw());
        // SAFETY: Actual Surface capabilities were queried, one queue supports
        // graphics/compute/present, and old belongs to the same Surface. A caller
        // retains old even if creation fails, because this call retires it.
        let handle = unsafe { loader.create_swapchain(&info, None) }
            .map_err(|r| native(&owner, "vkCreateSwapchainKHR", r))?;
        let mut chain = Self {
            owner,
            _surface: surface,
            loader,
            handle,
            images: Vec::new(),
            views: Vec::new(),
            extent,
            format: format.format,
        };
        // SAFETY: The successful Swapchain is immediately owned by chain.
        chain.images = unsafe { chain.loader.get_swapchain_images(handle) }
            .map_err(|r| native(&chain.owner, "vkGetSwapchainImagesKHR", r))?;
        for &image in &chain.images {
            let info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format.format)
                .subresource_range(color_range());
            // SAFETY: Views reference this owned Swapchain's images; no VMA
            // Allocation or separate image ownership is fabricated for them.
            let view = unsafe { chain.owner.device.raw().create_image_view(&info, None) }
                .map_err(|r| native(&chain.owner, "vkCreateImageView(swapchain)", r))?;
            chain.views.push(view);
        }
        Ok(Arc::new(chain))
    }
}
impl Drop for Swapchain {
    fn drop(&mut self) {
        // SAFETY: Work and queued-present records retain this owner until both
        // GPU references and all corresponding present fences have completed.
        unsafe {
            for view in self.views.drain(..) {
                self.owner.device.raw().destroy_image_view(view, None);
            }
            self.loader.destroy_swapchain(self.handle, None);
        }
    }
}
fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}

struct Slot {
    owner: Arc<MemoryOwner>,
    acquire: vk::Semaphore,
    finished: vk::Semaphore,
    present_fence: vk::Fence,
    cleanup_fence: vk::Fence,
    binding: Binding,
    busy: AtomicBool,
    accepted: AtomicBool,
}
impl Slot {
    fn new(owner: Arc<MemoryOwner>, program: Arc<Program>) -> Result<Arc<Self>, RuntimeError> {
        let binding = Binding::new(program)?;
        let mut slot = Self {
            owner,
            acquire: vk::Semaphore::null(),
            finished: vk::Semaphore::null(),
            present_fence: vk::Fence::null(),
            cleanup_fence: vk::Fence::null(),
            binding,
            busy: AtomicBool::new(false),
            accepted: AtomicBool::new(false),
        };
        // SAFETY: Handles are immediately stored in the partial-construction
        // owner; no GPU or acquire references exist until construction returns.
        unsafe {
            slot.acquire = slot
                .owner
                .device
                .raw()
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                .map_err(|r| native(&slot.owner, "vkCreateSemaphore(acquire)", r))?;
            slot.finished = slot
                .owner
                .device
                .raw()
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                .map_err(|r| native(&slot.owner, "vkCreateSemaphore(present)", r))?;
            slot.present_fence = slot
                .owner
                .device
                .raw()
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(|r| native(&slot.owner, "vkCreateFence(present)", r))?;
            slot.cleanup_fence = slot
                .owner
                .device
                .raw()
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(|r| native(&slot.owner, "vkCreateFence(cancel)", r))?;
        }
        Ok(Arc::new(slot))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        // SAFETY: Pending/uncertain work and present records keep this Slot
        // alive. Normal release requires GPU completion AND present completion.
        unsafe {
            let device = self.owner.device.raw();
            device.destroy_fence(self.cleanup_fence, None);
            device.destroy_fence(self.present_fence, None);
            device.destroy_semaphore(self.finished, None);
            device.destroy_semaphore(self.acquire, None);
        }
    }
}
struct SlotRecord {
    slot: Arc<Slot>,
    present: Option<Arc<Swapchain>>,
}

/// Holds a successful acquire immediately, including across a logging unwind.
/// Exceptional cancellation may wait; the normal frame path never does.
struct Acquired {
    chain: Arc<Swapchain>,
    slot: Arc<Slot>,
    image: u32,
    handled: bool,
    cleanup_submitted: bool,
    uncertain_release: bool,
}
impl Acquired {
    fn cancel(&mut self) -> Result<(), RuntimeError> {
        let owner = &self.slot.owner;
        let semaphore = if self.slot.accepted.load(Ordering::Acquire) {
            self.slot.finished
        } else {
            self.slot.acquire
        };
        let waits = [vk::SemaphoreSubmitInfo::default()
            .semaphore(semaphore)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let submits = [vk::SubmitInfo2::default().wait_semaphore_infos(&waits)];
        if !self.cleanup_submitted {
            let _queue = owner
                .device
                .lock_queue()
                .map_err(RuntimeError::device_unavailable)?;
            // SAFETY: The acquire signal (or accepted rendering signal) exists.
            // No other wait consumes it on this unpresented path. Slot/chain
            // retain all dependencies through the cleanup Fence wait below.
            self.cleanup_submitted = true;
            // SAFETY: The Slot retains the sole unconsumed signal and cleanup
            // Fence described above; the Device-owned queue lock is held.
            if let Err(result) = unsafe {
                owner.device.raw().queue_submit2(
                    owner.device.queue(),
                    &submits,
                    self.slot.cleanup_fence,
                )
            } {
                if matches!(
                    result,
                    vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY
                ) {
                    self.cleanup_submitted = false;
                }
                owner.device.observe_submit_result(result);
                return Err(native(owner, "vkQueueSubmit2(cancel acquire)", result));
            }
        }
        // SAFETY: Successful submit associated this owned cleanup Fence with
        // the wait above; no pool reset or owner destruction can race it.
        unsafe {
            owner.device.raw().wait_for_fences(
                &[self.slot.cleanup_fence],
                true,
                timeout_ns(CLEANUP_BUDGET),
            )
        }
        .map_err(|r| native(owner, "vkWaitForFences(cancel acquire)", r))?;
        let maintenance = ash::ext::swapchain_maintenance1::Device::new(
            owner.device.instance_raw(),
            owner.device.raw(),
        );
        let images = [self.image];
        self.uncertain_release = true;
        // SAFETY: The image remains acquired, and the Fence proved its acquire
        // signal plus all optional rendering work is no longer in use by Device.
        if let Err(result) = unsafe {
            maintenance.release_swapchain_images(
                &vk::ReleaseSwapchainImagesInfoEXT::default()
                    .swapchain(self.chain.handle)
                    .image_indices(&images),
            )
        } {
            return Err(native(owner, "vkReleaseSwapchainImagesEXT", result));
        }
        self.uncertain_release = false;
        self.slot.busy.store(false, Ordering::Release);
        self.handled = true;
        Ok(())
    }
}
impl Drop for Acquired {
    fn drop(&mut self) {
        if !self.handled && (self.uncertain_release || self.cancel().is_err()) {
            self.slot.owner.device.stop();
            // Completion could not be proved. Preserve native semaphores,
            // Swapchain, Window, Device/Allocator and Instance as one chain.
            std::mem::forget(Arc::clone(&self.slot));
            std::mem::forget(Arc::clone(&self.chain));
        }
    }
}

struct PresentWork {
    owner: Arc<MemoryOwner>,
    source: Arc<Physical>,
    previous: Snapshot,
    next: Option<Snapshot>,
    serial: u64,
    chain: Arc<Swapchain>,
    slot: Arc<Slot>,
    image: u32,
}
impl TransferPayload for PresentWork {
    fn owner(&self) -> &Arc<MemoryOwner> {
        &self.owner
    }
    fn validate(&self) -> Result<(), TransferError> {
        Ok(())
    }
    fn bytes(&self) -> u64 {
        0
    }
    fn label(&self) -> &'static std::ffi::CStr {
        c"Graph linear output -> sRGB presentation"
    }
    unsafe fn record(&self, command: vk::CommandBuffer) {
        let source = self.source.image();
        let barriers = [
            vk::ImageMemoryBarrier2::default()
                .src_stage_mask(self.previous.stage | self.previous.writer_stage)
                .src_access_mask(self.previous.access | self.previous.writer_access)
                .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                .old_layout(self.previous.layout)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(source.image)
                .subresource_range(color_range()),
            vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(self.chain.images[self.image as usize])
                .subresource_range(color_range()),
        ];
        let extent = self.chain.extent;
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent,
        };
        let attachments = [vk::RenderingAttachmentInfo::default()
            .image_view(self.chain.views[self.image as usize])
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::DONT_CARE)
            .store_op(vk::AttachmentStoreOp::STORE)];
        let rendering = vk::RenderingInfo::default()
            .render_area(area)
            .layer_count(1)
            .color_attachments(&attachments);
        let final_barriers = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::NONE)
            .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.chain.images[self.image as usize])
            .subresource_range(color_range())];
        // SAFETY: Payload retains the source, views, descriptors and program.
        // Source state came from the serialized canonical registry; acquire is
        // waited at COLOR_ATTACHMENT_OUTPUT before the full target overwrite.
        // Linear sampled values are encoded by the actual SRGB attachment.
        unsafe {
            let device = self.owner.device.raw();
            device.cmd_pipeline_barrier2(
                command,
                &vk::DependencyInfo::default().image_memory_barriers(&barriers),
            );
            device.cmd_begin_rendering(command, &rendering);
            device.cmd_set_viewport(
                command,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            device.cmd_set_scissor(command, 0, &[area]);
            device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.slot.binding.program.pipeline,
            );
            device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                self.slot.binding.program.layout,
                0,
                &[self.slot.binding.set],
                &[],
            );
            device.cmd_draw(command, 3, 1, 0, 0);
            device.cmd_end_rendering(command);
            device.cmd_pipeline_barrier2(
                command,
                &vk::DependencyInfo::default().image_memory_barriers(&final_barriers),
            );
        }
    }
    fn submitted(&mut self) {
        self.slot.accepted.store(true, Ordering::Release);
        if let Some(next) = self.next.take() {
            self.source.lock().scheduled = next;
        }
    }
    unsafe fn establish_contents(&mut self) {
        let mut state = self.source.lock();
        state.completed_serial = state.completed_serial.max(self.serial);
    }
}

struct Inner {
    owner: Arc<MemoryOwner>,
    surface: Arc<WindowSurface>,
    current: Option<Arc<Swapchain>>,
    retired: Vec<Arc<Swapchain>>,
    slots: Vec<SlotRecord>,
    programs: Vec<Arc<Program>>,
    pending: Vec<PendingTransfer<PresentWork>>,
    desired: (u32, u32),
    dirty: bool,
    closed: bool,
    surface_lost: bool,
    config: PresentationConfig,
    #[cfg(test)]
    cancel_next_acquire: bool,
}

impl Inner {
    fn query_present_mode(&self, vsync: bool) -> Result<vk::PresentModeKHR, RuntimeError> {
        // SAFETY: This live Surface and physical Device were selected together.
        let supported = unsafe {
            self.surface
                .loader
                .get_physical_device_surface_present_modes(
                    self.owner.device.physical_device(),
                    self.surface.handle,
                )
        }
        .map_err(|r| native(&self.owner, "vkGetPhysicalDeviceSurfacePresentModesKHR", r))?;
        choose_present_mode(&supported, vsync)
    }

    fn poll(&mut self, timeout: Duration) -> Result<usize, RuntimeError> {
        let start = Instant::now();
        let mut retired = 0;
        let mut first_error = None;
        let mut index = 0;
        while index < self.pending.len() {
            match self.pending[index].wait(timeout.saturating_sub(start.elapsed())) {
                Ok(Some(_)) => {
                    drop(self.pending.swap_remove(index));
                    retired += 1;
                }
                Ok(None) => index += 1,
                Err(error) => {
                    // A successful/lost native wait may retire ownership while
                    // still refusing contents on a stopped/lost Device. Remove
                    // terminal entries, then observe every other real Fence.
                    if self.pending[index].is_pending() {
                        index += 1;
                    } else {
                        drop(self.pending.swap_remove(index));
                    }
                    if first_error.is_none() {
                        first_error = Some(error.into());
                    }
                }
            }
        }
        for record in &mut self.slots {
            if record.present.is_some() {
                // SAFETY: This Fence was associated with this exact present
                // operation and is never reset while the record remains live.
                match unsafe {
                    self.owner.device.raw().wait_for_fences(
                        &[record.slot.present_fence],
                        true,
                        timeout_ns(timeout.saturating_sub(start.elapsed())),
                    )
                } {
                    Ok(()) => {
                        record.present = None;
                        record.slot.busy.store(false, Ordering::Release);
                    }
                    Err(vk::Result::TIMEOUT) => {}
                    Err(r) => {
                        // Loss is not a successful presentation-payload transfer
                        // proof. Retain this record, but still query the others.
                        if first_error.is_none() {
                            first_error = Some(native(&self.owner, "vkWaitForFences(present)", r));
                        } else {
                            self.owner.device.observe_result(r);
                        }
                    }
                }
            }
        }
        self.retired.retain(|chain| Arc::strong_count(chain) != 1);
        match first_error {
            Some(error) => Err(error),
            None => Ok(retired),
        }
    }

    fn ensure_swapchain(&mut self) -> Result<Option<PresentOutcome>, RuntimeError> {
        if self.closed {
            return Err(RuntimeError::invalid("Presenter is closed"));
        }
        if self.surface_lost {
            return Err(RuntimeError::unsupported(
                "Surface was lost; create a new window runtime",
            ));
        }
        if self.desired.0 == 0 || self.desired.1 == 0 {
            return Ok(Some(PresentOutcome::Suspended));
        }
        if !self.dirty && self.current.is_some() {
            return Ok(None);
        }
        self.poll(Duration::ZERO)?;
        // SAFETY: Live Surface and physical Device were selected together.
        let caps = unsafe {
            self.surface
                .loader
                .get_physical_device_surface_capabilities(
                    self.owner.device.physical_device(),
                    self.surface.handle,
                )
        }
        .map_err(|r| native(&self.owner, "vkGetPhysicalDeviceSurfaceCapabilitiesKHR", r))?;
        let Some(extent) = choose_extent(&caps, self.desired) else {
            return Ok(Some(PresentOutcome::Suspended));
        };
        // SAFETY: Same retained Surface and adapter.
        let formats = unsafe {
            self.surface.loader.get_physical_device_surface_formats(
                self.owner.device.physical_device(),
                self.surface.handle,
            )
        }
        .map_err(|r| native(&self.owner, "vkGetPhysicalDeviceSurfaceFormatsKHR", r))?;
        let format = choose_format(&formats)?;
        // Validate before retiring the old Swapchain. A mode change uses the
        // same bounded generation/lifetime path as a resize, without idle waits.
        let present_mode = self.query_present_mode(self.config.vsync)?;
        if !caps
            .supported_usage_flags
            .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        {
            return Err(RuntimeError::unsupported(
                "Surface lacks COLOR_ATTACHMENT usage",
            ));
        }
        if !caps
            .supported_composite_alpha
            .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
        {
            return Err(RuntimeError::unsupported(
                "M3 presentation requires an OPAQUE Surface",
            ));
        }
        let alpha = vk::CompositeAlphaFlagsKHR::OPAQUE;
        let query = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(format.format)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT);
        let mut support = vk::ImageFormatProperties2::default();
        // SAFETY: Check the actual implied Swapchain image creation parameters
        // before retiring the current Swapchain or creating its replacement.
        unsafe {
            self.owner
                .device
                .instance_raw()
                .get_physical_device_image_format_properties2(
                    self.owner.device.physical_device(),
                    &query,
                    &mut support,
                )
        }
        .map_err(|r| {
            native(
                &self.owner,
                "vkGetPhysicalDeviceImageFormatProperties2(present)",
                r,
            )
        })?;
        if extent.width > support.image_format_properties.max_extent.width
            || extent.height > support.image_format_properties.max_extent.height
            || !support
                .image_format_properties
                .sample_counts
                .contains(vk::SampleCountFlags::TYPE_1)
        {
            return Err(RuntimeError::unsupported(
                "Swapchain image extent/sample count exceeds format support",
            ));
        }
        let plan = SwapchainPlan {
            extent,
            format,
            alpha,
            count: image_count(&caps, self.config.desired_images),
            transform: caps.current_transform,
            present_mode,
        };
        if self.current.is_some() && self.retired.len() >= self.config.max_retired_swapchains {
            return Err(RuntimeError::capacity(
                "retired Swapchain capacity exhausted; poll/wait before retry",
            ));
        }
        // Retire the old owner before the native create: even a failed create
        // retires oldSwapchain. No subsequent acquire may use that old handle.
        let old = self.current.take();
        let old_handle = old
            .as_ref()
            .map_or(vk::SwapchainKHR::null(), |chain| chain.handle);
        if let Some(old) = old {
            self.retired.push(old);
        }
        self.current = Some(Swapchain::new(
            Arc::clone(&self.owner),
            Arc::clone(&self.surface),
            plan,
            old_handle,
        )?);
        self.dirty = false;
        Ok(Some(PresentOutcome::Recreated))
    }

    fn slot(
        &mut self,
        format: vk::Format,
        source: vk::ImageView,
    ) -> Result<Option<usize>, RuntimeError> {
        let index = self.slots.iter().position(|record| {
            !record.slot.busy.load(Ordering::Acquire)
                && record.present.is_none()
                && Arc::strong_count(&record.slot) == 1
        });
        let index = match index {
            Some(index) => index,
            None if self.slots.len() < self.config.max_frames => {
                let program = self.program(format)?;
                self.slots.push(SlotRecord {
                    slot: Slot::new(Arc::clone(&self.owner), program)?,
                    present: None,
                });
                self.slots.len() - 1
            }
            None => return Ok(None),
        };
        let program = self.program(format)?;
        let slot = Arc::get_mut(&mut self.slots[index].slot).expect("idle Slot has one owner");
        if slot.binding.program.format != format {
            slot.binding = Binding::new(program)?;
        }
        // SAFETY: Both prior cleanup and present usages completed before this
        // Slot became idle; no recorded work retains it (unique Arc above).
        unsafe {
            self.owner
                .device
                .raw()
                .reset_fences(&[slot.present_fence, slot.cleanup_fence])
        }
        .map_err(|r| native(&self.owner, "vkResetFences(presentation slot)", r))?;
        slot.accepted.store(false, Ordering::Release);
        slot.binding.update(source);
        Ok(Some(index))
    }
    fn program(&mut self, format: vk::Format) -> Result<Arc<Program>, RuntimeError> {
        if let Some(program) = self
            .programs
            .iter()
            .find(|program| program.format == format)
        {
            return Ok(Arc::clone(program));
        }
        let program = Program::new(Arc::clone(&self.owner), format)?;
        self.programs.push(Arc::clone(&program));
        Ok(program)
    }
    fn idle(&self) -> bool {
        self.pending.is_empty()
            && self
                .slots
                .iter()
                .all(|record| record.present.is_none() && !record.slot.busy.load(Ordering::Acquire))
    }
}

/// Owns presentation state and the Arc<Window> that backs its native Surface.
/// Call `poll` regularly and `close` explicitly; event loops remain app-owned.
pub struct Presenter {
    inner: Option<Inner>,
}
impl Presenter {
    /// Requested VSync setting, or None after close. A pending change takes
    /// effect on the next successful Swapchain recreation; old presents retain
    /// their original mode until their own completion.
    pub fn vsync(&self) -> Option<bool> {
        self.inner
            .as_ref()
            .filter(|inner| !inner.closed)
            .map(|inner| inner.config.vsync)
    }

    /// Request FIFO (on) or IMMEDIATE (off). Checks Surface support before
    /// changing the request; unsupported modes leave the existing state intact.
    /// Call `prepare` before building the next output. Recreation may require
    /// polling bounded retired generations; this method never waits for idle.
    pub fn set_vsync(&mut self, enabled: bool) -> Result<(), RuntimeError> {
        let inner = self
            .inner
            .as_mut()
            .ok_or_else(|| RuntimeError::invalid("Presenter is closed"))?;
        if inner.closed {
            return Err(RuntimeError::invalid("Presenter is closed"));
        }
        if inner.surface_lost {
            return Err(RuntimeError::unsupported(
                "Surface was lost; create a new window runtime",
            ));
        }
        inner
            .owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if inner.config.vsync != enabled {
            inner.query_present_mode(enabled)?;
            inner.config.vsync = enabled;
            inner.dirty = true;
        }
        Ok(())
    }

    /// Resolve a resize or VSync change before building a new output Graph. `None` means the
    /// existing extent is ready; Suspended/Recreated carry no submitted work.
    pub fn prepare(&mut self) -> Result<Option<PresentOutcome>, RuntimeError> {
        let inner = self
            .inner
            .as_mut()
            .ok_or_else(|| RuntimeError::invalid("Presenter is closed"))?;
        inner
            .owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        inner.poll(Duration::ZERO)?;
        inner.ensure_swapchain()
    }
    pub fn extent(&self) -> Option<(u32, u32)> {
        let inner = self.inner.as_ref()?;
        if inner.closed || inner.desired.0 == 0 || inner.desired.1 == 0 {
            return None;
        }
        inner
            .current
            .as_ref()
            .map(|chain| (chain.extent.width, chain.extent.height))
    }
    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(inner) = &mut self.inner {
            if inner.desired != (width, height) {
                inner.dirty = true;
            }
            inner.desired = (width, height);
        }
    }
    pub fn poll(&mut self) -> Result<usize, RuntimeError> {
        self.inner
            .as_mut()
            .ok_or_else(|| RuntimeError::invalid("Presenter is closed"))?
            .poll(Duration::ZERO)
    }
    /// Wait only this Presenter's GPU/present work, never Device/Queue idle.
    pub fn wait_idle(&mut self, timeout: Duration) -> Result<bool, RuntimeError> {
        let inner = self
            .inner
            .as_mut()
            .ok_or_else(|| RuntimeError::invalid("Presenter is closed"))?;
        inner.poll(timeout)?;
        Ok(inner.idle())
    }
    pub fn close(&mut self, timeout: Duration) -> Result<bool, RuntimeError> {
        let Some(inner) = &mut self.inner else {
            return Ok(true);
        };
        inner.poll(timeout)?;
        if !inner.idle() {
            return Ok(false);
        }
        inner.closed = true;
        inner.current = None;
        inner.retired.clear();
        Ok(true)
    }
    #[cfg(test)]
    fn cancel_next_acquire(&mut self) {
        self.inner.as_mut().unwrap().cancel_next_acquire = true;
    }
}
impl Drop for Presenter {
    fn drop(&mut self) {
        let Some(mut inner) = self.inner.take() else {
            return;
        };
        if inner.poll(CLEANUP_BUDGET).is_err() || !inner.idle() {
            inner.owner.device.stop();
            // As with unresolved Graph work, do not destroy potentially used
            // objects or the native Window/Instance behind them on uncertainty.
            std::mem::forget(inner);
        }
    }
}

impl VulkanRuntime {
    pub fn for_window(
        config: RuntimeConfig,
        window: Arc<Window>,
        presentation: PresentationConfig,
    ) -> Result<(Self, Presenter), RuntimeError> {
        Self::for_window_with_validation(config, window, presentation, ValidationMode::Required)
    }
    pub fn for_window_with_validation(
        config: RuntimeConfig,
        window: Arc<Window>,
        presentation: PresentationConfig,
        validation: ValidationMode,
    ) -> Result<(Self, Presenter), RuntimeError> {
        Self::validate_config(config)?;
        validate_config(presentation)?;
        let mut retired = Vec::new();
        let mut slots = Vec::new();
        let mut pending = Vec::new();
        retired
            .try_reserve_exact(presentation.max_retired_swapchains)
            .map_err(|e| {
                RuntimeError::capacity(format!("cannot reserve retired Swapchain records: {e}"))
            })?;
        slots
            .try_reserve_exact(presentation.max_frames)
            .map_err(|e| {
                RuntimeError::capacity(format!("cannot reserve presentation Slot records: {e}"))
            })?;
        pending
            .try_reserve_exact(presentation.max_frames)
            .map_err(|e| {
                RuntimeError::capacity(format!("cannot reserve pending presentation records: {e}"))
            })?;
        let size = window.inner_size();
        let diagnostics = Diagnostics::new();
        let surface = WindowSurface::new(window, Arc::clone(&diagnostics), validation)?;
        let device = VulkanDevice::for_surface(Arc::clone(&surface.instance), &surface)?;
        let runtime = Self::from_device(config, diagnostics, device)?;
        let mut inner = Inner {
            owner: Arc::clone(&runtime.owner),
            surface,
            current: None,
            retired,
            slots,
            programs: Vec::with_capacity(2),
            pending,
            desired: (size.width, size.height),
            dirty: true,
            closed: false,
            surface_lost: false,
            config: presentation,
            #[cfg(test)]
            cancel_next_acquire: false,
        };
        // Reject an unsupported initial request even for a minimized Window.
        inner.query_present_mode(presentation.vsync)?;
        inner.ensure_swapchain()?;
        Ok((runtime, Presenter { inner: Some(inner) }))
    }

    /// Sample a fresh exported linear RGBA8 image into this Window's SRGB
    /// output. This is a second same-queue submit, with no intervening CPU wait.
    /// Sampling preserves its content revision while advancing canonical native
    /// scopes/layout. A snapshot remains valid until its contents are mutated.
    pub fn present(
        &mut self,
        presenter: &mut Presenter,
        image: &ImportImage,
    ) -> Result<PresentOutcome, RuntimeError> {
        let inner = presenter
            .inner
            .as_mut()
            .ok_or_else(|| RuntimeError::invalid("Presenter is closed"))?;
        if !Arc::ptr_eq(&self.owner, &inner.owner) {
            return Err(RuntimeError::wrong_owner(
                "Presenter belongs to another runtime",
            ));
        }
        self.owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        inner.poll(Duration::ZERO)?;
        if let Some(outcome) = inner.ensure_swapchain()? {
            return Ok(outcome);
        }
        let source = Arc::clone(&self.entry(image.key)?.physical);
        let ResourceDesc::Image(desc) = source.desc else {
            return Err(RuntimeError::invalid("presentation source is not an image"));
        };
        let previous = source.lock().scheduled.clone();
        if desc != image.desc
            || previous.revision != image.revision
            || !image.initialized
            || previous.initialized.len() != 1
            || previous.initialized[0] != (0..1)
        {
            return Err(RuntimeError::stale(
                "presentation source snapshot is stale/uninitialized; refresh and retry",
            ));
        }
        let chain = Arc::clone(inner.current.as_ref().expect("configured Swapchain"));
        if desc.format != ImageFormat::Rgba8Unorm
            || (desc.width, desc.height) != (chain.extent.width, chain.extent.height)
        {
            return Err(RuntimeError::invalid(
                "presentation requires linear RGBA8 with the current Swapchain extent",
            ));
        }
        // Reserve before acquire, so ordinary runtime capacity exhaustion never
        // leaves an acquired image or a signaled acquire Semaphore behind.
        let reservation = self.execution.reserve()?;
        let Some(index) = inner.slot(chain.format, source.image().view)? else {
            return Ok(PresentOutcome::NotReady);
        };
        let slot = Arc::clone(&inner.slots[index].slot);
        // SAFETY: Idle Slot has no pending acquire/signal/wait. Finite zero
        // timeout also works for minimized/occluded surfaces without progress.
        let (image_index, suboptimal) = match unsafe {
            chain
                .loader
                .acquire_next_image(chain.handle, 0, slot.acquire, vk::Fence::null())
        } {
            Ok(value) => value,
            Err(vk::Result::TIMEOUT | vk::Result::NOT_READY) => {
                return Ok(PresentOutcome::NotReady);
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                inner.dirty = true;
                return Ok(PresentOutcome::Recreated);
            }
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                inner.surface_lost = true;
                return Err(RuntimeError::unsupported("Surface lost during acquire"));
            }
            Err(r) => return Err(native(&self.owner, "vkAcquireNextImageKHR", r)),
        };
        slot.busy.store(true, Ordering::Release);
        let mut acquired = Acquired {
            chain: Arc::clone(&chain),
            slot: Arc::clone(&slot),
            image: image_index,
            handled: false,
            cleanup_submitted: false,
            uncertain_release: false,
        };
        #[cfg(test)]
        if std::mem::take(&mut inner.cancel_next_acquire) {
            acquired.cancel()?;
            return Ok(PresentOutcome::NotReady);
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or_else(|| RuntimeError::capacity("submission serial exhausted"))?;
        let mut next = previous.clone();
        next.serial = serial;
        next.stage = vk::PipelineStageFlags2::FRAGMENT_SHADER;
        next.access = vk::AccessFlags2::SHADER_SAMPLED_READ;
        next.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        let work = PresentWork {
            owner: Arc::clone(&self.owner),
            source,
            previous,
            next: Some(next),
            serial,
            chain: Arc::clone(&chain),
            slot: Arc::clone(&slot),
            image: image_index,
        };
        let recorded = reservation.record(work)?;
        let waits = [vk::SemaphoreSubmitInfo::default()
            .semaphore(slot.acquire)
            .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)];
        let signals = [vk::SemaphoreSubmitInfo::default()
            .semaphore(slot.finished)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        self.serial = serial;
        // SAFETY: Binary acquire signal belongs to this image acquisition;
        // finished is idle and is consumed exactly once by present or cancel.
        // PresentWork retains the Slot, Surface, source, views and native Device
        // throughout recorded/pending/uncertain work. Stages match the barriers.
        let pending = unsafe { recorded.submit_with_sync(&waits, &signals) }?;
        inner.pending.push(pending);
        let swapchains = [chain.handle];
        let indices = [image_index];
        let wait = [slot.finished];
        let fences = [slot.present_fence];
        let mut fence_info = vk::SwapchainPresentFenceInfoEXT::default().fences(&fences);
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait)
            .swapchains(&swapchains)
            .image_indices(&indices)
            .push_next(&mut fence_info);
        let result = {
            let _queue = self
                .owner
                .device
                .lock_queue()
                .map_err(RuntimeError::device_unavailable)?;
            // SAFETY: Rendering signal was accepted before this call, image is
            // acquired and transitioned to PRESENT_SRC by that submit, this
            // Surface supports the same queue, and the unique fence is idle.
            match unsafe { chain.loader.queue_present(self.owner.device.queue(), &info) } {
                Ok(false) => vk::Result::SUCCESS,
                Ok(true) => vk::Result::SUBOPTIMAL_KHR,
                Err(r) => r,
            }
        };
        match classify_present(result) {
            PresentResult::Enqueued { recreate, lost } => {
                inner.slots[index].present = Some(chain);
                acquired.handled = true;
                inner.dirty |= recreate || suboptimal;
                inner.surface_lost |= lost;
                if lost {
                    return Err(RuntimeError::unsupported(
                        "Surface lost during present; pending presentation retained",
                    ));
                }
                Ok(PresentOutcome::Presented {
                    suboptimal: suboptimal || recreate,
                })
            }
            PresentResult::Unchanged => {
                // The error leaves finished unconsumed. Exceptional cleanup
                // waits it and releases the acquired image before returning.
                acquired.cancel()?;
                Err(native(&self.owner, "vkQueuePresentKHR", result))
            }
            PresentResult::Uncertain => {
                self.owner.device.observe_result(result);
                self.owner.device.stop();
                // Acquired Drop observes the stopped Device and preserves its
                // dependency chain rather than guessing signal/wait completion.
                Err(native(&self.owner, "vkQueuePresentKHR", result))
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod state_tests {
    use super::*;

    #[test]
    fn presentation_errors_preserve_their_actual_enqueue_semantics() {
        for result in [
            vk::Result::SUCCESS,
            vk::Result::SUBOPTIMAL_KHR,
            vk::Result::ERROR_OUT_OF_DATE_KHR,
            vk::Result::ERROR_SURFACE_LOST_KHR,
        ] {
            assert!(matches!(
                classify_present(result),
                PresentResult::Enqueued { .. }
            ));
        }
        assert_eq!(
            classify_present(vk::Result::ERROR_OUT_OF_DATE_KHR),
            PresentResult::Enqueued {
                recreate: true,
                lost: false
            }
        );
        assert_eq!(
            classify_present(vk::Result::ERROR_SURFACE_LOST_KHR),
            PresentResult::Enqueued {
                recreate: false,
                lost: true
            }
        );
        for result in [
            vk::Result::ERROR_OUT_OF_HOST_MEMORY,
            vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
        ] {
            assert_eq!(classify_present(result), PresentResult::Unchanged);
        }
        assert_eq!(
            classify_present(vk::Result::ERROR_DEVICE_LOST),
            PresentResult::Uncertain
        );
        assert_eq!(
            classify_present(vk::Result::ERROR_UNKNOWN),
            PresentResult::Uncertain
        );
    }

    #[test]
    fn minimized_surface_never_creates_zero_extent_and_unbounded_count_is_not_zero() {
        let mut caps = vk::SurfaceCapabilitiesKHR {
            current_extent: vk::Extent2D {
                width: u32::MAX,
                height: u32::MAX,
            },
            min_image_extent: vk::Extent2D {
                width: 16,
                height: 16,
            },
            max_image_extent: vk::Extent2D {
                width: 1024,
                height: 768,
            },
            min_image_count: 2,
            ..Default::default()
        };
        assert_eq!(choose_extent(&caps, (0, 720)), None);
        assert_eq!(
            choose_extent(&caps, (1280, 1)),
            Some(vk::Extent2D {
                width: 1024,
                height: 16
            })
        );
        assert_eq!(image_count(&caps, 3), 3);
        caps.max_image_count = 2;
        assert_eq!(image_count(&caps, 3), 2);
        caps.current_extent = vk::Extent2D {
            width: 640,
            height: 480,
        };
        assert_eq!(choose_extent(&caps, (800, 600)), Some(caps.current_extent));
        caps.max_image_extent.width = 0;
        assert_eq!(choose_extent(&caps, (800, 600)), None);
    }

    #[test]
    fn linear_unorm_surface_is_not_silently_treated_as_srgb() {
        let unorm = vk::SurfaceFormatKHR {
            format: vk::Format::B8G8R8A8_UNORM,
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
        };
        assert!(choose_format(&[unorm]).is_err());
        let srgb = vk::SurfaceFormatKHR {
            format: vk::Format::R8G8B8A8_SRGB,
            ..unorm
        };
        assert_eq!(choose_format(&[unorm, srgb]).unwrap().format, srgb.format);
        assert!(
            validate_config(PresentationConfig {
                max_frames: 0,
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn vsync_selects_only_the_requested_supported_mode() {
        let modes = [
            vk::PresentModeKHR::IMMEDIATE,
            vk::PresentModeKHR::FIFO,
            vk::PresentModeKHR::MAILBOX,
        ];
        assert!(PresentationConfig::default().vsync);
        assert_eq!(
            choose_present_mode(&modes, true).unwrap(),
            vk::PresentModeKHR::FIFO
        );
        assert_eq!(
            choose_present_mode(&modes, false).unwrap(),
            vk::PresentModeKHR::IMMEDIATE
        );
        let synchronized = [vk::PresentModeKHR::FIFO, vk::PresentModeKHR::MAILBOX];
        assert_eq!(
            choose_present_mode(&synchronized, false)
                .unwrap_err()
                .kind(),
            super::super::RuntimeErrorKind::Unsupported
        );
        assert!(choose_present_mode(&[], true).is_err());
    }
}
