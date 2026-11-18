//! Event-loop ownership for the Bistro example.

#[path = "window/adapter.rs"]
mod adapter;

use super::{
    Error, Options, WAIT,
    asset::Scene,
    camera::{Camera, Conditions},
    ibl::Environment,
    pipeline::{self, Budget, Resident, Workspace},
    window_camera::Motion,
    window_input::{CameraInput, REPLAY_DT},
};
use adapter::{Surface, WindowBackend};
use renderer::{
    PresentationMetrics, SubmissionMetrics,
    capacity::{CapacityRecovery, RuntimeCapacityKind},
    graph::{GraphPool, ImportImage, PassHandle},
    presentation::{PresentOutcome, PresentationConfig},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

const RETRY: Duration = Duration::from_millis(1);
const TOUR_FRAMES: usize = 1800;

struct Frame<B: WindowBackend> {
    pending: B::Pending,
    replay: Option<(usize, PassHandle)>,
    survey: bool,
}

struct Timings {
    accepted: Vec<u64>,
    host_phases: Vec<[u64; 3]>,
    graph: Vec<SubmissionMetrics>,
    present: Vec<PresentationMetrics>,
    last: Option<Instant>,
    started: Option<Instant>,
    duration: Duration,
    present_calls: u64,
    present_ns: u64,
    poll_ns: u64,
    #[cfg(feature = "memory-profiling")]
    start_memory: Option<renderer::measurement::Snapshot>,
    #[cfg(feature = "memory-profiling")]
    memory: Option<renderer::measurement::Snapshot>,
}

impl Timings {
    fn new(limit: usize) -> Self {
        Self {
            accepted: Vec::with_capacity(limit),
            host_phases: Vec::with_capacity(limit),
            graph: Vec::with_capacity(limit),
            present: Vec::with_capacity(limit.saturating_mul(4).saturating_add(16)),
            last: None,
            started: None,
            duration: Duration::ZERO,
            present_calls: 0,
            present_ns: 0,
            poll_ns: 0,
            #[cfg(feature = "memory-profiling")]
            start_memory: None,
            #[cfg(feature = "memory-profiling")]
            memory: None,
        }
    }

    fn start(&mut self) {
        self.started = Some(Instant::now());
        #[cfg(feature = "memory-profiling")]
        {
            self.start_memory = Some(renderer::measurement::Snapshot::now());
        }
    }

    fn finish(&mut self) {
        #[cfg(feature = "memory-profiling")]
        if let Some(start) = self.start_memory.take() {
            self.memory = Some(renderer::measurement::Snapshot::now().since(start));
        }
        if let Some(start) = self.started.take() {
            self.duration = start.elapsed();
        }
    }
}

#[derive(Default)]
struct Smoke {
    resize_requested: bool,
    resized: bool,
    minimize_requested: bool,
    minimized: bool,
    restored: bool,
    restore_at: Option<Instant>,
    replay: [[u8; 32]; 5],
    replay_count: usize,
    replay_step: usize,
}

struct State<B: WindowBackend> {
    window: Arc<Window>,
    surface: B::Surface,
    backend: B,
    resident: Option<Resident>,
    pool: GraphPool,
    workspace: Workspace,
    budget: Budget,
    frames: VecDeque<Frame<B>>,
    awaiting_present: Option<ImportImage>,
    frame_phases: [u64; 3],
    pixels: Vec<u8>,
    first_pixels: Vec<u8>,
    accepted: usize,
    finished: bool,
    tour: bool,
    desired: (u32, u32),
    input: CameraInput,
    seed: Camera,
    seed_dimensions: (u32, u32),
    last_camera_tick: Instant,
    last_progress: Instant,
    smoke: Smoke,
    timings: Timings,
    setup_ns: u64,
    preparation_ns: u64,
    upload_ns: u64,
    pending_on_exit: usize,
}

impl<B: WindowBackend> State<B> {
    fn new(
        window: Arc<Window>,
        scene: &mut Scene,
        environment: &mut Environment,
        conditions: &Conditions,
        options: &mut Options,
    ) -> Result<Self, Error> {
        let setup = Instant::now();
        let size = window.inner_size();
        options.width = size.width;
        options.height = size.height;
        let budget = Budget::new(scene, environment, options)?;
        let config = PresentationConfig {
            max_frames: options.inflight,
            vsync: options.vsync,
            ..PresentationConfig::default()
        };
        let started = Instant::now();
        let (mut backend, mut surface) = B::open(options, &budget, Arc::clone(&window), config)?;
        let preparation_ns = ns(started.elapsed());
        let seed = camera_seed(conditions, options)?;
        let initial = uniform(seed, conditions, options)?;
        let started = Instant::now();
        let resident = Resident::upload(&mut backend, scene, environment, &budget, &initial)?;
        scene.vertices = Vec::new();
        scene.indices = Vec::new();
        environment.release_pixels();
        let upload_ns = ns(started.elapsed());
        backend.set_profiling(options.gpu_timing)?;
        surface.profile(options.gpu_timing)?;
        let pool = GraphPool::new(budget.graph, options.inflight + 1)?;
        let mut workspace = Workspace::new(scene);
        if options.reuse_shadows && options.shadows {
            workspace.enable_shadow_reuse();
        }
        let mut input = CameraInput::default();
        input.set_pose(seed.position, seed.target);
        let pixels = if options.window_smoke {
            vec![0; size.width as usize * size.height as usize * 4]
        } else {
            Vec::new()
        };
        let first_pixels = vec![0; pixels.len()];
        Ok(Self {
            window,
            surface,
            backend,
            resident: Some(resident),
            pool,
            workspace,
            budget,
            frames: VecDeque::with_capacity(options.inflight),
            awaiting_present: None,
            frame_phases: [0; 3],
            pixels,
            first_pixels,
            accepted: 0,
            finished: false,
            tour: options.view == "route" && !options.window_smoke,
            desired: (size.width, size.height),
            input,
            seed,
            seed_dimensions: (size.width, size.height),
            last_camera_tick: Instant::now(),
            last_progress: Instant::now(),
            smoke: Smoke::default(),
            timings: Timings::new(options.frame_limit.unwrap_or(0)),
            setup_ns: ns(setup.elapsed()),
            preparation_ns,
            upload_ns,
            pending_on_exit: 0,
        })
    }

    fn set_extent(&mut self, extent: (u32, u32)) {
        if extent != self.desired {
            self.desired = extent;
            self.surface.resize(extent);
        }
    }

    fn discard_presentation_source(&mut self) -> Result<(), Error> {
        if let Some(image) = self.awaiting_present.take() {
            self.backend.delete(image.key())?;
        }
        Ok(())
    }

    fn poll(&mut self) -> Result<(), Error> {
        while let Some(frame) = self.frames.front_mut() {
            let Some(mut result) = B::poll(&mut frame.pending)? else {
                break;
            };
            if let Some((ordinal, pass)) = frame.replay {
                B::readback(&mut result, pass, &mut self.pixels)?;
                if ordinal == 0 {
                    self.first_pixels.copy_from_slice(&self.pixels);
                }
                self.smoke.replay[ordinal] = Sha256::digest(&self.pixels).into();
                self.smoke.replay_count += 1;
            }
            if frame.survey
                && let Some(metrics) = B::metrics(&result)
            {
                push(&mut self.timings.graph, metrics)?;
            }
            self.frames.pop_front();
        }
        self.collect_presentation_timings()
    }

    fn collect_presentation_timings(&mut self) -> Result<(), Error> {
        while let Some(metrics) = self.surface.pop_profile() {
            if self.timings.started.is_some() {
                push(&mut self.timings.present, metrics)?;
            }
        }
        Ok(())
    }

    fn run_smoke_events(&mut self) {
        if self.accepted >= 6 && !self.smoke.resize_requested {
            self.smoke.resize_requested = true;
            let _ = self.window.request_inner_size(PhysicalSize::new(1280, 720));
        }
        if self.accepted >= 12 && self.smoke.resized && !self.smoke.minimize_requested {
            self.smoke.minimize_requested = true;
            self.window.set_minimized(true);
        }
        if self.smoke.minimize_requested
            && !self.smoke.minimized
            && self.window.is_minimized() == Some(true)
        {
            self.smoke.minimized = true;
            self.set_extent((0, 0));
            self.smoke.restore_at = Some(Instant::now() + Duration::from_millis(80));
        }
        if self
            .smoke
            .restore_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.smoke.restore_at = None;
            self.window.set_minimized(false);
        }
    }

    fn camera(&mut self, conditions: &Conditions, options: &Options) -> Result<Camera, Error> {
        if self.tour && !options.fixture {
            let frame = (self.accepted + TOUR_FRAMES - options.warmup % TOUR_FRAMES) % TOUR_FRAMES;
            return conditions.camera("route", frame);
        }
        let since = self.last_camera_tick.elapsed();
        self.last_camera_tick = Instant::now();
        if options.window_smoke {
            if self.accepted == 0 {
                self.smoke.replay_step = 1;
                return Ok(self.seed);
            }
            // Repeat calls on a backpressured frame don't advance replay input.
            if self.smoke.replay_step == self.accepted && self.accepted < self.smoke.replay.len() {
                self.smoke.replay_step += 1;
                match self.accepted {
                    1 => self.input.motion(Motion::Forward, true),
                    2 => {
                        self.input.capture(true);
                        self.input.relative_motion(28.0, -12.0, true);
                    }
                    3 => self.input.capture(false),
                    4 => self.input.focus_lost(),
                    _ => {}
                }
                if (1..=4).contains(&self.accepted) {
                    self.input.step(REPLAY_DT);
                }
            }
        } else {
            self.input.step(since);
        }
        Ok(Camera {
            position: self.input.position(),
            target: self.input.target(),
            fov: self.seed.fov,
        })
    }

    fn tick(
        &mut self,
        scene: &Scene,
        conditions: &Conditions,
        options: &mut Options,
    ) -> Result<(), Error> {
        if options.frame_limit.is_some() && self.last_progress.elapsed() >= WAIT {
            return Err(format!(
                "Bistro Window stopped progressing at {} accepted frames (warmup {}, pending Graphs {}, awaiting present {}, finishing {}, extent {:?})",
                self.accepted,
                options.warmup,
                self.frames.len(),
                self.awaiting_present.is_some(),
                self.finished,
                self.desired,
            ).into());
        }
        let started = Instant::now();
        self.poll()?;
        if self.timings.started.is_some() {
            self.timings.poll_ns += ns(started.elapsed());
        }
        if options.window_smoke {
            self.run_smoke_events();
        }
        let actual = self.window.inner_size();
        if self.window.is_minimized() == Some(true) {
            self.set_extent((0, 0));
        } else {
            self.set_extent((actual.width, actual.height));
        }
        let readiness = self.surface.prepare()?;
        self.collect_presentation_timings()?;
        if self.finished {
            if self.frames.is_empty() && self.surface.idle()? {
                self.poll()?;
                self.timings.finish();
            }
            return Ok(());
        }
        if self.desired.0 == 0
            || self.desired.1 == 0
            || matches!(
                readiness,
                Some(PresentOutcome::Suspended | PresentOutcome::RecreateRequired)
            )
        {
            return self.discard_presentation_source();
        }
        if readiness == Some(PresentOutcome::NotReady) {
            return Ok(());
        }
        let Some(extent) = self.surface.extent() else {
            return Ok(());
        };
        if extent != self.desired {
            return self.discard_presentation_source();
        }
        if extent != (options.width, options.height) {
            self.discard_presentation_source()?;
            if !self.frames.is_empty() || !self.surface.idle()? {
                return Ok(());
            }
            self.workspace.clear_shadow_reuse(&mut self.backend)?;
            (options.width, options.height) = extent;
            let budget = &self.budget;
            self.resident = Some(
                self.resident
                    .take()
                    .unwrap()
                    .reprepare(&mut self.backend, |backend| {
                        backend.reprepare(options, budget)
                    })?,
            );
            if options.reuse_shadows && options.shadows {
                self.workspace.enable_shadow_reuse();
            }
            self.smoke.resized = self.smoke.resize_requested;
        }
        if self.awaiting_present.is_none() {
            if !options.window_smoke
                && self.accepted == options.warmup
                && self.timings.started.is_none()
                && options.frame_limit.is_some()
            {
                if !self.frames.is_empty() || !self.surface.idle()? {
                    return Ok(());
                }
                self.poll()?;
                eprintln!(
                    "Bistro Window steady run after {} warmup frames",
                    self.accepted
                );
                self.timings.start();
            }
            if self.frames.len() == options.inflight {
                return Ok(());
            }
            let camera = self.camera(conditions, options)?;
            let bytes = uniform(camera, conditions, options)?;
            let resident = self.resident.as_mut().unwrap();
            let replay = options.window_smoke && self.accepted < self.smoke.replay.len();
            let started = Instant::now();
            let built = pipeline::build(
                &mut self.pool,
                &mut self.workspace,
                resident,
                scene,
                options,
                camera,
                &bytes,
                conditions.background,
                replay,
            )?;
            let build_ns = ns(started.elapsed());
            let started = Instant::now();
            let plan = built.graph.compile()?;
            let compile_ns = ns(started.elapsed());
            let started = Instant::now();
            let pending = match self.backend.submit(&plan) {
                Ok(pending) => pending,
                Err(error) if backpressure::<B>(&error) => return Ok(()),
                Err(error) => return Err(error),
            };
            resident.camera = B::export_buffer(&pending, built.camera)?;
            self.workspace
                .accept_shadow_updates::<B>(&pending, built.shadow_updates)?;
            let image = B::export_image(&pending, built.output)?;
            self.frames.push_back(Frame {
                pending,
                replay: built
                    .present_capture
                    .map(|capture| (self.accepted, capture)),
                survey: self.timings.started.is_some(),
            });
            self.awaiting_present = Some(image);
            self.frame_phases = [build_ns, compile_ns, ns(started.elapsed())];
        }
        let start = Instant::now();
        let outcome = match self
            .backend
            .present(&mut self.surface, self.awaiting_present.as_ref().unwrap())
        {
            Ok(outcome) => outcome,
            Err(error) if backpressure::<B>(&error) => PresentOutcome::NotReady,
            Err(error) => return Err(error),
        };
        if self.timings.started.is_some() {
            self.timings.present_calls += 1;
            self.timings.present_ns += ns(start.elapsed());
        }
        match outcome {
            PresentOutcome::Presented { .. } => {
                self.discard_presentation_source()?;
                self.accepted += 1;
                self.last_progress = Instant::now();
                if self.smoke.minimized && self.smoke.restore_at.is_none() {
                    self.smoke.restored = true;
                }
                if self.timings.started.is_some() {
                    let now = Instant::now();
                    let before = self
                        .timings
                        .last
                        .replace(now)
                        .or(self.timings.started)
                        .unwrap();
                    push(&mut self.timings.accepted, ns(now.duration_since(before)))?;
                    push(&mut self.timings.host_phases, self.frame_phases)?;
                }
                let total = options.frame_limit.map(|frames| frames + options.warmup);
                if total.is_some_and(|total| self.accepted >= total) {
                    self.finished = true;
                    self.pending_on_exit = self.frames.len();
                }
            }
            PresentOutcome::NotReady => {}
            PresentOutcome::Recreated
            | PresentOutcome::RecreateRequired
            | PresentOutcome::Suspended => {
                self.discard_presentation_source()?;
            }
        }
        Ok(())
    }

    fn close(mut self, options: &Options, asset_load_ns: u64) -> Result<(), Error> {
        self.discard_presentation_source()?;
        if self.pending_on_exit == 0 {
            self.pending_on_exit = self.frames.len();
        }
        let deadline = Instant::now() + WAIT;
        while !self.frames.is_empty() {
            self.poll()?;
            if Instant::now() >= deadline {
                return Err("Bistro Window graph retirement timed out".into());
            }
            std::thread::yield_now();
        }
        if !self.surface.close(WAIT)? {
            return Err("Bistro Window presentation retirement timed out".into());
        }
        self.timings.finish();
        self.workspace.clear_shadow_reuse(&mut self.backend)?;
        let runtime = self.backend.info();
        let memory = self.backend.memory_snapshot()?;
        drop((self.resident, self.surface));
        let validation = self.backend.close()?;
        #[cfg(feature = "memory-profiling")]
        let steady_memory = self.timings.memory.map(super::run::memory_report);
        #[cfg(not(feature = "memory-profiling"))]
        let steady_memory: Option<Value> = None;
        let replay = self.smoke.replay_count == self.smoke.replay.len()
            && self.smoke.replay[..4]
                .windows(2)
                .all(|pair| pair[0] != pair[1])
            && self.smoke.replay[3] == self.smoke.replay[4];
        let smoke_passed = self.smoke.resized
            && self.smoke.minimized
            && self.smoke.restored
            && replay
            && self.pending_on_exit > 0;
        if options.window_smoke {
            std::fs::create_dir_all(&options.output)?;
            std::fs::write(options.output.join("linear-first.bin"), &self.first_pixels)?;
        }
        let report = json!({
            "schema_version": 1, "presentation": true, "runtime": runtime, "validation": validation,
            "initial_width": self.seed_dimensions.0, "initial_height": self.seed_dimensions.1,
            "bloom": options.bloom.name(), "vsync": options.vsync, "aa": options.aa,
            "samples": options.sample_count.as_u32(),
            "warmup": options.warmup, "accepted": self.accepted, "pending_on_exit": self.pending_on_exit,
            "setup_ns": self.setup_ns, "native_memory": memory, "steady_memory": steady_memory,
            "setup_phases_ns": {"asset_load": asset_load_ns, "native_prepare": self.preparation_ns,
                "upload": self.upload_ns},
            "smoke": options.window_smoke.then(|| json!({
                "passed": smoke_passed, "resized": self.smoke.resized, "minimized": self.smoke.minimized,
                "restored": self.smoke.restored, "rendered_camera_replay": replay,
                "linear_first_sha256": super::digest(&self.first_pixels),
            })),
            "timing": {
                "wall_ns": ns(self.timings.duration), "frame_intervals_ns": self.timings.accepted,
                "present_calls": self.timings.present_calls, "present_elapsed_ns": self.timings.present_ns,
                "graph_poll_elapsed_ns": self.timings.poll_ns,
                "frame_phases_ns": self.timings.host_phases, "phase_order": ["build", "compile", "submit"],
                "graph_samples": self.timings.graph.iter().map(graph_json).collect::<Vec<_>>(),
                "present_samples": self.timings.present.iter().map(present_json).collect::<Vec<_>>(),
                "scope": "window event-loop and native pacing; accepted present is not display scanout",
            },
        });
        super::run::write_report(&options.output, &report)?;
        if options.window_smoke && !smoke_passed {
            return Err("Bistro Window smoke acceptance failed".into());
        }
        if let Some(frames) = options.frame_limit
            && self.accepted != frames + options.warmup
        {
            return Err("Bistro Window closed before all requested frames were accepted".into());
        }
        Ok(())
    }
}

fn camera_seed(conditions: &Conditions, options: &Options) -> Result<Camera, Error> {
    if options.fixture {
        Ok(Camera::fixture())
    } else {
        conditions.camera(&options.view, 0)
    }
}

fn uniform(camera: Camera, conditions: &Conditions, options: &Options) -> Result<[u8; 128], Error> {
    camera.uniform(
        options.width as f32 / options.height as f32,
        conditions.near,
        conditions.far,
        conditions.light_direction,
        conditions.light_color,
    )
}

fn backpressure<B: WindowBackend>(error: &Error) -> bool {
    B::capacity_error(error).is_some_and(|capacity| {
        matches!(
            (capacity.resource, capacity.recovery),
            (_, CapacityRecovery::Poll)
                | (
                    RuntimeCapacityKind::TimingSamples,
                    CapacityRecovery::ReleaseResults
                )
                | (
                    RuntimeCapacityKind::ExecutionSlots,
                    CapacityRecovery::ReleaseOwners
                )
        )
    })
}

fn push<T>(storage: &mut Vec<T>, value: T) -> Result<(), Error> {
    if storage.len() == storage.capacity() {
        return Err("prepared Bistro Window timing capacity exhausted".into());
    }
    storage.push(value);
    Ok(())
}

fn ns(duration: Duration) -> u64 {
    duration.as_nanos() as u64
}

fn graph_json(metrics: &SubmissionMetrics) -> Value {
    json!({"prepare_ns": ns(metrics.cpu_prepare), "record_ns": ns(metrics.cpu_record),
        "submit_ns": ns(metrics.cpu_submit), "retire_ns": ns(metrics.cpu_retire), "gpu_ns": gpu_ns(metrics.gpu)})
}

fn present_json(metrics: &PresentationMetrics) -> Value {
    json!({"prepare_ns": ns(metrics.cpu_prepare), "acquire_ns": ns(metrics.cpu_acquire),
        "record_ns": ns(metrics.cpu_record), "submit_ns": ns(metrics.cpu_submit),
        "present_ns": ns(metrics.cpu_present), "retire_ns": ns(metrics.cpu_retire),
        "gpu_ns": gpu_ns(metrics.gpu)})
}

fn gpu_ns(timing: renderer::GpuTiming) -> Option<u64> {
    if let renderer::GpuTiming::Available { elapsed, .. } = timing {
        Some(ns(elapsed))
    } else {
        None
    }
}

struct App<B: WindowBackend> {
    options: Options,
    scene: Scene,
    environment: Environment,
    conditions: Conditions,
    state: Option<State<B>>,
    failure: Option<Error>,
    deadline: Option<Instant>,
}

impl<B: WindowBackend> ApplicationHandler for App<B> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let created = event_loop.create_window(
            Window::default_attributes()
                .with_title("Bistro")
                .with_inner_size(PhysicalSize::new(self.options.width, self.options.height)),
        );
        let result = created.map_err(Error::from).and_then(|window| {
            State::new(
                Arc::new(window),
                &mut self.scene,
                &mut self.environment,
                &self.conditions,
                &mut self.options,
            )
        });
        match result {
            Ok(state) => self.state = Some(state),
            Err(error) => {
                self.failure = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else {
            return;
        };
        if id != state.window.id() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Focused(false) => state.input.focus_lost(),
            WindowEvent::MouseInput {
                state: button,
                button: MouseButton::Right,
                ..
            } => state.input.capture(button == ElementState::Pressed),
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let pressed = event.state == ElementState::Pressed;
                    match code {
                        KeyCode::Escape if pressed => event_loop.exit(),
                        KeyCode::KeyT if pressed && !event.repeat => {
                            state.tour = !state.tour;
                            state.input.clear_motion();
                        }
                        KeyCode::ShiftLeft | KeyCode::ShiftRight => state.input.fast(pressed),
                        KeyCode::KeyW => state.input.motion(Motion::Forward, pressed),
                        KeyCode::KeyS => state.input.motion(Motion::Backward, pressed),
                        KeyCode::KeyA => state.input.motion(Motion::Left, pressed),
                        KeyCode::KeyD => state.input.motion(Motion::Right, pressed),
                        KeyCode::KeyQ => state.input.motion(Motion::Down, pressed),
                        KeyCode::KeyE => state.input.motion(Motion::Up, pressed),
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = state.tick(&self.scene, &self.conditions, &mut self.options) {
                    self.failure = Some(error);
                    event_loop.exit();
                } else if state.finished
                    && state.timings.started.is_none()
                    && state.frames.is_empty()
                {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        if let Some(state) = &mut self.state
            && let DeviceEvent::MouseMotion { delta } = event
        {
            state.input.relative_motion(delta.0, delta.1, !state.tour);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.deadline.is_some_and(|time| Instant::now() >= time) {
            self.failure = Some("Bistro Window acceptance timed out".into());
            event_loop.exit();
        } else if let Some(state) = &mut self.state {
            // Minimized Windows can stop dispatching RedrawRequested. Keep
            // maintenance (and scripted restore) independent of a redraw event.
            if state.desired.0 == 0 || state.desired.1 == 0 || state.finished {
                if let Err(error) = state.tick(&self.scene, &self.conditions, &mut self.options) {
                    self.failure = Some(error);
                    event_loop.exit();
                } else if state.finished
                    && state.timings.started.is_none()
                    && state.frames.is_empty()
                {
                    event_loop.exit();
                }
            } else {
                state.window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + RETRY));
        }
    }
}

pub(super) fn execute<B: WindowBackend>(mut options: Options) -> Result<(), Error> {
    if options.output.join("report.json").exists() {
        return Err("output report already exists; choose a new output directory".into());
    }
    if options.window_smoke {
        options.frame_limit.get_or_insert(options.frames);
        options.warmup = 0;
    }
    let load_started = Instant::now();
    let conditions = Conditions::parse(&serde_json::from_slice(include_bytes!(
        "../../../../../tools/bistro/conditions.json"
    ))?)?;
    let mut scene = if let Some(path) = &options.manifest {
        Scene::load(path, false)?
    } else {
        Scene::fixture(false)
    };
    if !options.fixture && !conditions.ambient_occlusion {
        scene.disable_environment_occlusion();
    }
    let environment = if options.fixture {
        Environment::fixture()
    } else {
        Environment::load(
            options
                .ibl_manifest
                .as_deref()
                .ok_or("Windowed Bistro requires --ibl-manifest")?,
        )?
    };
    let asset_load_ns = ns(load_started.elapsed());
    let deadline = options
        .frame_limit
        .map(|_| Instant::now() + Duration::from_secs(900));
    let mut app = App::<B> {
        options,
        scene,
        environment,
        conditions,
        state: None,
        failure: None,
        deadline,
    };
    EventLoop::new()?.run_app(&mut app)?;
    let result = if let Some(state) = app.state {
        state.close(&app.options, asset_load_ns)
    } else {
        Ok(())
    };
    if let Some(error) = app.failure {
        return Err(error);
    }
    result
}
