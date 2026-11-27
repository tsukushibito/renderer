use super::{
    Evidence,
    history::History,
    video::{self, Sample, Video},
};
use sashiko::inspect::{
    Event, EventSchema, Field, Journal, Kind, Object, Schema, Session, StdioPort, Value, World,
    jobs::{Action, Capabilities, CaptureRequest, Jobs, RenderLayers, Status, Ticket, Trigger},
    picking::{PickView, Pointer},
    report::Outcome,
};
use sashiko_renderer::{CapturePlane, WindowRenderer, pipeline::PipelineResult};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Instant,
};
use survivor::{Events, Game, Input};

const fn number(id: &'static str, description: &'static str) -> Field {
    Field {
        id,
        description,
        unit: None,
        kind: Kind::Number,
    }
}
const PLAYER: &[Field] = &[
    number("hp", "Current hit points"),
    number("x", "World X"),
    number("z", "World Z"),
];
const ENEMY: &[Field] = &[number("x", "World X"), number("z", "World Z")];
const EVENTS: &[EventSchema] = &[
    EventSchema {
        id: "gameplay",
        description: "Survivor fixed update gameplay outcomes",
        fields: &[],
    },
    EventSchema {
        id: "damage",
        description: "Player hit by an enemy, with values copied at damage application",
        fields: &[
            number("hp_before", "Player HP before damage"),
            number("hp_after", "Player HP after damage"),
            Field {
                id: "dashing",
                kind: Kind::Bool,
                description: "Enemy had an active dash",
                unit: None,
            },
        ],
    },
];

#[derive(Clone, Copy, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum Facts {
    Gameplay(Events),
    Damage(survivor::Damage),
}
const SCHEMAS: &[Schema] = &[
    Schema {
        id: "Player",
        version: 1,
        description: "Survivor",
        fields: PLAYER,
        events: EVENTS,
    },
    Schema {
        id: "Enemy",
        version: 1,
        description: "Enemy",
        fields: ENEMY,
        events: &[],
    },
];

pub(crate) struct LiveWorld {
    displayed: PickView,
    pub game: Game,
    evidence: Evidence,
    next_input: Input,
    last_events: Events,
    velocity: sashiko::math::Vec2,
    damage_trigger: Option<(u64, u64)>,
    detect_damage: bool,
}

impl World for LiveWorld {
    type Facts = Facts;
    fn displayed(&self) -> Option<&PickView> {
        Some(&self.displayed)
    }
    fn schemas(&self) -> &'static [Schema] {
        SCHEMAS
    }
    fn find(
        &self,
        kind: Option<&str>,
        offset: usize,
        limit: usize,
        visit: &mut dyn FnMut(Object<'_>),
    ) {
        let player = Object {
            id: self.evidence.ids.player,
            kind: "Player",
            name: "Survivor",
        };
        for object in std::iter::once(player)
            .chain(self.game.enemies.iter().map(|enemy| Object {
                id: self.evidence.ids.player + 1 + enemy.id,
                kind: "Enemy",
                name: "Enemy",
            }))
            .filter(|o| kind.is_none_or(|k| k == o.kind))
            .skip(offset)
            .take(limit)
        {
            visit(object);
        }
    }
    fn object_type(&self, id: u64) -> Option<&'static str> {
        if id == self.evidence.ids.player {
            return Some("Player");
        }
        self.enemy(id).map(|_| "Enemy")
    }
    fn read(&self, id: u64, field: &str) -> Option<Value<'_>> {
        let pos = if id == self.evidence.ids.player {
            if field == "hp" {
                return Some(Value::Number(self.game.hp as f64));
            }
            self.game.player
        } else {
            self.enemy(id)?.pos
        };
        match field {
            "x" => Some(Value::Number(pos.x().into())),
            "z" => Some(Value::Number(pos.y().into())),
            _ => None,
        }
    }
    fn step(&mut self, journal: &mut Journal<Self::Facts>) {
        let input = std::mem::take(&mut self.next_input);
        let position = self.game.player;
        let events = self.game.step(input);
        self.velocity = (self.game.player - position) * survivor::TICK_HZ as f32;
        self.evidence.observe(&self.game, input, events);
        let player = self.evidence.ids.player;
        if let Some(damage) = events.damage {
            if self.detect_damage && self.damage_trigger.is_none() {
                self.damage_trigger = Some((
                    self.game.tick,
                    self.evidence.origin.elapsed().as_nanos() as u64,
                ));
            }
            journal.emit(Event {
                kind: "damage",
                subject: player,
                actor: Some(player + 1 + damage.enemy),
                facts: Facts::Damage(damage),
            });
        }
        if events != Events::default() {
            journal.emit(Event {
                kind: "gameplay",
                subject: player,
                actor: None,
                facts: Facts::Gameplay(events),
            });
        }
        self.last_events = events;
    }
}
impl LiveWorld {
    fn enemy(&self, id: u64) -> Option<&survivor::Enemy> {
        let local = id.checked_sub(self.evidence.ids.player)?.checked_sub(1)?;
        self.game.enemies.iter().find(|e| e.id == local)
    }
}

/// The original game remains the owner of simulation state; all three controls
/// advance this same Session and publish the same job status.
pub(crate) struct Live {
    presentation: u64,
    presented_tick: u64,
    readback_frame: Option<u64>,
    stall_at: Option<u64>,
    #[cfg(feature = "tracy")]
    tracy: tracy_client::Client,
    history: Option<History>,
    layers: RenderLayers,
    pending_view: PickView,
    session: Session<LiveWorld>,
    port: Option<StdioPort<Facts>>,
    root: PathBuf,
    origin: Instant,
    next_frame: u64,
    waiting: Option<Ticket>,
    capture: Option<(Sample, Option<(Ticket, Evidence)>)>,
    encoder: Option<std::ffi::OsString>,
    video: Option<Video>,
    stop_job: Option<u64>,
    finishing: Option<thread::JoinHandle<()>>,
    worker: Option<thread::JoinHandle<()>>,
    done: mpsc::Receiver<(u64, Status)>,
    send: mpsc::Sender<(u64, Status)>,
}
impl Live {
    pub(crate) fn new(
        game: Game,
        mut evidence: Evidence,
        root: &Path,
        stdio: bool,
        clip_history: bool,
        stall_at: Option<u64>,
    ) -> PipelineResult<Self> {
        std::fs::create_dir_all(root)?;
        let encoder = video::probe();
        let video_unavailable = encoder
            .is_none()
            .then_some("Python/PyAV FFV1 encoder unavailable (SASHIKO_VIDEO_PYTHON)");
        let clip_unavailable =
            video_unavailable.or_else(|| (!clip_history).then_some("start with --clip-history"));
        if clip_history && let Some(reason) = video_unavailable {
            eprintln!("Survivor clip history: {reason}");
        }
        let clip_history = clip_unavailable.is_none();
        let mut actions = vec![Action::Report, Action::SetLayers];
        if encoder.is_some() {
            actions.extend([Action::StartVideo, Action::StopVideo]);
        }
        if clip_history {
            actions.push(Action::Clip);
        }
        let origin = evidence.origin;
        evidence.performance = Some(super::performance::Log::new(origin));
        let capabilities = Capabilities {
            video_unavailable,
            clip_unavailable,
            actions,
            views: vec!["main".into()],
            stages: vec!["final_srgb_ui".into(), "scene_depth_d32".into()],
            layers: ["hud", "collision", "velocity", "ai"]
                .map(String::from)
                .into(),
            depth_limit: Some(
                "main scene, single-sample D32, still images only; before HUD and debug layers",
            ),
        };

        evidence.last_tick = game.tick;
        let mut session = Session::new(LiveWorld {
            displayed: PickView::new("main", survivor::MAX_ENEMIES + 1),
            game,
            evidence,
            next_input: Input::default(),
            last_events: Events::default(),
            velocity: sashiko::math::Vec2::ZERO,
            damage_trigger: None,
            detect_damage: clip_history,
        })
        .with_jobs(Jobs::new(capabilities, 16.try_into()?));
        session.set_paused(false);
        session.world_mut().evidence.session = session.session_id().to_owned();
        let (send, done) = mpsc::channel();
        Ok(Self {
            presentation: 0,
            presented_tick: 0,
            readback_frame: None,
            stall_at,
            #[cfg(feature = "tracy")]
            tracy: tracy_client::Client::start(),
            layers: RenderLayers::default(),
            history: clip_history.then(History::new),
            pending_view: PickView::new("main", survivor::MAX_ENEMIES + 1),
            session,
            port: stdio.then(StdioPort::open),
            root: root.into(),
            origin,
            encoder,
            video: None,
            stop_job: None,
            finishing: None,
            next_frame: 0,
            waiting: None,
            capture: None,
            worker: None,
            done,
            send,
        })
    }
    pub(crate) fn game(&self) -> &Game {
        &self.session.world().game
    }
    pub(crate) fn overlay(&self) -> (RenderLayers, sashiko::math::Vec2) {
        (self.layers, self.session.world().velocity)
    }
    pub(crate) fn presented(
        &mut self,
        interval: std::time::Duration,
        update: std::time::Duration,
        draw: std::time::Duration,
        extent: [u32; 2],
        enemies: [usize; 2],
        renderer: &mut WindowRenderer,
    ) {
        use super::performance::{Row, ns};
        let frame = self.next_frame.saturating_sub(1);
        let tick = self.game().tick;
        let row = Row {
            frame,
            tick,
            updates: tick.saturating_sub(self.presented_tick),
            presentation: self.presentation,
            elapsed_ns: ns(self.origin.elapsed()),
            interval_ns: ns(interval),
            update_ns: ns(update),
            draw_ns: ns(draw),
            extent,
            visible_enemies: enemies[0],
            drawn_enemies: enemies[1],
            recording: self.video.is_some(),
            history: self.history.is_some(),
            readback: self.readback_frame == Some(frame),
        };
        self.session
            .world_mut()
            .evidence
            .performance
            .as_mut()
            .unwrap()
            .push(row, renderer);
        self.presentation += 1;
        self.presented_tick = tick;
        #[cfg(feature = "tracy")]
        if tracy_client::Client::is_connected() {
            self.tracy.message(
                &format!("{} frame {frame} tick {tick}", self.session.session_id()),
                0,
            );
            self.tracy.frame_mark();
        }
    }
    pub(crate) fn archive_metrics(&self, path: &Path, warmup: usize) -> PipelineResult<()> {
        let session = self.session.session_id();
        let file = format!("{session}.native.csv");
        std::fs::copy(path, self.root.join(&file))?;
        std::fs::write(
            self.root.join(format!("{session}.native.json")),
            serde_json::to_vec(
                &serde_json::json!({"session":session,"warmup":warmup,"file":file}),
            )?,
        )?;
        Ok(())
    }
    pub(crate) fn paused(&self) -> bool {
        self.session.is_paused()
    }
    pub(crate) fn invalidate_view(&mut self) {
        self.pending_view.invalidate();
        self.session.world_mut().displayed.invalidate();
    }
    pub(crate) fn click(&mut self, point: [f64; 2], extent: [u32; 2]) {
        match self.session.pick(&Pointer {
            view: "main".into(),
            frame: None,
            point,
            viewport: [0., 0., extent[0].into(), extent[1].into()],
        }) {
            Ok(result) => eprintln!("Survivor selection: {result}"),
            Err(reason) => eprintln!("Survivor selection: {reason}"),
        }
    }
    pub(crate) fn into_game(self) -> Game {
        self.session.into_world().game
    }
    pub(crate) fn step(&mut self, input: Input) -> Events {
        if self.session.is_paused() {
            return Events::default();
        }
        self.session.world_mut().next_input = input;
        self.session.step();
        self.session.world().last_events
    }
    pub(crate) fn poll(
        &mut self,
        manual: bool,
        toggle_video: bool,
        clip: bool,
        extent: [u32; 2],
    ) -> PipelineResult<()> {
        if self
            .port
            .as_mut()
            .is_some_and(|port| !port.poll(&mut self.session))
        {
            self.port = None;
            self.session.set_paused(false);
        }
        let tick = self.game().tick;
        self.finish_clip(false)?;
        if let Some((t, ns)) = self.session.world_mut().damage_trigger.take() {
            self.enqueue_trigger(Trigger::Gameplay, t, ns);
        }
        if clip {
            self.trigger_now(Trigger::Manual);
        }
        while let Ok((id, status)) = self.done.try_recv() {
            if matches!(status, Status::Failed { .. })
                && self.session.jobs().unwrap().action(id) != Some(Action::Clip)
                && self.history.is_some()
            {
                self.trigger_now(Trigger::Warning);
            }
            if matches!(status, Status::Failed { .. })
                && self.video.as_ref().is_some_and(|v| v.id == id)
            {
                self.finishing = Some(
                    self.video
                        .take()
                        .unwrap()
                        .stop(self.session.world().evidence.clone(), self.stop_job.take()),
                );
            }
            self.session.jobs().unwrap().update(id, status)?;
        }
        let jobs = self.session.jobs().unwrap();
        if self
            .finishing
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            self.finishing
                .take()
                .unwrap()
                .join()
                .map_err(|_| "video worker panicked")?;
        }
        if self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            self.worker
                .take()
                .unwrap()
                .join()
                .map_err(|_| "report worker panicked")?;
        }
        if manual && let Err(reason) = jobs.submit(tick, CaptureRequest::new(Action::Report)) {
            eprintln!("report request: {reason}");
        }
        if toggle_video {
            let action = if self.video.is_some() {
                Action::StopVideo
            } else {
                Action::StartVideo
            };
            if let Err(reason) = jobs.submit(tick, CaptureRequest::new(action)) {
                eprintln!("video request: {reason}");
            }
        }
        if self.waiting.is_none()
            && let Some(ticket) = jobs.take()
        {
            let id = ticket.id;
            let status = match ticket.request.action {
                Action::Clip => {
                    let ns = ticket
                        .request
                        .raised_ns
                        .unwrap_or_else(|| self.origin.elapsed().as_nanos() as u64);
                    self.history
                        .as_mut()
                        .ok_or("clip history disabled")?
                        .trigger(
                            id,
                            ticket.request.trigger.unwrap_or(Trigger::Manual),
                            ticket.requested_tick,
                            ns,
                            self.session.session_id(),
                        )
                }
                Action::SetLayers => {
                    self.layers = ticket.request.layers.ok_or("missing requested layers")?;
                    Status::Applied
                }
                Action::StartVideo if self.video.is_none() && self.finishing.is_none() => {
                    let dir = self
                        .root
                        .join(format!("{}-{id}", self.session.session_id()));
                    let opened = Video::start(
                        id,
                        dir,
                        &self.session.world().evidence,
                        ticket.request.extent.unwrap_or(extent),
                        ticket.request.fps.unwrap_or(30),
                        self.encoder.clone().ok_or("encoder unavailable")?,
                        self.send.clone(),
                    );
                    match opened {
                        Ok(v) => {
                            self.video = Some(v);
                            Status::Running { phase: "recording" }
                        }
                        Err(e) => Status::Failed {
                            reason: e.to_string(),
                        },
                    }
                }
                Action::StopVideo if self.video.is_some() && self.stop_job.is_none() => {
                    self.stop_job = Some(id);
                    Status::Running { phase: "draining" }
                }
                Action::Report => {
                    self.waiting = Some(ticket);
                    return Ok(());
                }
                _ => Status::Failed {
                    reason: "recording state does not support this request".into(),
                },
            };
            self.session.jobs().unwrap().update(id, status)?;
        }
        Ok(())
    }
    fn enqueue_trigger(&mut self, cause: Trigger, tick: u64, ns: u64) {
        let mut request = CaptureRequest::new(Action::Clip);
        request.trigger = Some(cause);
        request.raised_ns = Some(ns);
        if let Err(reason) = self.session.jobs().unwrap().submit(tick, request) {
            eprintln!("clip trigger {cause:?}: {reason}");
        }
    }
    fn trigger_now(&mut self, cause: Trigger) {
        self.enqueue_trigger(
            cause,
            self.game().tick,
            self.origin.elapsed().as_nanos() as u64,
        );
    }
    pub fn frame_time_trigger(&mut self) {
        if self.history.is_some() {
            self.trigger_now(Trigger::FrameTime);
        }
    }
    fn finish_clip(&mut self, closing: bool) -> PipelineResult<()> {
        if self.capture.is_some() && !closing {
            return Ok(());
        }
        if let Some(history) = &mut self.history {
            history.finish_due(
                self.origin.elapsed().as_nanos().try_into()?,
                closing,
                &self.root,
                &self.session.world().evidence,
                self.encoder.as_ref().ok_or("clip encoder unavailable")?,
                &self.send,
            )?;
        }
        Ok(())
    }
    pub(crate) fn draw(
        &mut self,
        renderer: &mut WindowRenderer,
        draw: impl FnOnce(&mut sashiko_renderer::Frame<'_>, &Game) -> PipelineResult<()>,
    ) -> PipelineResult<sashiko_renderer::core::PresentOutcome> {
        self.collect(renderer)?;
        self.finish_clip(false)?;
        if self.stop_job.is_some() && self.capture.is_none() {
            self.finishing = Some(
                self.video
                    .take()
                    .unwrap()
                    .stop(self.session.world().evidence.clone(), self.stop_job.take()),
            );
        }
        let mut submitted = None;
        #[cfg(feature = "tracy")]
        let _zone = tracy_client::Client::is_connected().then(|| {
            let zone = self
                .tracy
                .clone()
                .span(tracy_client::span_location!("survivor.diagnostic.draw"), 0);
            zone.emit_value(self.next_frame);
            zone
        });
        if self.stall_at == Some(self.next_frame) {
            self.stall_at = None;
            std::thread::sleep(std::time::Duration::from_millis(90));
        }
        let game = self.game();
        let render = |frame: &mut sashiko_renderer::Frame<'_>| {
            draw(frame, game)?;
            submitted = Some(*frame.scene_view());
            Ok(())
        };
        let tag = self.next_frame;
        let time = self.origin.elapsed();
        let ns = time.as_nanos().try_into()?;
        let for_video = self.stop_job.is_none() && self.video.as_ref().is_some_and(|v| v.due(ns));
        let still = self.waiting.is_some() && self.worker.is_none();
        let for_history = self.history.as_ref().is_some_and(|h| h.due(ns));
        let capturing = (still || for_video || for_history) && renderer.capture_available();
        let plane = if still
            && self
                .waiting
                .as_ref()
                .is_some_and(|t| t.request.stage == "scene_depth_d32")
        {
            CapturePlane::SceneDepth
        } else {
            CapturePlane::DisplayColor
        };
        let outcome = if capturing {
            renderer.draw_with_capture(tag, plane, render)?
        } else {
            renderer.draw(render)?
        };
        if let Some(view) = submitted {
            let world = self.session.world();
            self.pending_view.begin(tag, world.game.tick, view.extent);
            let matrix = view.camera.clip_from_world();
            let mut project =
                |id, pos: sashiko::math::Vec2, height: f32, radius: f32| -> PipelineResult<()> {
                    if !sashiko::view::on_screen(&view, pos, height, radius) {
                        return Ok(());
                    }
                    let x = matrix[0][0] * pos.x() + matrix[3][0];
                    let y = matrix[2][1] * pos.y() + matrix[1][1] * height * 0.5 + matrix[3][1];
                    let z = matrix[2][2] * pos.y() + matrix[1][2] * height * 0.5 + matrix[3][2];
                    self.pending_view.ellipse(
                        id,
                        [f64::from(x + 1.) / 2., f64::from(y + 1.) / 2.],
                        [
                            f64::from(matrix[0][0].abs() * radius) / 2.,
                            f64::from(matrix[1][1].abs() * height + matrix[2][1].abs() * radius)
                                / 4.,
                        ],
                        z,
                    )?;
                    Ok(())
                };
            let player = world.evidence.ids.player;
            project(player, world.game.player, 1.8, 0.35)?;
            for enemy in &world.game.enemies {
                project(player + 1 + enemy.id, enemy.pos, 1.9, 0.35)?;
            }
            self.next_frame = tag.checked_add(1).ok_or("diagnostic frame overflow")?;
            if capturing {
                self.readback_frame = Some(tag);
                let report = if still {
                    let ticket = self.waiting.take().unwrap();
                    self.session
                        .jobs()
                        .unwrap()
                        .update(ticket.id, Status::Running { phase: "gpu" })?;
                    Some((ticket, self.session.world().evidence.clone()))
                } else {
                    None
                };
                self.capture = Some((
                    Sample {
                        frame: tag,
                        layers: self.layers,
                        tick: self.game().tick,
                        ns,
                        source: [0, 0],
                    },
                    report,
                ));
            } else if for_video || for_history {
                let tick = self.game().tick;
                let sample = Sample {
                    frame: tag,
                    layers: self.layers,
                    tick,
                    ns,
                    source: [0, 0],
                };
                if for_video {
                    self.video.as_mut().unwrap().accept(sample, None);
                }
                if for_history {
                    self.history.as_mut().unwrap().accept(sample, None);
                }
            }
        }
        if matches!(
            outcome,
            sashiko_renderer::core::PresentOutcome::Presented { .. }
        ) {
            std::mem::swap(
                &mut self.pending_view,
                &mut self.session.world_mut().displayed,
            );
        }
        Ok(outcome)
    }
    fn collect(&mut self, renderer: &mut WindowRenderer) -> PipelineResult<()> {
        if self.capture.is_none() {
            return Ok(());
        }
        let Some(image) = renderer.take_capture()? else {
            return Ok(());
        };
        let (mut sample, report) = self.capture.take().unwrap();
        if image.tag != sample.frame {
            return Err("capture submission identity mismatch".into());
        }
        sample.source = image.extent;
        if let Some(history) = &mut self.history {
            history.accept(
                sample,
                (image.plane == CapturePlane::DisplayColor).then_some(image.bytes),
            );
        }
        if let Some(v) = &mut self.video {
            v.accept(
                sample,
                (image.plane == CapturePlane::DisplayColor).then_some(image.bytes),
            );
        }
        let Some((ticket, mut evidence)) = report else {
            return Ok(());
        };
        evidence
            .performance
            .clone_from(&self.session.world().evidence.performance);
        let mut stamp = sample.stamp();
        let plane = image.plane;
        let mut pixels = image.bytes.to_vec();
        let id = ticket.id;
        self.session
            .jobs()
            .unwrap()
            .update(id, Status::Running { phase: "writing" })?;
        let directory = self
            .root
            .join(format!("{}-{id}", self.session.session_id()));
        let send = self.send.clone();
        self.worker = Some(thread::spawn(move || {
            let result = evidence
                .save(&directory, &id.to_string(), |m| {
                    m.requested_tick = ticket.requested_tick;
                    m.world = "game".into();
                    m.job = format!("{}/j{id}", m.session);
                    match plane {
                        CapturePlane::SceneDepth => {
                            std::fs::write(directory.join("depth.f32le"), &pixels)?;
                            m.files.insert("depth".into(), "depth.f32le".into());
                            m.settings["depth"] = serde_json::json!({"format":"D32_FLOAT","layout":"top-down, tightly packed little endian","range":[0,1],"preview":"normalized device depth × 255","source":"main scene, before overlays","msaa":"single-sample only"});
                            stamp.stage = "scene_depth_d32".into();
                            stamp.layers = None;
                            for pixel in pixels.as_chunks_mut::<4>().0 {
                                let level = (f32::from_le_bytes(*pixel).clamp(0.,1.) * 255.).round() as u8;
                                *pixel = [level,level,level,255];
                            }
                        }
                        CapturePlane::DisplayColor => {
                            let lut = video::srgb_table();
                            for pixel in pixels.as_chunks_mut::<4>().0 {
                                for c in &mut pixel[..3] { *c = lut[*c as usize]; }
                            }
                        }
                    }
                    super::save_image(&directory, m, stamp, &pixels)
                })
                .and_then(|()| {
                    sashiko::inspect::report::Manifest::read(&directory).map_err(Into::into)
                });
            let artifact = directory.display().to_string();
            let status = match result {
                Ok(m) => match m.outcome {
                    Outcome::Partial { reason } => Status::Partial { artifact, reason },
                    _ => Status::Complete { artifact },
                },
                Err(e) => Status::Failed {
                    reason: e.to_string(),
                },
            };
            eprintln!("Survivor report {id}: {status:?}");
            let _ = send.send((id, status));
        }));
        Ok(())
    }
    pub(crate) fn finish(&mut self, renderer: &mut WindowRenderer) -> PipelineResult<()> {
        if self.capture.is_some() {
            if !renderer.wait_idle(std::time::Duration::from_secs(60))? {
                return Err("capture drain timed out".into());
            }
            self.collect(renderer)?;
        }
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "report worker panicked")?;
        }
        // A fixed update can raise a trigger just before the host stops drawing.
        if let Some((t, ns)) = self.session.world_mut().damage_trigger.take() {
            self.enqueue_trigger(Trigger::Gameplay, t, ns);
        }
        while let Some(ticket) = self.session.jobs().unwrap().take() {
            let status = if ticket.request.action == Action::Clip {
                self.history
                    .as_mut()
                    .ok_or("clip history disabled")?
                    .trigger(
                        ticket.id,
                        ticket.request.trigger.unwrap_or(Trigger::Manual),
                        ticket.requested_tick,
                        ticket
                            .request
                            .raised_ns
                            .unwrap_or_else(|| self.origin.elapsed().as_nanos() as u64),
                        self.session.session_id(),
                    )
            } else {
                Status::Failed {
                    reason: "session ended before requested operation".into(),
                }
            };
            self.session.jobs().unwrap().update(ticket.id, status)?;
        }
        if let Some(ticket) = self.waiting.take() {
            self.session.jobs().unwrap().update(
                ticket.id,
                Status::Failed {
                    reason: "session ended before next draw".into(),
                },
            )?;
        }
        self.finish_clip(true)?;
        if let Some(v) = self.video.take() {
            self.finishing =
                Some(v.stop(self.session.world().evidence.clone(), self.stop_job.take()));
        }
        if let Some(w) = self.finishing.take() {
            w.join().map_err(|_| "video worker panicked")?;
        }
        while let Ok((id, status)) = self.done.try_recv() {
            self.session.jobs().unwrap().update(id, status)?;
        }
        Ok(())
    }
}
