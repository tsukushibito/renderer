use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use windows_core::Interface;

use super::bindings::*;
use super::diagnostics::{D3D12ValidationMonitor, Session};
use super::{D3D12RuntimeError, D3D12RuntimeErrorKind, D3D12ValidationMode};

#[derive(Clone, Debug)]
pub struct D3D12DeviceInfo {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub luid: [u8; 8],
    pub driver_version: i64,
    pub shader_model: u32,
    pub resource_binding_tier: u32,
    pub resource_heap_tier: u32,
    pub rgba8_typed_uav_load: bool,
    pub rgba8_typed_uav_store: bool,
}

struct ProcessState {
    mode: Option<D3D12ValidationMode>,
    owner: Weak<DeviceOwner>,
    // Strong count reaches zero before Drop starts. This flag prevents creating
    // another native owner in that gap or while final COM Release is running.
    live: bool,
}
static PROCESS: Mutex<ProcessState> = Mutex::new(ProcessState {
    mode: None,
    owner: Weak::new(),
    live: false,
});

pub(super) struct DeviceOwner {
    pub(super) queue: ManuallyDrop<ID3D12CommandQueue>,
    pub(super) raw: ManuallyDrop<ID3D12Device>,
    pub(super) queue_gate: Mutex<()>,
    pub(super) info: D3D12DeviceInfo,
    pub(super) monitor: D3D12ValidationMonitor,
    #[cfg(feature = "d3d12-diagnostics")]
    session: Option<Session>,
    stopped: AtomicBool,
}

// SAFETY: D3D12 Device and queue interfaces are free-threaded. Only immutable
// Device queries/creation are shared; all queue submission transactions use
// queue_gate. Final COM Release is serialized with process initialization.
unsafe impl Send for DeviceOwner {}
// SAFETY: The same free-threaded interface and queue serialization argument
// applies to shared references. State transitions use an atomic flag.
unsafe impl Sync for DeviceOwner {}

impl DeviceOwner {
    pub(super) fn new(mode: D3D12ValidationMode) -> Result<Arc<Self>, D3D12RuntimeError> {
        if mode == D3D12ValidationMode::Required && !cfg!(feature = "d3d12-diagnostics") {
            return Err(D3D12RuntimeError::unsupported(
                "Required D3D12 validation needs d3d12-diagnostics",
            ));
        }
        let mut process = PROCESS.lock().unwrap_or_else(|error| error.into_inner());
        if process.mode.is_some_and(|previous| previous != mode) {
            return Err(D3D12RuntimeError::unsupported(
                "D3D12 validation mode is fixed by the first runtime in this process",
            ));
        }
        if let Some(owner) = process.owner.upgrade() {
            drop(process);
            owner.ensure_active()?;
            return Ok(owner);
        }
        if process.live {
            return Err(D3D12RuntimeError::classified(
                D3D12RuntimeErrorKind::DeviceUnavailable,
                "previous D3D12 Device teardown is still in progress; retry creation",
            ));
        }
        // The process guard covers debug setters, Device creation and final
        // native destruction. D3D12CreateDevice is a per-adapter singleton.
        let monitor = D3D12ValidationMonitor::new();
        let mut session = Session::begin(mode, monitor.clone(), process.mode.is_none())?;
        // Once a debug setter ran, later attempts must not change its mode even
        // if subsequent native initialization fails.
        process.mode = Some(mode);
        let flags = if mode == D3D12ValidationMode::Required {
            DXGI_CREATE_FACTORY_DEBUG as u32
        } else {
            0
        };
        // SAFETY: Supported DXGI factory interface and valid creation flags.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory2(flags) }
            .map_err(|error| D3D12RuntimeError::native("CreateDXGIFactory2", "DXGI", error))?;
        let mut selected = None;
        for index in 0.. {
            // SAFETY: Live owned factory; returned adapters own their COM ref.
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => {
                    return Err(D3D12RuntimeError::native("EnumAdapters1", "DXGI", error));
                }
            };
            let mut desc = DXGI_ADAPTER_DESC1::default();
            // SAFETY: Live adapter and correctly sized stack output.
            unsafe { adapter.GetDesc1(&mut desc) }
                .ok()
                .map_err(|error| D3D12RuntimeError::native("GetDesc1", "DXGI adapter", error))?;
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE as u32 != 0 {
                continue;
            }
            // SAFETY: Null output tests baseline D3D12 support without creating
            // a Device. The exact interface IID is supplied by the binding.
            let support = unsafe {
                D3D12CreateDevice::<_, ID3D12Device>(
                    &adapter,
                    D3D_FEATURE_LEVEL_11_0,
                    std::ptr::null_mut(),
                )
            };
            match support {
                Ok(()) => {
                    selected = Some((adapter, desc));
                    break;
                }
                Err(error) if error.code() == DXGI_ERROR_UNSUPPORTED => continue,
                Err(error) => {
                    return Err(D3D12RuntimeError::native(
                        "D3D12CreateDevice capability probe",
                        "DXGI adapter",
                        error,
                    ));
                }
            }
        }
        let (adapter, desc) = selected.ok_or_else(|| {
            D3D12RuntimeError::unsupported("no hardware adapter supports D3D12 feature level 11_0")
        })?;
        let name = String::from_utf16_lossy(
            &desc.Description[..desc
                .Description
                .iter()
                .position(|v| *v == 0)
                .unwrap_or(desc.Description.len())],
        );
        // SAFETY: IDXGIDevice is the documented driver-version query IID.
        let driver_version =
            unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }.map_err(|error| {
                D3D12RuntimeError::native("CheckInterfaceSupport driver version", &name, error)
            })?;
        let mut raw = None;
        // SAFETY: Capability-tested hardware adapter, live output and valid IID.
        // Failure does not trigger a silent fallback to another adapter.
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut raw) }
            .map_err(|error| D3D12RuntimeError::native("D3D12CreateDevice", &name, error))?;
        let raw: ID3D12Device = raw.ok_or_else(|| {
            D3D12RuntimeError::classified(
                D3D12RuntimeErrorKind::Native,
                "D3D12CreateDevice succeeded without returning a Device",
            )
        })?;
        session.attach(&raw)?;
        let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
        feature(&raw, &name, D3D12_FEATURE_D3D12_OPTIONS, &mut options)?;
        let mut shader_model = D3D12_FEATURE_DATA_SHADER_MODEL {
            HighestShaderModel: D3D_SHADER_MODEL_6_0,
        };
        // SM6 was added after the initial D3D12 runtime. An older runtime can
        // reject this optional query with E_INVALIDARG; report it as unavailable
        // without excluding the buffer-only baseline.
        if let Err(error) = feature(&raw, &name, D3D12_FEATURE_SHADER_MODEL, &mut shader_model) {
            if error
                .source
                .as_deref()
                .and_then(|source| source.downcast_ref::<super::NativeCause>())
                .is_some_and(|cause| cause.error.code().0 as u32 == 0x80070057)
            {
                shader_model.HighestShaderModel = D3D_SHADER_MODEL_NONE;
            } else {
                return Err(error);
            }
        }
        let mut format = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ..Default::default()
        };
        feature(&raw, &name, D3D12_FEATURE_FORMAT_SUPPORT, &mut format)?;
        // Query future shader capabilities, but buffer-only execution does not
        // require SM6.0 or RGBA8 UAV support.
        let queue_desc = D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            ..Default::default()
        };
        // SAFETY: Live Device and a valid single-node DIRECT queue descriptor.
        let queue = unsafe { raw.CreateCommandQueue(&queue_desc) }
            .map_err(|error| D3D12RuntimeError::native("CreateCommandQueue", &name, error))?;
        let mut luid = [0; 8];
        luid[..4].copy_from_slice(&desc.AdapterLuid.LowPart.to_le_bytes());
        luid[4..].copy_from_slice(&desc.AdapterLuid.HighPart.to_le_bytes());
        let owner = Arc::new(Self {
            queue: ManuallyDrop::new(queue),
            raw: ManuallyDrop::new(raw),
            queue_gate: Mutex::new(()),
            info: D3D12DeviceInfo {
                name,
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                luid,
                driver_version,
                shader_model: shader_model.HighestShaderModel as u32,
                resource_binding_tier: options.ResourceBindingTier as u32,
                resource_heap_tier: options.ResourceHeapTier as u32,
                rgba8_typed_uav_load: format.Support2 & D3D12_FORMAT_SUPPORT2_UAV_TYPED_LOAD != 0,
                rgba8_typed_uav_store: format.Support2 & D3D12_FORMAT_SUPPORT2_UAV_TYPED_STORE != 0,
            },
            monitor,
            #[cfg(feature = "d3d12-diagnostics")]
            session: Some(session),
            stopped: AtomicBool::new(false),
        });
        process.owner = Arc::downgrade(&owner);
        process.live = true;
        Ok(owner)
    }
    pub(super) fn ensure_active(&self) -> Result<(), D3D12RuntimeError> {
        if self.stopped.load(Ordering::Acquire) {
            Err(D3D12RuntimeError::classified(
                D3D12RuntimeErrorKind::DeviceUnavailable,
                "D3D12 Device is stopped",
            ))
        } else {
            Ok(())
        }
    }
    pub(super) fn mark_stopped(&self) {
        self.stopped.store(true, Ordering::Release);
    }
    pub(super) fn check_native_active(&self) -> Result<(), D3D12RuntimeError> {
        self.ensure_active()?;
        // SAFETY: Live owned Device. Native loss can happen after an earlier
        // Fence observation, so CPU result/access boundaries query it explicitly.
        unsafe { self.raw.GetDeviceRemovedReason() }
            .ok()
            .map_err(|error| {
                self.mark_stopped();
                self.native("GetDeviceRemovedReason", error)
            })
    }
    pub(super) fn native(
        &self,
        operation: &'static str,
        error: windows_core::Error,
    ) -> D3D12RuntimeError {
        let error = D3D12RuntimeError::native(operation, &self.info.name, error);
        if error.kind() == D3D12RuntimeErrorKind::DeviceRemoved {
            self.mark_stopped();
        }
        error
    }
    pub(super) fn removed(&self) -> D3D12RuntimeError {
        self.mark_stopped();
        // SAFETY: Live owned Device; this query is valid after removal.
        let result = unsafe { self.raw.GetDeviceRemovedReason() };
        if let Err(error) = result.ok() {
            self.native("GetDeviceRemovedReason", error)
        } else {
            D3D12RuntimeError::classified(
                D3D12RuntimeErrorKind::DeviceRemoved,
                "D3D12 Fence reported Device removal",
            )
        }
    }
}

fn feature<T>(
    device: &ID3D12Device,
    name: &str,
    feature: D3D12_FEATURE,
    data: &mut T,
) -> Result<(), D3D12RuntimeError> {
    // SAFETY: All private call sites pair the feature with its exact initialized
    // SDK structure; no arbitrary external feature/pointer reaches this helper.
    unsafe {
        device.CheckFeatureSupport(
            feature,
            std::ptr::from_mut(data).cast(),
            size_of::<T>() as u32,
        )
    }
    .ok()
    .map_err(|error| D3D12RuntimeError::native("CheckFeatureSupport", name, error))
}

impl Drop for DeviceOwner {
    fn drop(&mut self) {
        let mut process = PROCESS.lock().unwrap_or_else(|error| error.into_inner());
        // SAFETY: This is the sole final Arc owner. All GPU work retains this
        // owner until actual completion (or conservatively leaks it). Each COM
        // reference is released exactly once, queue before primary Device, while
        // the initialization lock excludes a concurrent singleton recreation.
        unsafe {
            ManuallyDrop::drop(&mut self.queue);
            ManuallyDrop::drop(&mut self.raw);
        }
        // Diagnostics have no Device references; keep their callback registry
        // and DXGI queue through both Releases, then complete the report.
        #[cfg(feature = "d3d12-diagnostics")]
        drop(self.session.take());
        process.live = false;
    }
}
