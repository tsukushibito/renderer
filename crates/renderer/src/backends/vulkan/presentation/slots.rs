//! Acquire/present synchronization slots and exceptional cancellation owners.

use super::*;

pub(in crate::backends::vulkan) struct Slot {
    owner: Arc<MemoryOwner>,
    acquire: vk::Semaphore,
    finished: vk::Semaphore,
    present_fence: vk::Fence,
    cleanup_fence: vk::Fence,
    pub(super) binding: Binding,
    busy: AtomicBool,
    accepted: AtomicBool,
}
impl Slot {
    pub(in crate::backends::vulkan) fn new(
        program: Arc<Program>,
    ) -> Result<Arc<Self>, RuntimeError> {
        let owner = Arc::clone(program.owner());
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
pub(in crate::backends::vulkan) struct SlotRecord {
    slot: Arc<Slot>,
    present: Option<Arc<Swapchain>>,
}

/// Holds a successful acquire immediately, including across a logging unwind.
/// Exceptional cancellation may wait; the normal frame path never does.
pub(in crate::backends::vulkan) struct Acquired {
    chain: Arc<Swapchain>,
    slot: Arc<Slot>,
    image: u32,
    handled: bool,
    cleanup_submitted: bool,
    uncertain_release: bool,
}
impl Acquired {
    /// Own the one successful acquire before any fallible preparation follows.
    ///
    /// # Safety
    /// `image` must be the image just acquired from `chain` using `slot.acquire`,
    /// with no other owner consuming that signal or presenting/releasing it.
    /// The Slot must have been idle and exclusively prepared for this acquire.
    pub(in crate::backends::vulkan) unsafe fn new(
        chain: Arc<Swapchain>,
        slot: Arc<Slot>,
        image: u32,
    ) -> Self {
        slot.busy.store(true, Ordering::Release);
        Self {
            chain,
            slot,
            image,
            handled: false,
            cleanup_submitted: false,
            uncertain_release: false,
        }
    }
    pub(in crate::backends::vulkan) fn mark_present_enqueued(&mut self) {
        self.handled = true;
    }
    pub(in crate::backends::vulkan) fn cancel(&mut self) -> Result<(), RuntimeError> {
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

impl Slot {
    pub(super) fn submitted(&self) {
        self.accepted.store(true, Ordering::Release);
    }
    pub(in crate::backends::vulkan) fn acquire(&self) -> vk::Semaphore {
        self.acquire
    }
    pub(in crate::backends::vulkan) fn finished(&self) -> vk::Semaphore {
        self.finished
    }
    pub(in crate::backends::vulkan) fn present_fence(&self) -> vk::Fence {
        self.present_fence
    }
    /// Reset the idle slot and point its descriptor at the retained source.
    ///
    /// # Safety
    /// All earlier GPU and present uses must have retired, no other Arc may
    /// retain this Slot, and its acquire signal must be unowned. `program` and
    /// `source` must belong to this Device; source must remain live through the
    /// new recorded/pending work. Its View must match the fixed sampled ABI.
    unsafe fn prepare(
        &mut self,
        program: Arc<Program>,
        source: vk::ImageView,
    ) -> Result<(), RuntimeError> {
        if self.binding.program.format != program.format {
            self.binding = Binding::new(program)?;
        }
        // SAFETY: Both prior cleanup and present usages completed before this
        // Slot became idle; no recorded work retains it (unique Arc above).
        unsafe {
            self.owner
                .device
                .raw()
                .reset_fences(&[self.present_fence, self.cleanup_fence])
        }
        .map_err(|r| native(&self.owner, "vkResetFences(presentation slot)", r))?;
        self.accepted.store(false, Ordering::Release);
        self.binding.update(source);
        Ok(())
    }
}

impl SlotRecord {
    pub(in crate::backends::vulkan) fn new(program: Arc<Program>) -> Result<Self, RuntimeError> {
        Ok(Self {
            slot: Slot::new(program)?,
            present: None,
        })
    }
    pub(in crate::backends::vulkan) fn slot(&self) -> &Arc<Slot> {
        &self.slot
    }
    pub(in crate::backends::vulkan) fn reusable(&self) -> bool {
        !self.slot.busy.load(Ordering::Acquire)
            && self.present.is_none()
            && Arc::strong_count(&self.slot) == 1
    }
    /// # Safety
    /// The caller has selected a reusable record and retains the same-Device
    /// sampled source through all new recorded/pending uses. `program` matches
    /// this source/target's fixed presentation ABI and belongs to this Device.
    pub(in crate::backends::vulkan) unsafe fn prepare(
        &mut self,
        program: Arc<Program>,
        source: vk::ImageView,
    ) -> Result<(), RuntimeError> {
        let slot = Arc::get_mut(&mut self.slot).expect("idle Slot has one owner");
        // SAFETY: Caller proved both completion conditions; sole Arc is also
        // enforced above. Source/program obligations are forwarded unchanged.
        unsafe { slot.prepare(program, source) }
    }
    pub(in crate::backends::vulkan) fn present_enqueued(&mut self, chain: Arc<Swapchain>) {
        self.present = Some(chain);
    }
    pub(in crate::backends::vulkan) fn idle(&self) -> bool {
        self.present.is_none() && !self.slot.busy.load(Ordering::Acquire)
    }
    pub(in crate::backends::vulkan) fn poll_present(
        &mut self,
        timeout: Duration,
        started: Instant,
    ) -> Result<(), vk::Result> {
        if self.present.is_none() {
            return Ok(());
        }
        // SAFETY: This Fence was associated with this exact present operation
        // and is never reset while the record remains live. GPU ownership is
        // independent: reusable() additionally requires its Slot Arc to retire.
        match unsafe {
            self.slot.owner.device.raw().wait_for_fences(
                &[self.slot.present_fence],
                true,
                timeout_ns(timeout.saturating_sub(started.elapsed())),
            )
        } {
            Ok(()) => {
                self.present = None;
                self.slot.busy.store(false, Ordering::Release);
                Ok(())
            }
            Err(vk::Result::TIMEOUT) => Ok(()),
            Err(result) => Err(result),
        }
    }
}
