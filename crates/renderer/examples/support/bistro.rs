//! A consumer of Renderer; scene formats and material policy stay outside it.

#![cfg_attr(
    not(any(feature = "vulkan", all(windows, feature = "d3d12"))),
    allow(dead_code)
)]

#[path = "bistro/asset.rs"]
mod asset;
#[path = "bistro/backend.rs"]
mod backend;
#[path = "bloom.rs"]
mod bloom;
#[path = "bistro/camera.rs"]
mod camera;
#[cfg(windows)]
#[path = "profile_workload/clock.rs"]
mod clock;
#[path = "bistro/ibl.rs"]
mod ibl;
#[path = "bistro/lifecycle.rs"]
mod lifecycle;
#[path = "bistro/memory.rs"]
mod memory;
#[path = "bistro/pipeline.rs"]
mod pipeline;
#[path = "bistro/program.rs"]
mod program;
#[path = "bistro/run.rs"]
mod run;
#[path = "bistro/shadow.rs"]
mod shadow;
#[cfg(feature = "tracy")]
#[path = "bistro/trace.rs"]
mod trace;

use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};

type Error = Box<dyn std::error::Error>;

#[derive(Clone, Debug)]
struct Options {
    api: String,
    manifest: Option<PathBuf>,
    ibl_manifest: Option<PathBuf>,
    fixture: bool,
    view: String,
    width: u32,
    height: u32,
    frames: usize,
    warmup: usize,
    inflight: usize,
    gpu_timing: bool,
    steady_state_cpu: bool,
    alpha_only: bool,
    force_opaque: bool,
    capture_all_views: bool,
    capture_route_samples: bool,
    resource_checks: bool,
    bloom: bloom::Bloom,
    aa: bool,
    shadows: bool,
    reuse_shadows: bool,
    capture_shadows: bool,
    #[cfg(feature = "tracy")]
    capture_handshake: bool,
    output: PathBuf,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, Error> {
        let mut result = Self {
            api: "vulkan".into(),
            manifest: None,
            ibl_manifest: None,
            fixture: false,
            view: "street".into(),
            width: 1920,
            height: 1080,
            frames: 120,
            warmup: 24,
            inflight: 2,
            gpu_timing: false,
            steady_state_cpu: false,
            alpha_only: false,
            force_opaque: false,
            capture_all_views: false,
            capture_route_samples: false,
            resource_checks: false,
            bloom: bloom::Bloom::Off,
            aa: true,
            shadows: true,
            reuse_shadows: true,
            capture_shadows: false,
            #[cfg(feature = "tracy")]
            capture_handshake: false,
            output: PathBuf::from(".tools/bistro-runtime"),
        };
        let mut args = arguments.into_iter();
        let mut specified_size = false;
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--fixture" => result.fixture = true,
                "--gpu-timing" => result.gpu_timing = true,
                "--steady-state-cpu" => result.steady_state_cpu = true,
                "--alpha-only" => result.alpha_only = true,
                "--capture-all-views" => result.capture_all_views = true,
                "--capture-route-samples" => result.capture_route_samples = true,
                "--resource-checks" => result.resource_checks = true,
                "--bloom" => {
                    result.bloom = match args.next().as_deref() {
                        Some("off") => bloom::Bloom::Off,
                        Some("tent") => bloom::Bloom::Tent,
                        Some("box") => bloom::Bloom::Box,
                        _ => return Err("--bloom needs off, tent or box".into()),
                    };
                }
                "--aa-off" => result.aa = false,
                "--shadows-off" => result.shadows = false,
                "--redraw-shadows" => result.reuse_shadows = false,
                "--capture-shadow-maps" => result.capture_shadows = true,
                #[cfg(feature = "tracy")]
                "--capture-handshake" => result.capture_handshake = true,
                "--api" => result.api = args.next().ok_or("--api needs vulkan or d3d12")?,
                "--manifest" => {
                    result.manifest = Some(args.next().ok_or("--manifest needs a path")?.into())
                }
                "--ibl-manifest" => {
                    result.ibl_manifest =
                        Some(args.next().ok_or("--ibl-manifest needs a path")?.into())
                }
                "--view" => {
                    result.view = args.next().ok_or("--view needs a fixed name or route")?
                }
                "--width" => {
                    result.width = args.next().ok_or("missing width")?.parse()?;
                    specified_size = true;
                }
                "--height" => {
                    result.height = args.next().ok_or("missing height")?.parse()?;
                    specified_size = true;
                }
                "--frames" => result.frames = args.next().ok_or("missing frames")?.parse()?,
                "--warmup" => result.warmup = args.next().ok_or("missing warmup")?.parse()?,
                "--inflight" => result.inflight = args.next().ok_or("missing inflight")?.parse()?,
                "--output" => result.output = args.next().ok_or("missing output directory")?.into(),
                "--alpha-override" => {
                    if args.next().as_deref() != Some("opaque") {
                        return Err("only --alpha-override opaque is supported".into());
                    }
                    result.force_opaque = true;
                }
                "--help" | "-h" => {
                    let usage = "usage: bistro --api vulkan|d3d12 (--manifest manifest.json --ibl-manifest ibl/manifest.json | --fixture) [--view street|overview|material-detail|glass-detail|label-detail|street-mid|street-end|route] [--width N --height N] [--frames N --warmup N --inflight 1..4] [--gpu-timing] [--steady-state-cpu (warmup >= inflight)] [--bloom off|tent|box] [--capture-all-views] [--capture-route-samples (requires full scene)] [--capture-shadow-maps] [--aa-off] [--shadows-off] [--redraw-shadows] [--resource-checks (requires memory-profiling, inflight >= 2)] [--alpha-only --alpha-override opaque] --output DIR";
                    #[cfg(feature = "tracy")]
                    return Err(format!("{usage} [--capture-handshake]").into());
                    #[cfg(not(feature = "tracy"))]
                    return Err(usage.into());
                }
                _ => return Err(format!("unknown option: {argument}").into()),
            }
        }
        if !matches!(result.api.as_str(), "vulkan" | "d3d12") {
            return Err("API must be vulkan or d3d12".into());
        }
        if result.fixture == result.manifest.is_some() {
            return Err("select exactly one of --fixture or --manifest".into());
        }
        if result.fixture && !specified_size {
            result.width = 128;
            result.height = 96;
        }
        if result.width == 0
            || result.height == 0
            || result.width > 8192
            || result.height > 8192
            || result.frames == 0
            || result.frames > 100_000
            || result.warmup > 100_000
            || !(1..=4).contains(&result.inflight)
        {
            return Err("invalid extent/frame/in-flight limits".into());
        }
        if result.steady_state_cpu && result.warmup < result.inflight {
            return Err("stationary CPU measurement requires a fully warmed pipeline".into());
        }
        if result.capture_route_samples && result.fixture {
            return Err("route samples require a cooked Bistro scene".into());
        }
        if result.resource_checks
            && (!cfg!(feature = "memory-profiling")
                || result.inflight < 2
                || result.alpha_only
                || result.force_opaque)
        {
            return Err("resource checks require memory-profiling, at least two in-flight slots and original full-scene alpha".into());
        }
        Ok(result)
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
const WAIT: Duration = Duration::from_secs(60);

pub fn main() -> Result<(), Error> {
    let options = Options::parse(std::env::args().skip(1))?;
    #[cfg(feature = "tracy")]
    let _client = tracy_client::Client::start();
    #[cfg(feature = "tracy")]
    tracy_client::set_thread_name!("Bistro main");
    match options.api.as_str() {
        #[cfg(feature = "vulkan")]
        "vulkan" => run::execute::<backend::Vulkan>(&options),
        #[cfg(all(windows, feature = "d3d12"))]
        "d3d12" => run::execute::<backend::D3d12>(&options),
        _ => Err("selected backend was not compiled into this example".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options(values: &[&str]) -> Result<Options, Error> {
        Options::parse(values.iter().map(|value| value.to_string()))
    }
    #[test]
    fn source_and_capacity_options_are_explicit() {
        assert!(options(&[]).is_err());
        assert!(options(&["--fixture", "--manifest", "x"]).is_err());
        assert!(options(&["--fixture", "--inflight", "0"]).is_err());
        assert!(options(&["--fixture"]).unwrap().reuse_shadows);
        assert!(
            !options(&["--fixture", "--redraw-shadows"])
                .unwrap()
                .reuse_shadows
        );
        assert!(options(&["--fixture", "--width", "0"]).is_err());
        assert!(options(&["--fixture", "--frames", "0"]).is_err());
        let fixture = options(&["--fixture", "--gpu-timing"]).unwrap();
        assert_eq!(
            (fixture.width, fixture.height, fixture.inflight),
            (128, 96, 2)
        );
        assert!(fixture.gpu_timing);
    }

    #[test]
    fn capture_handshake_is_only_available_with_tracy() {
        let parsed = options(&["--fixture", "--capture-handshake"]);
        #[cfg(feature = "tracy")]
        assert!(parsed.unwrap().capture_handshake);
        #[cfg(not(feature = "tracy"))]
        assert!(parsed.is_err());
    }

    #[test]
    fn stationary_cpu_requires_a_prepared_pending_window() {
        assert!(!options(&["--fixture"]).unwrap().steady_state_cpu);
        assert!(
            options(&[
                "--fixture",
                "--steady-state-cpu",
                "--warmup",
                "1",
                "--inflight",
                "2"
            ])
            .is_err()
        );
        assert!(
            options(&[
                "--fixture",
                "--steady-state-cpu",
                "--warmup",
                "2",
                "--inflight",
                "2"
            ])
            .unwrap()
            .steady_state_cpu
        );
    }

    #[test]
    fn resource_checks_require_counting_overlap_and_unmodified_alpha() {
        let parsed = options(&["--fixture", "--resource-checks"]);
        assert_eq!(parsed.is_ok(), cfg!(feature = "memory-profiling"));
        assert!(options(&["--fixture", "--resource-checks", "--inflight", "1"]).is_err());
        assert!(options(&["--fixture", "--resource-checks", "--alpha-only"]).is_err());
        assert!(
            options(&[
                "--fixture",
                "--resource-checks",
                "--alpha-override",
                "opaque"
            ])
            .is_err()
        );
    }
}
