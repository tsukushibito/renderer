//! A Win32 window and its host-provided ownership lease.

use raw_window_handle::{HasWindowHandle, RawWindowHandle, Win32WindowHandle};
use std::{io, num::NonZeroIsize};
use windows_sys::Win32::{
    Foundation::RECT,
    UI::WindowsAndMessaging::{GWLP_HINSTANCE, GetClientRect, GetWindowLongPtrW},
};

pub(crate) struct HostWindow {
    _owner: Box<dyn HasWindowHandle>,
    pub(crate) handle: Win32WindowHandle,
    pub(crate) extent: (u32, u32),
}

impl HostWindow {
    pub(crate) fn new(owner: impl HasWindowHandle + 'static) -> io::Result<Self> {
        // The pointee remains in place until native teardown releases its final lease.
        let owner = Box::new(owner);
        let RawWindowHandle::Win32(mut handle) =
            owner.window_handle().map_err(io::Error::other)?.as_raw()
        else {
            return Err(io::Error::other("presentation requires a Win32 window"));
        };
        let hwnd = handle.hwnd.get() as _;
        let mut client = RECT::default();
        // SAFETY: The retained owner supplies a live HWND. The output is writable.
        if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if handle.hinstance.is_none() {
            // SAFETY: This index reads the instance associated with the retained native window.
            handle.hinstance =
                NonZeroIsize::new(unsafe { GetWindowLongPtrW(hwnd, GWLP_HINSTANCE) } as isize);
        }
        // Win32 defines client.left and client.top as zero.
        let extent = (client.right.max(0) as u32, client.bottom.max(0) as u32);
        Ok(Self {
            _owner: owner,
            handle,
            extent,
        })
    }
}
