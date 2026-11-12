//! Narrow CPU boundary tests: real Device creation/ownership code with limited
//! typed Vulkan entry points. Capability queries are tested separately.

use std::cell::RefCell;
use std::ffi::{CStr, c_char};

use ash::vk::Handle;

use super::*;
use crate::backends::vulkan::instance::lifecycle_tests::instance_for_device_test;

#[derive(Debug, PartialEq, Eq)]
enum Event {
    CreateDevice,
    DestroyDevice,
    DestroyInstance,
}

#[derive(Default)]
struct State {
    fail_create: bool,
    events: Vec<Event>,
    create_request: Option<(vk::PhysicalDevice, u32, u32)>,
    queue_request: Option<(u32, u32)>,
    fail_queue_name: bool,
    queue_names: Vec<(vk::Queue, String)>,
    debug_lookups: usize,
    property_queries: usize,
    max_draw_indexed_index_value: u32,
}

thread_local! {
    // The fake functions run synchronously on this test's thread, and no real
    // Vulkan calls receive the opaque handles produced here.
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn selected() -> SelectedDevice {
    SelectedDevice {
        physical_device: vk::PhysicalDevice::from_raw(3),
        info: AdapterInfo {
            name: "ownership test device".to_owned(),
            device_type: vk::PhysicalDeviceType::DISCRETE_GPU,
            api_version: REQUIRED_API,
            vendor_id: 0,
            device_id: 0,
            driver_version: 0,
            driver: None,
        },
        queue_family_index: 7,
    }
}

fn instance(fail_create: bool) -> VulkanInstance {
    STATE.with(|state| {
        *state.borrow_mut() = State {
            fail_create,
            max_draw_indexed_index_value: u32::MAX,
            ..State::default()
        };
    });
    instance_for_device_test(get_instance_proc_addr, || {
        STATE.with(|state| state.borrow_mut().events.push(Event::DestroyInstance));
    })
}

#[test]
fn successful_device_uses_selected_queue_and_destroys_before_instance() {
    let owner = VulkanDevice::create_selected(instance(false), selected())
        .expect("fake Device creation succeeds");
    STATE.with(|state| {
        let state = state.borrow();
        assert_eq!(state.events, [Event::CreateDevice]);
        assert_eq!(
            state.create_request,
            Some((vk::PhysicalDevice::from_raw(3), 7, 1))
        );
        assert_eq!(state.queue_request, Some((7, 0)));
    });
    assert_eq!(owner.queue, vk::Queue::from_raw(5));
    assert_eq!(owner.max_buffer_size(), 1024);
    assert_eq!(owner.max_draw_indexed_index_value(), u32::MAX);
    STATE.with(|state| assert_eq!(state.borrow().property_queries, 1));
    let lookups = STATE.with(|state| {
        let state = state.borrow();
        assert_eq!(
            state.queue_names,
            [(
                vk::Queue::from_raw(5),
                "renderer.graphics-compute.queue".to_owned()
            )]
        );
        state.debug_lookups
    });
    assert!(lookups > 0, "initial creation loads the debug dispatch");
    assert!(std::ptr::eq(owner.debug_utils(), owner.debug_utils()));
    STATE.with(|state| assert_eq!(state.borrow().debug_lookups, lookups));
    drop(owner);
    STATE.with(|state| {
        assert_eq!(
            state.borrow().events,
            [
                Event::CreateDevice,
                Event::DestroyDevice,
                Event::DestroyInstance
            ]
        );
    });
}

#[test]
fn indexed_draw_limit_preserves_the_reported_value_and_is_cached() {
    for limit in [0x00ff_ffff, u32::MAX] {
        let instance = instance(false);
        STATE.with(|state| state.borrow_mut().max_draw_indexed_index_value = limit);
        let owner = VulkanDevice::create_selected(instance, selected()).unwrap();
        assert_eq!(owner.max_draw_indexed_index_value(), limit);
        assert_eq!(owner.max_draw_indexed_index_value(), limit);
        STATE.with(|state| assert_eq!(state.borrow().property_queries, 1));
    }
}

#[test]
fn queue_naming_failure_retires_the_created_device_and_preserves_cause() {
    let instance = instance(false);
    STATE.with(|state| state.borrow_mut().fail_queue_name = true);
    let error = VulkanDevice::create_selected(instance, selected())
        .err()
        .expect("queue naming fails");
    assert!(matches!(
        error,
        DeviceInitError::Vulkan {
            operation: "vkSetDebugUtilsObjectNameEXT(queue)",
            result: vk::Result::ERROR_OUT_OF_HOST_MEMORY,
            ..
        }
    ));
    assert!(error.to_string().contains("ownership test device"));
    STATE.with(|state| {
        assert_eq!(
            state.borrow().events,
            [
                Event::CreateDevice,
                Event::DestroyDevice,
                Event::DestroyInstance
            ]
        );
    });
}

#[test]
fn terminal_device_state_is_shared_and_queue_use_stops() {
    let owner = VulkanDevice::create_selected(instance(false), selected()).unwrap();
    assert_eq!(owner.state(), DeviceState::Active);
    drop(owner.lock_queue().unwrap());
    owner.stop();
    assert_eq!(owner.ensure_active(), Err(DeviceState::Stopped));
    assert!(owner.lock_queue().is_err());
    owner.observe_result(vk::Result::ERROR_DEVICE_LOST);
    owner.stop();
    assert_eq!(owner.ensure_active(), Err(DeviceState::Lost));
    assert!(owner.lock_queue().is_err());
}

#[test]
fn rejected_submit_oom_is_retryable_but_unknown_submission_stops_future_use() {
    let owner = VulkanDevice::create_selected(instance(false), selected()).unwrap();
    for result in [
        vk::Result::ERROR_OUT_OF_HOST_MEMORY,
        vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
    ] {
        owner.observe_submit_result(result);
        assert_eq!(owner.state(), DeviceState::Active);
        drop(owner.lock_queue().unwrap());
    }
    owner.observe_submit_result(vk::Result::ERROR_UNKNOWN);
    assert_eq!(owner.state(), DeviceState::Stopped);
    assert!(owner.lock_queue().is_err());
    // A subsequent completion success does not reactivate old Graph state.
    owner.observe_result(vk::Result::SUCCESS);
    assert_eq!(owner.state(), DeviceState::Stopped);
    owner.observe_submit_result(vk::Result::ERROR_DEVICE_LOST);
    assert_eq!(owner.state(), DeviceState::Lost);
}

#[test]
fn failed_creation_preserves_cause_and_destroys_only_instance() {
    let error = match VulkanDevice::create_selected(instance(true), selected()) {
        Ok(_) => panic!("creation was configured to fail"),
        Err(error) => error,
    };
    let source = std::error::Error::source(&error).expect("Vulkan failure source");
    assert_eq!(
        source.downcast_ref::<vk::Result>(),
        Some(&vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
    );
    let message = error.to_string();
    assert!(message.contains("vkCreateDevice"), "{message}");
    assert!(message.contains("ownership test device"), "{message}");
    STATE.with(|state| {
        let state = state.borrow();
        assert_eq!(state.events, [Event::CreateDevice, Event::DestroyInstance]);
        assert!(state.queue_request.is_none());
    });
}

unsafe extern "system" fn get_instance_proc_addr(
    _instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    // SAFETY: ash supplies a live NUL-terminated lookup name.
    let name = unsafe { CStr::from_ptr(name) };
    match name.to_bytes() {
        b"vkGetPhysicalDeviceProperties2" => {
            let function: vk::PFN_vkGetPhysicalDeviceProperties2 = get_physical_device_properties2;
            // SAFETY: ash restores the exact property-query signature.
            Some(unsafe {
                std::mem::transmute::<
                    vk::PFN_vkGetPhysicalDeviceProperties2,
                    unsafe extern "system" fn(),
                >(function)
            })
        }
        b"vkCreateDevice" => {
            let function: vk::PFN_vkCreateDevice = create_device;
            // SAFETY: Vulkan lookup erases signatures; ash restores this exact
            // signature for vkCreateDevice.
            Some(unsafe {
                std::mem::transmute::<vk::PFN_vkCreateDevice, unsafe extern "system" fn()>(function)
            })
        }
        b"vkGetDeviceProcAddr" => {
            let function: vk::PFN_vkGetDeviceProcAddr = get_device_proc_addr;
            // SAFETY: Same Vulkan lookup protocol for vkGetDeviceProcAddr.
            Some(unsafe {
                std::mem::transmute::<vk::PFN_vkGetDeviceProcAddr, unsafe extern "system" fn()>(
                    function,
                )
            })
        }
        _ => None,
    }
}

unsafe extern "system" fn get_physical_device_properties2(
    _physical_device: vk::PhysicalDevice,
    properties: *mut vk::PhysicalDeviceProperties2<'_>,
) {
    let index_limit = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.property_queries += 1;
        state.max_draw_indexed_index_value
    });
    // SAFETY: The real Device constructor passes a live Properties2 output
    // chained to its sole Maintenance4Properties output. This narrow boundary
    // supplies only the cached buffer and indexed-draw limits needed here.
    unsafe {
        (*properties).properties.limits.max_draw_indexed_index_value = index_limit;
    }
    // SAFETY: The constructor's sole pNext entry is the live, uniquely borrowed
    // Maintenance4Properties output described above.
    let maintenance4 = unsafe {
        &mut *(*properties)
            .p_next
            .cast::<vk::PhysicalDeviceMaintenance4Properties<'_>>()
    };
    maintenance4.max_buffer_size = 1024;
}

unsafe extern "system" fn create_device(
    physical_device: vk::PhysicalDevice,
    create_info: *const vk::DeviceCreateInfo<'_>,
    _allocator: *const vk::AllocationCallbacks<'_>,
    device: *mut vk::Device,
) -> vk::Result {
    // SAFETY: The real constructor keeps its create info and queue array live
    // through this call. The API contract requires at least one queue request.
    let queue = unsafe { &*(*create_info).p_queue_create_infos };
    let fail = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.events.push(Event::CreateDevice);
        state.create_request = Some((physical_device, queue.queue_family_index, queue.queue_count));
        state.fail_create
    });
    if fail {
        return vk::Result::ERROR_OUT_OF_DEVICE_MEMORY;
    }
    // SAFETY: ash supplies the output slot; this opaque test handle is never
    // passed to a real driver and is destroyed only via the test function below.
    unsafe { device.write(vk::Device::from_raw(4)) };
    vk::Result::SUCCESS
}

unsafe extern "system" fn get_device_proc_addr(
    _device: vk::Device,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    // SAFETY: ash supplies a live NUL-terminated lookup name.
    let name = unsafe { CStr::from_ptr(name) };
    if name
        .to_bytes()
        .windows(b"DebugUtils".len())
        .any(|part| part == b"DebugUtils")
    {
        STATE.with(|state| state.borrow_mut().debug_lookups += 1);
    }
    match name.to_bytes() {
        b"vkDestroyDevice" => {
            let function: vk::PFN_vkDestroyDevice = destroy_device;
            // SAFETY: ash restores the exact vkDestroyDevice signature.
            Some(unsafe {
                std::mem::transmute::<vk::PFN_vkDestroyDevice, unsafe extern "system" fn()>(
                    function,
                )
            })
        }
        b"vkGetDeviceQueue" => {
            let function: vk::PFN_vkGetDeviceQueue = get_device_queue;
            // SAFETY: ash restores the exact vkGetDeviceQueue signature.
            Some(unsafe {
                std::mem::transmute::<vk::PFN_vkGetDeviceQueue, unsafe extern "system" fn()>(
                    function,
                )
            })
        }
        b"vkSetDebugUtilsObjectNameEXT" => {
            let function: vk::PFN_vkSetDebugUtilsObjectNameEXT = set_object_name;
            // SAFETY: ash restores the exact vkSetDebugUtilsObjectNameEXT signature.
            Some(unsafe {
                std::mem::transmute::<
                    vk::PFN_vkSetDebugUtilsObjectNameEXT,
                    unsafe extern "system" fn(),
                >(function)
            })
        }
        _ => None,
    }
}

unsafe extern "system" fn set_object_name(
    _device: vk::Device,
    info: *const vk::DebugUtilsObjectNameInfoEXT<'_>,
) -> vk::Result {
    // SAFETY: The real constructor passes a live info and static C-string name.
    let info = unsafe { &*info };
    // SAFETY: The constructor retains the static NUL-terminated name for the call.
    let name = unsafe { CStr::from_ptr(info.p_object_name) }
        .to_string_lossy()
        .into_owned();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state
            .queue_names
            .push((vk::Queue::from_raw(info.object_handle), name));
        if state.fail_queue_name {
            vk::Result::ERROR_OUT_OF_HOST_MEMORY
        } else {
            vk::Result::SUCCESS
        }
    })
}

unsafe extern "system" fn destroy_device(
    _device: vk::Device,
    _allocator: *const vk::AllocationCallbacks<'_>,
) {
    STATE.with(|state| state.borrow_mut().events.push(Event::DestroyDevice));
}

unsafe extern "system" fn get_device_queue(
    _device: vk::Device,
    family: u32,
    index: u32,
    queue: *mut vk::Queue,
) {
    STATE.with(|state| state.borrow_mut().queue_request = Some((family, index)));
    // SAFETY: ash supplies a live output slot. The test's opaque queue is never
    // submitted to or independently destroyed.
    unsafe { queue.write(vk::Queue::from_raw(5)) };
}
