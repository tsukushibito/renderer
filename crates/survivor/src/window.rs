//! Interactive and measured runs share the simulation, asset setup, and render path.
mod input;
mod metrics;

mod validation {
    use renderer::core as renderer_core;
    include!("../../renderer/examples/support/validation.rs");
}
use validation::check_validation;

use crate::visual::{CharacterVariant, Visual};
use input::{Controls, PLAY_TITLE};
use metrics::{Measurements, Sample};
use renderer::{
    Frame, Renderer, WindowRenderer,
    core::{PresentOutcome, runtime::RuntimeConfig},
    pipeline::PipelineResult,
};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use survivor::{Game, Input, Phase, TICK_HZ, benchmark_input};
use winit::{
    event_loop::EventLoop,
    platform::{pump_events::EventLoopExtPumpEvents, windows::EventLoopBuilderExtWindows},
    window::Window,
};

pub struct Options {
    pub config: RuntimeConfig,
    pub extent: [u32; 2],
    pub warmup: usize,
    pub frames: Option<usize>,
    pub metrics: Option<PathBuf>,
    pub gpu_profile: bool,
    pub replay: Option<Vec<Input>>,
    pub record: Option<PathBuf>,
    pub capture: Option<PathBuf>,
    pub alternate_model: bool,
}

fn variant(alternate: bool) -> CharacterVariant {
    if alternate {
        CharacterVariant::Alternate
    } else {
        CharacterVariant::Primary
    }
}

const TIMEOUT: Duration = Duration::from_secs(60);

fn open_window(events: &mut EventLoop<()>, controls: &mut Controls) -> PipelineResult<Arc<Window>> {
    let start = Instant::now();
    loop {
        events.pump_app_events(Some(Duration::from_millis(5)), controls);
        if let Some(error) = controls.error.take() {
            return Err(error.into());
        }
        if let Some(window) = &controls.window {
            return Ok(window.clone());
        }
        if controls.closed || start.elapsed() > TIMEOUT {
            return Err("could not open the survivor window".into());
        }
    }
}

fn draw(frame: &mut Frame<'_>, visual: &mut Visual, game: &Game) -> PipelineResult<()> {
    let extent = frame.scene_view().extent;
    frame.draw_scene(visual.scene(), 1.0)?;
    frame.draw_overlay(visual.overlay(game, extent))?;
    Ok(())
}

fn capture(game: &Game, o: &Options, extent: [u32; 2], path: &Path) -> PipelineResult<()> {
    let mut visual = Visual::new(game, variant(o.alternate_model))?;
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
    for rgba in pixels.chunks_exact(4) {
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

pub fn run(mut game: Game, o: Options) -> PipelineResult<Game> {
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
    let mut events = EventLoop::builder().with_any_thread(true).build()?;
    let mut controls = Controls::new(o.extent);
    let window = open_window(&mut events, &mut controls)?;
    let physical = window.inner_size();
    let mut extent = [physical.width, physical.height];
    if o.metrics.is_some() && extent != o.extent {
        return Err(format!(
            "requested benchmark size {:?}, received {extent:?}",
            o.extent
        )
        .into());
    }
    let mut visual = Visual::new(&game, variant(o.alternate_model))?;
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
    let mut presented = 0;
    let mut last_present = Instant::now();
    let mut clock = last_present;
    let mut accumulator = 0u128;
    let mut current_phase = game.phase;
    let mut last_extent = extent;
    'frames: while limit.is_none_or(|total| presented < total) {
        events.pump_app_events(Some(Duration::ZERO), &mut controls);
        if controls.closed {
            break;
        }
        if let Some(size) = controls.resize.take() {
            if o.metrics.is_some() && size != o.extent {
                return Err("window resized during benchmark".into());
            }
            renderer.resize(size, visual.assets())?;
            extent = size;
        }
        if extent.contains(&0) {
            events.pump_app_events(Some(Duration::from_millis(16)), &mut controls);
            clock = Instant::now();
            continue;
        }
        last_extent = extent;
        let start = Instant::now();
        let ticks = if limit.is_some() {
            // A frame in the repeatable workload consumes exactly one tick, also on slow GPUs.
            1
        } else {
            accumulator += start.duration_since(clock).as_nanos() * u128::from(TICK_HZ);
            let ticks = accumulator / 1_000_000_000;
            accumulator %= 1_000_000_000;
            ticks
        };
        clock = start;
        for _ in 0..ticks {
            let input = if let Some(tape) = &o.replay {
                tape[presented]
            } else if limit.is_some() {
                benchmark_input(&game)
            } else {
                controls.next_input()
            };
            game.step(input);
            if let Some(tape) = &mut recorded {
                tape.push(input);
            }
        }
        let update = start.elapsed();
        if game.phase != current_phase {
            current_phase = game.phase;
            window.set_title(title(current_phase));
        }
        let camera = visual.camera(&game, extent)?;
        renderer.set_camera(camera.camera);
        let draw_start = Instant::now();
        visual.sync(&game, &camera)?;
        let mut draw_cost = draw_start.elapsed();
        loop {
            let attempt = Instant::now();
            let outcome = renderer.draw(|frame| draw(frame, &mut visual, &game))?;
            draw_cost += attempt.elapsed();
            let now = Instant::now();
            if let Some(measured) = measured.as_mut() {
                measured.poll(&mut renderer);
            }
            if matches!(outcome, PresentOutcome::Presented { .. }) {
                if let Some(measured) = measured.as_mut() {
                    measured.push(Sample {
                        interval: now.duration_since(last_present),
                        update,
                        draw: draw_cost,
                        active: game.enemies.len(),
                        visible: visual.visible_enemies(),
                        drawn: visual.drawn_enemies(),
                        shots: game.shots.len(),
                        drops: game.drops.len(),
                        tick: game.tick,
                    });
                }
                last_present = now;
                presented += 1;
                break;
            }
            events.pump_app_events(Some(Duration::ZERO), &mut controls);
            if controls.closed {
                break 'frames;
            }
            if matches!(outcome, PresentOutcome::RecreateRequired) {
                let size = window.inner_size();
                controls.resize = Some([size.width, size.height]);
            }
            if let Some(size) = controls.resize.take() {
                if measured.is_some() && (size != o.extent || o.gpu_profile) {
                    return Err("benchmark presentation invalidated".into());
                }
                renderer.resize(size, visual.assets())?;
                extent = size;
                if !extent.contains(&0) {
                    let camera = visual.camera(&game, extent)?;
                    renderer.set_camera(camera.camera);
                    visual.sync(&game, &camera)?;
                    last_extent = extent;
                }
            }
            if extent.contains(&0) {
                events.pump_app_events(Some(Duration::from_millis(16)), &mut controls);
                clock = Instant::now();
            } else if now.duration_since(last_present) > TIMEOUT && limit.is_some() {
                return Err(format!("presentation timeout at tick {}", game.tick).into());
            } else {
                std::thread::yield_now();
            }
        }
    }
    if limit.is_some_and(|limit| presented < limit) {
        return Err(format!("window closed early after {presented} frames").into());
    }
    if !renderer.wait_idle(TIMEOUT)? {
        return Err("presentation drain timed out".into());
    }
    if let Some(mut measured) = measured {
        measured.poll(&mut renderer);
        measured.save(o.metrics.as_deref().expect("metrics path"), o.warmup)?;
    }
    if let (Some(path), Some(tape)) = (&o.record, recorded) {
        let mut output = BufWriter::new(File::create(path)?);
        serde_json::to_writer(&mut output, &tape)?;
        output.flush()?;
    }
    let mut runtime = renderer.into_runtime()?;
    visual.release(&mut runtime)?;
    check_validation(runtime)?;
    if let Some(path) = &o.capture {
        capture(&game, &o, last_extent, path)?;
    }
    Ok(game)
}
