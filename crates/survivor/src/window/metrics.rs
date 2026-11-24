use renderer::{WindowRenderer, core::profiling::GpuTiming, pipeline::PipelineResult};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    time::Duration,
};

pub(super) struct Sample {
    pub interval: Duration,
    pub update: Duration,
    pub draw: Duration,
    pub active: usize,
    pub visible: usize,
    pub drawn: usize,
    pub shots: usize,
    pub drops: usize,
    pub tick: u64,
}

pub(super) struct Measurements {
    samples: Vec<Sample>,
    graph_gpu: Vec<GpuTiming>,
    present_gpu: Vec<GpuTiming>,
    profile: bool,
}

impl Measurements {
    pub fn new(capacity: usize, profile: bool) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            graph_gpu: Vec::with_capacity(if profile { capacity } else { 0 }),
            present_gpu: Vec::with_capacity(if profile { capacity } else { 0 }),
            profile,
        }
    }

    pub fn poll(&mut self, renderer: &mut WindowRenderer) {
        if self.profile {
            if let Some(graph) = renderer.take_graph_profile() {
                self.graph_gpu.push(graph.gpu);
            }
            while let Some(present) = renderer.take_presentation_profile() {
                self.present_gpu.push(present.gpu);
            }
        }
    }

    pub fn push(&mut self, sample: Sample) {
        self.samples.push(sample);
    }

    pub fn save(self, path: &Path, warmup: usize) -> PipelineResult<()> {
        if self.profile
            && (self.graph_gpu.len() != self.samples.len()
                || self.present_gpu.len() != self.samples.len())
        {
            return Err(format!(
                "incomplete timestamp sequence: {} graphs, {} presentations, {} accepted frames",
                self.graph_gpu.len(),
                self.present_gpu.len(),
                self.samples.len()
            )
            .into());
        }
        let mut csv = BufWriter::new(File::create(path)?);
        writeln!(
            csv,
            "frame,interval_ms,update_ms,draw_ms,active,visible,drawn,shots,drops,graph_gpu_ms,present_gpu_ms,tick"
        )?;
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
            if self.profile {
                write!(
                    csv,
                    "{:.6},{:.6}",
                    elapsed(self.graph_gpu[index])?,
                    elapsed(self.present_gpu[index])?
                )?;
            } else {
                write!(csv, ",")?;
            }
            writeln!(csv, ",{}", sample.tick)?;
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
