//! App-owned Windows loop, free camera and repeatable two-API presentation.
//! `--api vulkan|d3d12 --graph standard|custom --vsync on|off` selects execution.
//! WASD/QE move, Shift accelerates, hold right mouse to look, T toggles the tour,
//! V toggles VSync and Escape exits. `--smoke` exercises window transitions;
//! `--measure` reports separate CPU/GPU phases from the displayed workload.

#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
#[path = "support/window_backend.rs"]
mod window_backend;
#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
#[path = "support/window_camera.rs"]
mod window_camera;
#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
#[path = "support/window_input.rs"]
mod window_input;
#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
#[path = "support/window_scene.rs"]
mod window_scene;

#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
mod demo {
    use super::{
        window_backend::{self as backend, Api, Backend, Monitor, Pending, Result},
        window_camera::Motion,
        window_input::{CameraInput, InputReplay, REPLAY_DT, REPLAY_STEPS},
        window_scene::{self as scene, Composition},
    };
    use renderer::{
        graph::{GraphPool, ImportBuffer, ImportImage, PassHandle, ResourceKey},
        presentation::PresentOutcome,
        profiling::{GpuTiming, PresentationMetrics, SubmissionMetrics},
    };
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use winit::{
        application::ApplicationHandler,
        dpi::PhysicalSize,
        event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent},
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        keyboard::{KeyCode, PhysicalKey},
        window::{CursorGrabMode, Window, WindowId},
    };

    const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);
    const RUN_BUDGET: Duration = Duration::from_secs(90);
    const RETRY_INTERVAL: Duration = Duration::from_millis(1);
    const IN_FLIGHT: usize = 3;

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Options {
        api: Api,
        composition: Composition,
        vsync: bool,
        smoke: bool,
        tour: bool,
        verify: bool,
        measure: bool,
        frames: Option<usize>,
        warmup: usize,
    }
    impl Options {
        fn parse(arguments: impl IntoIterator<Item = String>) -> std::result::Result<Self, String> {
            let mut value = Self {
                api: if cfg!(feature = "vulkan") {
                    Api::Vulkan
                } else {
                    Api::D3d12
                },
                composition: Composition::Standard,
                vsync: true,
                smoke: false,
                tour: false,
                verify: false,
                measure: false,
                frames: None,
                warmup: 24,
            };
            let mut args = arguments.into_iter();
            while let Some(argument) = args.next() {
                match argument.as_str() {
                    "--api" => {
                        value.api = match args.next().as_deref() {
                            Some("vulkan") => Api::Vulkan,
                            Some("d3d12") => Api::D3d12,
                            _ => return Err("--api requires vulkan or d3d12".into()),
                        }
                    }
                    "--graph" => {
                        value.composition = match args.next().as_deref() {
                            Some("standard") => Composition::Standard,
                            Some("custom") => Composition::Custom,
                            _ => return Err("--graph requires standard or custom".into()),
                        }
                    }
                    "--vsync" => {
                        value.vsync = match args.next().as_deref() {
                            Some("on") => true,
                            Some("off") => false,
                            _ => return Err("--vsync requires on or off".into()),
                        }
                    }
                    "--smoke" => value.smoke = true,
                    "--tour" => value.tour = true,
                    "--verify" => value.verify = true,
                    "--measure" => value.measure = true,
                    "--frames" | "--warmup" => {
                        let count = args
                            .next()
                            .and_then(|text| text.parse::<usize>().ok())
                            .filter(|count| *count <= 4096)
                            .ok_or_else(|| format!("{argument} requires an integer in 0..=4096"))?;
                        if argument == "--frames" {
                            if count == 0 {
                                return Err("--frames must be nonzero".into());
                            }
                            value.frames = Some(count);
                        } else {
                            value.warmup = count;
                        }
                    }
                    _ => {
                        return Err(format!(
                            "unknown argument {argument}; see windowed example source for options"
                        ));
                    }
                }
            }
            if value.smoke {
                value.tour = true;
                value.verify = true;
            }
            if value.measure {
                value.frames.get_or_insert(96);
                value.tour = true;
            }
            if value.verify {
                value
                    .frames
                    .get_or_insert(if value.smoke { 24 } else { 96 });
            }
            if value.smoke && value.measure {
                return Err("use separate --smoke and --measure runs so transitions are not included in steady timing".into());
            }
            Ok(value)
        }
        fn target(&self) -> Option<usize> {
            self.frames
                .map(|count| count + if self.measure { self.warmup } else { 0 })
        }
    }

    struct RedrawSchedule {
        next: Instant,
        drawable: bool,
    }
    impl RedrawSchedule {
        fn new(now: Instant) -> Self {
            Self {
                next: now,
                drawable: false,
            }
        }
        fn resize(&mut self, width: u32, height: u32) {
            self.drawable = width != 0 && height != 0;
        }
        fn next_wake(&self) -> Option<Instant> {
            self.drawable.then_some(self.next)
        }
        fn request_due(&mut self, now: Instant) -> bool {
            if !self.drawable || now < self.next {
                return false;
            }
            self.next = now + RETRY_INTERVAL;
            true
        }
    }

    #[derive(Default)]
    struct Sample {
        ordinal: u64,
        extent: (u32, u32),
        camera: [f32; 3],
        accepted: bool,
        digest: Option<u64>,
        geometry_pixels: usize,
        build: Duration,
        compile: Duration,
        submit_api: Duration,
        poll_api: Duration,
        result_return: Duration,
        frame_interval: Option<Duration>,
        graph: Option<SubmissionMetrics>,
    }
    struct Frame {
        pending: Option<Pending>,
        image: Option<ImportImage>,
        readback: Option<PassHandle>,
        bytes: usize,
        sample: Option<usize>,
    }
    struct App {
        options: Options,
        window: Option<Arc<Window>>,
        backend: Option<Backend>,
        monitor: Option<Monitor>,
        device: Option<serde_json::Value>,
        buffers: Option<[ImportBuffer; 2]>,
        keys: Option<[ResourceKey; 2]>,
        pool: GraphPool,
        frames: [Option<Frame>; IN_FLIGHT],
        readback: Vec<u8>,
        samples: Vec<Sample>,
        presentation_samples: Vec<PresentationMetrics>,
        prepared_extent: Option<(u32, u32)>,
        accepted: usize,
        input: CameraInput,
        replay: InputReplay,
        last_step: Instant,
        last_present: Option<Instant>,
        redraw: RedrawSchedule,
        started: Instant,
        transition: usize,
        restore_at: Option<Instant>,
        retries: usize,
        present_attempts: usize,
        present_api: Duration,
        poll_api: Duration,
        prepare_api: Duration,
        result_return: Duration,
        draw_cpu: Duration,
        shutdown_wait: Duration,
        backpressure_since: Option<Instant>,
        backpressure_time: Duration,
        pending_at_shutdown: usize,
        error: Option<String>,
        validation: Option<serde_json::Value>,
    }
    impl App {
        fn new(options: Options) -> Result<Self> {
            let now = Instant::now();
            let capacity = options.target().unwrap_or(0) + 64;
            Ok(Self {
                options,
                window: None,
                backend: None,
                monitor: None,
                device: None,
                buffers: None,
                keys: None,
                pool: GraphPool::new(scene::capacity(), IN_FLIGHT + 2)?,
                frames: std::array::from_fn(|_| None),
                readback: Vec::new(),
                samples: Vec::with_capacity(capacity),
                presentation_samples: Vec::with_capacity(capacity * 4),
                prepared_extent: None,
                accepted: 0,
                input: CameraInput::default(),
                replay: InputReplay::default(),
                last_step: now,
                last_present: None,
                redraw: RedrawSchedule::new(now),
                started: now,
                transition: 0,
                restore_at: None,
                retries: 0,
                present_attempts: 0,
                present_api: Duration::ZERO,
                poll_api: Duration::ZERO,
                prepare_api: Duration::ZERO,
                result_return: Duration::ZERO,
                draw_cpu: Duration::ZERO,
                shutdown_wait: Duration::ZERO,
                backpressure_since: None,
                backpressure_time: Duration::ZERO,
                pending_at_shutdown: 0,
                error: None,
                validation: None,
            })
        }
        fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
            if self.window.is_some() {
                return Ok(());
            }
            let window = Arc::new(
                event_loop.create_window(
                    Window::default_attributes()
                        .with_title("Renderer: WASD/QE, right mouse, T tour, V VSync")
                        .with_inner_size(PhysicalSize::new(640, 480)),
                )?,
            );
            let backend = Backend::new(
                self.options.api,
                Arc::clone(&window),
                self.options.vsync,
                self.options.measure,
            )?;
            let monitor = backend.monitor();
            monitor.check(false)?;
            self.device = Some(backend.device());
            let size = window.inner_size();
            self.redraw.resize(size.width, size.height);
            self.window = Some(window);
            self.backend = Some(backend);
            self.monitor = Some(monitor);
            self.update_title();
            Ok(())
        }
        fn update_title(&self) {
            if let Some(window) = &self.window {
                window.set_title(&format!(
                    "Renderer {} / {:?} / VSync {} / {} — WASD/QE, right mouse, T, V",
                    self.options.api.name(),
                    self.options.composition,
                    if self.options.vsync { "ON" } else { "OFF" },
                    if self.options.tour {
                        "fixed tour"
                    } else {
                        "free camera"
                    }
                ));
            }
        }
        fn retry(&mut self) {
            self.retries += 1;
            self.backpressure_since.get_or_insert_with(Instant::now);
        }
        fn drain_profiles(&mut self) -> Result<()> {
            let Some(backend) = self.backend.as_mut() else {
                return Ok(());
            };
            while let Some(profile) = backend.take_profile() {
                if self.presentation_samples.len() == self.presentation_samples.capacity() {
                    return Err("prepared presentation report capacity exhausted".into());
                }
                self.presentation_samples.push(profile);
            }
            Ok(())
        }
        fn poll_frames(&mut self) -> Result<()> {
            for slot in &mut self.frames {
                let Some(frame) = slot else {
                    continue;
                };
                if let Some(pending) = &mut frame.pending {
                    let started = self.options.measure.then(Instant::now);
                    let result = pending.wait(Duration::ZERO)?;
                    let poll_elapsed = elapsed(started);
                    self.poll_api += poll_elapsed;
                    if let Some(index) = frame.sample {
                        self.samples[index].poll_api += poll_elapsed;
                    }
                    if let Some(mut result) = result {
                        let started = self.options.measure.then(Instant::now);
                        if let Some(pass) = frame.readback.take() {
                            let bytes = &mut self.readback[..frame.bytes];
                            result.readback_into(pass, bytes)?;
                            let (digest, geometry) = scene::verify(bytes).map_err(str::to_owned)?;
                            if let Some(index) = frame.sample {
                                self.samples[index].digest = Some(digest);
                                self.samples[index].geometry_pixels = geometry;
                            }
                        }
                        if let Some(index) = frame.sample {
                            self.samples[index].graph = result.metrics();
                        }
                        drop(result);
                        frame.pending = None;
                        if let Some(index) = frame.sample {
                            self.samples[index].result_return = elapsed(started);
                            self.result_return += self.samples[index].result_return;
                        }
                    }
                }
                if frame.pending.is_none() && frame.image.is_none() {
                    *slot = None;
                }
            }
            Ok(())
        }
        fn discard_outputs(&mut self) -> Result<()> {
            let Some(backend) = &mut self.backend else {
                return Ok(());
            };
            for frame in self.frames.iter_mut().flatten() {
                if let Some(image) = frame.image.take() {
                    backend.delete(image.key())?;
                }
            }
            Ok(())
        }
        fn prepare_extent(&mut self, extent: (u32, u32)) -> Result<bool> {
            if self.prepared_extent == Some(extent) {
                return Ok(true);
            }
            self.discard_outputs()?;
            if self.frames.iter().any(Option::is_some) {
                return Ok(false);
            }
            let backend = self.backend.as_mut().expect("initialized backend");
            if !backend.presentation_idle()? {
                return Ok(false);
            }
            self.buffers = None;
            match backend.prepare_resources(extent) {
                Ok(()) => {}
                Err(error) if backend::is_capacity(error.as_ref()) => return Ok(false),
                Err(error) => return Err(error),
            }
            if let Some(keys) = self.keys {
                self.buffers = Some([
                    backend.refresh_buffer(keys[0])?,
                    backend.refresh_buffer(keys[1])?,
                ]);
            } else {
                let mut graph = self.pool.begin()?;
                let (vertices, indices) = scene::upload(&mut graph)?;
                let plan = graph.compile()?;
                let mut pending = backend.submit(&plan)?;
                let buffers = [
                    pending.export_buffer(vertices)?,
                    pending.export_buffer(indices)?,
                ];
                let result = pending
                    .wait(SHUTDOWN_BUDGET)?
                    .ok_or("initial scene upload timed out")?;
                drop(result);
                drop(pending);
                drop(plan);
                self.keys = Some(buffers.each_ref().map(|buffer| buffer.key()));
                self.buffers = Some(buffers);
            }
            if self.options.verify {
                let bytes = usize::try_from(u64::from(extent.0) * u64::from(extent.1) * 4)?;
                self.readback.resize(bytes, 0);
            }
            self.prepared_extent = Some(extent);
            Ok(true)
        }
        fn draw(&mut self) -> Result<()> {
            let started = self.options.measure.then(Instant::now);
            let result = self.draw_inner();
            self.draw_cpu += elapsed(started);
            result
        }
        fn poll(&mut self) -> Result<()> {
            let started = self.options.measure.then(Instant::now);
            let result = self.backend.as_mut().unwrap().poll();
            self.poll_api += elapsed(started);
            result?;
            let started = self.options.measure.then(Instant::now);
            let result = self.drain_profiles();
            self.result_return += elapsed(started);
            result?;
            self.poll_frames()
        }
        fn draw_inner(&mut self) -> Result<()> {
            let Some(backend) = &mut self.backend else {
                return Ok(());
            };
            let _ = backend;
            self.poll()?;
            if self
                .options
                .target()
                .is_some_and(|target| self.accepted >= target)
            {
                return Ok(());
            }
            if self.restore_at.is_some() {
                return Ok(());
            }
            let started = self.options.measure.then(Instant::now);
            let prepared = self.backend.as_mut().unwrap().prepare();
            self.prepare_api += elapsed(started);
            let prepared = match prepared {
                Ok(value) => value,
                Err(error) if backend::is_capacity(error.as_ref()) => {
                    self.retry();
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            if matches!(
                prepared,
                Some(PresentOutcome::NotReady | PresentOutcome::Suspended)
            ) {
                self.retry();
                return Ok(());
            }
            let Some(extent) = self.backend.as_ref().unwrap().extent() else {
                return Ok(());
            };
            if !self.prepare_extent(extent)? {
                self.retry();
                return Ok(());
            }
            let outstanding = self
                .frames
                .iter()
                .position(|value| value.as_ref().is_some_and(|frame| frame.image.is_some()));
            let slot = if let Some(slot) = outstanding {
                slot
            } else {
                let Some(slot) = self.frames.iter().position(Option::is_none) else {
                    self.retry();
                    return Ok(());
                };
                let now = Instant::now();
                if self.options.smoke && self.accepted < REPLAY_STEPS {
                    self.replay
                        .advance(&mut self.input, self.accepted as u64)
                        .map_err(str::to_owned)?;
                } else if self.options.tour {
                    self.input.tour(self.accepted as u64);
                } else {
                    self.input.step(now.duration_since(self.last_step));
                }
                self.last_step = now;
                let started = self.options.measure.then(Instant::now);
                let mut graph = self.pool.begin()?;
                let image = scene::build(
                    &mut graph,
                    self.buffers.as_ref().unwrap(),
                    self.input.clip_from_world(extent.0, extent.1),
                    extent,
                    self.options.composition,
                )?;
                let readback = if self.options.verify {
                    Some(graph.readback_image("verify displayed source", image)?)
                } else {
                    None
                };
                let build = elapsed(started);
                let started = self.options.measure.then(Instant::now);
                let plan = graph.compile()?;
                let compile = elapsed(started);
                let started = self.options.measure.then(Instant::now);
                let pending = match self.backend.as_mut().unwrap().submit(&plan) {
                    Ok(value) => value,
                    Err(error) if backend::is_capacity(error.as_ref()) => {
                        self.retry();
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                };
                let submit_api = elapsed(started);
                let image = pending.export_image(image)?;
                let sample = if self.options.verify || self.options.measure {
                    if self.samples.len() == self.samples.capacity() {
                        return Err("prepared report sample capacity exhausted".into());
                    }
                    let index = self.samples.len();
                    self.samples.push(Sample {
                        ordinal: self.accepted as u64,
                        extent,
                        camera: self.input.position(),
                        build,
                        compile,
                        submit_api,
                        ..Default::default()
                    });
                    Some(index)
                } else {
                    None
                };
                self.frames[slot] = Some(Frame {
                    pending: Some(pending),
                    image: Some(image),
                    readback,
                    bytes: self.readback.len(),
                    sample,
                });
                drop(plan);
                slot
            };
            let frame = self.frames[slot].as_mut().unwrap();
            let image = frame.image.as_ref().unwrap();
            let started = self.options.measure.then(Instant::now);
            self.present_attempts += 1;
            let outcome = self.backend.as_mut().unwrap().present(image);
            self.present_api += elapsed(started);
            let outcome = match outcome {
                Ok(value) => value,
                Err(error) if backend::is_capacity(error.as_ref()) => {
                    self.retry();
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            match outcome {
                PresentOutcome::Presented { .. } => {
                    let now = Instant::now();
                    if let Some(index) = frame.sample {
                        self.samples[index].accepted = true;
                        self.samples[index].frame_interval =
                            self.last_present.map(|last| now.duration_since(last));
                    }
                    self.last_present = Some(now);
                    self.backend
                        .as_mut()
                        .unwrap()
                        .delete(frame.image.take().unwrap().key())?;
                    self.accepted += 1;
                    if let Some(started) = self.backpressure_since.take() {
                        self.backpressure_time += now.duration_since(started);
                    }
                }
                PresentOutcome::Recreated | PresentOutcome::RecreateRequired => {
                    self.backend
                        .as_mut()
                        .unwrap()
                        .delete(frame.image.take().unwrap().key())?;
                }
                PresentOutcome::NotReady | PresentOutcome::Suspended => self.retry(),
            }
            Ok(())
        }
        fn toggle_vsync(&mut self) -> Result<()> {
            if let Some(backend) = &mut self.backend {
                backend.set_vsync(!self.options.vsync)?;
            }
            self.options.vsync = !self.options.vsync;
            self.update_title();
            Ok(())
        }
        fn capture_mouse(&mut self, enabled: bool) {
            self.input.capture(enabled);
            if let Some(window) = &self.window {
                if enabled {
                    if window.set_cursor_grab(CursorGrabMode::Confined).is_err() {
                        let _ = window.set_cursor_grab(CursorGrabMode::Locked);
                    }
                } else {
                    let _ = window.set_cursor_grab(CursorGrabMode::None);
                }
                window.set_cursor_visible(!enabled);
            }
        }
        fn shutdown(&mut self) -> Result<()> {
            if self.backend.is_none() {
                return Ok(());
            }
            let started = Instant::now();
            self.pending_at_shutdown = self
                .frames
                .iter()
                .filter(|frame| frame.as_ref().is_some_and(|frame| frame.pending.is_some()))
                .count();
            self.discard_outputs()?;
            let deadline = Instant::now() + SHUTDOWN_BUDGET;
            for slot in &mut self.frames {
                if let Some(frame) = slot
                    && let Some(pending) = &mut frame.pending
                {
                    let remaining = deadline
                        .checked_duration_since(Instant::now())
                        .ok_or("Graph shutdown timed out")?;
                    let mut result = pending.wait(remaining)?.ok_or("Graph shutdown timed out")?;
                    if let Some(pass) = frame.readback.take() {
                        let bytes = &mut self.readback[..frame.bytes];
                        result.readback_into(pass, bytes)?;
                        let (digest, geometry) = scene::verify(bytes).map_err(str::to_owned)?;
                        if let Some(index) = frame.sample {
                            self.samples[index].digest = Some(digest);
                            self.samples[index].geometry_pixels = geometry;
                        }
                    }
                    if let Some(index) = frame.sample {
                        self.samples[index].graph = result.metrics();
                    }
                    drop(result);
                }
                *slot = None;
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("presentation shutdown timed out")?;
            if !self.backend.as_mut().unwrap().close(remaining)? {
                return Err("presentation shutdown timed out".into());
            }
            self.drain_profiles()?;
            self.buffers = None;
            if let Some(keys) = self.keys.take() {
                for key in keys {
                    self.backend.as_mut().unwrap().delete(key)?;
                }
            }
            self.backend = None;
            self.window = None;
            if let Some(monitor) = &self.monitor {
                self.validation = Some(monitor.check(true)?);
            }
            self.shutdown_wait += started.elapsed();
            Ok(())
        }
        fn fail(&mut self, event_loop: &ActiveEventLoop, error: impl std::fmt::Display) {
            self.error = Some(error.to_string());
            if let Err(error) = self.shutdown() {
                self.error = Some(format!(
                    "{}; shutdown: {error}",
                    self.error.as_deref().unwrap_or("failure")
                ));
            }
            event_loop.exit();
        }
        fn finish(&self) -> Result<()> {
            if let Some(error) = &self.error {
                return Err(error.clone().into());
            }
            if self
                .options
                .target()
                .is_some_and(|target| self.accepted != target)
            {
                return Err(
                    "bounded windowed run ended before its requested accepted-frame count".into(),
                );
            }
            if self.options.smoke && self.transition != 6 {
                return Err("smoke run did not finish every window transition".into());
            }
            if self.options.measure && self.samples.iter().any(|sample| sample.graph.is_none()) {
                return Err("a submitted measured frame has no completed Graph timing".into());
            }
            if self.options.measure && self.presentation_samples.len() < self.accepted {
                return Err(
                    "not all accepted presentations returned completed timing samples".into(),
                );
            }
            if self.options.verify && self.samples.iter().any(|sample| sample.digest.is_none()) {
                return Err("a submitted verification frame has no completed readback".into());
            }
            let replay_digests: [Option<u64>; REPLAY_STEPS - 1] = std::array::from_fn(|ordinal| {
                self.samples
                    .iter()
                    .find(|sample| sample.accepted && sample.ordinal == ordinal as u64)
                    .and_then(|sample| sample.digest)
            });
            let replay_rendered_changes = replay_digests.iter().all(Option::is_some)
                && replay_digests.windows(2).all(|pair| pair[0] != pair[1]);
            if self.options.smoke && (!self.replay.complete() || !replay_rendered_changes) {
                return Err("application input replay did not change the rendered movement/look frames or preserve mouse-release/focus behavior".into());
            }
            if self.options.smoke || self.options.verify || self.options.measure {
                let samples:Vec<_>=self.samples.iter().map(|sample|serde_json::json!({
                    "ordinal":sample.ordinal,"extent":sample.extent,"camera":sample.camera,
                    "accepted":sample.accepted,"warmup":self.options.measure && sample.ordinal<(self.options.warmup as u64),
                    "digest":sample.digest.map(|digest|format!("{digest:016x}")),"geometry_pixels":sample.geometry_pixels,
                    "cpu_ns":{"build":ns(sample.build),"compile":ns(sample.compile),"submit_api":ns(sample.submit_api),
                        "poll_api":ns(sample.poll_api),"result_consumption_return":ns(sample.result_return)},
                    "graph":sample.graph.map(graph_json),"frame_interval_ns":sample.frame_interval.map(ns),
                })).collect();
                let presentation_samples: Vec<_> = self
                    .presentation_samples
                    .iter()
                    .copied()
                    .map(presentation_json)
                    .collect();
                println!(
                    "WINDOWED_ACCEPTANCE {}",
                    serde_json::json!({
                        "api":self.options.api.name(),"composition":format!("{:?}",self.options.composition),
                        "device":self.device,"validation":self.validation,"accepted_frames":self.accepted,
                        "smoke":self.options.smoke,"transitions":self.transition,"pending_graph_owners_at_shutdown":self.pending_at_shutdown,
                        "input_replay":self.options.smoke.then(||serde_json::json!({
                            "steps":self.replay.steps,"movement":self.replay.movement,
                            "fixed_step_ns":ns(REPLAY_DT),
                            "mouse_look":self.replay.mouse_look,
                            "release_preserves_movement":self.replay.release_preserves_movement,
                            "focus_stops_movement":self.replay.focus_stops_movement,
                            "rendered_digest_changes":replay_rendered_changes,
                            "scope":"application-local control replay through the same handlers as window events; no OS input injection",
                        })),
                        "verify":self.options.verify,"profiling":self.options.measure,"vsync_final":self.options.vsync,
                        "retry_interval_ns":ns(RETRY_INTERVAL),"backpressure_retries":self.retries,
                        "backpressure_wall_ns":ns(self.backpressure_time),"explicit_capacity_blocking_wait_ns":0,
                        "present_attempts":self.present_attempts,"present_api_total_ns":ns(self.present_api),
                    "poll_api_total_ns":ns(self.poll_api),"prepare_api_total_ns":ns(self.prepare_api),
                    "result_consumption_return_total_ns":ns(self.result_return),"draw_cpu_total_ns":ns(self.draw_cpu),
                    "shutdown_total_ns":ns(self.shutdown_wait),
                    "cpu_totals_scope":"outer API totals overlap native phase samples; do not add them; draw total includes initial preparation, warmup and nonblocking final drain",
                        "timing_scope":"real window; Graph GPU and fullscreen presentation GPU separately; frame interval includes event-loop scheduling and native pacing; not display completion",
                        "features":{"optimized":!cfg!(debug_assertions),"development_validation":cfg!(feature="development-validation"),
                            "vulkan_diagnostics":cfg!(feature="vulkan-diagnostics"),"d3d12_diagnostics":cfg!(feature="d3d12-diagnostics"),
                            "graph_diagnostics":cfg!(feature="graph-diagnostics"),
                            "memory_profiling":cfg!(feature="memory-profiling")},"samples":samples,
                        "presentation_samples":presentation_samples,
                    })
                );
            }
            Ok(())
        }
    }
    fn elapsed(started: Option<Instant>) -> Duration {
        started.map_or(Duration::ZERO, |started| started.elapsed())
    }
    fn ns(duration: Duration) -> u64 {
        duration.as_nanos().min(u64::MAX as u128) as u64
    }
    fn gpu_json(timing: GpuTiming) -> serde_json::Value {
        match timing {
            GpuTiming::Available {
                elapsed,
                timestamp_period_ns,
                valid_bits,
            } => {
                serde_json::json!({"ns":ns(elapsed),"timestamp_period_ns":timestamp_period_ns,"valid_bits":valid_bits})
            }
            _ => serde_json::json!({"unavailable":format!("{timing:?}")}),
        }
    }
    fn graph_json(value: SubmissionMetrics) -> serde_json::Value {
        serde_json::json!({"cpu_prepare_ns":ns(value.cpu_prepare),"cpu_record_ns":ns(value.cpu_record),
            "cpu_submit_ns":ns(value.cpu_submit),"cpu_retire_ns":ns(value.cpu_retire),"gpu":gpu_json(value.gpu)})
    }
    fn presentation_json(value: PresentationMetrics) -> serde_json::Value {
        serde_json::json!({"cpu_acquire_ns":ns(value.cpu_acquire),"cpu_prepare_ns":ns(value.cpu_prepare),
            "cpu_record_ns":ns(value.cpu_record),"cpu_submit_ns":ns(value.cpu_submit),"cpu_present_ns":ns(value.cpu_present),
            "cpu_retire_ns":ns(value.cpu_retire),"gpu":gpu_json(value.gpu)})
    }
    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if let Err(error) = self.initialize(event_loop) {
                self.fail(event_loop, error);
            }
        }
        fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
            if let DeviceEvent::MouseMotion { delta } = event {
                self.input
                    .relative_motion(delta.0, delta.1, !self.options.tour);
            }
        }
        fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            if self.window.as_ref().is_none_or(|window| window.id() != id) {
                return;
            }
            match event {
                WindowEvent::CloseRequested => {
                    if let Err(error) = self.shutdown() {
                        self.error = Some(error.to_string());
                    }
                    event_loop.exit();
                }
                WindowEvent::Resized(size) => {
                    if self.options.measure
                        && self
                            .prepared_extent
                            .is_some_and(|extent| extent != (size.width, size.height))
                    {
                        self.fail(event_loop, "window extent changed during a fixed-workload measurement; restart the run");
                        return;
                    }
                    self.redraw.resize(size.width, size.height);
                    if let Some(backend) = &mut self.backend {
                        backend.resize(size.width, size.height);
                    }
                }
                // The local smoke script owns its first five poses. Ambient
                // desktop focus/input must not modify that deterministic input;
                // the script exercises the same focus_lost operation itself.
                WindowEvent::Focused(false)
                    if !self.options.smoke || self.accepted >= REPLAY_STEPS =>
                {
                    self.capture_mouse(false);
                    self.input.focus_lost();
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Right,
                    ..
                } if !self.options.smoke || self.accepted >= REPLAY_STEPS => {
                    self.capture_mouse(state == ElementState::Pressed);
                }
                WindowEvent::KeyboardInput { event, .. }
                    if !self.options.smoke || self.accepted >= REPLAY_STEPS =>
                {
                    if let PhysicalKey::Code(key) = event.physical_key {
                        let pressed = event.state == ElementState::Pressed;
                        match key {
                            KeyCode::KeyW => self.input.motion(Motion::Forward, pressed),
                            KeyCode::KeyS => self.input.motion(Motion::Backward, pressed),
                            KeyCode::KeyA => self.input.motion(Motion::Left, pressed),
                            KeyCode::KeyD => self.input.motion(Motion::Right, pressed),
                            KeyCode::KeyQ => self.input.motion(Motion::Down, pressed),
                            KeyCode::KeyE => self.input.motion(Motion::Up, pressed),
                            KeyCode::ShiftLeft | KeyCode::ShiftRight => self.input.fast(pressed),
                            KeyCode::KeyT if pressed && !event.repeat => {
                                if self.options.measure {
                                    self.fail(event_loop, "camera mode changed during a fixed-workload measurement; restart the run");
                                    return;
                                }
                                self.options.tour = !self.options.tour;
                                self.input.clear_motion();
                                self.update_title();
                            }
                            KeyCode::KeyV if pressed && !event.repeat => {
                                if self.options.measure {
                                    self.fail(event_loop, "VSync changed during a fixed-workload measurement; restart the run");
                                    return;
                                }
                                if let Err(error) = self.toggle_vsync() {
                                    self.fail(event_loop, error);
                                }
                            }
                            KeyCode::Escape if pressed => {
                                if let Err(error) = self.shutdown() {
                                    self.error = Some(error.to_string());
                                }
                                event_loop.exit();
                            }
                            _ => {}
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    if let Err(error) = self.draw() {
                        self.fail(event_loop, error);
                    }
                }
                _ => {}
            }
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            let now = Instant::now();
            if self.options.target().is_some() && now.duration_since(self.started) >= RUN_BUDGET {
                self.fail(event_loop, "bounded windowed run timed out");
                return;
            }
            if self.options.smoke {
                let trigger = [2, 4, 6, 8, 10, 12][self.transition.min(5)];
                if self.transition < 6 && self.accepted >= trigger && self.restore_at.is_none() {
                    match self.transition {
                        0 | 5 => {
                            if let Err(error) = self.toggle_vsync() {
                                self.fail(event_loop, error);
                                return;
                            }
                        }
                        1..=3 => {
                            let (width, height) =
                                [(512, 384), (672, 448), (600, 400)][self.transition - 1];
                            if let Some(window) = &self.window {
                                let _ = window.request_inner_size(PhysicalSize::new(width, height));
                            }
                        }
                        4 => {
                            if let Some(window) = &self.window {
                                window.set_minimized(true);
                            }
                            if let Some(backend) = &mut self.backend {
                                backend.resize(0, 0);
                            }
                            self.redraw.resize(0, 0);
                            self.restore_at = Some(now + Duration::from_millis(80));
                        }
                        _ => {}
                    }
                    self.transition += 1;
                }
            }
            if self.restore_at.is_some_and(|deadline| now >= deadline) {
                self.restore_at = None;
                if let Some(window) = &self.window {
                    window.set_minimized(false);
                    let _ = window.request_inner_size(PhysicalSize::new(640, 480));
                }
                if let Some(backend) = &mut self.backend {
                    backend.resize(640, 480);
                }
                self.redraw.resize(640, 480);
            }
            if self
                .options
                .target()
                .is_some_and(|target| self.accepted >= target)
            {
                if self.options.measure {
                    // Include the tail in ordinary nonblocking completion and
                    // result return. Smoke deliberately enters close with live
                    // accepted owners instead.
                    let started = self.options.measure.then(Instant::now);
                    let drained = (|| -> Result<bool> {
                        self.poll()?;
                        if self.frames.iter().any(Option::is_some) {
                            return Ok(false);
                        }
                        let started = self.options.measure.then(Instant::now);
                        let idle = self.backend.as_mut().unwrap().presentation_idle();
                        self.poll_api += elapsed(started);
                        let idle = idle?;
                        self.drain_profiles()?;
                        Ok(idle)
                    })();
                    self.draw_cpu += elapsed(started);
                    match drained {
                        Ok(true) => {}
                        Ok(false) => {
                            event_loop
                                .set_control_flow(ControlFlow::WaitUntil(now + RETRY_INTERVAL));
                            return;
                        }
                        Err(error) => {
                            self.fail(event_loop, error);
                            return;
                        }
                    }
                }
                if let Err(error) = self.shutdown() {
                    self.error = Some(error.to_string());
                }
                event_loop.exit();
                return;
            }
            let next_redraw = if self.restore_at.is_none() {
                if self.redraw.request_due(now)
                    && let Some(window) = &self.window
                {
                    window.request_redraw();
                }
                self.redraw.next_wake()
            } else {
                None
            };
            let wake = [
                next_redraw,
                self.restore_at,
                self.options.target().map(|_| self.started + RUN_BUDGET),
            ]
            .into_iter()
            .flatten()
            .min();
            event_loop.set_control_flow(wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
        }
    }
    pub fn run() -> Result<()> {
        let options = Options::parse(std::env::args().skip(1))?;
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "renderer=warn".into()),
            )
            .try_init()
            .map_err(|error| error.to_string())?;
        let event_loop = EventLoop::new()?;
        let mut app = App::new(options)?;
        event_loop.run_app(&mut app)?;
        app.shutdown()?;
        app.finish()
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn options_keep_timing_and_transition_acceptance_separate() {
            let parse =
                |args: &[&str]| Options::parse(args.iter().map(|value| (*value).to_owned()));
            let options = parse(&[
                "--api",
                "d3d12",
                "--graph",
                "custom",
                "--measure",
                "--vsync",
                "off",
                "--frames",
                "8",
                "--warmup",
                "2",
            ])
            .unwrap();
            assert_eq!(options.target(), Some(10));
            assert!(options.tour);
            assert!(!options.verify);
            assert_eq!(options.api, Api::D3d12);
            assert_eq!(options.composition, Composition::Custom);
            assert_eq!(parse(&["--smoke"]).unwrap().target(), Some(24));
            for args in [
                &["--measure", "--smoke"][..],
                &["--frames", "0"],
                &["--warmup", "5000"],
                &["--api", "metal"],
                &["--vsync"],
            ] {
                assert!(parse(args).is_err());
            }
        }
        #[test]
        fn early_events_do_not_push_back_redraw_and_zero_extent_waits() {
            let now = Instant::now();
            let mut schedule = RedrawSchedule::new(now);
            assert!(!schedule.request_due(now));
            schedule.resize(640, 480);
            assert!(schedule.request_due(now));
            let next = now + RETRY_INTERVAL;
            assert!(!schedule.request_due(now));
            assert_eq!(schedule.next_wake(), Some(next));
            schedule.resize(0, 480);
            assert_eq!(schedule.next_wake(), None);
            schedule.resize(640, 480);
            assert!(schedule.request_due(now + Duration::from_secs(1)));
        }
    }
}

#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    demo::run()
}
#[cfg(not(all(windows, any(feature = "vulkan", feature = "d3d12"))))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("the windowed example requires Windows and --features vulkan,presentation or d3d12,presentation".into())
}
