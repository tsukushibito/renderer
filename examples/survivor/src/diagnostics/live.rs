use super::{
    Evidence,
    video::{self, Sample, Video},
};
use sashiko::inspect::{
    Event, EventSchema, Field, Journal, Kind, Object, Schema, Session, StdioPort, Value, World,
    jobs::{Action, Capabilities, CaptureRequest, Jobs, Status, Ticket},
    report::Outcome,
};
use sashiko_renderer::{WindowRenderer, pipeline::PipelineResult};
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
    pub game: Game,
    evidence: Evidence,
    next_input: Input,
    last_events: Events,
}

impl World for LiveWorld {
    type Facts = Facts;
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
        let events = self.game.step(input);
        self.evidence.observe(&self.game, input, events);
        let player = self.evidence.ids.player;
        if let Some(damage) = events.damage {
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
    ) -> PipelineResult<Self> {
        std::fs::create_dir_all(root)?;
        let encoder = video::probe();
        let mut actions = vec![Action::Report];
        if encoder.is_some() {
            actions.extend([Action::StartVideo, Action::StopVideo]);
        }
        let origin = evidence.origin;
        let capabilities = Capabilities {
            actions,
            views: vec!["main".into()],
            stages: vec!["final_srgb_ui".into()],
        };

        evidence.last_tick = game.tick;
        let mut session = Session::new(LiveWorld {
            game,
            evidence,
            next_input: Input::default(),
            last_events: Events::default(),
        })
        .with_jobs(Jobs::new(capabilities, 16.try_into()?));
        session.set_paused(false);
        session.world_mut().evidence.session = session.session_id().to_owned();
        let (send, done) = mpsc::channel();
        Ok(Self {
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
    pub(crate) fn paused(&self) -> bool {
        self.session.is_paused()
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
        while let Ok((id, status)) = self.done.try_recv() {
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
    pub(crate) fn draw(
        &mut self,
        renderer: &mut WindowRenderer,
        draw: impl FnOnce(&mut sashiko_renderer::Frame<'_>, &Game) -> PipelineResult<()>,
    ) -> PipelineResult<sashiko_renderer::core::PresentOutcome> {
        self.collect(renderer)?;
        if self.stop_job.is_some() && self.capture.is_none() {
            self.finishing = Some(
                self.video
                    .take()
                    .unwrap()
                    .stop(self.session.world().evidence.clone(), self.stop_job.take()),
            );
        }
        let mut submitted = false;
        let game = self.game();
        let render = |frame: &mut sashiko_renderer::Frame<'_>| {
            draw(frame, game)?;
            submitted = true;
            Ok(())
        };
        let tag = self.next_frame;
        let time = self.origin.elapsed();
        let ns = time.as_nanos().try_into()?;
        let for_video = self.stop_job.is_none() && self.video.as_ref().is_some_and(|v| v.due(ns));
        let still = self.waiting.is_some() && self.worker.is_none();
        let capturing = (still || for_video) && renderer.capture_available();
        let outcome = if capturing {
            renderer.draw_with_capture(tag, render)?
        } else {
            renderer.draw(render)?
        };
        if submitted {
            self.next_frame = tag.checked_add(1).ok_or("diagnostic frame overflow")?;
            if capturing {
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
                        tick: self.game().tick,
                        ns,
                        source: [0, 0],
                    },
                    report,
                ));
            } else if for_video {
                let tick = self.game().tick;
                self.video.as_mut().unwrap().accept(
                    Sample {
                        frame: tag,
                        tick,
                        ns,
                        source: [0, 0],
                    },
                    None,
                );
            }
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
        if let Some(v) = &mut self.video {
            v.accept(sample, Some(image.linear_rgba));
        }
        let Some((ticket, evidence)) = report else {
            return Ok(());
        };
        let stamp = sample.stamp();
        let mut pixels = image.linear_rgba.to_vec();
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
            let lut = video::srgb_table();
            for pixel in pixels.as_chunks_mut::<4>().0 {
                for c in &mut pixel[..3] {
                    *c = lut[*c as usize];
                }
            }
            let result = evidence
                .save(&directory, &id.to_string(), |m| {
                    m.requested_tick = ticket.requested_tick;
                    m.world = "game".into();
                    m.job = format!("{}/j{id}", m.session);
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
