use super::{backend, finish};
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
        self.renderer
            .set_camera(self.view.camera(extent, [0.02, 1500.])?.camera);
        let outcome = self
            .renderer
            .draw(|frame| Ok(frame.draw_scene(self.scene.input(), 1.)?))?;
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
    result: PipelineResult<()>,
}

impl App {
    fn open(&mut self, events: &ActiveEventLoop) -> PipelineResult<()> {
        let window = Arc::new(
            events.create_window(
                Window::default_attributes()
                    .with_title("Sashiko Landscape")
                    .with_inner_size(PhysicalSize::new(1280, 720)),
            )?,
        );
        let size = window.inner_size();
        let (scene, renderer) = DisplayScene::window(
            &self.package,
            &self.view_name,
            [size.width, size.height],
            window.clone(),
            backend(),
        )?;
        let view = scene.settings.views[&self.view_name];
        window.request_redraw();
        self.display = Some(Display {
            window,
            renderer,
            scene,
            view,
            keys: [false; 8],
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
            WindowEvent::Resized(size) => {
                display.window.request_redraw();
                display
                    .renderer
                    .resize([size.width, size.height], display.scene.assets())
            }
            WindowEvent::RedrawRequested => display.draw().map(|presented| {
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
    let mut app = App {
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
    if app.frames_left.is_some_and(|n| n != 0) {
        return Err("window closed before requested presents".into());
    }
    Ok(())
}
