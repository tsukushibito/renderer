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
    #[cfg(feature = "memory-profiling")]
    pub memory: super::allocations::FrameCounts,
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
    #[cfg(feature = "memory-profiling")]
    pub(super) allocations: super::allocations::FrameAllocations,
}

impl Measurements {
    pub fn new(capacity: usize, profile: bool) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
            timings: profile.then(|| WindowTimings::new(capacity)),
            #[cfg(feature = "memory-profiling")]
            allocations: super::allocations::FrameAllocations::new(),
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
        write!(
            csv,
            ",recorded_pose_bytes,alloc,alloc_zeroed,realloc,free,object_create,object_destroy,resource_create,resource_destroy,memory_allocate,memory_free,descriptor_allocate,descriptor_free,native_reset,vma_host_alloc,vma_host_realloc,vma_host_free"
        )?;
        #[cfg(feature = "memory-profiling")]
        for phase in super::allocations::Phase::NAMES {
            for event in ["alloc", "alloc_zeroed", "realloc", "free"] {
                write!(csv, ",{phase}_{event}")?;
            }
        }
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
            {
                if sample.memory.total.overflowed {
                    return Err("frame memory counters overflowed".into());
                }
                write!(csv, ",{}", sample.pose_bytes)?;
                for count in sample.memory.total.rust {
                    write!(csv, ",{}", count.calls)?;
                }
                for count in sample.memory.total.native {
                    write!(csv, ",{count}")?;
                }
                for event in sample.memory.total.vma_host {
                    write!(csv, ",{}", event.calls)?;
                }
                for count in sample.memory.phases.as_flattened() {
                    write!(csv, ",{count}")?;
                }
            }
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
