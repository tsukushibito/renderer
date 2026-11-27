use super::Evidence;
use sashiko::inspect::{
    Event, EventSchema, Field, Journal, Kind, Object, Schema, Session, StdioPort, Value, World,
    jobs::{Action, Capabilities, CaptureRequest, Jobs, Status, Ticket},
    report::{FrameStamp, Outcome},
};
use sashiko_renderer::{WindowRenderer, pipeline::PipelineResult};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Instant,
};
use survivor::{Events, Game, Input, Phase};

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
const EVENTS: &[EventSchema] = &[EventSchema {
    id: "gameplay",
    description: "Survivor fixed update gameplay outcomes",
    fields: &[],
}];
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
    base: u64,
    high_id: u64,
}

impl World for LiveWorld {
    type Facts = Events;
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
            id: self.base,
            kind: "Player",
            name: "Survivor",
        };
        for object in std::iter::once(player)
            .chain(self.game.enemies.iter().map(|enemy| Object {
                id: self.base + 1 + enemy.id,
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
        if id == self.base {
            return Some("Player");
        }
        self.enemy(id).map(|_| "Enemy")
    }
    fn read(&self, id: u64, field: &str) -> Option<Value<'_>> {
        let pos = if id == self.base {
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
        if self.game.phase == Phase::Defeated && input.restart {
            self.base = self.high_id.checked_add(1).expect("world ID overflow");
        }
        let events = self.game.step(input);
        if let Some(max) = self.game.enemies.iter().map(|e| e.id).max() {
            self.high_id = self.high_id.max(self.base + 1 + max);
        }
        self.evidence.observe(&self.game, input, events);
        if events != Events::default() {
            journal.emit(Event {
                kind: "gameplay",
                subject: self.base,
                actor: None,
                facts: events,
            });
        }
        self.last_events = events;
    }
}
impl LiveWorld {
    fn enemy(&self, id: u64) -> Option<&survivor::Enemy> {
        let local = id.checked_sub(self.base)?.checked_sub(1)?;
        self.game.enemies.iter().find(|e| e.id == local)
    }
}

/// The original game remains the owner of simulation state; all three controls
/// advance this same Session and publish the same job status.
pub(crate) struct Live {
    session: Session<LiveWorld>,
    port: Option<StdioPort<Events>>,
    root: PathBuf,
    origin: Instant,
    next_frame: u64,
    waiting: Option<Ticket>,
    capture: Option<(Ticket, FrameStamp, Evidence)>,
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
        let capabilities = Capabilities {
            actions: vec![Action::Report],
            views: vec!["main".into()],
            stages: vec!["final_srgb_ui".into()],
        };
        let high_id = game.enemies.iter().map(|e| e.id + 1).max().unwrap_or(0);
        evidence.last_tick = game.tick;
        let mut session = Session::new(LiveWorld {
            game,
            evidence,
            next_input: Input::default(),
            last_events: Events::default(),
            base: 0,
            high_id,
        })
        .with_jobs(Jobs::new(capabilities, 16.try_into()?));
        session.set_paused(false);
        session.world_mut().evidence.session = session.session_id().to_owned();
        let (send, done) = mpsc::channel();
        Ok(Self {
            session,
            port: stdio.then(StdioPort::open),
            root: root.into(),
            origin: Instant::now(),
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
    pub(crate) fn poll(&mut self, manual: bool) -> PipelineResult<()> {
        if self
            .port
            .as_mut()
            .is_some_and(|port| !port.poll(&mut self.session))
        {
            self.port = None;
            self.session.set_paused(false);
        }
        let tick = self.game().tick;
        let jobs = self.session.jobs().unwrap();
        while let Ok((id, status)) = self.done.try_recv() {
            jobs.update(id, status)?;
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
        if self.waiting.is_none() && self.capture.is_none() && self.worker.is_none() {
            self.waiting = jobs.take();
        }
        Ok(())
    }
    pub(crate) fn draw(
        &mut self,
        renderer: &mut WindowRenderer,
        draw: impl FnOnce(&mut sashiko_renderer::Frame<'_>, &Game) -> PipelineResult<()>,
    ) -> PipelineResult<sashiko_renderer::core::PresentOutcome> {
        self.collect(renderer)?;
        let mut submitted = false;
        let game = self.game();
        let render = |frame: &mut sashiko_renderer::Frame<'_>| {
            draw(frame, game)?;
            submitted = true;
            Ok(())
        };
        let tag = self.next_frame;
        let capturing = self.waiting.is_some() && renderer.capture_available();
        let time = self.origin.elapsed();
        let outcome = if capturing {
            renderer.draw_with_capture(tag, render)?
        } else {
            renderer.draw(render)?
        };
        if submitted {
            self.next_frame = tag.checked_add(1).ok_or("diagnostic frame overflow")?;
            if capturing {
                let ticket = self.waiting.take().unwrap();
                let stamp = FrameStamp {
                    frame: tag,
                    tick: self.game().tick,
                    elapsed_ns: time.as_nanos().try_into()?,
                    interpolation: None,
                    view: ticket.request.view.clone(),
                    stage: ticket.request.stage.clone(),
                    extent: [0, 0],
                };
                self.session
                    .jobs()
                    .unwrap()
                    .update(ticket.id, Status::Running { phase: "gpu" })?;
                self.capture = Some((ticket, stamp, self.session.world().evidence.clone()));
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
        let (ticket, mut stamp, evidence) = self.capture.take().unwrap();
        if image.tag != stamp.frame {
            return Err("capture submission identity mismatch".into());
        }
        stamp.extent = image.extent;
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
            let lut: [u8; 256] = std::array::from_fn(|i| {
                let linear = i as f64 / 255.;
                ((if linear <= 0.0031308 {
                    12.92 * linear
                } else {
                    1.055 * linear.powf(1. / 2.4) - 0.055
                }) * 255.)
                    .round() as u8
            });
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
        self.poll(false)
    }
}
