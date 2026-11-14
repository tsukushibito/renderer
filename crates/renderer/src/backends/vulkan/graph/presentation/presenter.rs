//! Public presentation control and conservative teardown.

use super::*;

/// Owns presentation state and the Arc<Window> that backs its native Surface.
/// Call `poll` regularly and `close` explicitly; event loops remain app-owned.
pub struct Presenter {
    pub(super) inner: Option<Inner>,
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
