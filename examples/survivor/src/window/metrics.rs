use sashiko::profiling::WindowTimings;
use sashiko_renderer::{WindowRenderer, core::profiling::GpuTiming, pipeline::PipelineResult};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    time::Duration,
};

pub(super) struct Sample {
    #[cfg(feature = "memory-profiling")]
    pub pose_bytes: u64,
    pub interval: Duration,
    pub update: Duration,
    pub draw: Duration,
    pub active: usize,
    pub visible: usize,
    pub drawn: usize,
    pub shots: usize,
    pub drops: usize,
    pub tick: u64,
    pub pulse: bool,
    pub upgrades: [u8; 3],
}

pub(super) struct Measurements {
    samples: Vec<Sample>,
    timings: Option<WindowTimings>,
}

impl Measurements {
    pub fn new(capacity: usize, profile: bool) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            timings: profile.then(|| WindowTimings::new(capacity)),
        }
    }

    pub fn poll(&mut self, renderer: &mut WindowRenderer) {
        if let Some(timings) = &mut self.timings {
            timings.poll(renderer);
        }
    }

    pub fn push(&mut self, sample: Sample) {
        self.samples.push(sample);
    }

    pub fn save(self, path: &Path, warmup: usize) -> PipelineResult<()> {
        let mut timings = self
            .timings
            .map(|v| v.finish(self.samples.len()))
            .transpose()?
            .map(|v| v.skip(warmup));
        let mut csv = BufWriter::new(File::create(path)?);
        write!(
            csv,
            "frame,interval_ms,update_ms,draw_ms,active,visible,drawn,shots,drops,graph_gpu_ms,present_gpu_ms,graph_prepare_ms,graph_record_ms,graph_submit_ms,graph_retire_ms,tick,pulse,rapid_level,pierce_level,pulse_level"
        )?;
        #[cfg(feature = "memory-profiling")]
        write!(csv, ",recorded_pose_bytes")?;
        writeln!(csv)?;
        for (index, sample) in self.samples.iter().enumerate().skip(warmup) {
            write!(
                csv,
                "{},{:.6},{:.6},{:.6},{},{},{},{},{},",
                index - warmup,
                millis(sample.interval),
                millis(sample.update),
                millis(sample.draw),
                sample.active,
                sample.visible,
                sample.drawn,
                sample.shots,
                sample.drops
            )?;
            if let Some(timings) = &mut timings {
                let (graph, present) = timings.next().expect("matching presented frame");
                write!(
                    csv,
                    "{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}",
                    elapsed(graph.gpu)?,
                    elapsed(present)?,
                    millis(graph.cpu_prepare),
                    millis(graph.cpu_record),
                    millis(graph.cpu_submit),
                    millis(graph.cpu_retire)
                )?;
            } else {
                write!(csv, ",,,,,")?;
            }
            let [rapid, pierce, pulse] = sample.upgrades;
            write!(
                csv,
                ",{},{},{rapid},{pierce},{pulse}",
                sample.tick,
                u8::from(sample.pulse)
            )?;
            #[cfg(feature = "memory-profiling")]
            write!(csv, ",{}", sample.pose_bytes)?;
            writeln!(csv)?;
        }
        csv.flush()?;
        Ok(())
    }
}

fn millis(value: Duration) -> f64 {
    value.as_secs_f64() * 1000.
}

fn elapsed(value: GpuTiming) -> PipelineResult<f64> {
    match value {
        GpuTiming::Available { elapsed, .. } => Ok(millis(elapsed)),
        _ => Err(format!("GPU timestamp unavailable: {value:?}").into()),
    }
}
