//! Exclusive prepared command/descriptor/query slots and source GPU lifetime.

use super::super::metrics::TimestampPair;
use super::*;

pub(super) struct Slot {
    allocator: NativeObject<ID3D12CommandAllocator>,
    list: NativeObject<ID3D12GraphicsCommandList>,
    fence: NativeObject<ID3D12Fence>,
    srv: DescriptorHeap,
    source: Option<PhysicalRef>,
    pub(super) image: Option<ImportImage>,
    pub(super) target: usize,
    pub(super) pending: bool,
    pub(super) sequence: u64,
    next: u64,
    wait_value: u64,
    timestamps: Option<TimestampPair>,
    metrics: Option<(Instant, PresentationMetrics)>,
}
impl Slot {
    pub(super) fn new(owner: &DeviceOwner) -> Result<Self, Error> {
        let allocator: NativeObject<ID3D12CommandAllocator> =
            object(owner, "CreateCommandAllocator(presentation)", || {
                // SAFETY: Fresh exclusively owned DIRECT allocator on this Device.
                unsafe {
                    owner
                        .raw
                        .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                }
            })?;
        let list: NativeObject<ID3D12GraphicsCommandList> =
            object(owner, "CreateCommandList(presentation)", || {
                // SAFETY: Matching fresh DIRECT allocator and live Device.
                unsafe {
                    owner.raw.CreateCommandList(
                        0,
                        D3D12_COMMAND_LIST_TYPE_DIRECT,
                        &*allocator,
                        None,
                    )
                }
            })?;
        // SAFETY: A new list is open; close before the first reset.
        unsafe { list.Close() }
            .ok()
            .map_err(|e| owner.native("Close(presentation initial)", e))?;
        let fence = object(owner, "CreateFence(presentation)", || {
            // SAFETY: Unique per-slot Fence starts at zero and uses increasing values.
            unsafe { owner.raw.CreateFence(0, D3D12_FENCE_FLAG_NONE) }
        })?;
        let srv = DescriptorHeap::new(owner, 1, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, true)?;
        Ok(Self {
            allocator,
            list,
            fence,
            srv,
            source: None,
            image: None,
            target: 0,
            pending: false,
            sequence: 0,
            next: 0,
            wait_value: 0,
            timestamps: None,
            metrics: None,
        })
    }
    pub(super) fn prepare_timestamps(&mut self, owner: &DeviceOwner) -> Result<(), Error> {
        if self.timestamps.is_none() {
            self.timestamps = Some(TimestampPair::new(owner)?);
        }
        Ok(())
    }
    pub(super) fn completed(&self, owner: &DeviceOwner) -> Result<bool, Error> {
        if !self.pending {
            return Ok(true);
        }
        // SAFETY: The retained slot owns this Fence and its submitted target.
        let value = unsafe { self.fence.GetCompletedValue() };
        if value == u64::MAX {
            return Err(owner.removed());
        }
        Ok(value >= self.wait_value)
    }
    pub(super) fn clear_recording(&mut self, owner: &DeviceOwner) -> Result<(), Error> {
        self.begin_recording(owner)?;
        // SAFETY: The empty list is open and discards prior generation references.
        unsafe { self.list.Close() }
            .ok()
            .map_err(|e| owner.native("Close(presentation empty)", e))?;
        Ok(())
    }
    fn begin_recording(&mut self, owner: &DeviceOwner) -> Result<(), Error> {
        if self.pending {
            return Err(Error::capacity("presentation slot is pending"));
        }
        // SAFETY: Real Fence completion preceded this reset; no recording or GPU
        // consumer remains, and the list is closed. Discard native buffer refs.
        unsafe { self.allocator.Reset() }
            .ok()
            .map_err(|e| owner.native("Reset(presentation allocator)", e))?;
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::Reset, 1);
        // SAFETY: The uniquely owned allocator/list pair is idle and matching.
        unsafe { self.list.Reset(&*self.allocator, None) }
            .ok()
            .map_err(|e| owner.native("Reset(presentation list)", e))?;
        #[cfg(feature = "memory-profiling")]
        crate::measurement::record_native(crate::measurement::NativeEvent::Reset, 1);
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record(
        &mut self,
        owner: &DeviceOwner,
        program: &Program,
        chain: &Chain,
        target: usize,
        source: PhysicalRef,
        image: &ImportImage,
        sequence: u64,
        profiling: bool,
        cpu_prepare: Duration,
        cpu_acquire: Duration,
    ) -> Result<(), Error> {
        let next = self
            .next
            .checked_add(2)
            .ok_or_else(|| Error::capacity("presentation Fence serial exhausted"))?;
        let record_started = profiling.then(Instant::now);
        self.begin_recording(owner)?;
        let srv_desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING as u32,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D12_TEX2D_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                    PlaneSlice: 0,
                    ResourceMinLODClamp: 0.0,
                },
            },
        };
        // SAFETY: Preparation validated exact initialized RGBA8 source/extent.
        // The descriptor is idle and source is retained through actual completion.
        unsafe {
            owner.raw.CreateShaderResourceView(
                source.raw(),
                Some(&srv_desc),
                self.srv.raw.GetCPUDescriptorHandleForHeapStart(),
            )
        };
        let mut source_state = D3D12_RESOURCE_STATE_COMMON;
        let mut target_state = D3D12_RESOURCE_STATE_PRESENT;
        if profiling {
            // SAFETY: This slot's query/readback is exclusively leased and its
            // previous GPU use completed before reset.
            unsafe {
                self.timestamps
                    .as_ref()
                    .expect("prepared profiling")
                    .begin(&self.list)
            };
        }
        transition(
            &self.list,
            source.raw(),
            &mut source_state,
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
        );
        transition(
            &self.list,
            &chain.targets[target],
            &mut target_state,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
        );
        let viewport = D3D12_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: chain.extent.0 as f32,
            Height: chain.extent.1 as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let rect = D3D12_RECT {
            left: 0,
            top: 0,
            right: chain.extent.0 as i32,
            bottom: chain.extent.1 as i32,
        };
        let rtv = chain.rtv(target);
        // SAFETY: The prepared fullscreen program consumes only t0, emits one
        // pixel per target sample, and both resources are in the required states.
        // All native owners remain anchored by this presenter and pending slot.
        unsafe {
            self.list.SetPipelineState(&*program.pipeline);
            self.list.SetGraphicsRootSignature(&*program.signature);
            self.list.SetDescriptorHeaps(&[Some(self.srv.raw.clone())]);
            self.list.SetGraphicsRootDescriptorTable(
                0,
                self.srv.raw.GetGPUDescriptorHandleForHeapStart(),
            );
            self.list.OMSetRenderTargets(1, Some(&rtv), false, None);
            self.list.RSSetViewports(&[viewport]);
            self.list.RSSetScissorRects(&[rect]);
            self.list.IASetPrimitiveTopology(D3D12_PRIMITIVE_TOPOLOGY(
                D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            ));
            self.list.DrawInstanced(3, 1, 0, 0);
        }
        transition(
            &self.list,
            source.raw(),
            &mut source_state,
            D3D12_RESOURCE_STATE_COMMON,
        );
        transition(
            &self.list,
            &chain.targets[target],
            &mut target_state,
            D3D12_RESOURCE_STATE_PRESENT,
        );
        if profiling {
            // SAFETY: The matching query pair's begin is recorded in this list;
            // end resolves to this slot's retained idle readback arena.
            unsafe { self.timestamps.as_ref().unwrap().end(&self.list) };
        }
        // SAFETY: The uniquely owned list has completed its recording.
        unsafe { self.list.Close() }
            .ok()
            .map_err(|e| owner.native("Close(presentation)", e))?;
        let cpu_record = record_started.map_or(Duration::ZERO, |s| s.elapsed());
        self.source = Some(source);
        self.image = Some(image.clone());
        self.target = target;
        self.sequence = sequence;
        self.next = next;
        self.wait_value = next - 1;
        self.pending = true;
        let submit_started = profiling.then(Instant::now);
        let command: ID3D12CommandList = self.list.clone().into();
        // SAFETY: Queue gate and source registry transaction remain held by the
        // caller. Commands, operands, descriptors and allocator are all retained.
        unsafe { owner.queue.ExecuteCommandLists(&[Some(command)]) };
        // SAFETY: Increasing target covers the immediately preceding submission.
        if let Err(error) = unsafe { owner.queue.Signal(&*self.fence, self.wait_value) }.ok() {
            owner.mark_stopped();
            return Err(owner.native("Signal(presentation render)", error));
        }
        if let Some(started) = submit_started {
            self.metrics = Some((
                started,
                PresentationMetrics {
                    cpu_acquire,
                    cpu_prepare,
                    cpu_record,
                    cpu_submit: started.elapsed(),
                    cpu_present: Duration::ZERO,
                    cpu_retire: Duration::ZERO,
                    gpu: GpuTiming::NotReady,
                },
            ));
        }
        Ok(())
    }
    pub(super) fn add_present_time(&mut self, time: Duration) {
        if let Some((_, metrics)) = &mut self.metrics {
            metrics.cpu_present += time;
        }
    }
    pub(super) fn mark_presented(&mut self, owner: &DeviceOwner) -> Result<(), Error> {
        // SAFETY: Queue gate remains held. DXGI schedules on this same DIRECT
        // queue, so this marker follows both rendering and accepted presentation.
        if let Err(error) = unsafe { owner.queue.Signal(&*self.fence, self.next) }.ok() {
            owner.mark_stopped();
            return Err(owner.native("Signal(presentation accepted)", error));
        }
        self.wait_value = self.next;
        Ok(())
    }
    pub(super) fn retire(
        &mut self,
        owner: &DeviceOwner,
        samples: &mut VecDeque<PresentationMetrics>,
    ) -> Result<bool, Error> {
        if !self.pending || !self.completed(owner)? {
            return Ok(false);
        }
        let retire_started = self.metrics.as_ref().map(|_| Instant::now());
        if let Some((started, mut metrics)) = self.metrics.take() {
            // SAFETY: Real Fence completion above covers query resolve and no CPU
            // consumer aliases the private mapped slot. Read never waits/grows.
            metrics.gpu = unsafe {
                self.timestamps
                    .as_ref()
                    .unwrap()
                    .read(owner, started.elapsed())
            }?;
            metrics.cpu_retire = retire_started.unwrap().elapsed();
            // Capacity was reserved before acquiring this sample; each pending
            // sample occupies one unit in the same fixed total budget.
            samples.push_back(metrics);
        }
        self.source = None;
        self.image = None;
        self.pending = false;
        Ok(true)
    }
}
