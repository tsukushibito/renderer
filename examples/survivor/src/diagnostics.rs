//! Replay evidence captured at completed fixed updates.
use crate::{RunStart, rate};
use sashiko::{
    app::Replay,
    asset::Digest,
    inspect::report::{Gap, Manifest, Outcome},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File},
    io::{self, BufRead, BufReader, BufWriter, Write},
    num::NonZeroUsize,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use survivor::{Events, Game, Input, Snapshot};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Tick {
    snapshot: Snapshot,
    events: Events,
    /// Two words keep the entire fingerprint exact in JSON clients.
    fingerprint: [u32; 2],
}

impl Tick {
    fn observe(game: &Game, events: Events) -> Self {
        let hash = game.replay_fingerprint();
        Self {
            snapshot: game.snapshot(),
            events,
            fingerprint: [(hash >> 32) as u32, hash as u32],
        }
    }
}

#[derive(Clone)]
pub(super) struct Evidence {
    start: RunStart,
    build: String,
    assets: BTreeMap<String, String>,
    session: String,
    workers: NonZeroUsize,
    ticks: Vec<Tick>,
    inputs: Vec<Input>,
    limit: usize,
    last_tick: u64,
}

fn build_id() -> Result<String> {
    let mut executable = File::open(std::env::current_exe()?)?;
    let mut digest = Sha256::new();
    io::copy(&mut executable, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

fn initial_assets(start: &RunStart) -> Result<BTreeMap<String, String>> {
    let mut assets = BTreeMap::new();
    assets.insert(
        "arena".into(),
        Digest::of(&serde_json::to_vec(&start.arena)?).to_string(),
    );
    if let Some(clips) = &start.locomotion {
        assets.insert(
            "locomotion".into(),
            Digest::of(&serde_json::to_vec(clips)?).to_string(),
        );
    }
    if let Some(visual) = &start.visual {
        assets.insert(
            "appearance".into(),
            Digest::of(&serde_json::to_vec(visual)?).to_string(),
        );
    }
    Ok(assets)
}

impl Evidence {
    pub(super) fn new(start: &RunStart, workers: NonZeroUsize, limit: usize) -> Result<Self> {
        let mut ticks = Vec::new();
        let mut inputs = Vec::new();
        ticks.try_reserve_exact(limit)?;
        inputs.try_reserve_exact(limit)?;
        Ok(Self {
            start: start.clone(),
            build: build_id()?,
            assets: initial_assets(start)?,
            session: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ),
            workers,
            ticks,
            inputs,
            limit,
            last_tick: 0,
        })
    }

    pub(super) fn observe(&mut self, game: &Game, input: Input, events: Events) {
        self.last_tick = game.tick;
        // Keep the replayable prefix paired with tick zero. Never evict its beginning.
        if self.ticks.len() < self.limit {
            self.inputs.push(input);
            self.ticks.push(Tick::observe(game, events));
        }
    }

    pub(super) fn save(
        self,
        directory: &Path,
        job: &str,
        extra: impl FnOnce(&mut Manifest) -> Result<()>,
    ) -> Result<()> {
        fs::create_dir(directory)?;
        let mut m = Manifest {
            version: Manifest::VERSION,
            session: self.session.clone(),
            world: "survivor".into(),
            job: job.into(),
            build: self.build.clone(),
            assets: self.assets.clone(),
            requested_tick: self.last_tick,
            settings: serde_json::json!({"physics_workers":self.workers,"tick_hz":rate(),"seed":self.start.seed.to_string(),
                "scenario":self.start.scenario,"rules_version":self.start.rules_version}),
            frames: vec![],
            gaps: vec![],
            files: BTreeMap::new(),
            outcome: Outcome::Recording,
        };
        m.publish(directory)?;
        let write = || -> Result<()> {
            let mut tape = BufWriter::new(File::create(directory.join("replay.json"))?);
            // Serialize the existing Replay wire format, including its immutable start.
            serde_json::to_writer(
                &mut tape,
                &Replay::new(&self.start, rate(), self.inputs),
            )?;
            tape.flush()?;
            m.files.insert("replay".into(), "replay.json".into());
            let mut out = BufWriter::new(File::create(directory.join("ticks.ndjson"))?);
            for tick in &self.ticks {
                serde_json::to_writer(&mut out, tick)?;
                out.write_all(b"\n")?;
            }
            out.flush()?;
            m.files.insert("ticks".into(), "ticks.ndjson".into());
            if self.last_tick > self.ticks.len() as u64 {
                m.gaps.push(Gap {
                    first: self.ticks.len() as u64 + 1,
                    last: self.last_tick,
                    unit: "tick".into(),
                    reason: "input_history_capacity".into(),
                });
            }
            extra(&mut m)
        };
        if let Err(err) = write() {
            m.outcome = Outcome::Failed {
                reason: err.to_string(),
            };
            m.publish(directory)?;
            return Err(err);
        }
        m.outcome = if m.gaps.is_empty() {
            Outcome::Complete
        } else {
            Outcome::Partial {
                reason: "see gaps".into(),
            }
        };
        m.publish(directory)?;
        Ok(())
    }
}

pub(super) fn verify(directory: &Path) -> Result<()> {
    let manifest = Manifest::read(directory)?;
    let artifact = |role: &str| -> Result<File> {
        let relative = manifest
            .files
            .get(role)
            .ok_or_else(|| format!("report lacks {role}"))?;
        let path = directory.join(relative);
        File::open(&path).map_err(|e| format!("opening {role} at {}: {e}", path.display()).into())
    };
    if !matches!(
        manifest.outcome,
        Outcome::Complete | Outcome::Partial { .. }
    ) {
        return Err(format!("report is not ready for replay: {:?}", manifest.outcome).into());
    }
    let replay = Replay::<RunStart, Input>::read(
        BufReader::new(artifact("replay")?),
        || Err(io::Error::other("report replay lacks its initial state")),
        rate(),
    )?;
    if replay.initial.rules_version != 3 {
        return Err("incompatible replay rules version".into());
    }
    if initial_assets(&replay.initial)? != manifest.assets {
        return Err("recorded initial assets are missing or differ".into());
    }
    let workers = serde_json::from_value(manifest.settings["physics_workers"].clone())?;
    let mut game = replay
        .initial
        .game(sashiko::physics::PhysicsWorkers::new(workers)?)?;
    if let Some(clips) = &replay.initial.locomotion {
        game.locomotion = clips.play()?;
    }
    let mut lines = BufReader::new(artifact("ticks")?).lines();
    for input in replay.inputs {
        let events = game.step(input);
        let expected: Tick = serde_json::from_str(
            &lines
                .next()
                .ok_or_else(|| format!("missing state at tick {}", game.tick))??,
        )?;
        let actual = Tick::observe(&game, events);
        if expected.snapshot != actual.snapshot
            || expected.fingerprint != actual.fingerprint
            || expected.events != actual.events
        {
            return Err(format!(
                "first state mismatch at tick {}: expected {}, actual {}",
                game.tick,
                serde_json::to_string(&expected)?,
                serde_json::to_string(&actual)?
            )
            .into());
        }
    }
    if lines.next().transpose()?.is_some() || game.tick < manifest.requested_tick {
        return Err(format!(
            "input history ends at tick {}, requested tick {}",
            game.tick, manifest.requested_tick
        )
        .into());
    }
    println!(
        "{}",
        serde_json::json!({"verified_tick":game.tick.to_string(),"session":manifest.session,
        "recorded_build":manifest.build,"replay_build":build_id()?})
    );
    Ok(())
}
