//! CPU tests for the real ownership/control flow, using only the queried Vulkan
//! entry points below. These functions are not a driver or a GPU test substitute.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::sync::{Arc, Weak};

use ash::vk::Handle;

use super::{Diagnostics, Entry, InitError, VALIDATION_LAYER, ValidationMode, VulkanInstance, vk};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    CreateInstance,
    CreateMessenger,
    DestroyMessenger,
    DestroyInstance,
}

#[derive(Default)]
struct Plan {
    fail_instance: bool,
    fail_messenger: bool,
    missing_proc: Option<&'static CStr>,
    device_lookup: Option<vk::PFN_vkGetInstanceProcAddr>,
    on_instance_destroy: Option<fn()>,
}

#[derive(Default)]
struct FakeState {
    plan: Plan,
    events: Vec<Event>,
    instance_callback: vk::PFN_vkDebugUtilsMessengerCallbackEXT,
    user_data: *mut c_void,
    diagnostics: Weak<Diagnostics>,
    destruction_observer: Option<Arc<Diagnostics>>,
    callback_delivered: bool,
    sync_validation_requested: bool,
    layer_queries: usize,
    requested_layers: Vec<String>,
    requested_extensions: Vec<String>,
}

thread_local! {
    // Every Vulkan call made by these tests is synchronous on its test thread.
    // No callback is invoked while this RefCell is borrowed.
    static STATE: RefCell<FakeState> = RefCell::new(FakeState::default());
}

fn initialize(plan: Plan) -> Result<VulkanInstance, InitError> {
    initialize_with_validation(plan, ValidationMode::Required)
}

fn initialize_with_validation(
    plan: Plan,
    validation: ValidationMode,
) -> Result<VulkanInstance, InitError> {
    let dispatch = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    tracing::dispatcher::with_default(&dispatch, || {
        let diagnostics = Diagnostics::new();
        STATE.with(|state| {
            *state.borrow_mut() = FakeState {
                plan,
                diagnostics: Arc::downgrade(&diagnostics),
                ..FakeState::default()
            };
        });
        // SAFETY: The typed functions returned by get_instance_proc_addr below
        // are live for the test process and implement the operations used by
        // initialize and Drop, including the enumeration count/data protocol.
        // No returned handle is ever passed to a real Vulkan implementation.
        let entry = unsafe {
            Entry::from_static_fn(ash::StaticFn {
                get_instance_proc_addr,
            })
        };
        VulkanInstance::initialize_with_validation(entry, diagnostics, validation, || Ok(()))
    })
}

/// Reuse the Instance boundary for Device ownership tests without mocking its
/// physical-device discovery. Only the Device entry points are supplied there.
pub(in crate::backends::vulkan) fn instance_for_device_test(
    device_lookup: vk::PFN_vkGetInstanceProcAddr,
    on_instance_destroy: fn(),
) -> VulkanInstance {
    initialize(Plan {
        device_lookup: Some(device_lookup),
        on_instance_destroy: Some(on_instance_destroy),
        ..Plan::default()
    })
    .expect("fake Instance initialization succeeds")
}

#[test]
fn instance_creation_failure_does_not_destroy_an_uncreated_instance() {
    let result = initialize(Plan {
        fail_instance: true,
        ..Plan::default()
    });
    assert!(matches!(
        result,
        Err(InitError::Vulkan {
            operation: "vkCreateInstance",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })
    ));
    STATE.with(|state| {
        let state = state.borrow();
        assert_eq!(state.events, [Event::CreateInstance]);
        assert!(state.diagnostics.upgrade().is_none());
    });
}

#[test]
fn messenger_creation_failure_destroys_the_instance_once() {
    let result = initialize(Plan {
        fail_messenger: true,
        ..Plan::default()
    });
    assert!(matches!(
        result,
        Err(InitError::Vulkan {
            operation: "vkCreateDebugUtilsMessengerEXT",
            result: vk::Result::ERROR_OUT_OF_HOST_MEMORY,
        })
    ));
    assert_destroyed(&[
        Event::CreateInstance,
        Event::CreateMessenger,
        Event::DestroyInstance,
    ]);
}

#[test]
fn drop_destroys_children_first_and_keeps_instance_callback_data_alive() {
    let owner = initialize(Plan::default()).expect("fake initialization succeeds");
    STATE.with(|state| {
        assert_eq!(
            state.borrow().events,
            [Event::CreateInstance, Event::CreateMessenger]
        );
        assert!(state.borrow().sync_validation_requested);
        assert_eq!(
            state.borrow().requested_layers,
            ["VK_LAYER_KHRONOS_validation"]
        );
        assert_eq!(
            state.borrow().requested_extensions,
            ["VK_EXT_debug_utils", "VK_EXT_layer_settings"]
        );
    });
    drop(owner);
    assert_destroyed(&[
        Event::CreateInstance,
        Event::CreateMessenger,
        Event::DestroyMessenger,
        Event::DestroyInstance,
    ]);
}

#[test]
fn disabled_validation_does_not_query_or_request_validation_prerequisites() {
    assert_eq!(ValidationMode::default(), ValidationMode::Required);
    let owner = initialize_with_validation(Plan::default(), ValidationMode::Disabled)
        .expect("debug-utils-only initialization succeeds");
    STATE.with(|state| {
        let state = state.borrow();
        assert_eq!(state.layer_queries, 0);
        assert!(state.requested_layers.is_empty());
        assert_eq!(state.requested_extensions, ["VK_EXT_debug_utils"]);
        assert!(!state.sync_validation_requested);
    });
    drop(owner);
    assert_destroyed(&[
        Event::CreateInstance,
        Event::CreateMessenger,
        Event::DestroyMessenger,
        Event::DestroyInstance,
    ]);
}

#[test]
fn missing_debug_entry_points_destroy_the_instance_without_creating_a_messenger() {
    for missing in [
        c"vkCreateDebugUtilsMessengerEXT",
        c"vkDestroyDebugUtilsMessengerEXT",
        c"vkSubmitDebugUtilsMessageEXT",
    ] {
        let result = initialize(Plan {
            missing_proc: Some(missing),
            ..Plan::default()
        });
        assert!(matches!(
            result,
            Err(InitError::MissingEntryPoint(name)) if name == missing
        ));
        assert_destroyed(&[Event::CreateInstance, Event::DestroyInstance]);
    }
}

fn assert_destroyed(expected_events: &[Event]) {
    STATE.with(|state| {
        let state = state.borrow();
        assert_eq!(state.events, expected_events);
        assert!(state.callback_delivered);
        let snapshot = state
            .destruction_observer
            .as_ref()
            .expect("callback state was live during Instance destruction")
            .snapshot();
        assert!(snapshot.is_clean(), "{snapshot:?}");
        assert!(snapshot.messages.iter().any(|message| {
            message.message_id_name.as_deref() == Some("lifecycle-destruction")
                && message.message == "Instance destruction callback received"
        }));
    });
}

unsafe extern "system" fn get_instance_proc_addr(
    _instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    // SAFETY: ash queries entry points using live NUL-terminated names.
    let name = unsafe { CStr::from_ptr(name) };
    if STATE.with(|state| state.borrow().plan.missing_proc == Some(name)) {
        return None;
    }
    macro_rules! erase {
        ($function:ident, $signature:ty) => {{
            let typed: $signature = $function;
            // SAFETY: Vulkan's lookup protocol erases function signatures. ash
            // restores the exact signature associated with each matched name.
            Some(unsafe { std::mem::transmute::<$signature, unsafe extern "system" fn()>(typed) })
        }};
    }
    match name.to_bytes() {
        b"vkCreateInstance" => erase!(create_instance, vk::PFN_vkCreateInstance),
        b"vkEnumerateInstanceVersion" => {
            erase!(
                enumerate_instance_version,
                vk::PFN_vkEnumerateInstanceVersion
            )
        }
        b"vkEnumerateInstanceLayerProperties" => erase!(
            enumerate_instance_layer_properties,
            vk::PFN_vkEnumerateInstanceLayerProperties
        ),
        b"vkEnumerateInstanceExtensionProperties" => erase!(
            enumerate_instance_extension_properties,
            vk::PFN_vkEnumerateInstanceExtensionProperties
        ),
        b"vkDestroyInstance" => erase!(destroy_instance, vk::PFN_vkDestroyInstance),
        b"vkCreateDebugUtilsMessengerEXT" => {
            erase!(create_messenger, vk::PFN_vkCreateDebugUtilsMessengerEXT)
        }
        b"vkDestroyDebugUtilsMessengerEXT" => {
            erase!(destroy_messenger, vk::PFN_vkDestroyDebugUtilsMessengerEXT)
        }
        b"vkSubmitDebugUtilsMessageEXT" => {
            erase!(submit_message, vk::PFN_vkSubmitDebugUtilsMessageEXT)
        }
        // ash loads additional optional tables, but these tests never call them.
        _ => {
            let lookup = STATE.with(|state| state.borrow().plan.device_lookup);
            // SAFETY: The test supplies typed, process-lifetime functions using
            // the same Vulkan lookup protocol and the live name from ash.
            lookup.and_then(|lookup| unsafe { lookup(_instance, name.as_ptr()) })
        }
    }
}

unsafe extern "system" fn enumerate_instance_version(version: *mut u32) -> vk::Result {
    // SAFETY: ash supplies a live output slot for this query.
    unsafe { version.write(vk::API_VERSION_1_3) };
    vk::Result::SUCCESS
}

unsafe extern "system" fn enumerate_instance_layer_properties(
    count: *mut u32,
    properties: *mut vk::LayerProperties,
) -> vk::Result {
    STATE.with(|state| state.borrow_mut().layer_queries += 1);
    let mut property = vk::LayerProperties {
        spec_version: vk::API_VERSION_1_3,
        implementation_version: 1,
        ..vk::LayerProperties::default()
    };
    for (output, byte) in property
        .layer_name
        .iter_mut()
        .zip(VALIDATION_LAYER.to_bytes_with_nul())
    {
        *output = *byte as c_char;
    }
    // SAFETY: ash supplies the count slot and, on its second enumeration call,
    // storage for the advertised property. The helper respects that capacity.
    unsafe { enumerate_one(count, properties, property) }
}

unsafe extern "system" fn enumerate_instance_extension_properties(
    layer: *const c_char,
    count: *mut u32,
    properties: *mut vk::ExtensionProperties,
) -> vk::Result {
    // Supply layer_settings only through the layer query, exercising discovery
    // of a required extension that is absent from the global extension list.
    let name = if layer.is_null() {
        ash::ext::debug_utils::NAME
    } else {
        ash::ext::layer_settings::NAME
    };
    let mut property = vk::ExtensionProperties::default();
    for (output, byte) in property
        .extension_name
        .iter_mut()
        .zip(name.to_bytes_with_nul())
    {
        *output = *byte as c_char;
    }
    // SAFETY: Same count/data enumeration protocol as the layer query above.
    unsafe { enumerate_one(count, properties, property) }
}

unsafe fn enumerate_one<T>(count: *mut u32, properties: *mut T, property: T) -> vk::Result {
    // SAFETY: The caller provides a live count and either no data storage or
    // storage of that capacity. Only one element is ever advertised or written.
    unsafe {
        if properties.is_null() {
            count.write(1);
            return vk::Result::SUCCESS;
        }
        let capacity = count.read();
        count.write(1);
        if capacity == 0 {
            count.write(0);
            return vk::Result::INCOMPLETE;
        }
        properties.write(property);
    }
    vk::Result::SUCCESS
}

unsafe extern "system" fn create_instance(
    create_info: *const vk::InstanceCreateInfo<'_>,
    _allocator: *const vk::AllocationCallbacks<'_>,
    instance: *mut vk::Instance,
) -> vk::Result {
    // SAFETY: The real constructor retains both name arrays and their strings
    // through this call; counts are zero or match their initialized storage.
    let (layers, extensions) = unsafe {
        let info = &*create_info;
        let names = |count, pointers| {
            if count == 0 {
                return Vec::new();
            }
            std::slice::from_raw_parts(pointers, count as usize)
                .iter()
                .map(|&name| CStr::from_ptr(name).to_string_lossy().into_owned())
                .collect()
        };
        (
            names(info.enabled_layer_count, info.pp_enabled_layer_names),
            names(
                info.enabled_extension_count,
                info.pp_enabled_extension_names,
            ),
        )
    };
    let fail = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.events.push(Event::CreateInstance);
        state.requested_layers = layers;
        state.requested_extensions = extensions;
        state.plan.fail_instance
    });
    if fail {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    // SAFETY: initialize supplies live InstanceCreateInfo and a valid pNext chain
    // for this call. Only the callback function and stable user_data are retained.
    let mut next = unsafe { (*create_info).p_next.cast::<vk::BaseInStructure<'_>>() };
    while !next.is_null() {
        // SAFETY: Each pNext element is live for the create call and starts with
        // the common sType/pNext header used by BaseInStructure.
        let header = unsafe { &*next };
        if header.s_type == vk::StructureType::DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT {
            // SAFETY: sType identifies this element's concrete Vulkan structure.
            let callback = unsafe { &*next.cast::<vk::DebugUtilsMessengerCreateInfoEXT<'_>>() };
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.instance_callback = callback.pfn_user_callback;
                state.user_data = callback.p_user_data;
            });
        }
        if header.s_type == vk::StructureType::LAYER_SETTINGS_CREATE_INFO_EXT {
            // SAFETY: The real constructor supplies this concrete structure and
            // its live setting array, names, and correctly aligned BOOL32 value.
            let settings = unsafe { &*next.cast::<vk::LayerSettingsCreateInfoEXT<'_>>() };
            // SAFETY: The setting and all referenced values are supplied by the
            // real constructor and remain live for this synchronous call.
            let requested = settings.setting_count == 1
                && unsafe {
                    let setting = &*settings.p_settings;
                    CStr::from_ptr(setting.p_layer_name) == VALIDATION_LAYER
                        && CStr::from_ptr(setting.p_setting_name) == c"validate_sync"
                        && setting.ty == vk::LayerSettingTypeEXT::BOOL32
                        && setting.value_count == 1
                        && *setting.p_values.cast::<vk::Bool32>() == vk::TRUE
                };
            STATE.with(|state| state.borrow_mut().sync_validation_requested = requested);
        }
        next = header.p_next;
    }
    // SAFETY: ash supplies an output slot; this opaque handle is used only by
    // these test function pointers and is never sent to a real driver.
    unsafe { instance.write(vk::Instance::from_raw(1)) };
    vk::Result::SUCCESS
}

unsafe extern "system" fn create_messenger(
    _instance: vk::Instance,
    _create_info: *const vk::DebugUtilsMessengerCreateInfoEXT<'_>,
    _allocator: *const vk::AllocationCallbacks<'_>,
    messenger: *mut vk::DebugUtilsMessengerEXT,
) -> vk::Result {
    let fail = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.events.push(Event::CreateMessenger);
        state.plan.fail_messenger
    });
    if fail {
        return vk::Result::ERROR_OUT_OF_HOST_MEMORY;
    }
    // SAFETY: ash supplies a live output slot; this opaque handle stays confined
    // to the test entry points just like the fake Instance handle.
    unsafe { messenger.write(vk::DebugUtilsMessengerEXT::from_raw(2)) };
    vk::Result::SUCCESS
}

unsafe extern "system" fn destroy_messenger(
    _instance: vk::Instance,
    _messenger: vk::DebugUtilsMessengerEXT,
    _allocator: *const vk::AllocationCallbacks<'_>,
) {
    STATE.with(|state| state.borrow_mut().events.push(Event::DestroyMessenger));
}

unsafe extern "system" fn destroy_instance(
    _instance: vk::Instance,
    _allocator: *const vk::AllocationCallbacks<'_>,
) {
    let (callback, user_data, diagnostics) = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.events.push(Event::DestroyInstance);
        (
            state.instance_callback,
            state.user_data,
            state.diagnostics.upgrade(),
        )
    });
    // Verify lifetime without dereferencing potentially dead callback state if
    // ownership regresses. Assertions are made after returning from the C hook.
    if let (Some(callback), Some(diagnostics)) = (callback, diagnostics)
        && user_data == Arc::as_ptr(&diagnostics).cast_mut().cast()
    {
        let data = vk::DebugUtilsMessengerCallbackDataEXT::default()
            .message_id_name(c"lifecycle-destruction")
            .message(c"Instance destruction callback received");
        // SAFETY: The original callback's Arc is proven live above; the callback
        // data and its static strings remain live through this synchronous call.
        let result = unsafe {
            callback(
                vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
                vk::DebugUtilsMessageTypeFlagsEXT::GENERAL,
                &data,
                user_data,
            )
        };
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.callback_delivered = result == vk::FALSE;
            state.destruction_observer = Some(diagnostics);
        });
    }
    let observer = STATE.with(|state| state.borrow().plan.on_instance_destroy);
    if let Some(observer) = observer {
        observer();
    }
}

unsafe extern "system" fn submit_message(
    _instance: vk::Instance,
    _severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    _data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
) {
}
