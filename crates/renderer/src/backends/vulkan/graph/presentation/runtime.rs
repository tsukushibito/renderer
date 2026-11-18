//! Connect canonical Graph source snapshots to native WSI submissions.

use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

use super::*;

impl VulkanRuntime {
    pub fn for_window(
        config: RuntimeConfig,
        window: Arc<Window>,
        presentation: PresentationConfig,
    ) -> Result<(Self, Presenter), RuntimeError> {
        Self::for_window_with_validation(config, window, presentation, ValidationMode::default())
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
                RuntimeError::reservation_error(format!(
                    "cannot reserve retired Swapchain records: {e}"
                ))
            })?;
        slots
            .try_reserve_exact(presentation.max_frames)
            .map_err(|e| {
                RuntimeError::reservation_error(format!(
                    "cannot reserve presentation Slot records: {e}"
                ))
            })?;
        pending
            .try_reserve_exact(presentation.max_frames)
            .map_err(|e| {
                RuntimeError::reservation_error(format!(
                    "cannot reserve pending presentation records: {e}"
                ))
            })?;
        let size = window.inner_size();
        let diagnostics = Diagnostics::new();
        let surface = WindowSurface::new(window, diagnostics.clone(), validation)?;
        let device = VulkanDevice::for_surface(Arc::clone(&surface.instance), &surface)?;
        let runtime = Self::from_device(config, diagnostics, device)?;
        let mut inner = Inner {
            owner: Arc::clone(&runtime.owner),
            surface,
            current: None,
            retired,
            slots,
            programs: Vec::with_capacity(2),
            prepared_format: None,
            pending,
            profiling_enabled: false,
            profiles: None,
            desired: (size.width, size.height),
            dirty: true,
            closed: false,
            surface_lost: false,
            config: presentation,
            #[cfg(test)]
            cancel_next_acquire: false,
            #[cfg(test)]
            fail_after_spare_binding: false,
            #[cfg(test)]
            fail_after_swapchain_create: false,
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
        let profiling_started = inner.profiling_enabled.then(Instant::now);
        let source = self.lease(image.key())?;
        let ResourceDesc::Image(desc) = source.desc else {
            return Err(RuntimeError::invalid("presentation source is not an image"));
        };
        let previous = {
            let state = source.lock();
            let previous = &state.scheduled;
            if desc != image.desc()
                || previous.revision != image.revision()
                || !image.initialized()
                || previous.initialized.len() != 1
                || previous.initialized[0] != (0..1)
            {
                return Err(RuntimeError::stale(
                    "presentation source snapshot is stale/uninitialized; refresh and retry",
                ));
            }
            prepare_source(previous)
        };
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
        let slot = Arc::clone(inner.slots[index].slot());
        let profile_lease = if profiling_started.is_some() {
            Some(
                inner
                    .profiles
                    .as_ref()
                    .expect("enabled profiling is prepared")
                    .reserve(inner.pending.len())?,
            )
        } else {
            None
        };
        let mut metrics = profiling_started.map(|started| PresentationMetrics {
            cpu_prepare: started.elapsed(),
            cpu_acquire: Duration::ZERO,
            cpu_record: Duration::ZERO,
            cpu_submit: Duration::ZERO,
            cpu_present: Duration::ZERO,
            cpu_retire: Duration::ZERO,
            gpu: GpuTiming::NotReady,
        });
        let acquire_started = profiling_started.map(|_| Instant::now());
        // SAFETY: Idle Slot has no pending acquire/signal/wait. Finite zero
        // timeout also works for minimized/occluded surfaces without progress.
        let (image_index, suboptimal) = match unsafe {
            chain
                .loader
                .acquire_next_image(chain.handle, 0, slot.acquire(), vk::Fence::null())
        } {
            Ok(value) => value,
            Err(vk::Result::TIMEOUT | vk::Result::NOT_READY) => {
                return Ok(PresentOutcome::NotReady);
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                inner.dirty = true;
                return Ok(PresentOutcome::RecreateRequired);
            }
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                inner.surface_lost = true;
                return Err(RuntimeError::unsupported("Surface lost during acquire"));
            }
            Err(r) => return Err(native(&self.owner, "vkAcquireNextImageKHR", r)),
        };
        if let (Some(metrics), Some(started)) = (&mut metrics, acquire_started) {
            metrics.cpu_acquire = started.elapsed();
        }
        // SAFETY: Successful acquire above returned this exact image using
        // the uniquely prepared idle Slot. Install its native guard immediately.
        let mut acquired =
            unsafe { Acquired::new(Arc::clone(&chain), Arc::clone(&slot), image_index) };
        #[cfg(test)]
        if std::mem::take(&mut inner.cancel_next_acquire) {
            acquired.cancel()?;
            return Ok(PresentOutcome::NotReady);
        }
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
        let work = PresentWork {
            source,
            previous,
            serial,
            commands: PresentCommands::new(
                Arc::clone(&self.owner),
                Arc::clone(&chain),
                Arc::clone(&slot),
                image_index,
            ),
            profile: profile_lease.map(|lease| {
                PresentProfile::new(lease, profiling_started.expect("profiling enabled"))
            }),
        };
        let record_started = profiling_started.map(|_| Instant::now());
        let recorded = reservation.record(work)?;
        if let (Some(metrics), Some(started)) = (&mut metrics, record_started) {
            metrics.cpu_record = started.elapsed();
        }
        let waits = [vk::SemaphoreSubmitInfo::default()
            .semaphore(slot.acquire())
            .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)];
        let signals = [vk::SemaphoreSubmitInfo::default()
            .semaphore(slot.finished())
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        self.serial = serial;
        let submit_started = profiling_started.map(|_| Instant::now());
        // SAFETY: Binary acquire signal belongs to this image acquisition;
        // finished is idle and is consumed exactly once by present or cancel.
        // PresentWork retains the Slot, Surface, source, views and native Device
        // throughout recorded/pending/uncertain work. Stages match the barriers.
        let pending = unsafe { recorded.submit_with_sync(&waits, &signals) }?;
        if let (Some(metrics), Some(started)) = (&mut metrics, submit_started) {
            metrics.cpu_submit = started.elapsed();
        }
        inner.pending.push(PendingPresent {
            work: pending,
            metrics,
        });
        let swapchains = [chain.handle];
        let indices = [image_index];
        let wait = [slot.finished()];
        let fences = [slot.present_fence()];
        let mut fence_info = vk::SwapchainPresentFenceInfoEXT::default().fences(&fences);
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait)
            .swapchains(&swapchains)
            .image_indices(&indices)
            .push_next(&mut fence_info);
        let present_started = profiling_started.map(|_| Instant::now());
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
        if let (Some(metrics), Some(started)) = (
            &mut inner.pending.last_mut().expect("just submitted").metrics,
            present_started,
        ) {
            metrics.cpu_present = started.elapsed();
        }
        match classify_present(result) {
            PresentResult::Enqueued(outcome) => {
                // OUT_OF_DATE and SURFACE_LOST reject the presentation request
                // but still enqueue its semaphore waits. Retain exactly the
                // same native ownership as an accepted request until retirement.
                inner.slots[index].present_enqueued(chain);
                acquired.mark_present_enqueued();
                outcome.finish(suboptimal, &mut inner.dirty, &mut inner.surface_lost)
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
