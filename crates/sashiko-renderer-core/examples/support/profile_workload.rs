//! Caller-owned timing and capacity preparation for the profiling executable.

#![cfg_attr(
    not(all(windows, any(feature = "vulkan", feature = "d3d12"))),
    allow(dead_code)
)]

#[cfg(any(test, all(windows, any(feature = "vulkan", feature = "d3d12"))))]
use sashiko_renderer_core::graph::CompiledGraph;
use sashiko_renderer_core::graph::{BufferDesc, Graph, GraphCapacity, GraphPool, PassHandle};
use std::time::Duration;

#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
#[path = "profile_workload/backend.rs"]
mod backend;
#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
use sashiko_example_support::clock;
#[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
#[path = "profile_workload/native.rs"]
mod native;

type Error = Box<dyn std::error::Error>;
const BYTES: usize = 16;
const MAX_RESOURCES: usize = 4096;
const HELP: &str = "profile_workload --api vulkan|d3d12 [--resources 4] [--seed 1] \
[--warmup-ms 500] [--duration-ms 3000] [--frames N] [--poll busy|yield|sleep] \
[--capture-handshake]\n\
Headless upload -> copy chain -> readback, with one in-flight frame and 16-byte \
content verification on every result. Resources: 1..4096. Duration (1..60000 ms) \
bounds starting frames; timed mode finishes its last submitted frame. --frames \
requests an exact count that must complete within that deadline. Warmup \
(0..60000 ms) includes at least one frame. Measured payload ordinals restart at zero. \
sleep explicitly sleeps 1 ms after each unsuccessful nonblocking poll; yield calls \
yield_now; busy immediately retries. These are caller pacing policies. Native GPU \
timestamps/metrics are off. Phase timings are elapsed wall time, not CPU time. \
Windows QPC and process/main-thread kernel/user CPU times bracket the measured loop.\n\
--capture-handshake prints PROFILE_WORKLOAD_READY <JSON>, waits up to 30 seconds \
for GO on stdin, measures, prints PROFILE_WORKLOAD_DONE <JSON>, then waits up to \
180 seconds for END before native teardown. EOF or another line fails. Final \
PROFILE_WORKLOAD_REPORT JSON includes validation after native teardown.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Api {
    Vulkan,
    D3d12,
}
impl Api {
    fn name(self) -> &'static str {
        match self {
            Self::Vulkan => "vulkan",
            Self::D3d12 => "d3d12",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Poll {
    Busy,
    Yield,
    Sleep,
}
impl Poll {
    fn name(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Yield => "yield",
            Self::Sleep => "sleep",
        }
    }
}
#[derive(Clone, Debug)]
struct Options {
    api: Api,
    resources: usize,
    seed: u64,
    warmup: Duration,
    duration: Duration,
    frames: Option<u64>,
    poll: Poll,
    capture_handshake: bool,
}
impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Self>, Error> {
        let mut options = Self {
            api: Api::Vulkan,
            resources: 4,
            seed: 1,
            warmup: Duration::from_millis(500),
            duration: Duration::from_millis(3000),
            frames: None,
            poll: Poll::Yield,
            capture_handshake: false,
        };
        let mut args = args.into_iter();
        let mut seen = 0_u16;
        while let Some(arg) = args.next() {
            if arg == "--help" {
                return Ok(None);
            }
            let bit = match arg.as_str() {
                "--api" => 1,
                "--resources" => 2,
                "--seed" => 4,
                "--warmup-ms" => 8,
                "--duration-ms" => 16,
                "--frames" => 32,
                "--poll" => 64,
                "--capture-handshake" => 128,
                _ => return Err(format!("unknown argument {arg}; use --help").into()),
            };
            if seen & bit != 0 {
                return Err(format!("duplicate argument {arg}").into());
            }
            seen |= bit;
            if bit == 128 {
                options.capture_handshake = true;
                continue;
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {arg}"))?;
            match arg.as_str() {
                "--api" => {
                    options.api = match value.as_str() {
                        "vulkan" => Api::Vulkan,
                        "d3d12" => Api::D3d12,
                        _ => return Err("--api must be vulkan or d3d12".into()),
                    }
                }
                "--resources" => options.resources = value.parse()?,
                "--seed" => options.seed = value.parse()?,
                "--warmup-ms" => options.warmup = Duration::from_millis(value.parse()?),
                "--duration-ms" => options.duration = Duration::from_millis(value.parse()?),
                "--frames" => options.frames = Some(value.parse()?),
                "--poll" => {
                    options.poll = match value.as_str() {
                        "busy" => Poll::Busy,
                        "yield" => Poll::Yield,
                        "sleep" => Poll::Sleep,
                        _ => return Err("--poll must be busy, yield or sleep".into()),
                    }
                }
                _ => unreachable!(),
            }
        }
        capacity(options.resources)?;
        if options.duration.is_zero()
            || options.duration > Duration::from_secs(60)
            || options.warmup > Duration::from_secs(60)
            || options.frames == Some(0)
        {
            return Err("duration must be 1..60000 ms, warmup 0..60000 ms, frames nonzero".into());
        }
        if seen & 1 == 0 {
            return Err("--api is required; use --help".into());
        }
        Ok(Some(options))
    }
}

fn capacity(resources: usize) -> Result<GraphCapacity, Error> {
    if !(1..=MAX_RESOURCES).contains(&resources) {
        return Err("resources must be 1..4096".into());
    }
    let passes = resources.checked_add(1).ok_or("pass capacity overflow")?;
    Ok(GraphCapacity {
        resources,
        passes,
        accesses: resources.checked_mul(2).ok_or("access capacity overflow")?,
        draws: 0,
        raster_constant_words: Some(0),
        bindings: 0,
        attachments: 0,
        dependencies: 0,
        initial_ranges: 0,
        name_bytes: passes.checked_mul(32).ok_or("name capacity overflow")?,
        upload_bytes: BYTES,
        edges: resources,
        segments: resources,
        readers: resources,
    })
}

// SplitMix64's integer permutation is a cheap reproducible payload, not a random source.
fn payload(seed: u64, frame: u64) -> [u8; BYTES] {
    let mut result = [0; BYTES];
    let mut state = seed.wrapping_add(frame.wrapping_mul(2));
    for chunk in result.as_chunks_mut::<8>().0 {
        state = state.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        chunk.copy_from_slice(&(value ^ (value >> 31)).to_le_bytes());
    }
    result
}

fn build(
    pool: &mut GraphPool,
    resources: usize,
    bytes: &[u8; BYTES],
) -> Result<(Graph, PassHandle), Error> {
    let mut graph = pool.begin()?;
    let mut previous = graph.buffer("buffer", BufferDesc { size: BYTES as u64 })?;
    graph.upload_buffer("upload", previous, 0, bytes)?;
    for _ in 1..resources {
        let next = graph.buffer("buffer", BufferDesc { size: BYTES as u64 })?;
        graph.copy_buffer("copy", previous, 0, next, 0, BYTES as u64)?;
        previous = next;
    }
    let readback = graph.readback_buffer("readback", previous, 0, BYTES as u64)?;
    Ok((graph, readback))
}

pub fn main() -> Result<(), Error> {
    let Some(options) = Options::parse(std::env::args().skip(1))? else {
        println!("{HELP}");
        return Ok(());
    };
    #[cfg(all(windows, any(feature = "vulkan", feature = "d3d12")))]
    {
        native::run(&options)
    }
    #[cfg(not(all(windows, any(feature = "vulkan", feature = "d3d12"))))]
    {
        let _ = options;
        Err("profile_workload requires Windows and the requested vulkan/d3d12 feature".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Option<Options>, Error> {
        Options::parse(args.iter().map(|arg| (*arg).to_owned()))
    }
    #[test]
    fn rejects_ambiguous_or_unbounded_runs() {
        for args in [
            vec!["--resources", "0"],
            vec!["--resources", "4097"],
            vec!["--duration-ms", "0"],
            vec!["--duration-ms", "60001"],
            vec!["--frames", "0"],
            vec!["--seed", "1", "--seed", "2"],
            vec!["--api", "other"],
            vec!["--poll", "other"],
            vec!["--seed"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(parse(&[]).is_err());
        let options = parse(&[
            "--api",
            "d3d12",
            "--resources",
            "4096",
            "--frames",
            "50",
            "--poll",
            "sleep",
            "--capture-handshake",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(options.api, Api::D3d12);
        assert_eq!(options.resources, 4096);
        assert_eq!(options.frames, Some(50));
        assert_eq!(options.poll, Poll::Sleep);
        assert!(options.capture_handshake);
    }
    #[test]
    fn identical_seed_and_frame_mean_identical_changing_input() {
        assert_eq!(payload(7, 9), payload(7, 9));
        assert_ne!(payload(7, 9), payload(7, 10));
        assert_ne!(payload(7, 9), payload(8, 9));
        assert_eq!(payload(0, 0)[..8], 0xe220a8397b1dcdaf_u64.to_le_bytes());
    }
    #[test]
    fn complete_chain_lives_and_reuses_one_exact_capacity_slot() {
        for count in [1, 4, 256, MAX_RESOURCES] {
            let mut pool = GraphPool::new(capacity(count).unwrap(), 1).unwrap();
            for frame in 0..3 {
                let (graph, _) = build(&mut pool, count, &payload(1, frame)).unwrap();
                assert!(pool.begin().is_err());
                let plan: CompiledGraph = graph.compile().unwrap();
                assert_eq!(plan.pass_count(), count + 1);
                let names: Vec<_> = plan.ordered_pass_names().collect();
                assert_eq!(names[0], "upload");
                assert_eq!(names[count], "readback");
                assert!(names[1..count].iter().all(|name| *name == "copy"));
                drop(plan);
                assert_eq!(pool.leased_slots(), 0);
            }
            let (mut graph, _) = build(&mut pool, count, &payload(1, 0)).unwrap();
            assert!(graph.buffer("overflow", BufferDesc { size: 16 }).is_err());
        }
    }
}
