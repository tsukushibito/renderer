//! Fixed-size native window, with frame submission and present retirement tracked independently.
use super::super::{Options, PipelineResult, SceneRenderer, WAIT};
use sashiko_renderer::pipeline::PreparedSetup;
use sashiko_renderer_core::{
    PresentOutcome, PresentationConfig,
    graph::ImageDesc,
    profiling::GpuTiming,
    runtime::{FrameSubmission, Runtime, RuntimeConfig, WindowPresenter, WindowReadiness},
};
use serde_json::{Value, json};
use std::{
    rc::Rc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::pump_events::EventLoopExtPumpEvents,
    window::{Window, WindowId},
};

struct Host {
    size: PhysicalSize<u32>,
    window: Option<Rc<Window>>,
    error: Option<String>,
}
impl ApplicationHandler for Host {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.window.is_some() || self.error.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Sashiko StandardPipeline benchmark")
            .with_inner_size(self.size)
            .with_min_inner_size(self.size)
            .with_max_inner_size(self.size)
            .with_resizable(false)
            .with_decorations(false)
            .with_active(false);
        match events.create_window(attrs) {
            Ok(window) => self.window = Some(Rc::new(window)),
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested | WindowEvent::Destroyed) {
            self.error = Some("benchmark window closed".into())
        }
    }
}

#[derive(Default)]
struct Counters {
    accepted: u64,
    retries: u64,
    samples: u64,
    cpu_acquire: Duration,
    cpu_prepare: Duration,
    cpu_record: Duration,
    cpu_submit: Duration,
    cpu_present: Duration,
    cpu_retire: Duration,
    gpu: Duration,
}

pub(in super::super) struct Display {
    // Presenter must release native window ownership before destroying the host.
    presenter: WindowPresenter,
    host: Host,
    events: EventLoop<()>,
    profiling: bool,
    counters: Counters,
}
impl Display {
    pub(super) fn build(
        mut plan: PreparedSetup<'_>,
        mut api: RuntimeConfig,
        opts: &Options,
    ) -> PipelineResult<(SceneRenderer, Self)> {
        let mut events = EventLoop::new()?;
        let [w, h] = opts.extent;
        let mut host = Host {
            size: PhysicalSize::new(w, h),
            window: None,
            error: None,
        };
        let deadline = Instant::now() + WAIT;
        let window = loop {
            events.pump_app_events(Some(Duration::from_millis(2)), &mut host);
            if let Some(reason) = host.error.take() {
                return Err(reason.into());
            }
            if let Some(window) = &host.window
                && window.inner_size() == host.size
            {
                break Rc::clone(window);
            }
            if Instant::now() >= deadline {
                return Err("window creation timed out".into());
            }
        };
        let config = PresentationConfig {
            vsync: false,
            max_frames: 2,
            ..Default::default()
        };
        // The previous graph may retire while its display-linear source remains
        // leased by the presentation submission.
        plan.preparation_mut()
            .reserve_image(ImageDesc::rgba8(w, h), config.max_frames)?;
        plan.fit_backend(&mut api)?;
        let (runtime, mut presenter) = Runtime::for_window(
            api,
            plan.runtime_limits(Default::default())?,
            plan.preparation().working_set(),
            window,
            config,
        )?;
        presenter.set_profiling(opts.profile)?;
        let renderer = plan.begin_with_runtime(runtime)?.wait()?;
        Ok((
            renderer,
            Self {
                presenter,
                events,
                host,
                profiling: opts.profile,
                counters: Counters::default(),
            },
        ))
    }

    pub(super) fn begin_measurement(&mut self) {
        self.counters = Counters::default()
    }

    fn pump(&mut self) -> PipelineResult<()> {
        self.events
            .pump_app_events(Some(Duration::ZERO), &mut self.host);
        if let Some(error) = self.host.error.take() {
            return Err(error.into());
        }
        Ok(())
    }

    pub(super) fn prepare_frame(&mut self) -> PipelineResult<()> {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pump()?;
            let ready = self.presenter.prepare()?;
            self.collect()?;
            match ready {
                WindowReadiness::Ready { extent, .. }
                    if extent == (self.host.size.width, self.host.size.height) =>
                {
                    return Ok(());
                }
                WindowReadiness::Pending if Instant::now() < deadline => std::thread::yield_now(),
                _ => {
                    return Err(
                        format!("presentation unavailable at requested extent: {ready:?}").into(),
                    );
                }
            }
        }
    }
    pub(super) fn present(
        &mut self,
        runtime: &mut Runtime,
        receipt: &mut FrameSubmission,
    ) -> PipelineResult<()> {
        self.presenter.queue_frame(receipt)?;
        let deadline = Instant::now() + WAIT;
        loop {
            let outcome = self.presenter.present_frame(runtime)?;
            self.collect()?;
            match outcome {
                PresentOutcome::Presented { .. } => {
                    self.counters.accepted += 1;
                    return Ok(());
                }
                PresentOutcome::NotReady if Instant::now() < deadline => {
                    self.counters.retries += 1;
                    self.pump()?;
                    std::thread::yield_now();
                }
                _ => return Err(format!("frame did not reach presentation: {outcome:?}").into()),
            }
        }
    }
    fn collect(&mut self) -> PipelineResult<()> {
        if self.profiling {
            while let Some(sample) = self.presenter.take_profile() {
                let GpuTiming::Available { elapsed, .. } = sample.gpu else {
                    return Err("missing presentation GPU timestamp".into());
                };
                let c = &mut self.counters;
                c.samples += 1;
                c.cpu_acquire += sample.cpu_acquire;
                c.cpu_prepare += sample.cpu_prepare;
                c.cpu_record += sample.cpu_record;
                c.cpu_submit += sample.cpu_submit;
                c.cpu_present += sample.cpu_present;
                c.cpu_retire += sample.cpu_retire;
                c.gpu += elapsed;
            }
        }
        Ok(())
    }
    pub(super) fn drain(&mut self) -> PipelineResult<()> {
        if !self.presenter.wait_idle(WAIT)? {
            return Err("presentation drain timed out".into());
        }
        self.collect()
    }
    pub(super) fn finish(&mut self) -> PipelineResult<Value> {
        self.drain()?;
        if !self.presenter.close(WAIT)? {
            return Err("presentation close timed out".into());
        }
        let c = &self.counters;
        if self.profiling && c.accepted != 0 && c.samples == 0 {
            return Err("no completed presentation samples".into());
        }
        Ok(
            json!({"vsync":false,"extent":[self.host.size.width,self.host.size.height],"accepted":c.accepted,
            "not_ready_retries":c.retries,"completed_profile_samples":c.samples,
            "profile_total_ns":{"acquire":c.cpu_acquire.as_nanos() as u64,"prepare":c.cpu_prepare.as_nanos() as u64,
                "record":c.cpu_record.as_nanos() as u64,"submit":c.cpu_submit.as_nanos() as u64,"present":c.cpu_present.as_nanos() as u64,
                "retire":c.cpu_retire.as_nanos() as u64,"gpu":c.gpu.as_nanos() as u64}}),
        )
    }
}
