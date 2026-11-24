//! Interactive and measured runs share the simulation, asset setup, and render path.
mod audio;
mod input;
mod metrics;

mod validation {
    use renderer::core as renderer_core;
    include!("../../renderer/examples/support/validation.rs");
}
use validation::check_validation;

use crate::visual::Visual;
use crate::{RunStart, rate};
use input::{Controls, PLAY_TITLE};
use metrics::{Measurements, Sample};
use renderer::{
    Frame, Renderer, WindowRenderer,
    core::{PresentOutcome, runtime::RuntimeConfig},
    pipeline::PipelineResult,
};
use sashiko::{App, app::Replay, window::WindowHost};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use survivor::{Game, Input, Phase, benchmark_input};
pub struct Options {
    pub config: RuntimeConfig,
    pub extent: [u32; 2],
    pub warmup: usize,
    pub volume: f32,
    pub frames: Option<usize>,
    pub metrics: Option<PathBuf>,
    pub gpu_profile: bool,
    pub replay: Option<Vec<Input>>,
    pub record: Option<PathBuf>,
    pub capture: Option<PathBuf>,
    pub character: String,
    pub catalog: PathBuf,
}

const TIMEOUT: Duration = Duration::from_secs(60);

fn draw(frame: &mut Frame<'_>, visual: &mut Visual, game: &Game) -> PipelineResult<()> {
    let extent = frame.scene_view().extent;
    frame.draw_scene(visual.scene(), 1.0)?;
    frame.draw_overlay(visual.overlay(game, extent))?;
    Ok(())
}

fn capture(
    game: &Game,
    o: &Options,
    extent: [u32; 2],
    path: &Path,
    content: &crate::RecordedVisual,
) -> PipelineResult<()> {
    let mut visual = Visual::new(game, &o.catalog, &o.character)?;
    if &visual.content(&o.character) != content {
        return Err("assets changed before final capture".into());
    }
    let camera = visual.camera(game, extent)?;
    let mut renderer = Renderer::headless(o.config, camera, visual.assets())?;
    visual.prepare_headless(&mut renderer)?;
    visual.sync(game, &camera)?;
    let mut frame = renderer.begin_frame()?;
    draw(&mut frame, &mut visual, game)?;
    let ticket = frame.capture()?;
    let mut pixels = vec![0; extent[0] as usize * extent[1] as usize * 4];
    if !renderer.read_capture(&ticket, &mut pixels, TIMEOUT)? {
        return Err("final scene capture timed out".into());
    }
    let mut output = BufWriter::new(File::create(path)?);
    writeln!(output, "P6\n{} {}\n255", extent[0], extent[1])?;
    for rgba in pixels.as_chunks::<4>().0 {
        output.write_all(&rgba[..3])?;
    }
    output.flush()?;
    let mut runtime = renderer.into_runtime()?;
    visual.release(&mut runtime)?;
    check_validation(runtime)
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

struct PlayWindow<'a> {
    sound: Option<audio::Soundscape>,
    profile: &'a mut crate::profile::Player,
    app: &'a mut App<Game>,
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
        let start = Instant::now();
        let ticks = if self.fixed {
            1
        } else {
            self.app.elapsed_ticks(elapsed)
        };
        self.app.try_advance(
            ticks,
            |tick, game| {
                let input = if let Some(tape) = &self.options.replay {
                    tape[tick]
                } else if self.fixed {
                    benchmark_input(game)
                } else {
                    controls.next_input()
                };
                if let Some(tape) = self.recorded {
                    tape.push(input);
                }
                input
            },
            |game, input| {
                let events = game.step(input);
                if let Some(sound) = &mut self.sound {
                    sound.update(game);
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
        self.sync_camera()?;
        self.draw_time = start.elapsed();
        Ok(())
    }
    fn draw(&mut self) -> PipelineResult<sashiko::window::Presentation> {
        use sashiko::window::Presentation;
        let start = Instant::now();
        let outcome = self
            .renderer
            .draw(|frame| draw(frame, self.visual, &self.app.game))?;
        let now = Instant::now();
        self.draw_time += now.duration_since(start);
        if let Some(samples) = self.measured {
            samples.poll(self.renderer);
        }
        Ok(match outcome {
            PresentOutcome::Presented { .. } => Presentation::Presented(now),
            PresentOutcome::RecreateRequired => Presentation::Resize,
            _ => Presentation::Retry,
        })
    }
    fn presented(&mut self, interval: Duration) {
        if let Some(samples) = self.measured {
            let game = &self.app.game;
            samples.push(Sample {
                interval,
                update: self.update_time,
                draw: self.draw_time,
                active: game.enemies.len(),
                visible: self.visual.visible_enemies(),
                drawn: self.visual.drawn_enemies(),
                shots: game.shots.len(),
                drops: game.drops.len(),
                tick: game.tick,
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
    let mut host = WindowHost::open(PLAY_TITLE, o.extent, Controls::default(), TIMEOUT)?;
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
    let mut visual = Visual::new(&game, &o.catalog, &o.character)?;
    let content = (initial.visual.is_some() || o.record.is_some() || o.capture.is_some())
        .then(|| visual.content(&o.character));
    if initial.visual.is_some() && initial.visual != content {
        return Err(
            "visual replay assets differ from recording (catalog, model, color or environment)"
                .into(),
        );
    }
    initial.visual.clone_from(&content);
    let camera = visual.camera(&game, extent)?;
    let mut renderer = WindowRenderer::new(window.clone(), o.config, camera, visual.assets())?;
    visual.prepare_window(&mut renderer)?;
    if o.metrics.is_some() {
        renderer.set_vsync(false)?;
    }
    if o.gpu_profile {
        renderer.set_profiling(true)?;
    }
    let mut measured = o
        .metrics
        .as_ref()
        .map(|_| Measurements::new(limit.unwrap_or(0), o.gpu_profile));
    let mut recorded = o
        .record
        .as_ref()
        .map(|_| Vec::with_capacity(limit.unwrap_or(0)));
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
    if let Some(mut measured) = measured {
        if !renderer.wait_idle(TIMEOUT)? {
            return Err("presentation drain timed out".into());
        }
        measured.poll(&mut renderer);
        measured.save(o.metrics.as_deref().expect("metrics path"), o.warmup)?;
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
            last_extent,
            path,
            content.as_ref().expect("capture content recorded"),
        )?;
    }
    Ok(app.game)
}
