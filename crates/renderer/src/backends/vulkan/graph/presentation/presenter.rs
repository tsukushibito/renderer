//! Public presentation control and conservative teardown.

use super::*;

/// Owns presentation state and the host window that backs its native Surface.
/// Call `poll` regularly and `close` explicitly; event loops remain app-owned.
pub struct Presenter {
    pub(super) inner: Option<Inner>,
}
impl Presenter {
    /// Enable optional CPU/API phases and a separate fullscreen GPU interval.
    /// Enabling reserves at most `max_frames` pending/completed samples at this
    /// explicit preparation boundary. Disabling affects future submissions;
    /// already submitted samples remain available and retain their queries.
    pub fn set_profiling(&mut self, enabled: bool) -> Result<(), RuntimeError> {
        let inner = self
            .inner
            .as_mut()
            .filter(|inner| !inner.closed)
            .ok_or_else(|| RuntimeError::invalid("Presenter is closed"))?;
        inner
            .owner
            .device
            .ensure_active()
            .map_err(RuntimeError::device_unavailable)?;
        if enabled && inner.profiles.is_none() {
            inner.profiles = Some(ProfileStorage::new(
                Arc::clone(&inner.owner),
                inner.config.max_frames,
            )?);
        }
        inner.profiling_enabled = enabled;
        Ok(())
    }

    /// Whether future presentation commands request timing. Closed presenters
    /// report false; completed samples can still be consumed after close.
    pub fn profiling_enabled(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|inner| !inner.closed && inner.profiling_enabled)
    }

    /// Consume the oldest completed presentation-command timing sample. Call
    /// `poll` to observe completion first. Every profiled GPU submission emits
    /// one sample, even if the subsequent display request was rejected; a sample
    /// is not proof that `present` returned `Presented` or reached the display.
    /// Consume samples regularly to avoid bounded timing backpressure.
    pub fn take_profile(&mut self) -> Option<PresentationMetrics> {
        self.inner.as_mut()?.profiles.as_mut()?.take()
    }

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
            return Err(RuntimeError::surface_lost(
                "Surface was lost; attach a new presenter",
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

    /// Resolve a resize, VSync change or `RecreateRequired` outcome before
    /// building a new output Graph. `None` means the existing extent is ready;
    /// `Recreated` means the new extent is ready. Neither submits rendering.
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
    /// Observe GPU/present completion and return reusable slots. Retired
    /// Swapchain generations remain anchored until collection or recreation.
    pub fn poll(&mut self) -> Result<usize, RuntimeError> {
        self.inner
            .as_mut()
            .ok_or_else(|| RuntimeError::invalid("Presenter is closed"))?
            .poll(Duration::ZERO)
    }
    /// Destroy retired Swapchain generations whose GPU and present ownership
    /// has already returned. Does not poll or wait; unobserved completions keep
    /// their generations retained. Call at an explicit change/maintenance
    /// boundary. Recreation also collects before checking retired capacity.
    pub fn collect_retired_swapchains(&mut self) -> usize {
        self.inner
            .as_mut()
            .map_or(0, Inner::collect_retired_swapchains)
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
    pub(super) fn cancel_next_acquire(&mut self) {
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
