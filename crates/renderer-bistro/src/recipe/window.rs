//! Event-loop ownership for the Bistro example.

use super::{
    Error, Options, WAIT,
    asset::Scene,
    backend,
    camera::{Camera, Conditions},
    ibl::Environment,
    pipeline::{self, Budget, Resident, Workspace},
    window_camera::Motion,
    window_input::{CameraInput, REPLAY_DT, REPLAY_STEPS},
};
use renderer::{
    PresentationMetrics, SubmissionMetrics,
    capacity::{CapacityRecovery, RuntimeCapacityKind},
    graph::{GraphPool, PassHandle},
    presentation::{PresentOutcome, PresentationConfig},
    runtime::{ExecutionError, FrameId, Runtime, WindowPresenter, WindowReadiness},
};
use renderer_pipeline::passes::output::AcceptedDisplaySdr;
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

struct Frame {
    id: FrameId,
    replay: Option<ReplayReadback>,
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

type ReplayReadback = (usize, PassHandle);

#[derive(Default)]
struct ReplayCaptures {
    hashes: [Option<[u8; 32]>; REPLAY_STEPS],
    first_pixels: Vec<u8>,
}

impl ReplayCaptures {
    fn record(&mut self, ordinal: usize, pixels: &[u8]) {
        if ordinal == 0 {
            self.first_pixels.copy_from_slice(pixels);
        }
        self.hashes[ordinal] = Some(Sha256::digest(pixels).into());
    }

    fn discard(&mut self, ordinal: usize, pending: Option<&mut Option<ReplayReadback>>) {
        if let Some(hash) = self.hashes.get_mut(ordinal)
            && hash.take().is_some()
            && ordinal == 0
        {
            self.first_pixels.fill(0);
        }
        if let Some(pending) = pending
            && pending.is_some_and(|(queued, _)| queued == ordinal)
        {
            *pending = None;
        }
    }

    fn passed(&self) -> bool {
        let [
            Some(seed),
            Some(moved),
            Some(looked),
            Some(released),
            Some(stopped),
        ] = self.hashes
        else {
            return false;
        };
        seed != moved && moved != looked && looked != released && released == stopped
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
    replay: ReplayCaptures,
    replay_step: usize,
}

struct State {
    window: Arc<Window>,
    surface: WindowPresenter,
    backend: Runtime,
    resident: Option<Resident>,
    pool: GraphPool,
    workspace: Workspace,
    budget: Budget,
    frames: VecDeque<Frame>,
    awaiting_present: Option<AcceptedDisplaySdr>,
    frame_phases: [u64; 3],
    pixels: Vec<u8>,
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

impl State {
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
        let mut budget = Budget::new(scene, environment, options, conditions.background)?;
        let config = PresentationConfig {
            max_frames: options.inflight,
            vsync: options.vsync,
            ..PresentationConfig::default()
        };
        let started = Instant::now();
        let (mut backend, mut surface) =
            backend::open(options, &budget, Arc::clone(&window), config)?;
        let preparation_ns = ns(started.elapsed());
        let seed = camera_seed(conditions, options)?;
        let initial = uniform(seed, conditions, options)?;
        let started = Instant::now();
        let resident = Resident::upload(&mut backend, scene, environment, &budget, &initial)?;
        let placements = resident.instance_placements(
            scene,
            options,
            seed,
            [conditions.near, conditions.far],
        )?;
        scene.vertices = Vec::new();
        scene.indices = Vec::new();
        environment.release_pixels();
        let upload_ns = ns(started.elapsed());
        backend.set_profiling(options.gpu_timing)?;
        surface.set_profiling(options.gpu_timing)?;
        let pool = GraphPool::new(budget.graph, options.inflight + 1)?;
        let mut workspace =
            Workspace::new(scene, budget.prepared.take().expect("prepared pipeline"));
        workspace.placements = placements;
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
        let replay = ReplayCaptures {
            first_pixels: vec![0; pixels.len()],
            ..ReplayCaptures::default()
        };
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
            accepted: 0,
            finished: false,
            tour: options.view == "route" && !options.window_smoke,
            desired: (size.width, size.height),
            input,
            seed,
            seed_dimensions: (size.width, size.height),
            last_camera_tick: Instant::now(),
            last_progress: Instant::now(),
            smoke: Smoke {
                replay,
                ..Smoke::default()
            },
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
            self.surface.resize(extent.0, extent.1);
        }
    }

    fn release_presentation_source(&mut self) -> Result<(), Error> {
        if let Some(image) = self.awaiting_present.take() {
            self.backend.delete(image.key())?;
        }
        Ok(())
    }

    fn discard_presentation_source(&mut self) -> Result<(), Error> {
        if !self.pixels.is_empty()
            && self.awaiting_present.is_some()
            && self.accepted < REPLAY_STEPS
        {
            // Only the current source can be retried at this ordinal. It is
            // either the last pending frame or was already read back by poll.
            self.smoke.replay.discard(
                self.accepted,
                self.frames.back_mut().map(|frame| &mut frame.replay),
            );
        }
        self.release_presentation_source()
    }

    fn poll(&mut self) -> Result<(), Error> {
        while let Some(frame) = self.frames.front() {
            let Some(mut result) = self.backend.wait_for_frame(frame.id, Duration::ZERO)? else {
                break;
            };
            if let Some((ordinal, pass)) = frame.replay {
                backend::readback(&mut result, pass, &mut self.pixels)?;
                self.smoke.replay.record(ordinal, &self.pixels);
            }
            if frame.survey
                && let Some(metrics) = result.metrics().copied()
            {
                push(&mut self.timings.graph, metrics)?;
            }
            self.frames.pop_front();
        }
        self.collect_presentation_timings()
    }

    fn collect_presentation_timings(&mut self) -> Result<(), Error> {
        while let Some(metrics) = self.surface.take_profile() {
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
            if self.smoke.replay_step == self.accepted && self.accepted < REPLAY_STEPS {
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
            if self.frames.is_empty() && self.surface.wait_idle(Duration::ZERO)? {
                self.poll()?;
                self.timings.finish();
            }
            return Ok(());
        }
        if self.desired.0 == 0
            || self.desired.1 == 0
            || matches!(readiness, WindowReadiness::Suspended)
        {
            return self.discard_presentation_source();
        }
        let WindowReadiness::Ready { extent, .. } = readiness else {
            return Ok(());
        };
        if extent != self.desired {
            return self.discard_presentation_source();
        }
        if extent != (options.width, options.height) {
            self.discard_presentation_source()?;
            if !self.frames.is_empty() || !self.surface.wait_idle(Duration::ZERO)? {
                return Ok(());
            }
            self.workspace.clear_shadow_reuse(&mut self.backend)?;
            (options.width, options.height) = extent;
            options.validate_lighting_extent()?;
            let budget = &self.budget;
            let prepared = budget.prepare_frame(options, scene, conditions.background)?;
            self.resident = Some(
                self.resident
                    .take()
                    .unwrap()
                    .reprepare(&mut self.backend, |backend| {
                        backend::resize(backend, options, budget, &prepared)
                    })?,
            );
            self.workspace.prepared = prepared;
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
                if !self.frames.is_empty() || !self.surface.wait_idle(Duration::ZERO)? {
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
            let replay = options.window_smoke && self.accepted < REPLAY_STEPS;
            let started = Instant::now();
            let built = pipeline::build(
                &mut self.pool,
                &mut self.workspace,
                resident,
                scene,
                options,
                camera,
                &bytes,
                replay,
            )?;
            let build_ns = ns(started.elapsed());
            let started = Instant::now();
            let plan = built.graph.compile()?;
            let compile_ns = ns(started.elapsed());
            let started = Instant::now();
            let pending = match self.backend.submit(&plan) {
                Ok(pending) => pending,
                Err(error) if backpressure(&error) => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            self.workspace
                .accept_shadow_updates(&pending, built.shadow_updates)?;
            resident.accept_camera(&pending, built.camera_update)?;
            let image = built
                .output
                .display()
                .ok_or("windowed Bistro requires display-linear output")?
                .accept(&pending)?;
            let id = pending.id();
            drop(pending);
            self.frames.push_back(Frame {
                id,
                replay: built
                    .present_capture
                    .map(|capture| (self.accepted, capture)),
                survey: self.timings.started.is_some(),
            });
            self.awaiting_present = Some(image);
            self.frame_phases = [build_ns, compile_ns, ns(started.elapsed())];
        }
        let start = Instant::now();
        let image = self.awaiting_present.as_ref().unwrap();
        let outcome = match self.backend.present(&mut self.surface, image.image()) {
            Ok(outcome) => outcome,
            Err(error) if backpressure(&error) => PresentOutcome::NotReady,
            Err(error) => return Err(error.into()),
        };
        if self.timings.started.is_some() {
            self.timings.present_calls += 1;
            self.timings.present_ns += ns(start.elapsed());
        }
        match outcome {
            PresentOutcome::Presented { .. } => {
                self.release_presentation_source()?;
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

    fn close(mut self, options: &Options, asset_load_ns: u64, inputs: &Value) -> Result<(), Error> {
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
        let runtime = backend::info(&self.backend);
        let memory = backend::memory_snapshot(&self.backend)?;
        drop((self.resident, self.surface));
        let validation = backend::close(self.backend)?;
        #[cfg(feature = "memory-profiling")]
        let steady_memory = self.timings.memory.map(super::run::memory_report);
        #[cfg(not(feature = "memory-profiling"))]
        let steady_memory: Option<Value> = None;
        let replay = self.smoke.replay.passed();
        let smoke_passed = self.smoke.resized
            && self.smoke.minimized
            && self.smoke.restored
            && replay
            && self.pending_on_exit > 0;
        if options.window_smoke {
            std::fs::create_dir_all(&options.output)?;
            std::fs::write(
                options.output.join("linear-first.bin"),
                &self.smoke.replay.first_pixels,
            )?;
        }
        let report = json!({
            "schema_version": 1, "presentation": true, "runtime": runtime, "validation": validation,
            "inputs": inputs,
            "initial_width": self.seed_dimensions.0, "initial_height": self.seed_dimensions.1,
            "bloom": options.bloom.name(), "vsync": options.vsync, "aa": options.aa,
            "samples":options.sample_count.as_u32(), "pipeline":options.pipeline.name(),
            "shader_manifest":options.shader_assets.as_ref().map(|selection| &selection.manifest),
            "local_lights":options.pipeline.uses_local_lights()
                .then(|| super::lighting::identity(options.fixture)),
            "instances": options.instances.name(), "instance_draws": options.instances.draws(),
            "submission": options.submission.name(),
            "packed_opaque": self.budget.packed_counts().map(|(groups, draws)| json!({"groups":groups,"primitives":draws})),
            "warmup": options.warmup, "accepted": self.accepted, "pending_on_exit": self.pending_on_exit,
            "setup_ns": self.setup_ns, "native_memory": memory, "steady_memory": steady_memory,
            "setup_phases_ns": {"asset_load": asset_load_ns, "native_prepare": self.preparation_ns,
                "upload": self.upload_ns},
            "smoke": options.window_smoke.then(|| json!({
                "passed": smoke_passed, "resized": self.smoke.resized, "minimized": self.smoke.minimized,
                "restored": self.smoke.restored, "rendered_camera_replay": replay,
                "linear_first_sha256": super::digest(&self.smoke.replay.first_pixels),
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

fn backpressure(error: &ExecutionError) -> bool {
    error.capacity().is_some_and(|capacity| {
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

struct App {
    options: Options,
    scene: Scene,
    environment: Environment,
    conditions: Conditions,
    state: Option<State>,
    failure: Option<Error>,
    deadline: Option<Instant>,
}

impl ApplicationHandler for App {
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
            WindowEvent::Focused(false) if !self.options.window_smoke => state.input.focus_lost(),
            WindowEvent::MouseInput {
                state: button,
                button: MouseButton::Right,
                ..
            } if !self.options.window_smoke => state.input.capture(button == ElementState::Pressed),
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let pressed = event.state == ElementState::Pressed;
                    match code {
                        KeyCode::Escape if pressed => event_loop.exit(),
                        _ if self.options.window_smoke => {}
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
        if !self.options.window_smoke
            && let Some(state) = &mut self.state
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

pub(super) fn execute(mut options: Options) -> Result<(), Error> {
    if options.output.join("report.json").exists() {
        return Err("output report already exists; choose a new output directory".into());
    }
    if options.window_smoke {
        options.frame_limit.get_or_insert(options.frames);
        options.warmup = 0;
    }
    let load_started = Instant::now();
    let condition_bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/bistro/conditions.json"
    ));
    let conditions = Conditions::parse(&serde_json::from_slice(condition_bytes)?)?;
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
    let inputs = json!({
        "cook_manifest_sha256": super::run::loaded_cook_hash(&scene, options.fixture)?,
        "ibl_manifest_sha256": environment.identity["manifest_sha256"],
        "conditions_sha256": super::digest(condition_bytes),
        "shaders_sha256": super::digest(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../renderer-pipeline/shaders/manifest.json"))),
    });
    let deadline = options
        .frame_limit
        .map(|_| Instant::now() + Duration::from_secs(900));
    let mut app = App {
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
        state.close(&app.options, asset_load_ns, &inputs)
    } else {
        Ok(())
    };
    if let Some(error) = app.failure {
        return Err(error);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::graph::BufferDesc;

    #[test]
    fn replay_discards_attempts_before_and_after_readback() {
        let mut pool = GraphPool::new(Default::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let buffer = graph.buffer("pixels", BufferDesc { size: 4 }).unwrap();
        let pass = graph.readback_buffer("capture", buffer, 0, 4).unwrap();
        let mut replay = ReplayCaptures {
            first_pixels: vec![0; 4],
            ..ReplayCaptures::default()
        };

        // A rejected first present may already have returned its readback.
        replay.record(0, &[9; 4]);
        replay.discard(0, None);
        assert_eq!(replay.hashes[0], None);
        assert_eq!(replay.first_pixels, [0; 4]);
        replay.record(0, &[1; 4]);
        let first_hash = replay.hashes[0];

        // Leave earlier accepted captures intact while canceling a retry.
        let mut older = Some((0, pass));
        replay.discard(1, Some(&mut older));
        assert_eq!(older, Some((0, pass)));
        assert_eq!(replay.hashes[0], first_hash);
        let mut pending = Some((1, pass));
        replay.discard(1, Some(&mut pending));
        assert_eq!(pending, None);

        // A later rejected readback cannot stand in for the accepted retry.
        replay.record(1, &[8; 4]);
        replay.discard(1, None);
        for (ordinal, value) in [(2, 3), (3, 4), (4, 4)] {
            replay.record(ordinal, &[value; 4]);
        }
        assert!(!replay.passed());
        replay.record(1, &[2; 4]);
        assert!(replay.passed());
        assert_eq!(replay.first_pixels, [1; 4]);
    }
}
