//! Window events and replay feed the same yard actions.
use crate::{
    Action, REPLAY_FRAMES, Yard, camera,
    effects::{self, FiringLane},
    replay_action, standalone, validation,
};
use renderer::{
    WindowRenderer, canvas,
    core::{
        ImageFormat, PresentOutcome, PresentationConfig,
        capacity::{CapacityRecovery, Preparation},
        graph::{ImageDesc, SampleCount},
        runtime::{ManagedImage, Runtime, RuntimeConfig, WindowPresenter, WindowReadiness},
    },
    pipeline::PipelineResult,
};
use std::{
    ops::ControlFlow,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    platform::{pump_events::EventLoopExtPumpEvents, windows::EventLoopBuilderExtWindows},
    window::{Window, WindowId},
};

#[derive(Default)]
struct Events {
    window: Option<Arc<Window>>,
    size: Option<[u32; 2]>,
    actions: std::collections::VecDeque<Action>,
    equipment_step: usize,
    lane_step: Option<usize>,
    show_monitor_replay: bool,
    closed: bool,
    error: Option<winit::error::OsError>,
}

impl ApplicationHandler for Events {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("Training yard")
            .with_inner_size(PhysicalSize::new(384, 192));
        match events.create_window(attributes) {
            Ok(window) => self.window = Some(Arc::new(window)),
            Err(error) => self.error = Some(error),
        }
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.closed = true,
            WindowEvent::Resized(size) => self.size = Some([size.width, size.height]),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let step = match code {
                    KeyCode::Digit1 => 1,
                    KeyCode::Digit2 => 2,
                    KeyCode::Digit3 => 3,
                    KeyCode::Digit4 => 4,
                    KeyCode::Digit5 => 5,
                    KeyCode::Digit6 => 6,
                    KeyCode::KeyF => {
                        self.lane_step = match self.lane_step {
                            None => Some(0),
                            Some(n) if n + 1 < effects::FRAMES => Some(n + 1),
                            _ => None,
                        };
                        return;
                    }
                    KeyCode::KeyV => {
                        self.show_monitor_replay = true;
                        return;
                    }
                    KeyCode::Space => {
                        let step = 14 + self.equipment_step;
                        self.equipment_step = (self.equipment_step + 1) % (REPLAY_FRAMES - 14);
                        step
                    }
                    KeyCode::Escape => {
                        self.closed = true;
                        return;
                    }
                    _ => return,
                };
                if let Some(action) = replay_action(step) {
                    self.actions.push_back(action);
                }
            }
            _ => {}
        }
    }
}

pub fn run(config: RuntimeConfig, interactive: bool) -> PipelineResult<()> {
    let mut events = EventLoop::builder().with_any_thread(true).build()?;
    let mut input = Events::default();
    let start = Instant::now();
    let window = loop {
        events.pump_app_events(Some(Duration::from_millis(5)), &mut input);
        if let Some(error) = input.error.take() {
            return Err(error.into());
        }
        if let Some(window) = &input.window {
            break Arc::clone(window);
        }
        if start.elapsed() > Duration::from_secs(20) {
            return Err("window creation timed out".into());
        }
    };
    let size = window.inner_size();
    let mut camera = camera(1, [size.width, size.height])?;
    let mut yard = Yard::new()?;
    let mut lane = FiringLane::new()?;
    let mut active_lane = false;
    let mut lane_step = None;
    let mut scene_extent = None;
    let mut renderer = WindowRenderer::new(Arc::clone(&window), config, camera, yard.assets())?;
    let mut objects = Vec::with_capacity(4);
    let mut step = 0;
    let mut advance = true;
    let mut last_present = Instant::now();
    while !input.closed && (interactive || step < REPLAY_FRAMES + effects::FRAMES) {
        events.pump_app_events(Some(Duration::from_millis(4)), &mut input);
        if interactive && input.show_monitor_replay {
            break;
        }
        if let Some(extent) = input.size.take() {
            renderer.resize(
                extent,
                if active_lane {
                    lane.assets()
                } else {
                    yard.assets()
                },
            )?;
            if !extent.contains(&0) {
                camera.extent = extent;
            }
        }
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        let measured = (!interactive && (4..14).contains(&step))
            .then(renderer::core::measurement::Snapshot::now);
        if advance {
            lane_step = if interactive {
                input.lane_step
            } else {
                step.checked_sub(REPLAY_FRAMES)
            };
            let action = if lane_step.is_some() {
                None
            } else if interactive {
                input.actions.pop_front()
            } else {
                replay_action(step)
            };
            if let Some(action) = action {
                yard.apply(action, |assets| renderer.prepare_assets(assets))?;
            }
            advance = false;
        }
        let next_extent = lane_step
            .filter(|s| effects::reduced(*s))
            .map(|_| camera.extent.map(|v| (v / 2).max(1)));
        if lane_step.is_some() != active_lane || next_extent != scene_extent {
            renderer.set_scene_extent(
                next_extent,
                if lane_step.is_some() {
                    lane.assets()
                } else {
                    yard.assets()
                },
            )?;
            active_lane = lane_step.is_some();
            scene_extent = next_extent;
        }
        match renderer.draw(|frame| {
            if let Some(step) = lane_step {
                frame.draw_scene(lane.input(step)?, 1.)?;
                frame.draw_overlay(&[effects::HUD])?;
                Ok(())
            } else {
                standalone::draw(frame, &yard, &mut objects)
            }
        })? {
            PresentOutcome::Presented { .. } => {
                step += 1;
                advance = true;
                last_present = Instant::now();
            }
            PresentOutcome::RecreateRequired => {
                let size = window.inner_size();
                input.size = Some([size.width, size.height]);
            }
            _ => {}
        }
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        if let Some(before) = measured {
            if step == 14 && !renderer.wait_idle(Duration::from_secs(15))? {
                return Err("final presentation retirement timed out".into());
            }
            let events = renderer::core::measurement::Snapshot::now().since(before);
            assert!(
                events.is_allocation_free(),
                "window replay step {step}: {events:?}"
            );
        }
        if !interactive && last_present.elapsed() > Duration::from_secs(30) {
            return Err(format!("presentation timed out at replay step {step}").into());
        }
    }
    let mut runtime = renderer.into_runtime()?;
    lane.release(&mut runtime)?;
    yard.release(&mut runtime)?;
    if !interactive && step != REPLAY_FRAMES + effects::FRAMES {
        return Err("window closed during replay".into());
    }
    if input.closed {
        return validation::check_validation(runtime);
    }
    let size = window.inner_size();
    let extent = [size.width, size.height];
    let (runtime, mut viewport) = crate::replay::squad(None, |mut plan| {
        let mut base = Preparation::new(plan.preparation().frames_in_flight());
        base.add_requirements(
            plan.preparation().graph_requirements(),
            plan.preparation().working_set(),
        )?;
        reserve_screen(plan.preparation_mut(), extent)?;
        plan.prepare_runtime(&mut runtime)?;
        let presenter = runtime.create_presenter(window, PresentationConfig::default())?;
        Ok((
            plan.begin_with_runtime(runtime)?.wait()?,
            MonitorWindow {
                presenter,
                events,
                input,
                base,
                extent,
                interactive,
                presented: 0,
            },
        ))
    })?;
    while !viewport.presenter.close(Duration::from_secs(1))? {}
    if !interactive && viewport.presented != REPLAY_FRAMES {
        return Err(format!(
            "presented {} of {REPLAY_FRAMES} monitor frames",
            viewport.presented
        )
        .into());
    }
    if !interactive && viewport.extent != MONITOR_RESIZE {
        return Err(format!("monitor resize did not reach {MONITOR_RESIZE:?}").into());
    }
    drop(viewport);
    validation::check_validation(runtime)
}

fn reserve_screen(prep: &mut Preparation, [w, h]: [u32; 2]) -> PipelineResult<()> {
    let desc = ImageDesc::rgba8(w, h);
    prep.reserve_raster_pass("canvas", SampleCount::X1, |pass| {
        pass.transient_attachment("window viewport", desc)?;
        pass.draws(&canvas::programs(ImageFormat::Rgba8Unorm)?[0], 1)
    })?;
    prep.reserve_image(desc, PresentationConfig::default().max_frames)?;
    Ok(())
}

const MONITOR_RESIZE: [u32; 2] = [320, 160];

struct MonitorWindow {
    presenter: WindowPresenter,
    events: EventLoop<()>,
    input: Events,
    base: Preparation,
    extent: [u32; 2],
    interactive: bool,
    presented: usize,
}

impl crate::replay::Viewport for MonitorWindow {
    fn begin(
        &mut self,
        runtime: &mut Runtime,
        pending_click: bool,
    ) -> PipelineResult<ControlFlow<(), Option<[u32; 2]>>> {
        let start = Instant::now();
        if !self.interactive && self.presented == 8 {
            let [w, h] = MONITOR_RESIZE;
            if let Some(size) = self
                .input
                .window
                .as_ref()
                .expect("live replay window")
                .request_inner_size(PhysicalSize::new(w, h))
            {
                self.input.size = Some([size.width, size.height]);
            }
        }
        loop {
            self.events
                .pump_app_events(Some(Duration::from_millis(4)), &mut self.input);
            if self.input.closed {
                return Ok(ControlFlow::Break(()));
            }
            if let Some([w, h]) = self.input.size.take() {
                self.presenter.resize(w, h);
            }
            self.presenter.poll()?;
            if let WindowReadiness::Ready { extent: (w, h), .. } = self.presenter.prepare()? {
                if self.extent != [w, h] {
                    // Finish the pending pick at its normal replay step before resizing GPU storage.
                    if pending_click {
                        return Ok(ControlFlow::Continue(None));
                    }
                    if !self.presenter.wait_idle(Duration::from_secs(15))? {
                        return Err("monitor resize timed out".into());
                    }
                    let mut prep = Preparation::new(self.base.frames_in_flight());
                    prep.add_requirements(self.base.graph_requirements(), self.base.working_set())?;
                    reserve_screen(&mut prep, [w, h])?;
                    runtime.prepare_fitted_working_set(&prep, 0, 0)?;
                    self.extent = [w, h];
                }
                return Ok(ControlFlow::Continue(Some([w, h])));
            }
            if !self.interactive && start.elapsed() > Duration::from_secs(30) {
                return Err("monitor surface timed out".into());
            }
        }
    }
    fn present(&mut self, runtime: &mut Runtime, screen: ManagedImage) -> PipelineResult<()> {
        let start = Instant::now();
        loop {
            self.presenter.poll()?;
            match runtime.present(&mut self.presenter, screen.snapshot()) {
                Ok(PresentOutcome::Presented { .. }) => {
                    self.presented += 1;
                    return Ok(());
                }
                Ok(PresentOutcome::NotReady) => {}
                Ok(_) => return Ok(()),
                Err(error)
                    if error
                        .capacity()
                        .is_some_and(|c| c.recovery == CapacityRecovery::Poll) => {}
                Err(error) => return Err(error.into()),
            }
            self.events
                .pump_app_events(Some(Duration::from_millis(1)), &mut self.input);
            if self.input.closed || self.input.size.is_some() {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(30) {
                return Err("monitor presentation timed out".into());
            }
        }
    }
}
