use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use std::{marker::PhantomData, num::NonZeroIsize, rc::Rc};

/// A UI-thread-affine child window supplied by a native host.
pub struct HostChild {
    hwnd: NonZeroIsize,
    thread: PhantomData<Rc<()>>,
}

impl HostChild {
    /// # Safety
    /// The caller owns a live child HWND on this thread and keeps it alive
    /// until the renderer holding this object finishes teardown.
    pub unsafe fn from_hwnd(hwnd: NonZeroIsize) -> Self {
        Self {
            hwnd,
            thread: PhantomData,
        }
    }
}

impl HasWindowHandle for HostChild {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(self.hwnd));
        // SAFETY: from_hwnd requires the host to retain its child through teardown.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}
