use super::{compute::ComputePrograms, raster::Raster};
use crate::capacity::{
    CapacityRecovery as Recovery, RuntimeCapacityError, RuntimeCapacityKind as Capacity, WorkingSet,
};
use crate::graph::{
    BufferDesc, CompiledGraph, ImageDesc, ImportBuffer, ImportImage, Operation, ResourceKey,
};
use crate::program::SamplerDescriptor;
use crate::runtime::{ExecutionError as Error, ExecutionErrorKind as Kind, FrameId, RuntimeLimits};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::task::Waker;

mod frames;
mod proof;
mod registry;
mod transfer;
pub(super) use super::bindings::texture_format as format;
pub(crate) use frames::{Completion, Submission};
use frames::{FrameStorage, WakeSlot};
pub(super) use registry::Resources;
use registry::{Registry, Snapshot};
use transfer::Upload;

fn unsupported(message: &'static str) -> Error {
    Error::webgpu(Kind::Unsupported, message)
}
fn capacity(kind: Capacity, recovery: Recovery, limit: usize, required: usize) -> Error {
    Error::with_capacity(
        "WebGPU prepared capacity exhausted",
        RuntimeCapacityError::new(kind, recovery, Some(limit as u64), Some(required as u64)),
    )
}

struct Health {
    failure: Mutex<Option<(Kind, String)>>,
    wakers: Mutex<Vec<WakeSlot>>,
}
impl Health {
    fn fail(&self, kind: Kind, message: String) {
        let mut failure = self.failure.lock().expect("WebGPU failure");
        if failure.is_none() {
            *failure = Some((kind, message));
        }
        drop(failure);
        for slot in self.wakers.lock().expect("WebGPU wakers").iter() {
            if let Some(waker) = slot.lock().expect("WebGPU waker").take() {
                waker.wake();
            }
        }
    }
    fn check(&self) -> Result<(), Error> {
        match self.failure.lock().expect("WebGPU failure").as_ref() {
            Some((kind, reason)) => Err(Error::webgpu(*kind, reason.clone())),
            None => Ok(()),
        }
    }
    fn observe(&self, frame: &FrameStorage) {
        *self.wakers.lock().expect("WebGPU wakers") =
            frame.results.iter().map(|s| s.waker.clone()).collect();
    }
}

/// Queue ownership is single-threaded in the browser. Every accepted resource revision
/// and export is prepared before submit. Queue order supplies synchronization between
/// registered imports while transient storage is reusable only after GPU completion.
pub(crate) struct Engine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    owner: u64,
    serial: u64,
    completed: Arc<AtomicU64>,
    health: Arc<Health>,
    _errors: super::errors::Subscription,
    limits: RuntimeLimits,
    registry: Registry,
    indices: proof::IndexBounds,
    updates: Vec<(usize, Snapshot)>,
    upload: Upload,
    raster: Raster,
    compute: ComputePrograms,
    frames: FrameStorage,
}

impl Engine {
    pub(crate) fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        limits: RuntimeLimits,
        set: WorkingSet<'_>,
        upload_bytes: u64,
        readback_bytes: u64,
    ) -> Result<Self, Error> {
        let owner = crate::graph::next_id();
        let health = Arc::new(Health {
            failure: Mutex::default(),
            wakers: Mutex::default(),
        });
        let observer = Arc::clone(&health);
        let errors = super::errors::observe(
            &device,
            Arc::new(move |lost, message| {
                observer.fail(if lost { Kind::DeviceLost } else { Kind::Native }, message)
            }),
        );
        let mut registry = Registry::new(owner, limits)?;
        registry.prepare(&device, limits, set.resources)?;
        registry.prepare_samplers(&device, set.samplers);
        let frames = FrameStorage::new(&device, limits, readback_bytes)?;
        health.observe(&frames);
        Ok(Self {
            owner,
            serial: 0,
            completed: Arc::new(AtomicU64::new(0)),
            health,
            _errors: errors,
            limits,
            registry,
            indices: proof::IndexBounds::new(limits, set.raster),
            updates: Vec::with_capacity(limits.graph.resources),
            upload: Upload::new(&device, upload_bytes)?,
            raster: Raster::new(&device, limits, set)?,
            compute: ComputePrograms::new(&device, set.compute, limits.graph.bindings.max(1))?,
            frames,
            device,
            queue,
        })
    }
    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub(crate) fn prepare(
        &mut self,
        limits: RuntimeLimits,
        set: WorkingSet<'_>,
        upload: u64,
        readback: u64,
    ) -> Result<(), Error> {
        self.health.check()?;
        if !self.frames.pending.is_empty() {
            return Err(capacity(
                Capacity::ExecutionSlots,
                Recovery::Poll,
                0,
                self.frames.pending.len(),
            ));
        }
        if self.frames.results.iter().any(|s| Rc::strong_count(s) > 1)
            || self.frames.exports.iter().any(|s| Rc::strong_count(s) > 1)
        {
            return Err(capacity(
                Capacity::WorkingSet,
                Recovery::ReleaseOwners,
                0,
                1,
            ));
        }
        let next_upload = Upload::new(&self.device, upload)?;
        let raster = Raster::new(&self.device, limits, set)?;
        let compute =
            ComputePrograms::new(&self.device, set.compute, limits.graph.bindings.max(1))?;
        let frames = FrameStorage::new(&self.device, limits, readback)?;
        let updates = Vec::with_capacity(limits.graph.resources);
        self.registry.prepare(&self.device, limits, set.resources)?;
        self.registry.prepare_samplers(&self.device, set.samplers);
        self.health.observe(&frames);
        self.upload = next_upload;
        self.raster = raster;
        self.compute = compute;
        self.frames = frames;
        self.indices = proof::IndexBounds::new(limits, set.raster);
        self.limits = limits;
        self.updates = updates;
        Ok(())
    }
    pub(crate) fn prepare_samplers(&mut self, s: &[SamplerDescriptor]) -> Result<(), Error> {
        self.health.check()?;
        self.registry.prepare_samplers(&self.device, s);
        Ok(())
    }
    pub(crate) fn check_texture_support(&self, desc: ImageDesc, linear: bool) -> Result<(), Error> {
        self.health.check()?;
        registry::check_image(&self.device, desc, linear)
    }
    pub(crate) fn register_buffer(&mut self, desc: BufferDesc) -> Result<ResourceKey, Error> {
        self.health.check()?;
        self.registry
            .register_buffer(desc, self.completed.load(Ordering::Acquire))
    }
    pub(crate) fn refresh_buffer(&self, key: ResourceKey) -> Result<ImportBuffer, Error> {
        self.health.check()?;
        self.registry.refresh_buffer(key)
    }
    pub(crate) fn refresh_image(&self, key: ResourceKey) -> Result<ImportImage, Error> {
        self.health.check()?;
        self.registry.refresh_image(key)
    }
    pub(crate) fn texture(&self, image: &ImportImage) -> Result<&wgpu::Texture, Error> {
        self.health.check()?;
        self.registry.texture(image)
    }
    pub(crate) fn mark_presented(&self, image: &ImportImage) -> Result<(), Error> {
        self.registry.mark_presented(&self.queue, image)
    }
    pub(crate) fn delete(&mut self, key: ResourceKey) -> Result<(), Error> {
        self.registry.delete(key)
    }
    pub(crate) fn pending_frames(&self) -> usize {
        self.frames.pending.len()
    }

    pub(crate) fn submit(&mut self, graph: &CompiledGraph) -> Result<Submission, Error> {
        let result = self.record_and_submit(graph);
        if result.is_err() {
            self.indices.cancel(&mut self.registry.proofs);
            self.updates.clear();
        }
        result
    }

    fn record_and_submit(&mut self, graph: &CompiledGraph) -> Result<Submission, Error> {
        self.health.check()?;
        if self.frames.pending.len() == self.limits.frames {
            return Err(capacity(
                Capacity::ExecutionSlots,
                Recovery::Poll,
                self.limits.frames,
                self.limits.frames + 1,
            ));
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or_else(|| unsupported("WebGPU frame identity exhausted"))?;
        let result = self
            .frames
            .results
            .iter()
            .find(|s| Rc::strong_count(s) == 1)
            .cloned()
            .ok_or_else(|| {
                capacity(
                    Capacity::ResultSlots,
                    Recovery::ReleaseResults,
                    self.limits.results,
                    self.limits.results + 1,
                )
            })?;
        let exports = if graph.exports.is_empty() {
            None
        } else {
            Some(
                self.frames
                    .exports
                    .iter()
                    .find(|s| Rc::strong_count(s) == 1)
                    .cloned()
                    .ok_or_else(|| {
                        capacity(
                            Capacity::ExportMetadata,
                            Recovery::ReleaseOwners,
                            self.frames.exports.len(),
                            self.frames.exports.len() + 1,
                        )
                    })?,
            )
        };
        self.updates.clear();
        self.registry
            .bind(graph, self.completed.load(Ordering::Acquire))?;
        self.registry.snapshots(graph, &mut self.updates)?;
        self.registry.prove(graph, &mut self.indices)?;
        self.upload.clear();
        self.raster.begin();
        result.data.borrow_mut().reset();
        let resources = self.registry.resources();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for &p in &graph.order {
            match &graph.passes[p].operation {
                Operation::Raster { .. } | Operation::AttachmentPass { .. } => {
                    self.raster
                        .record(&self.device, &resources, graph, p, &mut encoder)?
                }
                Operation::Dispatch {
                    program,
                    bindings,
                    workgroups,
                } => self.compute.record(
                    &self.device,
                    &mut encoder,
                    graph,
                    *program,
                    bindings.clone(),
                    *workgroups,
                    &resources,
                )?,
                _ => transfer::record(
                    graph,
                    p,
                    &resources,
                    &mut encoder,
                    &mut self.upload,
                    &mut result.data.borrow_mut(),
                )?,
            }
        }
        // All fallible binding, staging and snapshot work precedes any queue mutation.
        let commands = encoder.finish();
        self.upload.flush(&self.queue);
        self.raster.flush_constants(&self.queue);
        self.queue.submit([commands]);
        self.registry.commit(serial, &self.updates);
        self.indices.commit(graph, &mut self.registry.proofs);
        if let Some(e) = &exports {
            let mut values = e.borrow_mut();
            values.clear();
            values.extend(
                self.updates
                    .iter()
                    .filter(|(i, _)| graph.has_export(*i))
                    .cloned(),
            );
        }
        self.updates.clear();
        self.serial = serial;
        let id = FrameId::new(self.owner, serial);
        result.mapped.store(0, Ordering::Release);
        result
            .data
            .borrow()
            .start_map(Arc::clone(&result.mapped), Arc::clone(&result.waker));
        let completed = Arc::clone(&self.completed);
        let wake = Arc::clone(&result.waker);
        self.queue.on_submitted_work_done(move || {
            completed.fetch_max(serial, Ordering::AcqRel);
            if let Some(waker) = wake.lock().expect("WebGPU waker").take() {
                waker.wake();
            }
        });
        self.frames.pending.push(Completion {
            id,
            graph: graph.id,
            serial,
            slot: result,
        });
        Ok(Submission::new(id, graph.id, exports))
    }

    pub(crate) fn poll_frame(&mut self, id: Option<FrameId>) -> Result<Option<Completion>, Error> {
        self.health.check()?;
        let index = match id {
            Some(id) => self
                .frames
                .pending
                .iter()
                .position(|f| f.id == id)
                .ok_or_else(|| Error::invalid("unknown pending WebGPU frame"))?,
            None => {
                let Some(index) = self.frames.pending.iter().position(|f| self.ready(f)) else {
                    return Ok(None);
                };
                index
            }
        };
        let pending = &self.frames.pending[index];
        if !self.ready(pending) {
            return Ok(None);
        }
        let result = self.frames.pending.remove(index);
        if result.slot.mapped.load(Ordering::Acquire) == 2 {
            return Err(Error::webgpu(
                Kind::Native,
                "WebGPU readback mapping failed",
            ));
        }
        Ok(Some(result))
    }
    fn ready(&self, frame: &Completion) -> bool {
        self.completed.load(Ordering::Acquire) >= frame.serial
            && frame.slot.mapped.load(Ordering::Acquire) != 0
    }
    pub(crate) fn set_frame_waker(&self, id: FrameId, waker: &Waker) -> Result<(), Error> {
        self.health.check()?;
        let frame = self
            .frames
            .pending
            .iter()
            .find(|f| f.id == id)
            .ok_or_else(|| Error::invalid("unknown pending WebGPU frame"))?;
        frame
            .slot
            .waker
            .lock()
            .expect("WebGPU waker")
            .replace(waker.clone());
        if self.ready(frame) {
            waker.wake_by_ref();
        }
        Ok(())
    }
}
