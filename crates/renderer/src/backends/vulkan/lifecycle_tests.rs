//! CPU tests for the real ownership/control flow, using only the queried Vulkan
//! entry points below. These functions are not a driver or a GPU test substitute.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::sync::{Arc, Weak};

use ash::vk::Handle;

use super::{Diagnostics, Entry, InitError, VALIDATION_LAYER, VulkanInstance, vk};

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
}

thread_local! {
    // Every Vulkan call made by these tests is synchronous on its test thread.
    // No callback is invoked while this RefCell is borrowed.
    static STATE: RefCell<FakeState> = RefCell::new(FakeState::default());
}

fn initialize(plan: Plan) -> Result<VulkanInstance, InitError> {
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
        VulkanInstance::initialize(entry, diagnostics, || Ok(()))
    })
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
        _ => None,
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
    _layer: *const c_char,
    count: *mut u32,
    properties: *mut vk::ExtensionProperties,
) -> vk::Result {
    let mut property = vk::ExtensionProperties::default();
    for (output, byte) in property
        .extension_name
        .iter_mut()
        .zip(ash::ext::debug_utils::NAME.to_bytes_with_nul())
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
    let fail = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.events.push(Event::CreateInstance);
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
}

unsafe extern "system" fn submit_message(
    _instance: vk::Instance,
    _severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    _data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
) {
}
