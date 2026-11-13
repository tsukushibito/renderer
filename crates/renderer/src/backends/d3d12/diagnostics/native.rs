use std::collections::BTreeMap;
use std::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use windows_core::Interface;

use super::super::bindings::*;
use super::{
    D3D12RuntimeError, D3D12ValidationMessage, D3D12ValidationMode, D3D12ValidationReport,
};

#[derive(Debug, Default)]
struct State {
    report: Mutex<D3D12ValidationReport>,
    active: AtomicUsize,
}
impl State {
    fn push(&self, message: D3D12ValidationMessage) {
        let mut report = self
            .report
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match message.severity {
            D3D12_MESSAGE_SEVERITY_CORRUPTION => report.corruption += 1,
            D3D12_MESSAGE_SEVERITY_ERROR => report.errors += 1,
            D3D12_MESSAGE_SEVERITY_WARNING => report.warnings += 1,
            _ => {}
        }
        if report.messages.len() < 1024 {
            report.messages.push(message);
        } else {
            report.dropped_messages += 1;
        }
    }
    fn fail(&self) {
        self.report
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .callback_failures += 1;
    }
}

/// Owns only diagnostics, never a native Device, queue or work payload.
#[derive(Clone, Debug, Default)]
pub struct D3D12ValidationMonitor {
    state: Arc<State>,
}
impl D3D12ValidationMonitor {
    pub(in super::super) fn new() -> Self {
        Self::default()
    }
    pub fn snapshot(&self) -> D3D12ValidationReport {
        self.state
            .report
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

// Context is an opaque monotonically allocated integer token, never a borrowed
// pointer. A callback clones the Arc while the registry is locked; a late lookup
// after removal is harmless. No registry lock is held across native calls.
static REGISTRY: Mutex<BTreeMap<usize, Arc<State>>> = Mutex::new(BTreeMap::new());
static NEXT_TOKEN: AtomicUsize = AtomicUsize::new(1);

unsafe extern "system" fn receive_message(
    _category: D3D12_MESSAGE_CATEGORY,
    severity: D3D12_MESSAGE_SEVERITY,
    id: D3D12_MESSAGE_ID,
    description: windows_core::PCSTR,
    context: *mut c_void,
) {
    let state = {
        let registry = REGISTRY.lock().unwrap_or_else(|error| error.into_inner());
        registry.get(&context.addr()).map(|state| {
            state.active.fetch_add(1, Ordering::AcqRel);
            state.clone()
        })
    };
    let Some(state) = state else {
        return;
    };
    let result = std::panic::catch_unwind(|| {
        // SAFETY: The native callback supplies a NUL-terminated description for
        // this call only. Copy it and never reenter D3D or caller logging here.
        let message = unsafe { CStr::from_ptr(description.as_ptr().cast()) }
            .to_string_lossy()
            .into_owned();
        state.push(D3D12ValidationMessage {
            source: "D3D12",
            severity,
            id,
            message,
        });
    });
    if let Err(payload) = result {
        state.fail();
        // Panic payload Drop may panic too; never unwind across native FFI.
        std::mem::forget(payload);
    }
    state.active.fetch_sub(1, Ordering::Release);
}

pub(in super::super) struct Session {
    monitor: D3D12ValidationMonitor,
    global: Option<IDXGIInfoQueue>,
    token: Option<usize>,
    enabled: bool,
}
impl Session {
    pub(in super::super) fn begin(
        mode: D3D12ValidationMode,
        monitor: D3D12ValidationMonitor,
        first: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        if mode == D3D12ValidationMode::Disabled {
            return Ok(Self {
                monitor,
                global: None,
                token: None,
                enabled: false,
            });
        }
        // SAFETY: Acquire global DXGI diagnostics before creating native objects
        // and retain them after every owned Device reference has been released.
        let global = unsafe { DXGIGetDebugInterface1(0) }.map_err(|error| {
            D3D12RuntimeError::native("DXGIGetDebugInterface1", "DXGI diagnostics", error)
        })?;
        if first {
            let mut debug = None;
            // SAFETY: This is serialized before the first owned Device exists.
            unsafe { D3D12GetDebugInterface(&mut debug) }.map_err(|error| {
                D3D12RuntimeError::native("D3D12GetDebugInterface", "D3D12 diagnostics", error)
            })?;
            let debug: ID3D12Debug = debug.ok_or_else(|| {
                D3D12RuntimeError::unsupported("D3D12 Debug Layer interface was not returned")
            })?;
            let debug1: ID3D12Debug1 = debug.cast().map_err(|error| {
                D3D12RuntimeError::native("QueryInterface ID3D12Debug1", "D3D12 diagnostics", error)
            })?;
            // SAFETY: Both process-global controls must run before any Device;
            // callers must initialize this runtime before external D3D12 users.
            unsafe {
                debug.EnableDebugLayer();
                debug1.SetEnableGPUBasedValidation(true);
            }
        }
        Ok(Self {
            monitor,
            global: Some(global),
            token: None,
            enabled: true,
        })
    }
    pub(in super::super) fn attach(
        &mut self,
        device: &ID3D12Device,
    ) -> Result<(), D3D12RuntimeError> {
        if !self.enabled {
            return Ok(());
        }
        let info: ID3D12InfoQueue1 = device.cast().map_err(|error| {
            D3D12RuntimeError::native(
                "QueryInterface ID3D12InfoQueue1",
                "D3D12 diagnostics",
                error,
            )
        })?;
        let debug_device: ID3D12DebugDevice1 = device.cast().map_err(|error| {
            D3D12RuntimeError::native(
                "QueryInterface ID3D12DebugDevice1",
                "D3D12 diagnostics",
                error,
            )
        })?;
        inspect_filter(&info)?;
        let mut settings = D3D12_DEBUG_DEVICE_GPU_BASED_VALIDATION_SETTINGS::default();
        // SAFETY: Documented matching parameter enum, output type and byte size.
        unsafe {
            debug_device.GetDebugParameter(
                D3D12_DEBUG_DEVICE_PARAMETER_GPU_BASED_VALIDATION_SETTINGS,
                std::ptr::from_mut(&mut settings).cast(),
                size_of_val(&settings) as u32,
            )
        }
        .ok()
        .map_err(|error| {
            D3D12RuntimeError::native("GetDebugParameter GBV", "D3D12 diagnostics", error)
        })?;
        let token = NEXT_TOKEN
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| {
                D3D12RuntimeError::capacity("D3D12 diagnostic callback identity exhausted")
            })?;
        REGISTRY
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(token, self.monitor.state.clone());
        let mut cookie = 0;
        // SAFETY: Opaque non-reused token is not dereferenced. Registry state is
        // retained through final Device Release, with no native refs of its own.
        let registration = unsafe {
            info.RegisterMessageCallback(
                Some(receive_message),
                D3D12_MESSAGE_CALLBACK_IGNORE_FILTERS,
                std::ptr::without_provenance_mut(token),
                &mut cookie,
            )
        }
        .ok();
        if let Err(error) = registration {
            REGISTRY
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&token);
            return Err(D3D12RuntimeError::native(
                "RegisterMessageCallback",
                "D3D12 diagnostics",
                error,
            ));
        }
        self.token = Some(token);
        // Register before scanning startup storage so an internal native thread
        // cannot emit an unobserved message between the two. The overlap may
        // duplicate a diagnostic; strict zero-warning acceptance is unaffected.
        // Never clear messages to obtain a clean result.
        let mut gbv = false;
        // SAFETY: Live unfiltered queue and bounded, aligned temporary storage.
        unsafe {
            for index in 0..info.GetNumStoredMessages() {
                let mut size = 0;
                info.GetMessage(index, None, &mut size)
                    .ok()
                    .map_err(|error| {
                        D3D12RuntimeError::native("GetMessage size", "D3D12 diagnostics", error)
                    })?;
                check_size::<D3D12_MESSAGE>(size)?;
                let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
                let pointer = storage.as_mut_ptr().cast::<D3D12_MESSAGE>();
                info.GetMessage(index, Some(pointer), &mut size)
                    .ok()
                    .map_err(|error| {
                        D3D12RuntimeError::native("GetMessage", "D3D12 diagnostics", error)
                    })?;
                let message = &*pointer;
                let text = CStr::from_ptr(message.pDescription)
                    .to_string_lossy()
                    .into_owned();
                if message.ID == D3D12_MESSAGE_ID_CREATEDEVICE_DEBUG_LAYER_STARTUP_OPTIONS {
                    gbv |= text.contains("GPU-Based Validation is enabled (disabled by default).");
                }
                self.monitor.state.push(D3D12ValidationMessage {
                    source: "D3D12",
                    severity: message.Severity,
                    id: message.ID,
                    message: text,
                });
            }
            if info.GetNumMessagesDiscardedByMessageCountLimit() != 0 {
                return Err(D3D12RuntimeError::unsupported(
                    "D3D12 diagnostic storage discarded startup messages",
                ));
            }
        }
        if !gbv {
            return Err(D3D12RuntimeError::unsupported(
                "Required D3D12 validation lacks a recognized effective GBV startup report",
            ));
        }
        let mut report = self
            .monitor
            .state
            .report
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        report.debug_layer_enabled = true;
        report.gpu_based_validation_enabled = true;
        Ok(())
    }
}

fn check_size<T>(size: usize) -> Result<(), D3D12RuntimeError> {
    if !(size_of::<T>()..=1024 * 1024).contains(&size) {
        return Err(D3D12RuntimeError::unsupported(
            "unrecognized native diagnostic payload size",
        ));
    }
    Ok(())
}

fn inspect_filter(info: &ID3D12InfoQueue1) -> Result<(), D3D12RuntimeError> {
    // SAFETY: Live queue; pointer lists returned by GetStorageFilter remain
    // within the aligned owned storage until inspected. No lists are retained.
    unsafe {
        let mut size = 0;
        info.GetStorageFilter(None, &mut size)
            .ok()
            .map_err(|error| {
                D3D12RuntimeError::native("GetStorageFilter size", "D3D12 diagnostics", error)
            })?;
        check_size::<D3D12_INFO_QUEUE_FILTER>(size)?;
        let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
        let pointer = storage.as_mut_ptr().cast::<D3D12_INFO_QUEUE_FILTER>();
        info.GetStorageFilter(Some(pointer), &mut size)
            .ok()
            .map_err(|error| {
                D3D12RuntimeError::native("GetStorageFilter", "D3D12 diagnostics", error)
            })?;
        let filter = &*pointer;
        let denied = &filter.DenyList;
        if filter.AllowList.NumCategories != 0
            || filter.AllowList.NumSeverities != 0
            || filter.AllowList.NumIDs != 0
            || denied.NumCategories != 0
            || denied.NumIDs != 0
        {
            return Err(D3D12RuntimeError::unsupported(
                "D3D12 diagnostic storage has unreviewed category/ID filtering",
            ));
        }
        if denied.NumSeverities != 0 {
            let severities =
                std::slice::from_raw_parts(denied.pSeverityList, denied.NumSeverities as usize);
            if severities
                .iter()
                .any(|severity| *severity <= D3D12_MESSAGE_SEVERITY_WARNING)
            {
                return Err(D3D12RuntimeError::unsupported(
                    "D3D12 diagnostic storage filters warning/error messages",
                ));
            }
        }
        info.ClearStorageFilter();
        info.ClearRetrievalFilter();
    }
    Ok(())
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        if let Some(global) = &self.global {
            // SAFETY: Final owned Device/queue Release has happened before this
            // session is dropped. The independently owned global DXGI queue
            // observes destruction diagnostics without keeping the Device alive.
            let outcome = unsafe {
                (|| -> Result<(), D3D12RuntimeError> {
                    if global.GetNumMessagesDeniedByStorageFilter(DXGI_DEBUG_ALL) != 0
                        || global.GetNumMessagesDiscardedByMessageCountLimit(DXGI_DEBUG_ALL) != 0
                    {
                        return Err(D3D12RuntimeError::unsupported(
                            "DXGI diagnostic messages were filtered or discarded",
                        ));
                    }
                    for index in 0..global.GetNumStoredMessages(DXGI_DEBUG_ALL) {
                        let mut size = 0;
                        global
                            .GetMessage(DXGI_DEBUG_ALL, index, None, &mut size)
                            .ok()
                            .map_err(|error| {
                                D3D12RuntimeError::native(
                                    "DXGI GetMessage size",
                                    "DXGI diagnostics",
                                    error,
                                )
                            })?;
                        check_size::<DXGI_INFO_QUEUE_MESSAGE>(size)?;
                        let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
                        let pointer = storage.as_mut_ptr().cast::<DXGI_INFO_QUEUE_MESSAGE>();
                        global
                            .GetMessage(DXGI_DEBUG_ALL, index, Some(pointer), &mut size)
                            .ok()
                            .map_err(|error| {
                                D3D12RuntimeError::native(
                                    "DXGI GetMessage",
                                    "DXGI diagnostics",
                                    error,
                                )
                            })?;
                        let message = &*pointer;
                        self.monitor.state.push(D3D12ValidationMessage {
                            source: "DXGI",
                            severity: message.Severity,
                            id: message.ID.0,
                            message: CStr::from_ptr(message.pDescription)
                                .to_string_lossy()
                                .into_owned(),
                        });
                    }
                    Ok(())
                })()
            };
            if let Err(error) = outcome {
                self.monitor.state.fail();
                self.monitor.state.push(D3D12ValidationMessage {
                    source: "DXGI",
                    severity: D3D12_MESSAGE_SEVERITY_ERROR,
                    id: 0,
                    message: error.to_string(),
                });
            }
        }
        if let Some(token) = self.token.take() {
            REGISTRY
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&token);
        }
        // Removal prevents new callback leases. A callback that cloned state
        // before removal must finish before the final report is marked complete.
        while self.monitor.state.active.load(Ordering::Acquire) != 0 {
            std::thread::yield_now();
        }
        self.monitor
            .state
            .report
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .teardown_checked = true;
    }
}
