//! Graph-owned pending submissions and bounded Swapchain generation retirement.

use super::*;

pub(super) struct Inner {
    pub(super) owner: Arc<MemoryOwner>,
    pub(super) surface: Arc<WindowSurface>,
    pub(super) current: Option<Arc<Swapchain>>,
    pub(super) retired: Vec<Arc<Swapchain>>,
    pub(super) slots: Vec<SlotRecord>,
    pub(super) programs: Vec<Arc<Program>>,
    pub(super) prepared_format: Option<vk::Format>,
    pub(super) pending: Vec<PendingPresent>,
    pub(super) profiling_enabled: bool,
    pub(super) profiles: Option<ProfileStorage>,
    pub(super) desired: (u32, u32),
    pub(super) dirty: bool,
    pub(super) closed: bool,
    pub(super) surface_lost: bool,
    pub(super) config: PresentationConfig,
    #[cfg(test)]
    pub(super) cancel_next_acquire: bool,
}

impl Inner {
    pub(super) fn query_present_mode(
        &self,
        vsync: bool,
    ) -> Result<vk::PresentModeKHR, RuntimeError> {
        query_present_mode(&self.owner, &self.surface, vsync)
    }

    pub(super) fn poll(&mut self, timeout: Duration) -> Result<usize, RuntimeError> {
        let start = Instant::now();
        let mut retired = 0;
        let mut first_error = None;
        let mut index = 0;
        while index < self.pending.len() {
            match self.pending[index]
                .work
                .wait(timeout.saturating_sub(start.elapsed()))
            {
                Ok(Some(work)) => {
                    // Preserve submit order for the public FIFO samples. All
                    // entries share this queue; Vec removal reuses its storage.
                    let pending = self.pending.remove(index);
                    let sample = (|| -> Result<Option<PresentationMetrics>, RuntimeError> {
                        let Some(mut metrics) = pending.metrics else {
                            return Ok(None);
                        };
                        let profile = work
                            .profile
                            .as_ref()
                            .expect("profiled work retains queries");
                        let retire_started = profile.retire_started.expect("Fence completed");
                        // SAFETY: wait returned the payload only after its actual
                        // successful Fence wait and active-Device check.
                        metrics.gpu = unsafe { profile.finish() }?;
                        drop(work);
                        metrics.cpu_retire = retire_started.elapsed();
                        Ok(Some(metrics))
                    })();
                    match sample {
                        Ok(Some(sample)) => self
                            .profiles
                            .as_mut()
                            .expect("profile storage retained")
                            .push(sample),
                        Ok(None) => {}
                        Err(error) if first_error.is_none() => first_error = Some(error),
                        Err(_) => {}
                    }
                    retired += 1;
                }
                // Completion may overlap on the queue. Retain a later finished
                // sample until the oldest submission is observed, so callers
                // always consume timings in accepted GPU-submission order.
                Ok(None) => break,
                Err(error) => {
                    // A successful/lost native wait may retire ownership while
                    // still refusing contents on a stopped/lost Device. Remove
                    // terminal entries, then observe every other real Fence.
                    if self.pending[index].work.is_pending() {
                        index += 1;
                    } else {
                        drop(self.pending.remove(index));
                    }
                    if first_error.is_none() {
                        first_error = Some(error.into());
                    }
                }
            }
        }
        for record in &mut self.slots {
            if let Err(result) = record.poll_present(timeout, start) {
                // Loss is not a successful presentation-payload transfer proof.
                // The native record retains ownership, and every other Fence
                // must still be observed before returning the first error.
                if first_error.is_none() {
                    first_error = Some(native(&self.owner, "vkWaitForFences(present)", result));
                } else {
                    self.owner.device.observe_result(result);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(retired),
        }
    }

    pub(super) fn collect_retired_swapchains(&mut self) -> usize {
        let before = self.retired.len();
        // Every outstanding GPU command and queued present owns its own Arc.
        // A sole retired anchor therefore proves both uses have completed.
        // Keep these anchors through ordinary polling so their last-owner
        // native destruction and backing release stay at a change boundary.
        self.retired.retain(|chain| Arc::strong_count(chain) != 1);
        before - self.retired.len()
    }

    pub(super) fn ensure_swapchain(&mut self) -> Result<Option<PresentOutcome>, RuntimeError> {
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
            self.prepare_slots(self.current.as_ref().unwrap().format)?;
            return Ok(None);
        }
        // The public prepare/present entry points already poll before calling
        // this method. Initial construction has no pending work to retire.
        let Some(plan) =
            SwapchainPlan::query(&self.owner, &self.surface, self.desired, self.config)?
        else {
            return Ok(Some(PresentOutcome::Suspended));
        };
        // This is an explicit recreation boundary, after the clean-ready and
        // suspended paths. Reclaim completed generations before reserving the
        // next bounded retirement entry, including capacity-error retries.
        self.collect_retired_swapchains();
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
        self.prepare_slots(self.current.as_ref().unwrap().format)?;
        Ok(Some(PresentOutcome::Recreated))
    }

    fn prepare_slots(&mut self, format: vk::Format) -> Result<(), RuntimeError> {
        if self.prepared_format == Some(format) {
            return Ok(());
        }
        let program = self.program(format)?;
        for slot in &mut self.slots {
            slot.prepare_format(Arc::clone(&program))?;
        }
        while self.slots.len() < self.config.max_frames {
            self.slots.push(SlotRecord::new(Arc::clone(&program))?);
        }
        self.prepared_format = Some(format);
        Ok(())
    }

    pub(super) fn slot(
        &mut self,
        format: vk::Format,
        source: vk::ImageView,
    ) -> Result<Option<usize>, RuntimeError> {
        let Some(index) = self.slots.iter().position(SlotRecord::reusable) else {
            return Ok(None);
        };
        let program = Arc::clone(
            self.programs
                .iter()
                .find(|program| program.format == format)
                .expect("successful preparation retains the current format program"),
        );
        // SAFETY: Selection above proved both completion conditions and sole
        // Arc ownership; the source is held by the caller's canonical lease.
        // Cached programs and source images belong to this runtime's Device.
        unsafe { self.slots[index].prepare(program, source) }?;
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
    pub(super) fn idle(&self) -> bool {
        self.pending.is_empty() && self.slots.iter().all(SlotRecord::idle)
    }
}
