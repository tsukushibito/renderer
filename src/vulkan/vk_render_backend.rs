use std::{
    ffi::{c_char, CStr, CString},
    rc::Rc,
};

use crate::{RenderBackend, Result};
use ash::{
    self,
    extensions::{ext, khr},
    vk, Entry, Instance,
};
use ash_window;
use raw_window_handle::RawDisplayHandle;

use super::vk_physical_device::VkPhysicalDevice;

pub struct VkRenderBackend {
    pub(crate) entry: Entry,
    pub(crate) instance: Instance,
    pub(crate) surface_loader: khr::Surface,
    pub(crate) physical_devices: Vec<Rc<VkPhysicalDevice>>,
}

impl VkRenderBackend {
    pub fn new(display_handle: &RawDisplayHandle) -> Result<Rc<Self>> {
        let entry = unsafe { Entry::load()? };
        let instance = create_instance(&entry, display_handle)?;
        let surface_loader = khr::Surface::new(&entry, &instance);
        let physical_devices = unsafe { instance.enumerate_physical_devices()? };
        let physical_devices = physical_devices
            .iter()
            .map(|pdev| VkPhysicalDevice::new(&instance, *pdev))
            .collect::<Vec<Rc<VkPhysicalDevice>>>();
        if physical_devices.is_empty() {
            return Err("No Vulkan-compatible devices found".into());
        }

        let output = Rc::new(Self {
            entry,
            instance,
            surface_loader,
            physical_devices,
        });
        Ok(output)
    }
}

impl Drop for VkRenderBackend {
    fn drop(&mut self) {
        unsafe { self.instance.destroy_instance(None) };
    }
}

impl RenderBackend for VkRenderBackend {
    type PhysDev = VkPhysicalDevice;

    fn physical_devices(&self) -> &Vec<Rc<Self::PhysDev>> {
        &self.physical_devices
    }
}

fn create_instance(entry: &Entry, display_handle: &RawDisplayHandle) -> Result<Instance> {
    let app_name = CString::new("tempura")?;
    let engine_name = CString::new("tempura")?;

    let app_info = vk::ApplicationInfo::builder()
        .application_name(&app_name)
        .application_version(vk::make_api_version(0, 0, 1, 0))
        .engine_name(&engine_name)
        .engine_version(vk::make_api_version(0, 0, 1, 0))
        .api_version(vk::make_api_version(0, 1, 3, 0));

    let mut layer_properties = entry.enumerate_instance_layer_properties()?;
    layer_properties.retain(|&prop| {
        let name = prop
            .layer_name
            .iter()
            .map(|&c| c as u8)
            .collect::<Vec<u8>>();
        !std::str::from_utf8(&name).unwrap().contains("VK_LAYER_EOS")
    });

    #[cfg(not(feature = "debug"))]
    {
        layer_properties.retain(|&prop| {
            let name = prop
                .layer_name
                .iter()
                .map(|&c| c as u8)
                .collect::<Vec<u8>>();
            !std::str::from_utf8(&name)
                .unwrap()
                .contains("VK_LAYER_LUNARG_api_dump")
        });
    }

    let layer_names = layer_properties
        .iter()
        .filter_map(|p| {
            if vk::api_version_major(p.spec_version) == 1
                && vk::api_version_minor(p.spec_version) == 3
            {
                Some(p.layer_name.as_ptr())
            } else {
                None
            }
        })
        .collect::<Vec<*const c_char>>();

    for &c_str_ptr in &layer_names {
        if !c_str_ptr.is_null() {
            let c_str = unsafe { CStr::from_ptr(c_str_ptr) };

            if let Ok(str_slice) = c_str.to_str() {
                println!("Layer Name: {}", str_slice);
            }
        }
    }

    let mut extension_names = ash_window::enumerate_required_extensions(*display_handle)?.to_vec();
    extension_names.push(ext::DebugUtils::name().as_ptr());

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        extension_names.push(vk::KhrPortabilityEnumerationFn::name().as_ptr());
        // Enabling this extension is a requirement when using `VK_KHR_portability_subset`
        extension_names.push(vk::KhrGetPhysicalDeviceProperties2Fn::name().as_ptr());
    }

    let create_flags = if cfg!(any(target_os = "macos", target_os = "ios")) {
        vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR
    } else {
        vk::InstanceCreateFlags::default()
    };

    let create_info = vk::InstanceCreateInfo::builder()
        .application_info(&app_info)
        .enabled_extension_names(&extension_names)
        .flags(create_flags);

    let create_info = if cfg!(any(feature = "develop", feature = "debug")) {
        create_info.enabled_layer_names(&layer_names)
    } else {
        create_info
    };

    let instance = unsafe { entry.create_instance(&create_info, None)? };

    Ok(instance)
}
