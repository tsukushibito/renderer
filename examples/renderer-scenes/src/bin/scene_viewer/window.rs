use super::{backend, finish, route::Route};
use sashiko_asset::scene::{DisplayScene, PerspectiveView};
use sashiko_renderer::{WindowRenderer, core::PresentOutcome, pipeline::PipelineResult};
use std::{path::PathBuf, sync::Arc, time::Instant};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

struct Display {
    window: Arc<Window>,
    renderer: WindowRenderer,
    scene: DisplayScene,
    view: PerspectiveView,
    keys: [bool; 8],
    last: Instant,
    route: Option<Route>,
    playback: Option<Instant>,
    benchmark: bool,
}

impl Display {
    fn draw(&mut self) -> PipelineResult<bool> {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32().min(0.1);
        self.last = now;
        let size = self.window.inner_size();
        let extent = [size.width, size.height];
        if extent.contains(&0) {
            return Ok(false);
        }
        let axis = |positive: usize, negative: usize| {
            f32::from(self.keys[positive] as u8) - f32::from(self.keys[negative] as u8)
        };
        let [x, y, z] = self.view.forward;
        let right_sign = if self.view.mirror_x { 1. } else { -1. };
        let yaw = x.atan2(z) + right_sign * axis(5, 4) * dt;
        let pitch = (y.atan2(x.hypot(z)) + axis(6, 7) * dt).clamp(-1.55, 1.55);
        let f = [
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        ];
        let right = [right_sign * yaw.cos(), 0., -right_sign * yaw.sin()];
        for i in 0..3 {
            self.view.position[i] += 8. * dt * (axis(0, 1) * f[i] + axis(3, 2) * right[i]);
        }
        self.view.forward = f;
        if let (Some(route), Some(start)) = (&self.route, self.playback) {
            self.view = route.pose(start.elapsed().as_secs_f32() % route.duration());
        }
        self.renderer
            .set_camera(self.view.camera(extent, [0.02, 1500.])?.camera);
        let outcome = self.renderer.draw(|frame| {
            if self.benchmark
                && (extent != [1920, 1080] || frame.scene_view().extent != [1920, 1080])
            {
                return Err(format!(
                    "benchmark requires 1920x1080, window {extent:?}, scene {:?}",
                    frame.scene_view().extent
                )
                .into());
            }
            self.scene
                .select_lods(&self.view, frame.scene_view().extent[1]);
            self.scene.draw(frame)
        })?;
        if matches!(
            outcome,
            PresentOutcome::RecreateRequired | PresentOutcome::Presented { suboptimal: true }
        ) {
            self.renderer.resize(extent, self.scene.assets())?;
        }
        self.window.request_redraw();
        Ok(matches!(outcome, PresentOutcome::Presented { .. }))
    }
}

struct App {
    frames_left: Option<usize>,
    package: PathBuf,
    view_name: String,
    display: Option<Display>,
    route: Option<Route>,
    report: Option<PathBuf>,
    startup: Instant,
    first_present: Option<Instant>,
    intervals: Vec<[f64; 2]>,
    graph_timings: Vec<[f64; 4]>,
    profile: bool,
    result: PipelineResult<()>,
}

impl App {
    fn open(&mut self, events: &ActiveEventLoop) -> PipelineResult<()> {
        let benchmark = self.report.is_some();
        let fullscreen = events
            .primary_monitor()
            .filter(|monitor| benchmark && monitor.size() == PhysicalSize::new(1920, 1080))
            .map(|monitor| winit::window::Fullscreen::Borderless(Some(monitor)));
        let window = Arc::new(
            events.create_window(
                Window::default_attributes()
                    .with_title("Sashiko Scene Viewer")
                    .with_resizable(!benchmark)
                    .with_decorations(!benchmark)
                    .with_fullscreen(fullscreen)
                    .with_inner_size(if self.route.is_some() {
                        PhysicalSize::new(1920, 1080)
                    } else {
                        PhysicalSize::new(1280, 720)
                    }),
            )?,
        );
        let size = window.inner_size();
        let (scene, mut renderer) = DisplayScene::window(
            &self.package,
            &self.view_name,
            [size.width, size.height],
            window.clone(),
            backend(),
        )?;
        if self.report.is_some() {
            renderer.set_vsync(false)?;
            renderer.set_profiling(self.profile)?;
            if [size.width, size.height] != [1920, 1080] {
                return Err("benchmark requires native 1920x1080".into());
            }
        }
        let view = scene.settings.views[&self.view_name];
        window.request_redraw();
        self.display = Some(Display {
            window,
            renderer,
            scene,
            view,
            keys: [false; 8],
            benchmark,
            playback: self.route.as_ref().map(|_| Instant::now()),
            route: self.route.take(),
            last: Instant::now(),
        });
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.display.is_none()
            && let Err(err) = self.open(events)
        {
            self.result = Err(err);
            events.exit();
        }
    }

    fn window_event(&mut self, events: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(display) = &mut self.display else {
            return;
        };
        let result = match event {
            WindowEvent::CloseRequested => {
                events.exit();
                Ok(())
            }
            WindowEvent::Focused(false) => {
                display.keys.fill(false);
                Ok(())
            }
            WindowEvent::Resized(_) => {
                // Resize notifications queued during creation can describe an old
                // size. Configure the surface from its current physical extent.
                let size = display.window.inner_size();
                display.window.request_redraw();
                display
                    .renderer
                    .resize([size.width, size.height], display.scene.assets())
            }
            WindowEvent::RedrawRequested => display.draw().map(|presented| {
                if self.profile {
                    let elapsed = self
                        .first_present
                        .map_or(0., |first| first.elapsed().as_secs_f64());
                    while display.renderer.take_presentation_profile().is_some() {}
                    if let Some(sample) = display.renderer.take_graph_profile()
                        && let sashiko_renderer::core::GpuTiming::Available { elapsed: gpu, .. } =
                            sample.gpu
                    {
                        self.graph_timings.push([
                            elapsed,
                            gpu.as_secs_f64() * 1000.,
                            sample.cpu_record.as_secs_f64() * 1000.,
                            (sample.cpu_prepare + sample.cpu_submit + sample.cpu_retire)
                                .as_secs_f64()
                                * 1000.,
                        ]);
                    }
                }
                if presented && self.report.is_some() {
                    let now = Instant::now();
                    let first = *self.first_present.get_or_insert(now);
                    let elapsed = now.duration_since(first).as_secs_f64();
                    if self.intervals.is_empty() {
                        display.playback = Some(first);
                    }
                    let previous = self.intervals.last().map_or(0., |row| row[0]);
                    self.intervals.push([elapsed, (elapsed - previous) * 1000.]);
                    if elapsed
                        >= (if self.profile { 2. } else { 4. })
                            * f64::from(display.route.as_ref().unwrap().duration())
                    {
                        events.exit();
                    }
                }
                if presented && let Some(n) = &mut self.frames_left {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        events.exit();
                    }
                }
            }),
            WindowEvent::KeyboardInput { event, .. } => {
                let controls = [
                    KeyCode::KeyW,
                    KeyCode::KeyS,
                    KeyCode::KeyA,
                    KeyCode::KeyD,
                    KeyCode::ArrowLeft,
                    KeyCode::ArrowRight,
                    KeyCode::ArrowUp,
                    KeyCode::ArrowDown,
                ];
                if let PhysicalKey::Code(code) = event.physical_key {
                    if let Some(i) = controls.iter().position(|&c| c == code) {
                        display.keys[i] = event.state.is_pressed();
                    }
                    if code == KeyCode::KeyR
                        && event.state.is_pressed()
                        && !event.repeat
                        && self.report.is_none()
                        && display.route.is_some()
                    {
                        display.playback = if display.playback.is_some() {
                            None
                        } else {
                            Some(Instant::now())
                        };
                    }
                    if code == KeyCode::Escape {
                        events.exit();
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(e) = result {
            self.result = Err(e);
            events.exit();
        }
    }
}

pub(super) fn run(
    package: PathBuf,
    view_name: String,
    frames_left: Option<usize>,
) -> PipelineResult<()> {
    execute(package, view_name, frames_left, None, None)
}

pub(super) fn benchmark(
    package: PathBuf,
    view: String,
    route: Route,
    report: PathBuf,
) -> PipelineResult<()> {
    execute(package, view, None, Some(route), Some(report))
}

pub(super) fn walk(package: PathBuf, view: String, route: Route) -> PipelineResult<()> {
    execute(package, view, None, Some(route), None)
}

fn execute(
    package: PathBuf,
    view_name: String,
    frames_left: Option<usize>,
    route: Option<Route>,
    report: Option<PathBuf>,
) -> PipelineResult<()> {
    let duration = route.as_ref().map(Route::duration);
    let profile = report.is_some() && std::env::var_os("SASHIKO_ROUTE_PROFILE").is_some();
    let mut app = App {
        profile,
        graph_timings: if profile {
            Vec::with_capacity(65536)
        } else {
            Vec::new()
        },
        intervals: if report.is_some() {
            Vec::with_capacity(65536)
        } else {
            Vec::new()
        },
        route,
        report,
        startup: Instant::now(),
        first_present: None,
        frames_left,
        package,
        view_name,
        display: None,
        result: Ok(()),
    };
    EventLoop::new()?.run_app(&mut app)?;
    if let Some(display) = app.display {
        finish(display.scene, display.renderer.into_runtime()?)?;
    }
    app.result?;
    if let Some(path) = app.report {
        let duration = f64::from(duration.unwrap());
        if app
            .intervals
            .last()
            .is_none_or(|r| r[0] < (if app.profile { 2. } else { 4. }) * duration)
        {
            return Err("benchmark route interrupted".into());
        }
        let report = serde_json::json!({"extent":[1920,1080],"vsync":false,"extent_checked_at_draw":true,"lap_seconds":duration,
            "first_present_seconds":app.first_present.unwrap().duration_since(app.startup).as_secs_f64(),
            "presented_seconds_and_interval_ms":app.intervals,"profile_enabled":app.profile,
            "graph_samples_seconds_gpu_record_other_cpu_ms":app.graph_timings});
        std::fs::write(path, serde_json::to_vec(&report)?)?;
    }
    if app.frames_left.is_some_and(|n| n != 0) {
        return Err("window closed before requested presents".into());
    }
    Ok(())
}
