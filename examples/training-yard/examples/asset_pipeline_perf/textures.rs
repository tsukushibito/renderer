use super::*;
use sashiko_asset::{BlobRef, ImageBlob, ObjectBytes, ObjectStore};
use sashiko_renderer::pipeline::AssetPreparation;
use sashiko_renderer_core::{graph::ImageDesc, runtime::FrameId};
use std::{
    io::{self, Read},
    sync::{Arc, mpsc},
    thread,
};

pub(super) const COUNT: usize = 4;
const WIDTH: u32 = 128;
const IO_RATE: u64 = 1_048_576;
const UPLOAD_LIMIT: usize = COUNT * WIDTH as usize * WIDTH as usize * 4;
pub(super) struct Tile {
    pub image: ManagedImage,
    pub mip: u32,
    pub channel: usize,
}
struct Pending {
    tile: Tile,
    id: FrameId,
    bytes: usize,
}

fn files(root: &Path) -> Result<Vec<BlobRef>> {
    fs::create_dir_all(root.join("objects"))?;
    let mut refs = Vec::with_capacity(2 * COUNT);
    for (size, channel) in [(WIDTH / 2, 1), (WIDTH, 2)] {
        for lane in 0..COUNT {
            let mut pixel = [0, 0, 0, 255];
            pixel[0] = lane as u8;
            pixel[channel] = 255;
            let blob = ImageBlob {
                width: size,
                height: size,
                mip_levels: 1,
                srgb: false,
                pixels: pixel.repeat((size * size) as usize),
            }
            .encode()?;
            let entry = BlobRef::of(&blob);
            fs::write(root.join("objects").join(entry.hash.to_string()), blob)?;
            refs.push(entry);
        }
    }
    Ok(refs)
}

fn paced_read(path: &Path, bytes: u64) -> io::Result<Vec<u8>> {
    let start = Instant::now();
    let mut file = fs::File::open(path)?.take(bytes + 1);
    let mut buffer = Vec::with_capacity(bytes as usize);
    let mut chunk = [0; 4096];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        let scheduled = Duration::from_secs_f64(buffer.len() as f64 / IO_RATE as f64);
        if let Some(remaining) = scheduled.checked_sub(start.elapsed()) {
            thread::sleep(remaining);
        }
    }
    Ok(buffer)
}

#[derive(Default)]
struct Memory {
    registered: u64,
    native: u64,
    staging: u64,
    reserved: Option<u64>,
}
impl Memory {
    fn sample(&mut self, renderer: &mut impl AssetPreparation) -> Result<()> {
        let rt = renderer.asset_renderer().runtime();
        #[cfg(feature = "vulkan")]
        if let Some(vk) = rt.as_vulkan() {
            let m = vk.memory_stats()?;
            self.registered = self.registered.max(m.registered_logical_bytes);
            self.native = self.native.max(m.vma_allocation_bytes);
            self.staging = self.staging.max(m.upload.capacity_bytes);
            self.reserved = Some(m.upload.peak_reserved_bytes);
        }
        #[cfg(all(windows, feature = "d3d12"))]
        if let Some(dx) = rt.as_d3d12() {
            let m = dx.memory_stats()?;
            let committed = m.committed.ok_or("enable memory-profiling")?;
            self.registered = self.registered.max(m.registered_logical_bytes);
            self.native = self.native.max(
                committed.default_heap.allocation_bytes
                    + committed.upload_heap.allocation_bytes
                    + committed.readback_heap.allocation_bytes,
            );
            self.staging = self.staging.max(m.upload.capacity_bytes);
            self.reserved = m.upload.peak_used_bytes;
        }
        Ok(())
    }
}

pub(super) fn run(
    config: RuntimeConfig,
    fixture: &Path,
    scratch: &Path,
    mode: &str,
) -> Result<Value> {
    let root = scratch.join("images");
    let refs = files(&root)?;
    let mut yard = Yard::with_asset_catalog(fixture, 2)?;
    let mut renderer = renderer(config, &mut yard, COUNT + 1)?;
    let desc = ImageDesc {
        mip_levels: 2,
        ..ImageDesc::rgba8(WIDTH, WIDTH)
    };
    renderer.prepare_texture("stream", desc, 3 * COUNT)?;
    let mut tiles = Vec::with_capacity(COUNT);
    let mut old = Vec::with_capacity(COUNT);
    let seed = [255, 0, 0, 255].repeat((WIDTH * WIDTH / 4) as usize);
    for _ in 0..COUNT {
        let (id, image) = renderer.upload_texture_mips("seed", desc, 1..2, &seed)?;
        let start = Instant::now();
        while !renderer.poll_texture_upload(id)? {
            if start.elapsed() > WAIT {
                return Err("seed upload timeout".into());
            }
            thread::yield_now();
        }
        tiles.push(Tile {
            image,
            mip: 1,
            channel: 0,
        });
    }
    let mut objects = Vec::with_capacity(16);
    for _ in 0..12 {
        frame(&mut renderer, &yard, &mut objects, &tiles)?;
    }
    let store = if mode == "throttled" {
        ObjectStore::with_reader(root, paced_read)
    } else {
        ObjectStore::new(root)
    };
    let mut memory = Memory::default();
    memory.sample(&mut renderer)?;
    let mut samples = Vec::with_capacity(65536);
    let initial_heap = crate::ALLOCATOR.start();
    let timed = Instant::now();
    let (tx, rx) = mpsc::sync_channel(4);
    let worker_store = store.clone();
    let worker = if mode == "baseline" {
        None
    } else {
        Some(thread::spawn(move || -> io::Result<()> {
            for object in refs {
                let loaded = worker_store.load(&object).and_then(|bytes| {
                    bytes.image()?;
                    Ok(bytes)
                });
                tx.send(loaded)
                    .map_err(|_| io::Error::other("frame loop stopped"))?;
            }
            Ok(())
        }))
    };
    let mut pending: [Option<Pending>; COUNT] = std::array::from_fn(|_| None);
    let mut staged = std::collections::VecDeque::<Arc<ObjectBytes>>::with_capacity(COUNT);
    let mut submissions = 0;
    let mut minimum = None;
    let mut target = None;
    let mut loading_frames = 0;
    let mut peak_pending = 0;
    let mut post_quality_registered = None;
    let mut max_issued = 0;
    let mut events = Snapshot::default();
    let mut update_max_us = 0;
    loop {
        let tick = Instant::now();
        let before = Snapshot::now();
        let mut changed = false;
        for (lane, p) in pending.iter_mut().enumerate() {
            if let Some(upload) = p
                && renderer.poll_texture_upload(upload.id)?
            {
                let upload = p.take().unwrap();
                let prior = std::mem::replace(&mut tiles[lane], upload.tile);
                if mode == "retained" && prior.channel == 1 {
                    old.push(prior);
                }
                changed = true;
            }
        }
        let mut issued = 0;
        while staged.len() < COUNT {
            let Ok(object) = rx.try_recv() else {
                break;
            };
            staged.push_back(object?);
        }
        // Synchronize arrivals for the burst cases; the paced case admits each
        // finished read independently. Neither path waits on the frame thread.
        let batch_available = mode == "throttled" || staged.len() == COUNT;
        while batch_available
            && worker.is_some()
            && submissions < 2 * COUNT
            && (submissions < COUNT || minimum.is_some())
        {
            let lane = submissions % COUNT;
            if pending[lane].is_some() {
                break;
            }
            let Some(blob) = staged.pop_front() else {
                break;
            };
            let image = blob.image()?;
            let high = submissions >= COUNT;
            let (id, new) = if high {
                renderer.promote_texture("high", &tiles[lane].image, 0..1, &image.pixels)?
            } else {
                renderer.upload_texture_mips("minimum", desc, 1..2, &image.pixels)?
            };
            issued += image.pixels.len();
            pending[lane] = Some(Pending {
                tile: Tile {
                    image: new,
                    mip: if high { 0 } else { 1 },
                    channel: if high { 2 } else { 1 },
                },
                id,
                bytes: image.pixels.len(),
            });
            submissions += 1;
            changed = true;
        }
        max_issued = max_issued.max(issued);
        if issued > UPLOAD_LIMIT {
            return Err("per-tick upload budget exceeded".into());
        }
        peak_pending = peak_pending.max(pending.iter().flatten().map(|p| p.bytes).sum::<usize>());
        update_max_us = update_max_us.max(tick.elapsed().as_micros());
        frame(&mut renderer, &yard, &mut objects, &tiles)?;
        let elapsed = tick.elapsed();
        if mode == "baseline" {
            events.accumulate(Snapshot::now().since(before));
        }
        if samples.len() == samples.capacity() || timed.elapsed() > WAIT {
            return Err("streaming case timed out".into());
        }
        samples.push(elapsed.as_nanos() as u64);
        if minimum.is_none() && tiles.iter().all(|t| t.channel == 1) {
            minimum = Some(timed.elapsed().as_micros());
        }
        if target.is_none() && tiles.iter().all(|t| t.channel == 2) {
            target = Some(timed.elapsed().as_micros());
            loading_frames = samples.len();
        }
        if changed {
            let mut current = Memory::default();
            current.sample(&mut renderer)?;
            memory.registered = memory.registered.max(current.registered);
            memory.native = memory.native.max(current.native);
            memory.reserved = current.reserved;
            if target.is_some() {
                post_quality_registered = Some(current.registered);
            }
        }
        if samples.len() >= 120
            && timed.elapsed() >= Duration::from_millis(500)
            && (worker.is_none() || target.is_some())
        {
            break;
        }
    }
    if let Some(worker) = worker {
        worker.join().map_err(|_| "I/O worker panicked")??;
    }
    if mode == "baseline"
        && !cfg!(feature = "development-validation")
        && !events.is_allocation_free()
    {
        return Err(format!("ordinary frame allocation: {events:?}").into());
    }
    let peak_heap = crate::ALLOCATOR.peak();
    if matches!(mode, "concurrent" | "retained") && max_issued != UPLOAD_LIMIT {
        return Err("four simultaneous promotions were not exercised".into());
    }
    let stats = store.stats();
    if mode != "baseline" && (submissions != 2 * COUNT || stats.reads != 2 * COUNT as u64) {
        return Err("missing or duplicate upload/read".into());
    }
    if mode == "retained" {
        if old.len() != COUNT {
            return Err("missing retained image".into());
        }
        frame(&mut renderer, &yard, &mut objects, &old)?;
    }
    let loading = distribution(&mut samples[..loading_frames]);
    let report = json!({
        "case":mode, "frames":samples.len(), "frame_us":distribution(&mut samples),
        "loading_frames":loading_frames, "loading_frame_us":loading,
        "minimum_us":minimum, "target_us":target, "max_update_us":update_max_us,
        "reads":stats.reads, "io_bytes":stats.bytes,
        "io_rate_bytes_s":(mode=="throttled").then_some(IO_RATE),
        "heap_start":initial_heap, "heap_peak":peak_heap, "store_peak_live_bytes":stats.peak_live_bytes,
        "registered_peak":memory.registered, "registered_at_target":post_quality_registered,
        "native_allocation_peak":memory.native, "upload_arena_capacity":memory.staging,
        "upload_arena_peak_reserved":memory.reserved, "peak_outstanding_upload_payload":peak_pending,
        "max_texture_submitted_per_tick":max_issued,
        "baseline_events":(mode=="baseline").then(||json!({
            "rust":events.rust.map(|e|e.calls), "vma_host":events.vma_host.map(|e|e.calls), "native":events.native
        })),
        "old_versions_held":old.len()
    });
    drop((old, tiles));
    finish(renderer, yard)?;
    Ok(report)
}

fn distribution(samples: &mut [u64]) -> Option<Value> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable();
    let us = |p: usize| samples[(samples.len() * p).div_ceil(100) - 1] as f64 / 1000.;
    Some(json!({"p50":us(50), "p95":us(95), "p99":us(99), "max":us(100)}))
}
