//! Explicit native prerequisites; no Graph execution or queue submission.

use std::ffi::{CStr, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use windows_core::Interface;

use super::bindings::*;

#[derive(Debug)]
struct Message {
    severity: i32,
    id: i32,
    text: String,
}

// The callback can run on another thread, including during final COM Release.
// Process-static storage avoids assuming an undocumented callback quiescence
// guarantee in order to free a context pointer. This single test runs in an
// isolated process for each repetition; callbacks never invoke D3D or logging.
static MESSAGES: Mutex<Vec<Message>> = Mutex::new(Vec::new());
static CALLBACK_FAILURES: AtomicUsize = AtomicUsize::new(0);
static DROPPED_MESSAGES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn receive_message(
    _category: D3D12_MESSAGE_CATEGORY,
    severity: D3D12_MESSAGE_SEVERITY,
    id: D3D12_MESSAGE_ID,
    description: windows_core::PCSTR,
    _context: *mut c_void,
) {
    let result = std::panic::catch_unwind(|| {
        // SAFETY: D3D12 supplies a valid NUL-terminated string for the duration
        // of this callback. Copy it without retaining the native pointer.
        let text = unsafe { CStr::from_ptr(description.as_ptr().cast()) }
            .to_string_lossy()
            .into_owned();
        let mut messages = MESSAGES.lock().unwrap_or_else(|error| error.into_inner());
        if messages.len() < 1024 {
            messages.push(Message { severity, id, text });
        } else {
            DROPPED_MESSAGES.fetch_add(1, Ordering::Relaxed);
        }
    });
    if let Err(payload) = result {
        CALLBACK_FAILURES.fetch_add(1, Ordering::Relaxed);
        // A panic payload can itself panic on Drop. Keep FFI infallible.
        std::mem::forget(payload);
    }
}

fn check_feature<T>(device: &ID3D12Device, feature: D3D12_FEATURE, data: &mut T) {
    // SAFETY: Each caller pairs the documented feature enum with its exact
    // initialized data structure. The synchronous call retains no pointers.
    unsafe {
        device.CheckFeatureSupport(
            feature,
            std::ptr::from_mut(data).cast(),
            size_of::<T>() as u32,
        )
    }
    .ok()
    .unwrap_or_else(|error| panic!("CheckFeatureSupport({feature}) failed: {error}"));
}

fn inspect_and_clear_filter(info: &ID3D12InfoQueue1) -> (u64, Vec<i32>) {
    // SAFETY: Live queue, correctly sized and aligned output storage. Returned
    // lists remain inside the owned storage until copied. No filter is assumed
    // safe without checking its actual categories, message IDs and severities.
    unsafe {
        let mut size = 0;
        info.GetStorageFilter(None, &mut size)
            .ok()
            .expect("storage filter size");
        assert!((size_of::<D3D12_INFO_QUEUE_FILTER>()..=1024 * 1024).contains(&size));
        let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
        let filter = storage.as_mut_ptr().cast::<D3D12_INFO_QUEUE_FILTER>();
        info.GetStorageFilter(Some(filter), &mut size)
            .ok()
            .expect("storage filter");
        let filter = &*filter;
        let denied = &filter.DenyList;
        let severities = if denied.NumSeverities == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(denied.pSeverityList, denied.NumSeverities as usize).to_vec()
        };
        let ids = if denied.NumIDs == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(denied.pIDList, denied.NumIDs as usize).to_vec()
        };
        eprintln!(
            "D3D12 initial filter: deny categories={} severities={severities:?} IDs={ids:?}; allow categories={} severities={} IDs={}",
            denied.NumCategories,
            filter.AllowList.NumCategories,
            filter.AllowList.NumSeverities,
            filter.AllowList.NumIDs
        );
        assert_eq!(
            (
                filter.AllowList.NumCategories,
                filter.AllowList.NumSeverities,
                filter.AllowList.NumIDs
            ),
            (0, 0, 0),
            "allow filter could restrict diagnostic delivery"
        );
        assert_eq!(
            (denied.NumCategories, denied.NumIDs),
            (0, 0),
            "category/ID filtering must be reviewed before accepting diagnostics"
        );
        assert!(
            severities
                .iter()
                .all(|s| *s == D3D12_MESSAGE_SEVERITY_INFO || *s == D3D12_MESSAGE_SEVERITY_MESSAGE),
            "warning/error filtering prohibited"
        );
        let count = info.GetNumMessagesDeniedByStorageFilter();
        info.ClearStorageFilter();
        info.ClearRetrievalFilter();
        (count, severities)
    }
}

fn probe_buffers(device: &ID3D12Device) -> Vec<Value> {
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: 64,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    };
    let mut reports = Vec::new();
    for (name, heap_type, state) in [
        (
            "default",
            D3D12_HEAP_TYPE_DEFAULT,
            D3D12_RESOURCE_STATE_COMMON,
        ),
        (
            "upload",
            D3D12_HEAP_TYPE_UPLOAD,
            D3D12_RESOURCE_STATE_GENERIC_READ,
        ),
        (
            "readback",
            D3D12_HEAP_TYPE_READBACK,
            D3D12_RESOURCE_STATE_COPY_DEST,
        ),
    ] {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: heap_type,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: Valid single-node buffer descriptor; mandatory upload/readback
        // initial states are explicit. The resulting COM reference owns the
        // implicit heap. No command lists or GPU accesses exist in this probe.
        let allocation = unsafe {
            device
                .CreateCommittedResource(
                    &heap,
                    D3D12_HEAP_FLAG_NONE,
                    &desc,
                    state,
                    None,
                    &mut resource,
                )
                .unwrap_or_else(|error| panic!("{name} CreateCommittedResource failed: {error}"));
            device.GetResourceAllocationInfo(0, &[desc])
        };
        assert_ne!(
            allocation.SizeInBytes,
            u64::MAX,
            "valid allocation size required"
        );
        assert!(allocation.SizeInBytes >= desc.Width && allocation.Alignment > 0);
        let resource = resource.expect("successful resource creation returns an owned reference");
        let mut actual_heap = D3D12_HEAP_PROPERTIES::default();
        // SAFETY: Owned live resource and valid outputs, no GPU users. CPU maps
        // never read uninitialized readback content or claim transfer success.
        unsafe {
            assert_eq!(resource.GetDesc().Width, desc.Width);
            resource
                .GetHeapProperties(Some(&mut actual_heap), None)
                .ok()
                .expect("GetHeapProperties");
            assert_eq!(actual_heap.Type, heap_type);
            if heap_type != D3D12_HEAP_TYPE_DEFAULT {
                let mut mapped = std::ptr::null_mut();
                let empty = D3D12_RANGE { Begin: 0, End: 0 };
                resource
                    .Map(0, Some(&empty), &mut mapped)
                    .ok()
                    .expect("Map CPU-visible probe buffer");
                assert!(!mapped.is_null());
                if heap_type == D3D12_HEAP_TYPE_UPLOAD {
                    std::ptr::write_bytes(mapped.cast::<u8>(), 0x5a, desc.Width as usize);
                    resource.Unmap(
                        0,
                        Some(&D3D12_RANGE {
                            Begin: 0,
                            End: desc.Width as usize,
                        }),
                    );
                } else {
                    resource.Unmap(0, Some(&empty));
                }
            }
        }
        reports.push(json!({"heap":name,"logical_bytes":desc.Width,"allocation_bytes":allocation.SizeInBytes,"alignment":allocation.Alignment}));
        drop(resource);
    }
    reports
}

fn dxgi_messages(queue: &IDXGIInfoQueue) -> Vec<Message> {
    // SAFETY: The global debug queue is retained after all Device/factory
    // references are released. No filters are installed by this probe.
    unsafe {
        assert_eq!(queue.GetNumMessagesDeniedByStorageFilter(DXGI_DEBUG_ALL), 0);
        assert_eq!(
            queue.GetNumMessagesDiscardedByMessageCountLimit(DXGI_DEBUG_ALL),
            0
        );
        let mut messages = Vec::new();
        for index in 0..queue.GetNumStoredMessages(DXGI_DEBUG_ALL) {
            let mut size = 0;
            queue
                .GetMessage(DXGI_DEBUG_ALL, index, None, &mut size)
                .ok()
                .expect("DXGI message size");
            assert!((size_of::<DXGI_INFO_QUEUE_MESSAGE>()..=1024 * 1024).contains(&size));
            let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
            let message = storage.as_mut_ptr().cast::<DXGI_INFO_QUEUE_MESSAGE>();
            queue
                .GetMessage(DXGI_DEBUG_ALL, index, Some(message), &mut size)
                .ok()
                .expect("DXGI message");
            let message = &*message;
            let text = CStr::from_ptr(message.pDescription)
                .to_string_lossy()
                .into_owned();
            messages.push(Message {
                severity: message.Severity,
                id: message.ID.0,
                text,
            });
        }
        messages
    }
}

#[test]
#[ignore = "explicit D3D12 hardware preparation; requires Debug Layer/GBV and Vulkan comparison when enabled"]
fn d3d12_preparation_hardware() {
    assert!(
        MESSAGES.lock().unwrap().is_empty(),
        "one preparation probe per process"
    );
    // SAFETY: Acquire a process-global debug queue before creating DXGI/D3D12
    // objects and retain it through their final Release, including failure.
    let global_queue: IDXGIInfoQueue =
        unsafe { DXGIGetDebugInterface1(0) }.expect("DXGI debug interface");
    let mut report;
    {
        let mut debug: Option<ID3D12Debug> = None;
        // SAFETY: Valid COM output; enable both modes before the first Device
        // creation. These void setters are call-order evidence, not getters.
        unsafe { D3D12GetDebugInterface(&mut debug) }.expect("D3D12 Debug Layer interface");
        let debug = debug.expect("Debug Layer reference");
        let debug1: ID3D12Debug1 = debug.cast().expect("GBV control interface");
        // SAFETY: No D3D12 device exists yet; both debug interfaces are live.
        unsafe {
            debug.EnableDebugLayer();
            debug1.SetEnableGPUBasedValidation(true);
        }
        // SAFETY: Debug flag is valid and the returned factory is owned.
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_DEBUG as u32) }
                .expect("debug DXGI factory");
        let mut selected = None;
        for index in 0.. {
            // SAFETY: Live factory; adapters are returned with an owned reference.
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => panic!("EnumAdapters1({index}): {error}"),
            };
            let mut desc = DXGI_ADAPTER_DESC1::default();
            // SAFETY: Live adapter and correctly sized output structure.
            unsafe { adapter.GetDesc1(&mut desc) }
                .ok()
                .expect("GetDesc1");
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE as u32 != 0 {
                continue;
            }
            selected = Some((adapter, desc));
            break;
        }
        let (adapter, desc) = selected.expect("hardware DXGI adapter required");
        let name = String::from_utf16_lossy(
            &desc.Description[..desc
                .Description
                .iter()
                .position(|v| *v == 0)
                .unwrap_or(desc.Description.len())],
        );

        // SAFETY: Adapter is live. IDXGIDevice is the documented driver-version
        // query IID; D3D11+ IIDs are invalid here. Device creation below tests
        // D3D12 support independently of this WDDM driver package version.
        let driver_version = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }
            .expect("adapter driver version");
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: Selected hardware adapter stays alive; valid owned COM output.
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }
            .expect("hardware D3D12 Device");
        let device = device.expect("D3D12 Device reference");
        let info: ID3D12InfoQueue1 = device.cast().expect("active Debug Layer InfoQueue1");
        let debug_device: ID3D12DebugDevice1 = device.cast().expect("Debug Device settings");
        let mut cookie = 0;
        // SAFETY: Callback has system ABI, copies messages and accesses only
        // process-static synchronized state. IGNORE_FILTERS observes all messages;
        // it stays registered until final Device Release to cover destruction.
        unsafe {
            info.RegisterMessageCallback(
                Some(receive_message),
                D3D12_MESSAGE_CALLBACK_IGNORE_FILTERS,
                std::ptr::null_mut(),
                &mut cookie,
            )
        }
        .ok()
        .expect("register diagnostic callback");
        let (initial_denied, initial_denied_severities) = inspect_and_clear_filter(&info);
        let mut gbv_settings = D3D12_DEBUG_DEVICE_GPU_BASED_VALIDATION_SETTINGS::default();
        // SAFETY: Parameter enum, output pointer and byte count match exactly.
        unsafe {
            debug_device.GetDebugParameter(
                D3D12_DEBUG_DEVICE_PARAMETER_GPU_BASED_VALIDATION_SETTINGS,
                std::ptr::from_mut(&mut gbv_settings).cast(),
                size_of_val(&gbv_settings) as u32,
            )
        }
        .ok()
        .expect("read GBV settings");
        let mut shader_model = D3D12_FEATURE_DATA_SHADER_MODEL {
            HighestShaderModel: D3D_SHADER_MODEL_6_0,
        };
        check_feature(&device, D3D12_FEATURE_SHADER_MODEL, &mut shader_model);
        assert!(
            shader_model.HighestShaderModel >= D3D_SHADER_MODEL_6_0,
            "SM6.0 fixed shaders required"
        );
        let mut format = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ..Default::default()
        };
        check_feature(&device, D3D12_FEATURE_FORMAT_SUPPORT, &mut format);
        assert_ne!(
            format.Support2 & D3D12_FORMAT_SUPPORT2_UAV_TYPED_LOAD,
            0,
            "RGBA8 typed UAV load required"
        );
        assert_ne!(
            format.Support2 & D3D12_FORMAT_SUPPORT2_UAV_TYPED_STORE,
            0,
            "RGBA8 typed UAV store required"
        );
        let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
        check_feature(&device, D3D12_FEATURE_D3D12_OPTIONS, &mut options);
        let buffers = probe_buffers(&device);
        let mut luid = [0u8; 8];
        luid[..4].copy_from_slice(&desc.AdapterLuid.LowPart.to_le_bytes());
        luid[4..].copy_from_slice(&desc.AdapterLuid.HighPart.to_le_bytes());
        #[cfg(feature = "vulkan")]
        let vulkan = {
            let (vk_name, vendor_id, device_id, vk_luid) =
                crate::backends::vulkan::preparation_adapter_identity();
            assert_eq!(desc.VendorId, vendor_id);
            assert_eq!(desc.DeviceId, device_id);
            assert_eq!(
                luid, vk_luid,
                "Vulkan and D3D12 must select the same physical GPU"
            );
            json!({"same_adapter":true,"luid_valid":true,"name":vk_name,"vendor_id":vendor_id,"device_id":device_id,"core_validation":true,"synchronization_validation":true,"teardown_checked":true})
        };
        #[cfg(not(feature = "vulkan"))]
        let vulkan = json!({"same_adapter":null,"luid_valid":null,"reason":"enable vulkan,d3d12 for cross-API acceptance"});
        let mut initial_messages = Vec::new();
        // SAFETY: Live, unfiltered queue. Check storage counters and all messages
        // preceding callback registration as well as later callback delivery.
        unsafe {
            assert_eq!(
                info.GetNumMessagesDeniedByStorageFilter(),
                initial_denied,
                "no additional messages hidden after clearing storage filter"
            );
            assert_eq!(info.GetNumMessagesDiscardedByMessageCountLimit(), 0);
            for index in 0..info.GetNumStoredMessages() {
                let mut size = 0;
                info.GetMessage(index, None, &mut size)
                    .ok()
                    .expect("D3D12 message size");
                assert!((size_of::<D3D12_MESSAGE>()..=1024 * 1024).contains(&size));
                let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
                let message = storage.as_mut_ptr().cast::<D3D12_MESSAGE>();
                info.GetMessage(index, Some(message), &mut size)
                    .ok()
                    .expect("D3D12 message");
                let message = &*message;
                initial_messages.push(Message {
                    severity: message.Severity,
                    id: message.ID,
                    text: CStr::from_ptr(message.pDescription)
                        .to_string_lossy()
                        .into_owned(),
                });
            }
        }
        for message in &initial_messages {
            eprintln!("D3D12 stored: {message:?}");
        }
        assert!(
            !initial_messages
                .iter()
                .any(|m| m.severity <= D3D12_MESSAGE_SEVERITY_WARNING),
            "D3D12 warnings/errors before teardown"
        );
        let startup = initial_messages
            .iter()
            .find(|message| message.id == D3D12_MESSAGE_ID_CREATEDEVICE_DEBUG_LAYER_STARTUP_OPTIONS)
            .expect("Debug Layer startup configuration report");
        assert!(
            startup
                .text
                .contains("GPU-Based Validation is enabled (disabled by default)."),
            "recognized effective GBV startup report required: {}",
            startup.text
        );
        report = json!({"schema_version":1,"adapter":{"name":name,"vendor_id":desc.VendorId,"device_id":desc.DeviceId,"luid_low":desc.AdapterLuid.LowPart,"luid_high":desc.AdapterLuid.HighPart,"driver_version":driver_version},"validation":{"debug_layer_requested":true,"gpu_based_validation_requested":true,"configuration_before_device":true,"debug_device_available":true,"info_queue_available":true,"gbv_settings":{"max_messages":gbv_settings.MaxMessagesPerCommandList,"shader_patch_mode":gbv_settings.DefaultShaderPatchMode,"pipeline_state_create_flags":gbv_settings.PipelineStateCreateFlags},"gbv_shader_exercised":false},"capabilities":{"shader_model":shader_model.HighestShaderModel,"rgba8_typed_uav_load":true,"rgba8_typed_uav_store":true,"resource_binding_tier":options.ResourceBindingTier,"resource_heap_tier":options.ResourceHeapTier},"vulkan":vulkan,"allocator":{"baseline":"committed_resource","probe_created":true,"submitted":false,"buffers":buffers}});
        report["validation"]["gpu_based_validation_effective"] = json!(true);
        report["validation"]["startup_report"] = json!(startup.text);
        report["validation"]["storage_denied_messages"] = json!(initial_denied);
        report["validation"]["additional_storage_denied_after_clear"] = json!(0);
        report["validation"]["storage_filter_problem_safe"] = json!(true);
        report["validation"]["initial_storage_filter"] = json!({
            "allow_categories":[],"allow_ids":[],"allow_severities":[],
            "deny_categories":[],"deny_ids":[],"deny_severities":initial_denied_severities,
            "cleared_after_inspection":true
        });
        // Explicit release order: no resources remain. Callback state is static;
        // release every secondary Device interface before its primary reference.
        drop(debug_device);
        drop(info);
        drop(device);
        drop(adapter);
        drop(factory);
        drop(debug1);
        drop(debug);
    }
    let dxgi = dxgi_messages(&global_queue);
    drop(global_queue);
    for message in &dxgi {
        eprintln!("DXGI: {message:?}");
    }
    assert!(
        !dxgi
            .iter()
            .any(|m| m.severity <= DXGI_INFO_QUEUE_MESSAGE_SEVERITY_WARNING),
        "DXGI warnings/errors through teardown"
    );
    assert_eq!(CALLBACK_FAILURES.load(Ordering::Relaxed), 0);
    assert_eq!(DROPPED_MESSAGES.load(Ordering::Relaxed), 0);
    let messages = MESSAGES.lock().unwrap();
    for message in messages.iter() {
        eprintln!(
            "D3D12 callback: {} {} {}",
            message.severity, message.id, message.text
        );
    }
    let warnings = messages
        .iter()
        .filter(|m| m.severity == D3D12_MESSAGE_SEVERITY_WARNING)
        .count();
    let errors = messages
        .iter()
        .filter(|m| m.severity == D3D12_MESSAGE_SEVERITY_ERROR)
        .count();
    let corruption = messages
        .iter()
        .filter(|m| m.severity == D3D12_MESSAGE_SEVERITY_CORRUPTION)
        .count();
    assert_eq!(
        (warnings, errors, corruption),
        (0, 0, 0),
        "D3D12 diagnostics through final Release"
    );
    report["validation"]["warnings"] = json!(warnings);
    report["validation"]["errors"] = json!(errors);
    report["validation"]["corruption"] = json!(corruption);
    report["validation"]["teardown_checked"] = json!(true);
    report["validation"]["callback_failures"] = json!(0);
    report["validation"]["dropped_messages"] = json!(0);
    report["validation"]["storage_discarded_messages"] = json!(0);
    let serialized = serde_json::to_string_pretty(&report).expect("serialize preparation evidence");
    println!("M4_PREPARATION_REPORT {serialized}");
    if let Some(path) = std::env::var_os("RENDERER_M4_PREPARATION_REPORT") {
        std::fs::write(path, serialized).expect("write preparation evidence");
    }
}
