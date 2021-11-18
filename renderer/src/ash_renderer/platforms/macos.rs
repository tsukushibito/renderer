use ash::extensions::ext::DebugUtils;
use ash::extensions::khr::{GetPhysicalDeviceProperties2, Surface};
use ash::vk;
use ash::{Entry, Instance};
use std::os::raw::c_void;

#[cfg(target_os = "macos")]
use ash::extensions::mvk::MacOSSurface;

#[cfg(target_os = "macos")]
use cocoa::appkit::{NSView, NSWindow};
#[cfg(target_os = "macos")]
use cocoa::base::id as cocoa_id;
#[cfg(target_os = "macos")]
use objc::runtime::YES;
#[cfg(target_os = "macos")]
extern crate metal;
#[cfg(target_os = "macos")]
extern crate objc;

#[cfg(target_os = "macos")]
pub fn required_instance_extension_names() -> Vec<String> {
    vec![
        Surface::name().to_str().unwrap().to_string(),
        MacOSSurface::name().to_str().unwrap().to_string(),
        DebugUtils::name().to_str().unwrap().to_string(),
        GetPhysicalDeviceProperties2::name()
            .to_str()
            .unwrap()
            .to_string(),
    ]
}

#[cfg(target_os = "macos")]
pub unsafe fn create_surface(
    entry: &Entry,
    instance: &Instance,
    ns_view: *mut c_void,
) -> Result<vk::SurfaceKHR, vk::Result> {
    use std::ptr;

    let create_info = vk::MacOSSurfaceCreateInfoMVK {
        s_type: vk::StructureType::MACOS_SURFACE_CREATE_INFO_MVK,
        p_next: ptr::null(),
        flags: Default::default(),
        p_view: ns_view,
    };

    let macos_surface_loader = MacOSSurface::new(entry, instance);
    macos_surface_loader.create_mac_os_surface(&create_info, None)
}
