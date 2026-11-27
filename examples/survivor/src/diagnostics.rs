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

#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
pub(super) mod live;
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
mod video;

#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
pub(super) fn save_image(
    directory: &Path,
    m: &mut Manifest,
    stamp: sashiko::inspect::report::FrameStamp,
    srgb: &[u8],
) -> Result<()> {
    png(&directory.join("frame.png"), stamp.extent, srgb)?;
    m.files.insert("image".into(), "frame.png".into());
    std::fs::write(
        directory.join("summary.txt"),
        format!(
            "Survivor frame {} from tick {} ({} × {}, {}). Requested at tick {}.\nInputs: replay.json. End-of-tick state and events: ticks.ndjson.\n",
            stamp.frame,
            stamp.tick,
            stamp.extent[0],
            stamp.extent[1],
            stamp.stage,
            m.requested_tick
        ),
    )?;
    m.files.insert("summary".into(), "summary.txt".into());
    m.frames.push(stamp);
    Ok(())
}
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
fn png(path: &Path, extent: [u32; 2], srgb: &[u8]) -> Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    let mut encoder = png::Encoder::new(&mut out, extent[0], extent[1]);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(srgb)?;
    writer.finish()?;
    out.flush()?;
    Ok(())
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Tick {
    snapshot: Snapshot,
    events: Events,
    /// Two words keep the entire fingerprint exact in JSON clients.
    fingerprint: [u32; 2],
    #[serde(default)]
    player_id: u64,
    /// End-of-tick projection of the enemy that damaged the player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    actor_position: Option<sashiko::math::Vec2>,
}

impl Tick {
    fn observe(game: &Game, events: Events) -> Self {
        let hash = game.replay_fingerprint();
        Self {
            player_id: 0,
            actor_position: events.damage.and_then(|damage| {
                game.enemies
                    .iter()
                    .find(|e| e.id == damage.enemy)
                    .map(|e| e.pos)
            }),
            snapshot: game.snapshot(),
            events,
            fingerprint: [(hash >> 32) as u32, hash as u32],
        }
    }
}

#[derive(Clone)]
pub(super) struct Evidence {
    #[cfg(all(
        windows,
        feature = "presentation",
        any(feature = "vulkan", feature = "d3d12")
    ))]
    origin: std::time::Instant,
    start: RunStart,
    build: String,
    assets: BTreeMap<String, String>,
    session: String,
    workers: NonZeroUsize,
    ticks: Vec<Tick>,
    inputs: Vec<Input>,
    limit: usize,
    last_tick: u64,
    ids: ObjectIds,
}

#[derive(Clone)]
struct ObjectIds {
    player: u64,
    high: u64,
    phase: survivor::Phase,
}
impl ObjectIds {
    fn new() -> Self {
        Self {
            player: 0,
            high: 0,
            phase: survivor::Phase::Playing,
        }
    }
    fn observe(&mut self, game: &Game, input: Input) -> u64 {
        if self.phase == survivor::Phase::Defeated && input.restart {
            self.player = self.high.checked_add(1).expect("world ID overflow");
        }
        self.phase = game.phase;
        self.high = self
            .player
            .checked_add(game.enemy_ids_issued())
            .expect("world ID overflow");
        self.player
    }
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
    #[cfg(all(
        windows,
        feature = "presentation",
        any(feature = "vulkan", feature = "d3d12")
    ))]
    pub(super) fn elapsed_ns(&self) -> Result<u64> {
        Ok(self.origin.elapsed().as_nanos().try_into()?)
    }
    #[cfg(all(
        windows,
        feature = "presentation",
        any(feature = "vulkan", feature = "d3d12")
    ))]
    pub(super) fn set_visual(&mut self, visual: survivor::RecordedVisual) -> Result<()> {
        if self
            .start
            .visual
            .as_ref()
            .is_some_and(|expected| expected != &visual)
        {
            return Err("appearance assets differ from replay".into());
        }
        self.start.visual = Some(visual);
        self.assets = initial_assets(&self.start)?;
        Ok(())
    }
    pub(super) fn new(start: &RunStart, workers: NonZeroUsize, limit: usize) -> Result<Self> {
        let mut ticks = Vec::new();
        let mut inputs = Vec::new();
        ticks.try_reserve_exact(limit)?;
        inputs.try_reserve_exact(limit)?;
        Ok(Self {
            #[cfg(all(
                windows,
                feature = "presentation",
                any(feature = "vulkan", feature = "d3d12")
            ))]
            origin: std::time::Instant::now(),
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
            ids: ObjectIds::new(),
        })
    }

    pub(super) fn observe(&mut self, game: &Game, input: Input, events: Events) {
        self.last_tick = game.tick;
        let player_id = self.ids.observe(game, input);
        // Keep the replayable prefix paired with tick zero. Never evict its beginning.
        if self.ticks.len() < self.limit {
            self.inputs.push(input);
            self.ticks.push(Tick {
                player_id,
                ..Tick::observe(game, events)
            });
        }
    }

    pub(super) fn save(
        self,
        directory: &Path,
        job: &str,
        extra: impl FnOnce(&mut Manifest) -> Result<()>,
    ) -> Result<()> {
        fs::create_dir(directory)?;
        let manifest = self.manifest(job);
        manifest.publish(directory)?;
        self.finish_report(directory, manifest, extra)
    }

    fn manifest(&self, job: &str) -> Manifest {
        Manifest {
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
        }
    }

    fn finish_report(
        self,
        directory: &Path,
        mut m: Manifest,
        extra: impl FnOnce(&mut Manifest) -> Result<()>,
    ) -> Result<()> {
        let write = || -> Result<()> {
            let mut tape = BufWriter::new(File::create(directory.join("replay.json"))?);
            // Serialize the existing Replay wire format, including its immutable start.
            serde_json::to_writer(&mut tape, &Replay::new(&self.start, rate(), self.inputs))?;
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
    let mut ids = ObjectIds::new();
    for input in replay.inputs {
        let events = game.step(input);
        let expected: Tick = serde_json::from_str(
            &lines
                .next()
                .ok_or_else(|| format!("missing state at tick {}", game.tick))??,
        )?;
        let actual = Tick {
            player_id: ids.observe(&game, input),
            ..Tick::observe(&game, events)
        };
        if expected.snapshot != actual.snapshot
            || expected.fingerprint != actual.fingerprint
            || expected.events != actual.events
            || expected.actor_position != actual.actor_position
            || expected.player_id != actual.player_id
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
    let required = manifest
        .frames
        .iter()
        .map(|f| f.tick)
        .fold(manifest.requested_tick, u64::max);
    if lines.next().transpose()?.is_some() || game.tick < required {
        return Err(format!(
            "input history ends at tick {}, requested tick {}",
            game.tick, required
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
