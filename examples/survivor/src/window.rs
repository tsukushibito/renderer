//! Interactive and measured runs share the simulation, asset setup, and render path.
#[cfg(feature = "memory-profiling")]
mod allocations;
mod audio;
mod input;
mod memory;
mod metrics;

mod validation {
    use sashiko_renderer::core as sashiko_renderer_core;
    include!("../../../crates/sashiko-renderer/examples/support/validation.rs");
}
use validation::check_validation;

use crate::{RunStart, rate};
#[cfg(feature = "memory-profiling")]
use allocations::{Phase as AllocationPhase, mark};
use input::{Controls, PLAY_TITLE};
use metrics::{Measurements, Sample};
use sashiko::{App, app::Replay, window::WindowHost};
use sashiko_renderer::{
    Renderer, WindowRenderer,
    core::{PresentOutcome, runtime::RuntimeConfig},
    pipeline::PipelineResult,
};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use survivor::visual::Visual;
use survivor::{Game, Input, Phase, benchmark_input};
pub struct Options {
    #[cfg(feature = "diagnostics")]
    pub debug_dir: Option<PathBuf>,
    #[cfg(feature = "diagnostics")]
    pub inspect_stdio: bool,
    pub config: RuntimeConfig,
    pub parallelism: std::num::NonZeroUsize,
    pub extent: [u32; 2],
    pub warmup: usize,
    pub volume: f32,
    pub frames: Option<usize>,
    pub metrics: Option<PathBuf>,
    pub memory: Option<PathBuf>,
    pub gpu_profile: bool,
    pub replay: Option<Vec<Input>>,
    pub record: Option<PathBuf>,
    pub capture: Option<PathBuf>,
    pub character: String,
    pub characters: survivor::visual::CharacterRendering,
    pub catalog: PathBuf,
}

#[cfg(feature = "memory-profiling")]
use sashiko_renderer::core::measurement::CountingAllocator;
#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: CountingAllocator<std::alloc::System> =
    CountingAllocator::new(std::alloc::System);

const TIMEOUT: Duration = Duration::from_secs(60);

fn capture(
    game: &Game,
    o: &Options,
    jobs: sashiko::jobs::SharedJobs,
    extent: [u32; 2],
    path: &Path,
    content: &crate::RecordedVisual,
) -> PipelineResult<()> {
    let visual = Visual::new(game, &o.catalog, &o.character, o.characters, jobs)?;
    if &visual.content(&o.character) != content {
        return Err("assets changed before final capture".into());
    }
    let image = render_capture(game, extent, o.config, visual)?;
    image.write_ppm(BufWriter::new(File::create(path)?))?;
    Ok(())
}

pub(super) fn render_capture(
    game: &Game,
    extent: [u32; 2],
    config: RuntimeConfig,
    mut visual: Visual,
) -> PipelineResult<sashiko::capture::CaptureImage> {
    let camera = visual.camera(game, extent)?;
    let mut renderer = Renderer::headless(config, camera, visual.assets())?;
    visual.prepare_headless(&mut renderer)?;
    visual.sync(game, &camera)?;
    let image = sashiko::capture::CaptureImage::render(&mut renderer, TIMEOUT, |frame| {
        visual.draw(frame, game)
    })?;
    let mut runtime = renderer.into_runtime()?;
    visual.release(&mut runtime)?;
    check_validation(runtime)?;
    Ok(image)
}

fn title(phase: Phase) -> &'static str {
    match phase {
        Phase::Playing => PLAY_TITLE,
        Phase::Choosing => {
            "Containment Lab | Choose upgrade: 1 Rapid fire    2 Piercing rounds    3 Shockwave"
        }
        Phase::Defeated => "Containment Lab | Containment failed    Enter / R to retry    Esc quit",
    }
}

#[cfg(not(feature = "diagnostics"))]
type PlayState = Game;
#[cfg(feature = "diagnostics")]
enum PlayState {
    Normal(Box<Game>),
    Observed(Box<crate::diagnostics::live::Live>),
}
#[cfg(feature = "diagnostics")]
impl std::ops::Deref for PlayState {
    type Target = Game;
    fn deref(&self) -> &Game {
        match self {
            Self::Normal(g) => g,
            Self::Observed(s) => s.game(),
        }
    }
}
#[cfg(feature = "diagnostics")]
impl PlayState {
    fn step(&mut self, input: Input) -> survivor::Events {
        match self {
            Self::Normal(g) => g.step(input),
            Self::Observed(s) => s.step(input),
        }
    }
}

struct PlayWindow<'a> {
    sound: Option<audio::Soundscape>,
    profile: &'a mut crate::profile::Player,
    app: &'a mut App<PlayState>,
    window: std::sync::Arc<sashiko::window::NativeWindow>,
    visual: &'a mut Visual,
    renderer: &'a mut WindowRenderer,
    measured: &'a mut Option<Measurements>,
    recorded: &'a mut Option<Vec<Input>>,
    options: &'a Options,
    extent: [u32; 2],
    last_extent: [u32; 2],
    current_phase: Phase,
    fixed: bool,
    update_time: Duration,
    draw_time: Duration,
}

impl PlayWindow<'_> {
    fn sync_camera(&mut self) -> PipelineResult<()> {
        let camera = self.visual.camera(&self.app.game, self.extent)?;
        self.renderer.set_camera(camera.camera);
        self.visual.sync(&self.app.game, &camera)
    }
}

impl sashiko::window::WindowGame<Controls> for PlayWindow<'_> {
    type Error = Box<dyn std::error::Error>;
    fn resize(&mut self, size: [u32; 2], retrying: bool) -> PipelineResult<()> {
        if self.measured.is_some()
            && (size != self.options.extent || retrying && self.options.gpu_profile)
        {
            return Err("benchmark presentation invalidated".into());
        }
        self.renderer.resize(size, self.visual.assets())?;
        #[cfg(feature = "diagnostics")]
        if let PlayState::Observed(live) = &mut self.app.game {
            live.invalidate_view();
        }
        self.extent = size;
        if !size.contains(&0) {
            self.last_extent = size;
            if retrying {
                self.sync_camera()?;
            }
        }
        Ok(())
    }
    fn update(&mut self, controls: &mut Controls, elapsed: Duration) -> PipelineResult<()> {
        #[cfg(feature = "diagnostics")]
        if let PlayState::Observed(live) = &mut self.app.game {
            if let Some(point) = controls.pick.take() {
                live.click(point, self.extent);
            }
            live.poll(
                std::mem::take(&mut controls.report),
                std::mem::take(&mut controls.video),
                self.extent,
            )?;
        }
        let start = Instant::now();
        #[cfg(feature = "memory-profiling")]
        mark(self.measured, AllocationPhase::Host);
        let ticks = if self.fixed {
            1
        } else {
            self.app.elapsed_ticks(elapsed)
        };
        #[cfg(feature = "diagnostics")]
        let ticks = if matches!(&self.app.game, PlayState::Observed(s) if s.paused()) {
            0
        } else {
            ticks
        };
        self.app.try_advance(
            ticks,
            |tick, game| {
                let input = if let Some(tape) = &self.options.replay {
                    tape[tick]
                } else if self.fixed {
                    benchmark_input(game)
                } else {
                    input::next_input(controls)
                };
                if let Some(tape) = self.recorded {
                    tape.push(input);
                }
                input
            },
            |game, input| {
                let events = game.step(input);
                if let Some(sound) = &mut self.sound {
                    sound.update(game, events.footsteps);
                }
                self.profile.observe(events.kills)
            },
        )?;

        self.update_time = start.elapsed();
        if self.app.game.phase != self.current_phase {
            self.current_phase = self.app.game.phase;
            self.window.set_title(title(self.current_phase));
        }
        let start = Instant::now();
        #[cfg(feature = "memory-profiling")]
        mark(self.measured, AllocationPhase::Game);
        self.sync_camera()?;
        #[cfg(feature = "memory-profiling")]
        mark(self.measured, AllocationPhase::Scene);
        self.draw_time = start.elapsed();
        Ok(())
    }
    fn draw(&mut self) -> PipelineResult<sashiko::window::Presentation> {
        use sashiko::window::Presentation;
        let start = Instant::now();
        #[cfg(feature = "memory-profiling")]
        mark(self.measured, AllocationPhase::Host);
        #[cfg(feature = "diagnostics")]
        let overlay = match &self.app.game {
            PlayState::Normal(_) => None,
            PlayState::Observed(live) => Some(live.overlay()),
        };
        let mut render = |frame: &mut sashiko_renderer::Frame<'_>, game: &Game| {
            #[cfg(feature = "memory-profiling")]
            mark(self.measured, AllocationPhase::Backend);
            #[cfg(feature = "diagnostics")]
            if let Some((layers, velocity)) = overlay {
                self.visual.draw_debug(frame, game, layers, velocity)?;
            } else {
                self.visual.draw(frame, game)?;
            }
            #[cfg(not(feature = "diagnostics"))]
            self.visual.draw(frame, game)?;
            #[cfg(feature = "memory-profiling")]
            mark(self.measured, AllocationPhase::Graph);
            Ok(())
        };
        #[cfg(feature = "diagnostics")]
        let outcome = match &mut self.app.game {
            PlayState::Observed(live) => live.draw(self.renderer, render)?,
            PlayState::Normal(game) => self.renderer.draw(|frame| render(frame, game))?,
        };
        #[cfg(not(feature = "diagnostics"))]
        let outcome = self.renderer.draw(|frame| render(frame, &self.app.game))?;
        #[cfg(feature = "memory-profiling")]
        mark(self.measured, AllocationPhase::Backend);
        let now = Instant::now();
        self.draw_time += now.duration_since(start);
        if let Some(samples) = self.measured {
            samples.poll(self.renderer);
        }
        #[cfg(feature = "memory-profiling")]
        mark(self.measured, AllocationPhase::Metrics);
        Ok(match outcome {
            PresentOutcome::Presented { .. } => Presentation::Presented(now),
            PresentOutcome::RecreateRequired => Presentation::Resize,
            _ => Presentation::Retry,
        })
    }
    fn presented(&mut self, interval: Duration) {
        if let Some(samples) = self.measured {
            let game = &self.app.game;
            #[cfg(feature = "memory-profiling")]
            let memory = samples.allocations.presented();
            samples.push(Sample {
                #[cfg(feature = "memory-profiling")]
                memory,
                #[cfg(feature = "memory-profiling")]
                pose_bytes: self.visual.take_pose_bytes(),
                interval,
                update: self.update_time,
                draw: self.draw_time,
                active: game.enemies.len(),
                visible: self.visual.visible_enemies(),
                drawn: self.visual.drawn_enemies(),
                shots: game.shots.len(),
                drops: game.drops.len(),
                tick: game.tick,
                pulse: game.pulse_radius > 0.,
                upgrades: game.upgrades,
            });
        }
    }
}
pub fn run(
    game: Game,
    mut initial: RunStart,
    o: Options,
    profile: &mut crate::profile::Player,
) -> PipelineResult<Game> {
    let limit = match o.frames {
        Some(frames) => Some(frames.checked_add(o.warmup).ok_or("frame count overflow")?),
        None => o.replay.as_ref().map(Vec::len),
    };
    if let (Some(limit), Some(tape)) = (limit, &o.replay)
        && tape.len() < limit
    {
        return Err("input replay is shorter than warmup + measured frames".into());
    }
    if o.gpu_profile && o.metrics.is_none() || o.metrics.is_some() && o.frames.is_none() {
        return Err("profiling requires --metrics; metrics require --frames".into());
    }
    let mut memory = memory::Snapshots::new(o.memory.as_deref());
    memory.before_preparation("before_window", &game)?;
    let mut controls = input::controls();
    if limit.is_some() {
        controls.gamepad_slot = None;
    }
    let host = WindowHost::open(PLAY_TITLE, o.extent, controls, TIMEOUT)?;
    let window = host.window();
    let physical = window.inner_size();
    let extent = [physical.width, physical.height];
    if o.metrics.is_some() && extent != o.extent {
        return Err(format!(
            "requested benchmark size {:?}, received {extent:?}",
            o.extent
        )
        .into());
    }
    let jobs = sashiko::jobs::SharedJobs::from(sashiko::jobs::JobSystem::new(o.parallelism)?);
    let mut visual = Visual::new(&game, &o.catalog, &o.character, o.characters, jobs.clone())?;
    #[cfg(feature = "diagnostics")]
    let inspecting = o.debug_dir.is_some();
    #[cfg(not(feature = "diagnostics"))]
    let inspecting = false;
    let content =
        (initial.visual.is_some() || o.record.is_some() || o.capture.is_some() || inspecting)
            .then(|| visual.content(&o.character));
    if initial.visual.is_some() && initial.visual != content {
        return Err(
            "visual replay assets differ from recording (catalog, model, color or environment)"
                .into(),
        );
    }
    initial.visual.clone_from(&content);
    memory.before_preparation("visual_assets", &game)?;
    let camera = visual.camera(&game, extent)?;
    let mut renderer = WindowRenderer::new(window.clone(), o.config, camera, visual.assets())?;
    memory.observe("window_renderer", &mut renderer, &game)?;
    visual.prepare_window(&mut renderer)?;
    #[cfg(feature = "diagnostics")]
    if inspecting {
        renderer.prepare_capture(visual.assets())?;
    }
    if o.metrics.is_some() {
        renderer.set_vsync(false)?;
    }
    if o.gpu_profile {
        renderer.set_profiling(true)?;
    }
    memory.observe("prepared", &mut renderer, &game)?;
    let mut measured = o
        .metrics
        .as_ref()
        .map(|_| Measurements::new(limit.unwrap_or(0), o.gpu_profile));
    let mut recorded = o
        .record
        .as_ref()
        .map(|_| Vec::with_capacity(limit.unwrap_or(0)));
    #[cfg(feature = "diagnostics")]
    let game = match &o.debug_dir {
        Some(root) => {
            let evidence = crate::diagnostics::Evidence::new(
                &initial,
                o.parallelism,
                limit.unwrap_or(216_000),
            )?;
            PlayState::Observed(Box::new(crate::diagnostics::live::Live::new(
                game,
                evidence,
                root,
                o.inspect_stdio,
            )?))
        }
        None => PlayState::Normal(Box::new(game)),
    };
    let mut app = App::new(game, rate());
    let current_phase = app.game.phase;
    let mut client = PlayWindow {
        sound: if limit.is_none() {
            audio::Soundscape::open(o.volume)
        } else {
            None
        },
        profile,
        app: &mut app,
        window,
        visual: &mut visual,
        renderer: &mut renderer,
        measured: &mut measured,
        recorded: &mut recorded,
        options: &o,
        extent,
        last_extent: extent,
        current_phase,
        fixed: limit.is_some(),
        update_time: Duration::ZERO,
        draw_time: Duration::ZERO,
    };
    host.run(&mut client, limit, TIMEOUT)?;
    let last_extent = client.last_extent;
    #[cfg(feature = "diagnostics")]
    if let PlayState::Observed(live) = &mut app.game {
        live.finish(&mut renderer)?;
    }
    memory.observe("active", &mut renderer, &app.game)?;
    if let Some(mut measured) = measured {
        if !renderer.wait_idle(TIMEOUT)? {
            return Err("presentation drain timed out".into());
        }
        measured.poll(&mut renderer);
        measured.save(o.metrics.as_deref().expect("metrics path"), o.warmup)?;
    }
    if o.memory.is_some() {
        if !renderer.wait_idle(TIMEOUT)? {
            return Err("memory observation drain timed out".into());
        }
        memory.observe("drained", &mut renderer, &app.game)?;
        memory.save()?;
    }
    if let (Some(path), Some(tape)) = (&o.record, recorded) {
        let mut output = BufWriter::new(File::create(path)?);
        serde_json::to_writer(&mut output, &Replay::new(initial, rate(), tape))?;
        output.flush()?;
    }
    let mut runtime = renderer.into_runtime()?;
    visual.release(&mut runtime)?;
    check_validation(runtime)?;
    if let Some(path) = &o.capture {
        capture(
            &app.game,
            &o,
            jobs,
            last_extent,
            path,
            content.as_ref().expect("capture content recorded"),
        )?;
    }
    #[cfg(feature = "diagnostics")]
    return Ok(match app.game {
        PlayState::Normal(g) => *g,
        PlayState::Observed(s) => s.into_game(),
    });
    #[cfg(not(feature = "diagnostics"))]
    Ok(app.game)
}
