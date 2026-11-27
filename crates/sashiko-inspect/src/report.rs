//! Portable index for files produced by a development capture job.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::{Component, Path},
};

/// Wire counters use decimal strings, including for JavaScript clients.
mod decimal {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(n: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(n)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        String::deserialize(d)?.parse().map_err(D::Error::custom)
    }
}

/// Monotonic time is measured from the session origin, before GPU submission.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrameStamp {
    #[serde(with = "decimal")]
    pub frame: u64,
    #[serde(with = "decimal")]
    pub tick: u64,
    #[serde(with = "decimal")]
    pub elapsed_ns: u64,
    /// Fraction between the previous and source tick, absent for un-interpolated output.
    pub interpolation: Option<f32>,
    pub view: String,
    pub extent: [u32; 2],
    pub stage: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Gap {
    #[serde(with = "decimal")]
    pub first: u64,
    #[serde(with = "decimal")]
    pub last: u64,
    /// Unit of the inclusive interval, e.g. frame, tick, or session_ns.
    pub unit: String,
    pub reason: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Outcome {
    Recording,
    Complete,
    Partial { reason: String },
    Failed { reason: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub session: String,
    pub world: String,
    pub job: String,
    pub build: String,
    pub assets: BTreeMap<String, String>,
    pub settings: serde_json::Value,
    #[serde(with = "decimal")]
    pub requested_tick: u64,
    pub frames: Vec<FrameStamp>,
    pub gaps: Vec<Gap>,
    /// Artifact role to portable path relative to the report directory.
    pub files: BTreeMap<String, String>,
    pub outcome: Outcome,
}

impl Manifest {
    pub const VERSION: u32 = 1;

    pub fn read(directory: &Path) -> io::Result<Self> {
        let manifest: Self = serde_json::from_reader(File::open(directory.join("manifest.json"))?)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// A single writer owns a newly created job directory. Publish an initial
    /// recording index and replace it only after referenced files are flushed.
    pub fn publish(&self, directory: &Path) -> io::Result<()> {
        self.validate()?;
        let temporary = directory.join("manifest.json.tmp");
        let mut out = BufWriter::new(File::create(&temporary)?);
        serde_json::to_writer_pretty(&mut out, self)?;
        out.flush()?;
        drop(out);
        fs::rename(temporary, directory.join("manifest.json"))
    }

    fn validate(&self) -> io::Result<()> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
        if self.version != Self::VERSION {
            return Err(invalid("unsupported diagnostic manifest version"));
        }
        for path in self.files.values() {
            if path.is_empty()
                || path.contains(['\\', ':'])
                || path.split('/').any(|part| matches!(part, "" | "." | ".."))
                || Path::new(path)
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(invalid("artifact path must be relative to the report"));
            }
        }
        if self.frames.iter().any(|f| {
            f.extent.contains(&0) || f.interpolation.is_some_and(|t| !(0.0..=1.0).contains(&t))
        }) {
            return Err(invalid(
                "invalid diagnostic frame dimensions or interpolation",
            ));
        }
        if self.gaps.iter().any(|gap| gap.first > gap.last) {
            return Err(invalid("reversed diagnostic gap"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frame_identity_survives_json_and_interrupted_publication() {
        let directory = std::env::temp_dir().join(format!("sashiko-report-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let mut report: Manifest = serde_json::from_value(json!({
            "version":1,"session":"play","world":"lab","job":"1","build":"fixture",
            "assets":{},"settings":{},"requested_tick":"40","frames":[],
            "gaps":[],"files":{},"outcome":{"state":"recording"}
        }))
        .unwrap();
        report.publish(&directory).unwrap();
        assert_eq!(
            Manifest::read(&directory).unwrap().outcome,
            Outcome::Recording
        );
        for frame in [u64::MAX - 1, u64::MAX] {
            report.frames.push(FrameStamp {
                frame,
                tick: 42,
                elapsed_ns: frame,
                interpolation: None,
                view: "main".into(),
                extent: [640, 480],
                stage: "final_srgb_ui".into(),
            });
        }
        report.outcome = Outcome::Partial {
            reason: "shutdown".into(),
        };
        report.publish(&directory).unwrap();
        let result = Manifest::read(&directory).unwrap();
        assert_eq!(result.frames[1].frame, u64::MAX);
        assert_eq!(result.frames[0].tick, result.frames[1].tick);
        assert_eq!(result.outcome, report.outcome);
        for invalid in ["../escape", "C:/elsewhere", "/absolute", "a\\b"] {
            report.files.insert("image".into(), invalid.into());
            assert!(report.publish(&directory).is_err());
        }
        fs::remove_file(directory.join("manifest.json")).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
