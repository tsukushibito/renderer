//! Minimal Windows presentation example. `--smoke` exercises a bounded run,
//! resize, minimization, restoration and shutdown while work may be pending.

#[cfg(windows)]
#[path = "support/scene.rs"]
mod scene;

#[cfg(windows)]
mod demo {
    use std::{
        error::Error,
        sync::Arc,
        time::{Duration, Instant},
    };

    use renderer::{
        EngineRenderError, PresentOutcome, PresentationConfig, Presenter, RuntimeConfig,
        RuntimeErrorKind, ValidationMonitor, VulkanEngine, VulkanRuntime,
        engine::{Material, MaterialHandle, MeshHandle},
        graph::ImportImage,
    };
    use winit::{
        application::ApplicationHandler,
        dpi::PhysicalSize,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        window::{Window, WindowId},
    };

    use super::scene;

    const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

    struct SceneAssets {
        mesh: MeshHandle,
        material: MaterialHandle,
    }

    struct App {
        smoke: bool,
        window: Option<Arc<Window>>,
        engine: Option<VulkanEngine>,
        presenter: Option<Presenter>,
        assets: Option<SceneAssets>,
        validation: Option<ValidationMonitor>,
        output: Option<ImportImage>,
        presented: u32,
        resized: bool,
        minimized: bool,
        restore_at: Option<Instant>,
        started: Instant,
        error: Option<String>,
    }

    impl App {
        fn new(smoke: bool) -> Self {
            Self {
                smoke,
                window: None,
                engine: None,
                presenter: None,
                assets: None,
                validation: None,
                output: None,
                presented: 0,
                resized: false,
                minimized: false,
                restore_at: None,
                started: Instant::now(),
                error: None,
            }
        }

        fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
            if self.window.is_some() {
                return Ok(());
            }
            let window = Arc::new(
                event_loop.create_window(
                    Window::default_attributes()
                        .with_title("Renderer M3")
                        .with_inner_size(PhysicalSize::new(640, 480)),
                )?,
            );
            let (runtime, presenter) = VulkanRuntime::for_window(
                RuntimeConfig::default(),
                Arc::clone(&window),
                PresentationConfig::default(),
            )?;
            let validation = runtime.validation_monitor();
            let report = validation.report();
            if report.core_validation != Some(true)
                || report.synchronization_validation != Some(true)
            {
                return Err(
                    format!("Core/SyncVal effective settings are unverified: {report:?}").into(),
                );
            }
            let mut engine = VulkanEngine::new(runtime, 4, 4)?;
            let assets = SceneAssets {
                mesh: engine.create_mesh(scene::triangle([1.0, 0.0, 0.0, 1.0])?)?,
                material: engine.create_material(Material::default())?,
            };
            self.window = Some(window);
            self.engine = Some(engine);
            self.presenter = Some(presenter);
            self.assets = Some(assets);
            self.validation = Some(validation);
            Ok(())
        }

        fn draw(&mut self) -> Result<(), Box<dyn Error>> {
            let (Some(engine), Some(presenter), Some(assets)) =
                (&mut self.engine, &mut self.presenter, &self.assets)
            else {
                return Ok(());
            };
            while engine.runtime_mut().poll_frame()?.is_some() {}
            presenter.poll()?;
            if self.restore_at.is_some() {
                return Ok(());
            }
            match presenter.prepare()? {
                Some(PresentOutcome::Recreated) => {
                    if let Some(output) = self.output.take() {
                        engine.runtime_mut().delete(output.key())?;
                    }
                }
                Some(PresentOutcome::NotReady | PresentOutcome::Suspended) => return Ok(()),
                _ => {}
            }
            if self.output.is_none() {
                let Some((width, height)) = presenter.extent() else {
                    return Ok(());
                };
                let view = scene::view(width, height);
                let draws = [scene::draw(assets.mesh, assets.material, 0.0)];
                let prepared = engine.build_frame(&view, &draws, false, |graph, color| {
                    scene::custom_pass(graph, color, width, height, true)
                })?;
                let plan = prepared.graph.compile()?;
                match engine.submit_frame(prepared, &plan) {
                    Ok(rendered) => self.output = Some(rendered.output),
                    Err(EngineRenderError::Runtime(error))
                        if error.kind() == RuntimeErrorKind::CapacityExhausted =>
                    {
                        return Ok(());
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            let output = self.output.as_ref().expect("output created above");
            match engine.runtime_mut().present(presenter, output) {
                Ok(PresentOutcome::Presented { .. }) => {
                    engine.runtime_mut().delete(output.key())?;
                    self.output = None;
                    self.presented += 1;
                }
                Ok(PresentOutcome::Recreated) => {
                    engine.runtime_mut().delete(output.key())?;
                    self.output = None;
                }
                Ok(PresentOutcome::NotReady | PresentOutcome::Suspended) => {}
                Err(error) if error.kind() == RuntimeErrorKind::CapacityExhausted => {}
                Err(error) => return Err(error.into()),
            }
            Ok(())
        }

        fn shutdown(&mut self) -> Result<(), Box<dyn Error>> {
            if let Some(presenter) = &mut self.presenter
                && !presenter.close(SHUTDOWN_BUDGET)?
            {
                return Err("presentation shutdown timed out".into());
            }
            if let Some(engine) = &mut self.engine {
                if !engine.runtime_mut().drain_frames(SHUTDOWN_BUDGET)? {
                    return Err("frame shutdown timed out".into());
                }
                if let Some(output) = self.output.take() {
                    engine.runtime_mut().delete(output.key())?;
                }
                if let Some(assets) = self.assets.take() {
                    engine.delete_mesh(assets.mesh)?;
                    engine.delete_material(assets.material)?;
                }
            }
            self.presenter = None;
            self.engine = None;
            self.window = None;
            if let Some(validation) = &self.validation {
                let report = validation.report();
                if !report.is_clean() {
                    return Err(format!(
                        "Vulkan validation failed, including shutdown: {report:?}"
                    )
                    .into());
                }
            }
            Ok(())
        }

        fn fail(&mut self, event_loop: &ActiveEventLoop, error: impl std::fmt::Display) {
            self.error = Some(error.to_string());
            let _ = self.shutdown();
            event_loop.exit();
        }
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if let Err(error) = self.initialize(event_loop) {
                self.fail(event_loop, error);
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
                    if let Some(presenter) = &mut self.presenter {
                        presenter.resize(size.width, size.height);
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
            if self.smoke && self.started.elapsed() > Duration::from_secs(20) {
                self.fail(event_loop, "windowed smoke run timed out");
                return;
            }
            if self.smoke && self.presented >= 4 && !self.resized {
                self.resized = true;
                if let Some(window) = &self.window {
                    let _ = window.request_inner_size(PhysicalSize::new(512, 384));
                }
            }
            if self.smoke && self.presented >= 8 && !self.minimized {
                self.minimized = true;
                if let Some(window) = &self.window {
                    window.set_minimized(true);
                }
                if let Some(presenter) = &mut self.presenter {
                    presenter.resize(0, 0);
                }
                self.restore_at = Some(Instant::now() + Duration::from_millis(50));
            }
            if self
                .restore_at
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.restore_at = None;
                if let Some(window) = &self.window {
                    window.set_minimized(false);
                    let _ = window.request_inner_size(PhysicalSize::new(640, 480));
                }
                if let Some(presenter) = &mut self.presenter {
                    presenter.resize(640, 480);
                }
            }
            if self.smoke && self.presented >= 12 {
                if let Err(error) = self.shutdown() {
                    self.error = Some(error.to_string());
                }
                event_loop.exit();
                return;
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(4),
            ));
        }
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let smoke = std::env::args()
            .skip(1)
            .any(|argument| argument == "--smoke");
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "renderer=warn".into()),
            )
            .try_init()
            .map_err(|error| error.to_string())?;
        let event_loop = EventLoop::new()?;
        let mut app = App::new(smoke);
        event_loop.run_app(&mut app)?;
        app.shutdown()?;
        if let Some(error) = app.error {
            return Err(error.into());
        }
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    demo::run()
}

#[cfg(not(windows))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("the M3 windowed example currently requires Windows".into())
}
