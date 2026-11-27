use super::{
    Evidence, Result,
    video::{Encoder, Sample, sample_rgb, srgb_table},
};
use sashiko::inspect::{
    jobs::{Status, Trigger},
    report::{Gap, Manifest, Outcome},
};
use serde::Serialize;
use std::{
    ffi::OsString,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    sync::mpsc::Sender,
    thread::{self, JoinHandle},
};

const FPS: u32 = 10;
const PERIOD: u64 = 1_000_000_000 / FPS as u64;
const BEFORE: usize = 5 * FPS as usize;
const AFTER: usize = 2 * FPS as usize;
const COUNT: usize = BEFORE + AFTER + 1;
const EXTENT: [u32; 2] = [160, 90];
const BYTES: usize = EXTENT[0] as usize * EXTENT[1] as usize * 3;

#[derive(Clone, Copy, Serialize)]
struct Mark {
    cause: Trigger,
    first_tick: u64,
    last_tick: u64,
    first_ns: u64,
    last_ns: u64,
    count: u64,
}
struct Window {
    id: u64,
    ns: u64,
    tick: u64,
    marks: Vec<Mark>,
}
impl Window {
    fn mark(&mut self, cause: Trigger, tick: u64, ns: u64) {
        if let Some(m) = self.marks.iter_mut().find(|m| m.cause == cause) {
            m.last_tick = tick;
            m.last_ns = ns;
            m.count += 1;
        } else {
            self.marks.push(Mark {
                cause,
                first_tick: tick,
                last_tick: tick,
                first_ns: ns,
                last_ns: ns,
                count: 1,
            });
        }
    }
    fn end(&self) -> u64 {
        self.ns.saturating_add(AFTER as u64 * PERIOD)
    }
}

pub(super) struct History {
    pixels: Vec<u8>,
    slots: [Option<(Sample, bool)>; COUNT],
    lut: [u8; 256],
    active: Option<Window>,
    writer: Option<JoinHandle<()>>,
}
impl History {
    pub fn new() -> Self {
        Self {
            pixels: vec![0; BYTES * COUNT],
            slots: [None; COUNT],
            lut: srgb_table(),
            active: None,
            writer: None,
        }
    }
    pub fn due(&self, ns: u64) -> bool {
        let bucket = ns / PERIOD;
        self.slots[(bucket % COUNT as u64) as usize]
            .is_none_or(|(s, valid)| s.ns / PERIOD < bucket || s.ns / PERIOD == bucket && !valid)
    }
    pub fn accept(&mut self, sample: Sample, rgba: Option<&[u8]>) {
        if !self.due(sample.ns) || self.active.as_ref().is_some_and(|w| sample.ns > w.end()) {
            return;
        }
        let index = ((sample.ns / PERIOD) % COUNT as u64) as usize;
        self.slots[index] = Some((sample, rgba.is_some()));
        if let Some(rgba) = rgba {
            sample_rgb(
                rgba,
                sample.source,
                &mut self.pixels[index * BYTES..(index + 1) * BYTES],
                EXTENT,
                &self.lut,
            );
        }
    }
    pub fn trigger(
        &mut self,
        id: u64,
        cause: Trigger,
        tick: u64,
        ns: u64,
        session: &str,
    ) -> Status {
        if let Some(window) = &mut self.active {
            if ns > window.end() {
                return Status::Failed {
                    reason: "previous clip awaiting finalization".into(),
                };
            }
            window.mark(cause, tick, ns);
            return Status::Linked {
                job: format!("{session}/j{}", window.id),
            };
        }
        if self.writer.as_ref().is_some_and(|w| !w.is_finished()) {
            return Status::Failed {
                reason: "clip writer busy".into(),
            };
        }
        let mut window = Window {
            id,
            ns,
            tick,
            marks: Vec::with_capacity(5),
        };
        window.mark(cause, tick, ns);
        self.active = Some(window);
        Status::Running {
            phase: "post_trigger",
        }
    }
    fn snapshot(&self, w: &Window, now: u64, closing: bool) -> Vec<Slot> {
        let trigger = i128::from(w.ns / PERIOD);
        (0..COUNT)
            .map(|i| {
                let bucket = trigger + i as i128 - BEFORE as i128;
                let Ok(bucket) = u64::try_from(bucket) else {
                    return Slot::Gap("before_session");
                };
                let index = (bucket % COUNT as u64) as usize;
                match self.slots[index] {
                    Some((s, true)) if s.ns / PERIOD == bucket => {
                        Slot::Frame(s, self.pixels[index * BYTES..(index + 1) * BYTES].to_vec())
                    }
                    Some((s, false)) if s.ns / PERIOD == bucket => Slot::Gap("readback_busy"),
                    _ if closing && bucket > now / PERIOD => Slot::Gap("shutdown"),
                    _ => Slot::Gap("no_draw"),
                }
            })
            .collect()
    }
    pub fn finish_due(
        &mut self,
        now: u64,
        closing: bool,
        root: &Path,
        evidence: &Evidence,
        python: &OsString,
        done: &Sender<(u64, Status)>,
    ) -> Result<()> {
        if self
            .writer
            .as_ref()
            .is_some_and(|w| closing || w.is_finished())
        {
            self.writer
                .take()
                .unwrap()
                .join()
                .map_err(|_| "clip writer panicked")?;
        }
        if !self
            .active
            .as_ref()
            .is_some_and(|w| closing || now >= w.end())
        {
            return Ok(());
        }
        let w = self.active.take().unwrap();
        let slots = self.snapshot(&w, now, closing);
        let id = w.id;
        let dir = root.join(format!("{}-{id}", evidence.session));
        let evidence = evidence.clone();
        let python = python.clone();
        let done = done.clone();
        self.writer = Some(thread::spawn(move || {
            let session = evidence.session.clone();
            let job = format!("{session}/j{id}");
            let result=evidence.save(&dir,&job,|m| {
                m.world="game".into();
                m.requested_tick=w.tick;
                m.settings["clip"]=serde_json::json!({"trigger_ns":w.ns.to_string(),"pre_ms":5000,"post_ms":2000,
                    "fps":FPS,"extent":EXTENT,"history_bytes":COUNT*BYTES,"triggers":w.marks,"slot_zero_session_bucket":(i128::from(w.ns/PERIOD)-BEFORE as i128).to_string()});
                write_slots(&dir,&python,slots,m)
            }).and_then(|()|Ok(Manifest::read(&dir)?));
            let artifact = dir.display().to_string();
            let status = match result {
                Ok(m) if matches!(m.outcome, Outcome::Partial { .. }) => Status::Partial {
                    artifact,
                    reason: "see clip gaps".into(),
                },
                Ok(_) => Status::Complete { artifact },
                Err(e) => Status::Failed {
                    reason: e.to_string(),
                },
            };
            eprintln!("Survivor clip {id}: {status:?}");
            let _ = done.send((id, status));
        }));
        if closing {
            self.writer
                .take()
                .unwrap()
                .join()
                .map_err(|_| "clip writer panicked")?;
        }
        Ok(())
    }
}
enum Slot {
    Frame(Sample, Vec<u8>),
    Gap(&'static str),
}
fn write_slots(dir: &Path, python: &OsString, slots: Vec<Slot>, m: &mut Manifest) -> Result<()> {
    let mut map = BufWriter::new(File::create(dir.join("frames.ndjson"))?);
    let mut encoder = None;
    let mut index = 0;
    for (pts, slot) in (0u64..).zip(slots) {
        match slot {
            Slot::Gap(reason) => {
                if let Some(last) = m.gaps.last_mut()
                    && last.unit == "clip_sample"
                    && last.reason == reason
                    && last.last + 1 == pts
                {
                    last.last = pts;
                } else {
                    m.gaps.push(Gap {
                        unit: "clip_sample".into(),
                        first: pts,
                        last: pts,
                        reason: reason.into(),
                    });
                }
            }
            Slot::Frame(sample, rgb) => {
                if encoder.is_none() {
                    encoder = Some(Encoder::open(python, &dir.join("clip.mkv"), EXTENT, FPS)?);
                }
                encoder.as_mut().unwrap().frame(pts, &rgb)?;
                serde_json::to_writer(
                    &mut map,
                    &serde_json::json!({"index":index.to_string(),"pts":pts.to_string(),"frame":sample.stamp()}),
                )?;
                map.write_all(b"\n")?;
                index += 1;
            }
        }
    }
    map.flush()?;
    m.files.insert("frame_map".into(), "frames.ndjson".into());
    if let Some(e) = encoder {
        e.finish()?;
        m.files.insert("video".into(), "clip.mkv".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(bucket: u64) -> Sample {
        Sample {
            frame: bucket * 2,
            tick: bucket * 6,
            ns: bucket * PERIOD,
            source: [1, 1],
            layers: Default::default(),
        }
    }
    #[test]
    fn pre_and_post_window_survives_wrap_and_repeated_triggers() {
        let mut h = History::new();
        let bytes = h.pixels.capacity();
        for i in 0..=100 {
            h.accept(sample(i), Some(&[64, 0, 0, 255]));
        }
        assert!(matches!(
            h.trigger(7, Trigger::Manual, 600, 100 * PERIOD, "s"),
            Status::Running { .. }
        ));
        let second = h.trigger(8, Trigger::TestFailure, 606, 101 * PERIOD, "s");
        assert!(matches!(second,Status::Linked{job} if job=="s/j7"));
        for i in 101..=125 {
            h.accept(sample(i), Some(&[64, 0, 0, 255]));
        }
        let w = h.active.as_ref().unwrap();
        assert_eq!(w.end(), 120 * PERIOD);
        assert_eq!(w.marks.len(), 2);
        let frames = h.snapshot(w, 125 * PERIOD, false);
        assert_eq!(frames.len(), 71);
        for (bucket, slot) in (50..=120).zip(frames) {
            let Slot::Frame(frame, rgb) = slot else {
                panic!("missing expected sample");
            };
            assert_eq!((frame.frame, frame.tick), (bucket * 2, bucket * 6));
            assert_eq!(&rgb[..3], &[137, 0, 0]);
        }
        assert_eq!(bytes, h.pixels.capacity());
    }
    #[test]
    fn no_draw_at_start_and_early_shutdown_have_explicit_intervals() {
        let mut h = History::new();
        h.trigger(2, Trigger::Gameplay, 1, PERIOD, "s");
        h.accept(sample(2), None);
        let slots = h.snapshot(h.active.as_ref().unwrap(), 3 * PERIOD, true);
        assert!(matches!(slots[0], Slot::Gap("before_session")));
        assert!(matches!(slots[50], Slot::Gap("no_draw")));
        assert!(matches!(slots[51], Slot::Gap("readback_busy")));
        assert!(matches!(slots[52], Slot::Gap("no_draw")));
        assert!(matches!(slots[53], Slot::Gap("shutdown")));
    }
}
