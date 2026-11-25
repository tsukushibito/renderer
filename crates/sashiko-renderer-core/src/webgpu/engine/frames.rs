use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, atomic::AtomicU8};
use std::task::Waker;

use super::{Error, FrameId, registry::Snapshot, transfer::Readbacks};
use crate::graph::{BufferHandle, ImageHandle, ImportBuffer, ImportImage, PassHandle};

type Exports = Rc<RefCell<Vec<(usize, Snapshot)>>>;

pub(crate) struct Submission {
    id: FrameId,
    graph: u64,
    exports: Option<Exports>,
}

impl Submission {
    pub(super) fn new(id: FrameId, graph: u64, exports: Option<Exports>) -> Self {
        Self { id, graph, exports }
    }
    pub(crate) fn id(&self) -> FrameId {
        self.id
    }
    pub(crate) fn graph_id(&self) -> u64 {
        self.graph
    }

    fn get(&self, graph: u64, index: usize) -> Result<Snapshot, Error> {
        if self.graph != graph {
            return Err(Error::webgpu(
                super::Kind::WrongOwner,
                "export belongs to a different graph",
            ));
        }
        self.exports
            .as_ref()
            .and_then(|e| {
                let values = e.borrow();
                let i = values
                    .binary_search_by_key(&index, |(logical, _)| *logical)
                    .ok()?;
                Some(values[i].1.clone())
            })
            .ok_or_else(|| Error::invalid("resource was not exported"))
    }
    pub(crate) fn export_buffer(&self, h: BufferHandle) -> Result<ImportBuffer, Error> {
        match self.get(h.graph, h.index)? {
            Snapshot::Buffer(v) => Ok(v),
            _ => Err(Error::invalid("expected buffer export")),
        }
    }
    pub(crate) fn export_image(&self, h: ImageHandle) -> Result<ImportImage, Error> {
        match self.get(h.graph, h.index)? {
            Snapshot::Image(v) => Ok(v),
            _ => Err(Error::invalid("expected image export")),
        }
    }
}

impl Drop for Submission {
    fn drop(&mut self) {
        if let Some(exports) = self.exports.as_ref() {
            exports.borrow_mut().clear();
        }
    }
}

pub(super) type WakeSlot = Arc<Mutex<Option<Waker>>>;

pub(super) struct ResultSlot {
    pub(super) data: RefCell<Readbacks>,
    pub(super) mapped: Arc<AtomicU8>,
    pub(super) waker: WakeSlot,
}

impl ResultSlot {
    pub(super) fn new(device: &wgpu::Device, bytes: u64, passes: usize) -> Result<Self, Error> {
        Ok(Self {
            data: RefCell::new(Readbacks::new(device, bytes, passes)?),
            mapped: Arc::new(AtomicU8::new(0)),
            waker: Arc::default(),
        })
    }
}

pub(crate) struct Completion {
    pub(super) id: FrameId,
    pub(super) serial: u64,
    pub(super) graph: u64,
    pub(super) slot: Rc<ResultSlot>,
}
impl Completion {
    pub(crate) fn id(&self) -> FrameId {
        self.id
    }
    fn index(&self, pass: PassHandle) -> Result<usize, Error> {
        if self.graph != pass.graph {
            return Err(Error::webgpu(
                super::Kind::WrongOwner,
                "readback belongs to a different graph",
            ));
        }
        Ok(pass.index)
    }
    pub(crate) fn readback_len(&self, pass: PassHandle) -> Result<usize, Error> {
        self.slot.data.borrow().readback_len(self.index(pass)?)
    }
    pub(crate) fn readback_into(&mut self, pass: PassHandle, out: &mut [u8]) -> Result<(), Error> {
        self.slot
            .data
            .borrow_mut()
            .readback_into(self.index(pass)?, out)
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        self.slot.data.borrow_mut().unmap();
        self.slot.waker.lock().expect("WebGPU waker").take();
    }
}

pub(super) struct FrameStorage {
    pub(super) results: Vec<Rc<ResultSlot>>,
    pub(super) exports: Vec<Exports>,
    pub(super) pending: Vec<Completion>,
}
impl FrameStorage {
    pub(super) fn new(
        device: &wgpu::Device,
        limits: crate::runtime::RuntimeLimits,
        readback: u64,
    ) -> Result<Self, Error> {
        let results = (0..limits.results)
            .map(|_| ResultSlot::new(device, readback, limits.graph.passes).map(Rc::new))
            .collect::<Result<_, _>>()?;
        let count = limits
            .frames
            .checked_add(limits.results)
            .ok_or_else(|| Error::invalid("WebGPU metadata slot count overflow"))?;
        let exports = (0..count)
            .map(|_| Rc::new(RefCell::new(Vec::with_capacity(limits.graph.resources))))
            .collect();
        Ok(Self {
            results,
            exports,
            pending: Vec::with_capacity(limits.frames),
        })
    }
}
