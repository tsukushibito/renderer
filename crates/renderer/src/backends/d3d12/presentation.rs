//! Window-owned flip-model output. Graph resources remain independent of DXGI.
//!
//! DXGI owns display-side references to accepted buffers. Our queue marker only
//! establishes application GPU completion; it never proves scanout completion.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};
use windows_core::Interface;
use winit::window::Window;

use super::{
    D3D12Runtime, D3D12RuntimeConfig, D3D12RuntimeError as Error,
    bindings::*,
    device::DeviceOwner,
    frame::{NativeObject, object},
    graph::transition,
    registry::PhysicalRef,
};
use crate::{
    graph::{ImageFormat, ImportImage},
    presentation::{PresentOutcome, PresentationConfig},
    profiling::{GpuTiming, PresentationMetrics},
};

mod program;
use program::Program;
mod slots;
use super::raster_storage::DescriptorHeap;
use slots::Slot;

const CLEANUP_BUDGET: Duration = Duration::from_secs(10);

struct Chain {
    targets: Vec<ID3D12Resource>,
    rtv: DescriptorHeap,
    raw: NativeObject<IDXGISwapChain3>,
    extent: (u32, u32),
    count: u32,
    flags: u32,
    rtv_step: u32,
}
impl Chain {
    fn targets(&mut self, owner: &DeviceOwner) -> Result<(), Error> {
        self.targets.clear();
        // SAFETY: The live prepared heap contains one RTV per requested buffer.
        let start = unsafe { self.rtv.raw.GetCPUDescriptorHandleForHeapStart() };
        for index in 0..self.count {
            // SAFETY: Flip-model buffer indices are stable until ResizeBuffers.
            let resource: ID3D12Resource = unsafe { self.raw.GetBuffer(index) }
                .map_err(|error| owner.native("IDXGISwapChain::GetBuffer", error))?;
            let desc = D3D12_RENDER_TARGET_VIEW_DESC {
                Format: DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
                ViewDimension: D3D12_RTV_DIMENSION_TEXTURE2D,
                Anonymous: D3D12_RENDER_TARGET_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_RTV {
                        MipSlice: 0,
                        PlaneSlice: 0,
                    },
                },
            };
            // SAFETY: UNORM flip buffers support an SRGB RTV; the descriptor is
            // in this generation's idle prepared heap, never changed in flight.
            unsafe {
                owner.raw.CreateRenderTargetView(
                    &resource,
                    Some(&desc),
                    D3D12_CPU_DESCRIPTOR_HANDLE {
                        ptr: start.ptr + index as usize * self.rtv_step as usize,
                    },
                )
            };
            self.targets.push(resource);
        }
        Ok(())
    }
    fn new(
        owner: &DeviceOwner,
        factory: &IDXGIFactory2,
        window: &Window,
        config: PresentationConfig,
        extent: (u32, u32),
        tearing: bool,
    ) -> Result<Self, Error> {
        let RawWindowHandle::Win32(handle) = window
            .window_handle()
            .map_err(|e| Error::invalid(format!("Window handle unavailable: {e}")))?
            .as_raw()
        else {
            return Err(Error::unsupported(
                "D3D12 presentation requires a Win32 window",
            ));
        };
        let hwnd = HWND(handle.hwnd.get() as *mut _);
        let flags = if tearing {
            DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING as u32
        } else {
            0
        };
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: extent.0,
            Height: extent.1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE(DXGI_USAGE_RENDER_TARGET_OUTPUT),
            BufferCount: config.desired_images,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: flags,
            ..Default::default()
        };
        let raw = object(owner, "CreateSwapChainForHwnd", || {
            // SAFETY: D3D12 requires the DIRECT queue here. The owning Arc Window
            // remains retained through swapchain teardown, including uncertainty.
            unsafe { factory.CreateSwapChainForHwnd(&*owner.queue, hwnd, &desc, None, None) }
                .and_then(|chain| chain.cast::<IDXGISwapChain3>())
        })?;
        // SAFETY: This HWND is live and automatic fullscreen transitions would
        // violate the windowed ALLOW_TEARING contract; app events own resizing.
        unsafe { factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER as u32) }
            .ok()
            .map_err(|error| owner.native("MakeWindowAssociation", error))?;
        let rtv = DescriptorHeap::new(
            owner,
            config.desired_images as usize,
            D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
            false,
        )?;
        // SAFETY: Device immutable descriptor increment query matches this heap.
        let rtv_step = unsafe {
            owner
                .raw
                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV)
        };
        let mut targets = Vec::new();
        targets
            .try_reserve_exact(config.desired_images as usize)
            .map_err(|_| Error::capacity("presentation target capacity reservation failed"))?;
        let mut chain = Self {
            targets,
            rtv,
            raw,
            extent,
            count: config.desired_images,
            flags,
            rtv_step,
        };
        chain.targets(owner)?;
        Ok(chain)
    }
    fn rtv(&self, index: usize) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        // SAFETY: The immutable heap and index were bounded at acquisition.
        let start = unsafe { self.rtv.raw.GetCPUDescriptorHandleForHeapStart() };
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: start.ptr + index * self.rtv_step as usize,
        }
    }
}

struct Inner {
    slots: Vec<Slot>,
    chain: Option<Chain>,
    program: Program,
    factory: IDXGIFactory2,
    config: PresentationConfig,
    desired: (u32, u32),
    dirty: bool,
    force_resize: bool,
    closed: bool,
    tearing: bool,
    registry_owner: u64,
    retry: Option<usize>,
    profiling: bool,
    samples: VecDeque<PresentationMetrics>,
    sequence: u64,
    #[cfg(test)]
    cancel_next: bool,
    owner: Arc<DeviceOwner>,
    _window: Arc<Window>,
}
impl Inner {
    fn poll(&mut self) -> Result<usize, Error> {
        self.owner.ensure_active()?;
        let mut retired = 0;
        while let Some(index) = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.pending)
            .min_by_key(|(_, slot)| slot.sequence)
            .map(|(index, _)| index)
        {
            // Retirement order defines the public sample FIFO even when a
            // lower-index slot has been reused for a newer submission.
            if self.retry == Some(index)
                || !self.slots[index].retire(&self.owner, &mut self.samples)?
            {
                break;
            }
            retired += 1;
        }
        Ok(retired)
    }
    fn idle(&self) -> bool {
        self.slots.iter().all(|slot| !slot.pending)
    }
    fn prepare(&mut self) -> Result<Option<PresentOutcome>, Error> {
        if self.closed {
            return Err(Error::invalid("Presenter is closed"));
        }
        self.poll()?;
        if self.desired.0 == 0 || self.desired.1 == 0 {
            return Ok(Some(PresentOutcome::Suspended));
        }
        if !self.dirty {
            return Ok(None);
        }
        // DXGI has no acquired-image semaphore to consume. A rendered buffer
        // whose DO_NOT_WAIT present was rejected can be abandoned after its GPU
        // marker. This cancels only a presentation request, never accepted work.
        if let Some(index) = self.retry {
            if !self.slots[index].completed(&self.owner)? {
                return Ok(Some(PresentOutcome::NotReady));
            }
            self.retry = None;
            self.poll()?;
        }
        if !self.idle() {
            return Ok(Some(PresentOutcome::NotReady));
        }
        if let Some(chain) = &mut self.chain {
            if chain.extent != self.desired || self.force_resize {
                // Discard all command-list references before releasing buffers.
                // All slot Fences above are complete; no Graph owns DXGI buffers.
                for slot in &mut self.slots {
                    slot.clear_recording(&self.owner)?;
                }
                chain.targets.clear();
                // SAFETY: Every app GPU reference completed, command lists were
                // reset, and direct refs released. DXGI manages its own display
                // references. ResizeBuffers does not require display completion.
                let result = unsafe {
                    chain.raw.ResizeBuffers(
                        chain.count,
                        self.desired.0,
                        self.desired.1,
                        DXGI_FORMAT_R8G8B8A8_UNORM,
                        chain.flags,
                    )
                }
                .ok();
                if let Err(error) = result {
                    // Resize failure preserves the native chain. Recover its old
                    // buffer refs before reporting; no half-ready chain escapes.
                    chain.targets(&self.owner)?;
                    return Err(self.owner.native("ResizeBuffers", error));
                }
                chain.extent = self.desired;
                chain.targets(&self.owner)?;
            }
            if chain.targets.len() != chain.count as usize {
                chain.targets(&self.owner)?;
            }
        } else {
            self.chain = Some(Chain::new(
                &self.owner,
                &self.factory,
                &self._window,
                self.config,
                self.desired,
                self.tearing,
            )?);
        }
        self.dirty = false;
        self.force_resize = false;
        Ok(Some(PresentOutcome::Recreated))
    }
}

/// Owns a Window, bounded presentation slots and a DXGI flip-model swapchain.
/// The application owns events. Poll regularly and close explicitly at shutdown.
/// DXGI retains display-side buffer ownership; no method proves scanout time.
pub struct D3D12Presenter {
    inner: Option<Inner>,
}

impl D3D12Presenter {
    pub fn vsync(&self) -> Option<bool> {
        self.inner
            .as_ref()
            .filter(|i| !i.closed)
            .map(|i| i.config.vsync)
    }
    /// Require the selected mode. Unsupported OFF requests preserve current state.
    /// Preparation applies the change after this presenter's work becomes idle.
    pub fn set_vsync(&mut self, enabled: bool) -> Result<(), Error> {
        let inner = self.active()?;
        if !enabled && !inner.tearing {
            return Err(Error::unsupported(
                "DXGI VSync off requires tearing support",
            ));
        }
        if inner.config.vsync != enabled {
            inner.config.vsync = enabled;
            inner.dirty = true;
        }
        Ok(())
    }
    fn active(&mut self) -> Result<&mut Inner, Error> {
        let inner = self
            .inner
            .as_mut()
            .filter(|inner| !inner.closed)
            .ok_or_else(|| Error::invalid("Presenter is closed"))?;
        inner.owner.ensure_active()?;
        Ok(inner)
    }
    pub fn prepare(&mut self) -> Result<Option<PresentOutcome>, Error> {
        self.active()?.prepare()
    }
    pub fn extent(&self) -> Option<(u32, u32)> {
        let inner = self.inner.as_ref()?;
        if inner.closed || inner.desired.0 == 0 || inner.desired.1 == 0 {
            return None;
        }
        inner.chain.as_ref().map(|chain| chain.extent)
    }
    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(inner) = &mut self.inner
            && inner.desired != (width, height)
        {
            inner.desired = (width, height);
            inner.dirty = true;
        }
    }
    pub fn poll(&mut self) -> Result<usize, Error> {
        self.active()?.poll()
    }
    /// DXGI resizes its one chain after app GPU retirement rather than retaining
    /// retired generations. This shared maintenance hook therefore returns zero.
    pub fn collect_retired_swapchains(&mut self) -> usize {
        0
    }
    /// Wait only this presenter's queue markers, without Queue/Device idle.
    /// Completion permits application resource reuse, not display-time inference.
    pub fn wait_idle(&mut self, timeout: Duration) -> Result<bool, Error> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        loop {
            let inner = self.active()?;
            // A rejected, unqueued present needs no DXGI cancellation; its GPU
            // work still remains retained until the corresponding marker ends.
            if let Some(index) = inner.retry
                && inner.slots[index].completed(&inner.owner)?
            {
                inner.retry = None;
            }
            inner.poll()?;
            if inner.idle() {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::yield_now();
        }
    }
    pub fn close(&mut self, timeout: Duration) -> Result<bool, Error> {
        if self.inner.as_ref().is_none_or(|i| i.closed) {
            return Ok(true);
        }
        if !self.wait_idle(timeout)? {
            return Ok(false);
        }
        let inner = self.inner.as_mut().unwrap();
        inner.closed = true;
        // Native destruction is confined to this explicit close boundary.
        inner.chain = None;
        Ok(true)
    }
    /// Optional timing allocates its bounded query storage at this explicit
    /// preparation boundary. Changes affect future recordings; pending and
    /// completed samples remain available when timing is disabled.
    pub fn set_profiling(&mut self, enabled: bool) -> Result<(), Error> {
        let inner = self.active()?;
        inner.poll()?;
        if inner.profiling == enabled {
            return Ok(());
        }
        if enabled {
            for slot in &mut inner.slots {
                slot.prepare_timestamps(&inner.owner)?;
            }
        }
        inner.profiling = enabled;
        Ok(())
    }
    pub fn profiling_enabled(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|i| !i.closed && i.profiling)
    }
    pub fn take_profile(&mut self) -> Option<PresentationMetrics> {
        self.inner.as_mut()?.samples.pop_front()
    }
    #[cfg(test)]
    fn cancel_next_acquire(&mut self) {
        self.inner.as_mut().unwrap().cancel_next = true;
    }
}
impl Drop for D3D12Presenter {
    fn drop(&mut self) {
        if self.inner.as_ref().is_none_or(|i| i.closed) {
            return;
        }
        if self.close(CLEANUP_BUDGET).is_err() || self.inner.as_ref().is_some_and(|i| !i.idle()) {
            let inner = self.inner.take().unwrap();
            inner.owner.mark_stopped();
            // Uncertain queue use must not release source/native/window owners.
            std::mem::forget(inner);
        }
    }
}

impl D3D12Runtime {
    /// Create one Windows output for this runtime's DIRECT queue. Both owners
    /// retain the native Device; the presenter retains the Window through teardown.
    pub fn for_window(
        config: D3D12RuntimeConfig,
        window: Arc<Window>,
        presentation: PresentationConfig,
    ) -> Result<(Self, D3D12Presenter), Error> {
        if !presentation.has_valid_capacities() {
            return Err(Error::invalid("invalid presentation capacities"));
        }
        let runtime = Self::new(config)?;
        let owner = Arc::clone(&runtime.device);
        // SAFETY: Owned supported factory interface, no process-wide debug setter.
        let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(0) }
            .map_err(|e| owner.native("CreateDXGIFactory2(presentation)", e))?;
        let mut tearing = windows_core::BOOL(0);
        let tearing_supported = if let Ok(factory5) = factory.cast::<IDXGIFactory5>() {
            // SAFETY: Feature and BOOL output size match the native query.
            unsafe {
                factory5.CheckFeatureSupport(
                    DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                    std::ptr::from_mut(&mut tearing).cast(),
                    size_of_val(&tearing) as u32,
                )
            }
            .is_ok()
                && tearing.as_bool()
        } else {
            false
        };
        if !presentation.vsync && !tearing_supported {
            return Err(Error::unsupported(
                "DXGI VSync off requires tearing support",
            ));
        }
        let size = window.inner_size();
        let registry_owner = runtime
            .pool
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .owner_id();
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(presentation.max_frames)
            .map_err(|_| Error::capacity("presentation slot reservation failed"))?;
        for _ in 0..presentation.max_frames {
            slots.push(Slot::new(&owner)?);
        }
        let mut samples = VecDeque::new();
        samples
            .try_reserve_exact(presentation.max_frames)
            .map_err(|_| Error::capacity("presentation sample reservation failed"))?;
        let mut inner = Inner {
            slots,
            chain: None,
            program: Program::new(&owner)?,
            factory,
            config: presentation,
            desired: (size.width, size.height),
            dirty: true,
            force_resize: false,
            closed: false,
            tearing: tearing_supported,
            registry_owner,
            retry: None,
            profiling: false,
            samples,
            sequence: 0,
            #[cfg(test)]
            cancel_next: false,
            owner,
            _window: window,
        };
        inner.prepare()?;
        Ok((runtime, D3D12Presenter { inner: Some(inner) }))
    }

    /// Sample linear RGBA8 output into the current Window's SRGB RTV. The source
    /// extent must match; contents and revision are preserved. Same-queue ordering
    /// consumes pending Graph output without a CPU wait. NotReady leaves the
    /// request unaccepted; the next call may retry or replace its source image.
    pub fn present(
        &mut self,
        presenter: &mut D3D12Presenter,
        image: &ImportImage,
    ) -> Result<PresentOutcome, Error> {
        let inner = presenter.active()?;
        if !Arc::ptr_eq(&self.device, &inner.owner)
            || self
                .pool
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .owner_id()
                != inner.registry_owner
        {
            return Err(Error::wrong_owner("presenter belongs to another runtime"));
        }
        let prepare_started = inner.profiling.then(Instant::now);
        if let Some(outcome) = inner.prepare()? {
            return Ok(outcome);
        }
        let extent = inner.chain.as_ref().expect("ready chain").extent;
        if image.desc.format != ImageFormat::Rgba8Unorm
            || !image.desc.is_single_subresource()
            || (image.desc.width, image.desc.height) != extent
        {
            return Err(Error::invalid(
                "presentation requires matching single-subresource linear RGBA8 extent",
            ));
        }
        let owner = Arc::clone(&inner.owner);
        let _gate = owner.queue_gate.lock().map_err(|_| {
            owner.mark_stopped();
            Error::invalid("D3D12 queue transaction lock was poisoned")
        })?;
        owner.ensure_active()?;
        let registry = self.pool.registry.lock().unwrap_or_else(|e| e.into_inner());
        let source = registry.presentation_source(image)?;
        if let Some(index) = inner.retry
            && inner.slots[index].image.as_ref() != Some(image)
        {
            // A caller may replace any unaccepted request. Retain the prior
            // source/allocator until its actual rendering Fence completes; no
            // new work is submitted and no implicit wait is introduced meanwhile.
            if !inner.slots[index].completed(&owner)? {
                return Ok(PresentOutcome::NotReady);
            }
            inner.retry = None;
            inner.poll()?;
        }
        let chain = inner.chain.as_ref().expect("ready chain");
        let index = if let Some(index) = inner.retry {
            index
        } else {
            if inner.profiling
                && inner.samples.len() + inner.slots.iter().filter(|s| s.pending).count()
                    >= inner.config.max_frames
            {
                return Err(Error::capacity(
                    "consume completed presentation timing samples",
                ));
            }
            let Some(index) = inner.slots.iter().position(|slot| !slot.pending) else {
                return Ok(PresentOutcome::NotReady);
            };
            let acquire_started = inner.profiling.then(Instant::now);
            // SAFETY: This immutable index query does not transfer native ownership
            // or acquire a semaphore. Slot/backbuffer availability is checked below.
            let target = unsafe { chain.raw.GetCurrentBackBufferIndex() } as usize;
            let acquire = acquire_started.map_or(Duration::ZERO, |s| s.elapsed());
            if target >= chain.targets.len() {
                return Err(Error::invalid("DXGI returned an invalid backbuffer index"));
            }
            if inner
                .slots
                .iter()
                .any(|slot| slot.pending && slot.target == target)
            {
                return Ok(PresentOutcome::NotReady);
            }
            #[cfg(test)]
            if std::mem::take(&mut inner.cancel_next) {
                return Ok(PresentOutcome::NotReady);
            }
            let sequence = inner
                .sequence
                .checked_add(1)
                .ok_or_else(|| Error::capacity("presentation sequence exhausted"))?;
            inner.slots[index].record(
                &owner,
                &inner.program,
                chain,
                target,
                source,
                image,
                sequence,
                inner.profiling,
                prepare_started
                    .map_or(Duration::ZERO, |s| s.elapsed())
                    .saturating_sub(acquire),
                acquire,
            )?;
            inner.sequence = sequence;
            inner.retry = Some(index);
            // Queue acceptance advances no image contents/revision: commands
            // return COMMON and read-only sampling has no canonical write.
            index
        };
        drop(registry);
        let present_started = inner.profiling.then(Instant::now);
        let flags = DXGI_PRESENT_DO_NOT_WAIT
            | if inner.config.vsync {
                0
            } else {
                DXGI_PRESENT_ALLOW_TEARING
            };
        // SAFETY: Backbuffer is transitioned to PRESENT by the prior accepted
        // DIRECT submission. Flags match the windowed tearing creation contract.
        let result = unsafe { chain.raw.Present(u32::from(inner.config.vsync), flags) };
        inner.slots[index]
            .add_present_time(present_started.map_or(Duration::ZERO, |s| s.elapsed()));
        if result == DXGI_ERROR_WAS_STILL_DRAWING || result == DXGI_STATUS_OCCLUDED {
            return Ok(PresentOutcome::NotReady);
        }
        if result == DXGI_STATUS_MODE_CHANGED || result == DXGI_STATUS_MODE_CHANGE_IN_PROGRESS {
            inner.dirty = true;
            inner.force_resize = true;
            return Ok(PresentOutcome::RecreateRequired);
        }
        if let Err(error) = result.ok() {
            // A rendering submit already happened. Keep the complete slot owner
            // on every present failure; only its real Fence can release it.
            return Err(owner.native("IDXGISwapChain::Present", error));
        }
        if result.0 != 0 {
            return Err(Error::unsupported("unrecognized DXGI presentation status"));
        }
        // A distinct marker follows DXGI's queue present operation. Its value
        // proves app queue use ended; DXGI owns any remaining display references.
        inner.slots[index].mark_presented(&owner)?;
        inner.retry = None;
        Ok(PresentOutcome::Presented { suboptimal: false })
    }
}

#[cfg(test)]
mod tests;
