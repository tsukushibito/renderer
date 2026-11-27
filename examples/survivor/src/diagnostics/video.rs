use super::{Evidence, Result};
use sashiko::inspect::{
    jobs::Status,
    report::{FrameStamp, Gap, Outcome},
};
use std::{
    ffi::OsString,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    thread::{self, JoinHandle},
};

const SCRIPT: &str = include_str!("../../../../tools/diagnostics/encode_ffv1.py");
const SLOTS: usize = 3;

pub(super) fn probe() -> Option<OsString> {
    let python = std::env::var_os("SASHIKO_VIDEO_PYTHON").unwrap_or_else(|| "python".into());
    Command::new(&python)
        .args(["-c", SCRIPT, "--probe"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?
        .success()
        .then_some(python)
}

#[derive(Clone, Copy)]
pub(super) struct Sample {
    pub layers: sashiko::inspect::jobs::RenderLayers,
    pub frame: u64,
    pub tick: u64,
    pub ns: u64,
    pub source: [u32; 2],
}
impl Sample {
    pub fn stamp(self) -> FrameStamp {
        FrameStamp {
            frame: self.frame,
            tick: self.tick,
            elapsed_ns: self.ns,
            extent: self.source,
            view: "main".into(),
            stage: if self.layers.hud {
                "final_srgb_ui"
            } else {
                "final_srgb"
            }
            .into(),
            layers: Some(self.layers),
            interpolation: None,
        }
    }
}

pub(super) fn srgb_table() -> [u8; 256] {
    std::array::from_fn(|i| {
        let x = i as f64 / 255.;
        ((if x <= 0.0031308 {
            12.92 * x
        } else {
            1.055 * x.powf(1. / 2.4) - 0.055
        }) * 255.)
            .round() as u8
    })
}

/// Sample the whole current view into fixed prepared output storage, including after resize.
pub(super) fn sample_rgb(
    src: &[u8],
    [sw, sh]: [u32; 2],
    dst: &mut [u8],
    [w, h]: [u32; 2],
    lut: &[u8; 256],
) {
    for (row, out) in dst.chunks_exact_mut(w as usize * 3).enumerate() {
        let y = row * sh as usize / h as usize;
        for (x, rgb) in out.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let offset = (y * sw as usize + x * sw as usize / w as usize) * 4;
            for (to, from) in rgb.iter_mut().zip(&src[offset..offset + 3]) {
                *to = lut[*from as usize];
            }
        }
    }
}

pub(super) struct Encoder(Child);
impl Encoder {
    pub(super) fn open(python: &OsString, path: &Path, extent: [u32; 2], fps: u32) -> Result<Self> {
        Ok(Self(
            Command::new(python)
                .args(["-c", SCRIPT])
                .arg(path)
                .args([
                    extent[0].to_string(),
                    extent[1].to_string(),
                    fps.to_string(),
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()?,
        ))
    }
    pub(super) fn frame(&mut self, pts: u64, pixels: &[u8]) -> Result<()> {
        let input = self.0.stdin.as_mut().ok_or("encoder input closed")?;
        input.write_all(&pts.to_le_bytes())?;
        input.write_all(pixels)?;
        Ok(())
    }
    pub(super) fn finish(mut self) -> Result<()> {
        drop(self.0.stdin.take());
        let status = self.0.wait()?;
        if !status.success() {
            return Err(format!("FFV1 encoder exited with {status}").into());
        }
        Ok(())
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct Packet {
    sample: Sample,
    pts: u64,
    rgb: Vec<u8>,
}
struct End {
    evidence: Evidence,
    gaps: Vec<Gap>,
    stop_job: Option<u64>,
}

pub(super) struct Video {
    pub id: u64,
    pub worker: JoinHandle<()>,
    extent: [u32; 2],
    fps: u32,
    start_ns: u64,
    next_pts: u64,
    lut: [u8; 256],
    free: Receiver<Vec<u8>>,
    packets: SyncSender<Packet>,
    end: Sender<End>,
    gaps: Vec<Gap>,
}
impl Video {
    pub fn start(
        id: u64,
        dir: PathBuf,
        evidence: &Evidence,
        extent: [u32; 2],
        fps: u32,
        python: OsString,
        done: Sender<(u64, Status)>,
    ) -> Result<Self> {
        if extent.contains(&0) || extent.iter().any(|v| *v > 4096) || !(1..=120).contains(&fps) {
            return Err("video extent must be 1..4096 per axis and fps 1..120".into());
        }
        let bytes = extent[0] as usize * extent[1] as usize * 3;
        let (release, free) = mpsc::sync_channel(SLOTS);
        for _ in 0..SLOTS {
            release.send(vec![0; bytes])?;
        }
        let (packets, input) = mpsc::sync_channel::<Packet>(SLOTS);
        let (end, finish) = mpsc::channel::<End>();
        let mut manifest = evidence.manifest(&format!("{}/j{id}", evidence.session));
        manifest.world = "game".into();
        manifest.settings["video"] = serde_json::json!({"encoder":"ffv1","container":"matroska","audio":false,"extent":extent,"fps":fps});
        let worker = thread::spawn(move || {
            let mut owned = false;
            let encode = || -> Result<()> {
                std::fs::create_dir(&dir)?;
                owned = true;
                manifest.publish(&dir)?;
                let mut encoder = Encoder::open(&python, &dir.join("video.mkv"), extent, fps)?;
                let mut map = BufWriter::new(File::create(dir.join("frames.ndjson"))?);
                let mut count = 0u64;
                for packet in input {
                    encoder.frame(packet.pts, &packet.rgb)?;
                    serde_json::to_writer(
                        &mut map,
                        &serde_json::json!({"index":count.to_string(),"pts":packet.pts.to_string(),
                        "frame":packet.sample.stamp()}),
                    )?;
                    map.write_all(b"\n")?;
                    count += 1;
                    // The host may already be closing. It no longer needs returned slots.
                    let _ = release.try_send(packet.rgb);
                }
                encoder.finish()?;
                map.flush()?;
                if count == 0 {
                    return Err("recording ended before a frame was encoded".into());
                }
                Ok(())
            };
            let encoded = encode().map_err(|e| e.to_string());
            let early_failure = encoded.is_err();
            if let Err(reason) = &encoded {
                manifest.outcome = Outcome::Failed {
                    reason: reason.clone(),
                };
                if owned {
                    let _ = manifest.publish(&dir);
                }
                let _ = done.send((
                    id,
                    Status::Failed {
                        reason: reason.clone(),
                    },
                ));
            }
            if let Ok(end) = finish.recv() {
                let result = encoded.and_then(|()| {
                    manifest.gaps = end.gaps;
                    end.evidence
                        .finish_report(&dir, manifest, |m| {
                            m.files.insert("video".into(), "video.mkv".into());
                            m.files.insert("frame_map".into(), "frames.ndjson".into());
                            Ok(())
                        })
                        .map_err(|e| e.to_string())
                });
                let status = || match &result {
                    Ok(()) => match sashiko::inspect::report::Manifest::read(&dir) {
                        Ok(m) if !m.gaps.is_empty() => Status::Partial {
                            artifact: dir.display().to_string(),
                            reason: "see gaps".into(),
                        },
                        Ok(_) => Status::Complete {
                            artifact: dir.display().to_string(),
                        },
                        Err(e) => Status::Failed {
                            reason: e.to_string(),
                        },
                    },
                    Err(reason) => Status::Failed {
                        reason: reason.clone(),
                    },
                };
                // An encoder failure was already published while the game was running.
                let status = status();
                if !early_failure {
                    let _ = done.send((id, status.clone()));
                }
                if let Some(stop) = end.stop_job {
                    let _ = done.send((stop, status));
                }
            }
        });
        Ok(Self {
            id,
            extent,
            fps,
            start_ns: evidence.elapsed_ns()?,
            next_pts: 0,
            lut: srgb_table(),
            free,
            packets,
            end,
            gaps: vec![],
            worker,
        })
    }
    pub fn due(&self, ns: u64) -> bool {
        self.pts(ns) >= self.next_pts
    }
    fn pts(&self, ns: u64) -> u64 {
        ((ns.saturating_sub(self.start_ns) as u128 * self.fps as u128) / 1_000_000_000) as u64
    }
    fn gap(&mut self, first: u64, last: u64, reason: &str) {
        if let Some(g) = self.gaps.last_mut()
            && g.reason == reason
            && g.last.checked_add(1) == Some(first)
        {
            g.last = last;
            return;
        }
        self.gaps.push(Gap {
            unit: "video_pts".into(),
            first,
            last,
            reason: reason.into(),
        });
    }
    pub fn accept(&mut self, sample: Sample, pixels: Option<&[u8]>) {
        if sample.ns < self.start_ns {
            return;
        }
        let pts = self.pts(sample.ns);
        if pts < self.next_pts {
            return;
        }
        if pts > self.next_pts {
            self.gap(self.next_pts, pts - 1, "no_sample");
        }
        self.next_pts = pts + 1;
        let Some(pixels) = pixels else {
            self.gap(pts, pts, "readback_busy");
            return;
        };
        let Ok(mut rgb) = self.free.try_recv() else {
            self.gap(pts, pts, "encoder_busy");
            return;
        };
        sample_rgb(pixels, sample.source, &mut rgb, self.extent, &self.lut);
        if self.packets.try_send(Packet { pts, sample, rgb }).is_err() {
            self.gap(pts, pts, "encoder_unavailable");
        }
    }
    pub fn stop(self, evidence: Evidence, stop_job: Option<u64>) -> JoinHandle<()> {
        drop(self.packets);
        let _ = self.end.send(End {
            evidence,
            gaps: self.gaps,
            stop_job,
        });
        self.worker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_encoder_releases_a_slot_without_waiting_for_the_next_frame() {
        let (release, free) = mpsc::sync_channel(1);
        let (packets, receive) = mpsc::sync_channel(1);
        let (end, _) = mpsc::channel();
        let mut video = Video {
            id: 1,
            extent: [2, 1],
            fps: 10,
            start_ns: 0,
            next_pts: 0,
            lut: srgb_table(),
            free,
            packets,
            end,
            gaps: vec![],
            worker: thread::spawn(|| {}),
        };
        release.send(vec![0; 6]).unwrap();
        let mut sample = Sample {
            layers: Default::default(),
            frame: 9,
            tick: 7,
            ns: 0,
            source: [1, 1],
        };
        video.accept(sample, Some(&[64, 0, 0, 255]));
        let held = receive.try_recv().unwrap();
        assert_eq!(&held.rgb, [137, 0, 0, 137, 0, 0].as_slice());
        for i in 1..=3 {
            sample.ns = i * 100_000_000;
            video.accept(sample, Some(&[255; 4]));
        }
        assert_eq!(video.gaps.len(), 1);
        assert_eq!(
            (
                video.gaps[0].first,
                video.gaps[0].last,
                video.gaps[0].reason.as_str()
            ),
            (1, 3, "encoder_busy")
        );
        release.send(held.rgb).unwrap();
        sample.ns = 400_000_000;
        video.accept(sample, Some(&[255; 4]));
        assert_eq!(receive.try_recv().unwrap().pts, 4);
    }
}
