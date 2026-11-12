//! Minimal Windows presentation example. `--smoke` exercises a bounded run,
//! resize, minimization, restoration and shutdown while work may be pending.
//! `--vsync on|off` selects the initial mode; V toggles it at runtime.

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
        EngineRenderError, MeshCacheConfig, MeshUploadState, PresentOutcome, PresentationConfig,
        Presenter, RenderedFrame, RuntimeConfig, RuntimeErrorKind, ValidationMonitor, VulkanEngine,
        VulkanRuntime,
        engine::{Engine, Material, MaterialHandle, MeshHandle},
    };
    use winit::{
        application::ApplicationHandler,
        dpi::PhysicalSize,
        event::{ElementState, WindowEvent},
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        keyboard::{KeyCode, PhysicalKey},
        window::{Window, WindowId},
    };

    use super::scene;

    const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

    #[derive(Debug, PartialEq, Eq)]
    struct Options {
        smoke: bool,
        vsync: bool,
    }

    impl Options {
        fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
            let mut options = Self {
                smoke: false,
                vsync: true,
            };
            let mut arguments = arguments.into_iter();
            while let Some(argument) = arguments.next() {
                match argument.as_str() {
                    "--smoke" => options.smoke = true,
                    "--vsync" => {
                        options.vsync = match arguments.next().as_deref() {
                            Some("on") => true,
                            Some("off") => false,
                            _ => return Err("--vsync requires on or off".into()),
                        }
                    }
                    _ => {
                        return Err(format!(
                            "unknown argument {argument}; use --smoke and/or --vsync on|off"
                        ));
                    }
                }
            }
            Ok(options)
        }
    }

    struct SceneAssets {
        inputs: Engine,
        mesh: MeshHandle,
        material: MaterialHandle,
    }

    struct App {
        smoke: bool,
        vsync: bool,
        mode_switches: u8,
        window: Option<Arc<Window>>,
        engine: Option<VulkanEngine>,
        presenter: Option<Presenter>,
        assets: Option<SceneAssets>,
        validation: Option<ValidationMonitor>,
        output: Option<RenderedFrame>,
        presented: u32,
        resized: bool,
        minimized: bool,
        restore_at: Option<Instant>,
        started: Instant,
        error: Option<String>,
    }

    impl App {
        fn new(options: Options) -> Self {
            Self {
                smoke: options.smoke,
                vsync: options.vsync,
                mode_switches: 0,
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
                PresentationConfig {
                    vsync: self.vsync,
                    ..Default::default()
                },
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
            let geometry = scene::triangle([1.0, 0.0, 0.0, 1.0])?;
            let engine = VulkanEngine::new(
                runtime,
                MeshCacheConfig {
                    max_meshes: 1,
                    max_bytes: geometry.byte_len(),
                    max_upload_submissions: 1,
                },
            )?;
            let mut inputs = Engine::new(1, 1)?;
            let mesh = inputs.create_mesh(geometry)?;
            let material = inputs.create_material(Material::default())?;
            let assets = SceneAssets {
                inputs,
                mesh,
                material,
            };
            self.window = Some(window);
            self.engine = Some(engine);
            self.presenter = Some(presenter);
            self.assets = Some(assets);
            self.validation = Some(validation);
            self.update_title();
            Ok(())
        }

        fn update_title(&self) {
            if let Some(window) = &self.window {
                window.set_title(&format!(
                    "Renderer M3 - VSync {} (V to toggle)",
                    if self.vsync { "ON" } else { "OFF" }
                ));
            }
        }

        fn toggle_vsync(&mut self) -> Result<(), Box<dyn Error>> {
            if let Some(presenter) = &mut self.presenter {
                presenter.set_vsync(!self.vsync)?;
                self.vsync = !self.vsync;
                self.update_title();
            }
            Ok(())
        }

        fn draw(&mut self) -> Result<(), Box<dyn Error>> {
            let (Some(engine), Some(presenter), Some(assets)) =
                (&mut self.engine, &mut self.presenter, &self.assets)
            else {
                return Ok(());
            };
            while engine.runtime_mut().poll_frame()?.is_some() {}
            engine.poll_uploads()?;
            presenter.poll()?;
            if self.restore_at.is_some() {
                return Ok(());
            }
            match presenter.prepare() {
                Ok(Some(PresentOutcome::Recreated)) => {
                    if let Some(mut output) = self.output.take() {
                        engine.release_output(&mut output)?;
                    }
                }
                Ok(Some(PresentOutcome::NotReady | PresentOutcome::Suspended)) => return Ok(()),
                Ok(_) => {}
                Err(error) if error.kind() == RuntimeErrorKind::CapacityExhausted => return Ok(()),
                Err(error) => return Err(error.into()),
            }
            if self.output.is_none() {
                let Some((width, height)) = presenter.extent() else {
                    return Ok(());
                };
                let view = scene::view(width, height);
                let draws = [scene::draw(assets.mesh, assets.material, 0.0)];
                let snapshot = assets.inputs.snapshot(&view, &draws)?;
                let upload = engine.prepare_snapshot(&snapshot)?;
                if upload.state != MeshUploadState::Ready {
                    return Ok(());
                }
                let prepared = engine.build_snapshot(&snapshot, false, |graph, color| {
                    scene::custom_pass(graph, color, true)
                })?;
                let plan = prepared.graph.compile()?;
                match engine.submit_frame(prepared, &plan) {
                    Ok(rendered) => self.output = Some(rendered),
                    Err(EngineRenderError::Runtime(error))
                        if error.kind() == RuntimeErrorKind::CapacityExhausted =>
                    {
                        return Ok(());
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            let output = self.output.as_mut().expect("output created above");
            match engine.runtime_mut().present(presenter, &output.output) {
                Ok(PresentOutcome::Presented { .. }) => {
                    engine.release_output(output)?;
                    self.output = None;
                    self.presented += 1;
                }
                Ok(PresentOutcome::Recreated | PresentOutcome::RecreateRequired) => {
                    engine.release_output(output)?;
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
                let deadline = Instant::now() + SHUTDOWN_BUDGET;
                loop {
                    let remaining = deadline
                        .checked_duration_since(Instant::now())
                        .ok_or("mesh upload shutdown timed out")?;
                    let progress = engine.wait_uploads(remaining)?;
                    if progress.state == MeshUploadState::Ready && progress.pending_submissions == 0
                    {
                        break;
                    }
                }
                if let Some(mut output) = self.output.take() {
                    engine.release_output(&mut output)?;
                }
                engine.unload_all_meshes()?;
                if let Some(mut assets) = self.assets.take() {
                    assets.inputs.delete_mesh(assets.mesh)?;
                    assets.inputs.delete_material(assets.material)?;
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
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && !event.repeat
                        && event.physical_key == PhysicalKey::Code(KeyCode::KeyV) =>
                {
                    if let Err(error) = self.toggle_vsync() {
                        eprintln!("VSync unchanged: {error}");
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
            if self.smoke
                && ((self.presented >= 2 && self.mode_switches == 0)
                    || (self.presented >= 6 && self.mode_switches == 1))
            {
                if let Err(error) = self.toggle_vsync() {
                    self.fail(event_loop, error);
                    return;
                }
                self.mode_switches += 1;
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
        let options = Options::parse(std::env::args().skip(1))?;
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "renderer=warn".into()),
            )
            .try_init()
            .map_err(|error| error.to_string())?;
        let event_loop = EventLoop::new()?;
        let mut app = App::new(options);
        event_loop.run_app(&mut app)?;
        app.shutdown()?;
        if let Some(error) = app.error {
            return Err(error.into());
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::Options;

        #[test]
        fn options_select_vsync_and_reject_invalid_requests() {
            let parse =
                |args: &[&str]| Options::parse(args.iter().map(|argument| (*argument).to_owned()));
            assert_eq!(
                parse(&[]).unwrap(),
                Options {
                    smoke: false,
                    vsync: true
                }
            );
            assert_eq!(
                parse(&["--vsync", "off", "--smoke"]).unwrap(),
                Options {
                    smoke: true,
                    vsync: false
                }
            );
            assert!(parse(&["--vsync", "on"]).unwrap().vsync);
            for arguments in [&["--vsync"][..], &["--vsync", "invalid"], &["--unknown"]] {
                assert!(parse(arguments).is_err());
            }
        }
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
