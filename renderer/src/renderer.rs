use ash::extensions::{
    ext::DebugUtils,
    khr::{Surface, Swapchain},
};
use ash::version::EntryV1_0;
use ash::{vk, Device, Entry, EntryCustom, Instance};
use raw_window_handle::{HasRawWindowHandle, RawWindowHandle};
use std::borrow::Cow;
use std::ffi::CStr;
use std::ffi::CString;

pub struct Renderer {
    pub entry: Entry,
    pub instance: Instance,
    pub debug_utils_loader: DebugUtils,
    pub debug_call_back: vk::DebugUtilsMessengerEXT,
    /*
    pub device: Device,
    pub surface_loader: Surface,
    pub swapchain_loader: Swapchain,

    pub pdevice: vk::PhysicalDevice,
    pub device_memory_properties: vk::PhysicalDeviceMemoryProperties,
    pub queue_family_index: u32,
    pub present_queue: vk::Queue,

    pub surface: vk::SurfaceKHR,
    pub surface_format: vk::SurfaceFormatKHR,
    pub surface_resolution: vk::Extent2D,

    pub swapchain: vk::SwapchainKHR,
    pub present_images: Vec<vk::Image>,
    pub present_image_views: Vec<vk::ImageView>,

    pub pool: vk::CommandPool,
    pub draw_command_buffer: vk::CommandBuffer,
    pub setup_command_buffer: vk::CommandBuffer,

    pub depth_image: vk::Image,
    pub depth_image_view: vk::ImageView,
    pub depth_image_memory: vk::DeviceMemory,

    pub present_complete_semaphore: vk::Semaphore,
    pub rendering_complete_semaphore: vk::Semaphore,

    pub draw_commands_reuse_fence: vk::Fence,
    pub setup_commands_reuse_fence: vk::Fence,
    */
}

impl Renderer {
    pub fn new(window: &dyn HasRawWindowHandle) -> Self {
        unsafe {
            let entry = Entry::new().unwrap();
            let instance = create_instance(&entry, "Renderer", window);
            let debug_utils_loader = DebugUtils::new(&entry, &instance);
            let debug_call_back = create_debug_call_back(&debug_utils_loader);

            Renderer {
                entry,
                instance,
                debug_utils_loader,
                debug_call_back,
            }
        }
    }
}

/// vk::Instanceを作成
fn create_instance(entry: &Entry, app_name: &str, window: &dyn HasRawWindowHandle) -> Instance {
    let app_name = CString::new(app_name).unwrap();
    let layer_names = [CString::new("VK_LAYER_KHRONOS_validation").unwrap()];
    let layer_names: Vec<*const i8> = layer_names
        .iter()
        .map(|raw_name| raw_name.as_ptr())
        .collect();
    let surface_extensions = ash_window::enumerate_required_extensions(window).unwrap();
    let mut extensions = surface_extensions
        .iter()
        .map(|ext| ext.as_ptr())
        .collect::<Vec<_>>();
    extensions.push(DebugUtils::name().as_ptr());

    let appinfo = vk::ApplicationInfo::builder()
        .application_name(&app_name)
        .application_version(0)
        .engine_name(&app_name)
        .engine_version(0)
        .api_version(vk::API_VERSION_1_2);

    let create_info = vk::InstanceCreateInfo::builder()
        .application_info(&appinfo)
        .enabled_layer_names(&layer_names)
        .enabled_extension_names(&extensions);

    unsafe {
        entry
            .create_instance(&create_info, None)
            .expect("Instance creation error")
    }
}

/// DebugUtilsMessengerEXTを作成
fn create_debug_call_back(debug_utils_loader: &DebugUtils) -> vk::DebugUtilsMessengerEXT {
    let debug_info = vk::DebugUtilsMessengerCreateInfoEXT::builder()
        .message_severity(
            vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
        )
        .message_type(vk::DebugUtilsMessageTypeFlagsEXT::all())
        .pfn_user_callback(Some(vulkan_debug_callback));

    unsafe {
        debug_utils_loader
            .create_debug_utils_messenger(&debug_info, None)
            .unwrap()
    }
}

/// デバッグコールバック
unsafe extern "system" fn vulkan_debug_callback(
    message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_type: vk::DebugUtilsMessageTypeFlagsEXT,
    p_callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT,
    _user_data: *mut std::os::raw::c_void,
) -> vk::Bool32 {
    let callback_data = *p_callback_data;
    let message_id_number: i32 = callback_data.message_id_number as i32;

    let message_id_name = if callback_data.p_message_id_name.is_null() {
        Cow::from("")
    } else {
        CStr::from_ptr(callback_data.p_message_id_name).to_string_lossy()
    };

    let message = if callback_data.p_message.is_null() {
        Cow::from("")
    } else {
        CStr::from_ptr(callback_data.p_message).to_string_lossy()
    };

    println!(
        "{:?}:\n{:?} [{} ({})] : {}\n",
        message_severity,
        message_type,
        message_id_name,
        &message_id_number.to_string(),
        message,
    );

    vk::FALSE
}
