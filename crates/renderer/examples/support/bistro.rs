//! A consumer of Renderer; scene formats and material policy stay outside it.

#![cfg_attr(
    not(any(feature = "vulkan", all(windows, feature = "d3d12"))),
    allow(dead_code)
)]

#[path = "bistro/asset.rs"]
mod asset;
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "bistro/backend.rs"]
mod backend;
#[path = "../../../renderer-pipeline/src/passes/bloom/effect.rs"]
mod bloom;
#[path = "bistro/camera.rs"]
mod camera;
#[cfg(windows)]
#[path = "profile_workload/clock.rs"]
mod clock;
#[path = "bistro/deferred.rs"]
mod deferred;
#[path = "bistro/forward_lighting.rs"]
mod forward_lighting;
#[path = "bistro/ibl.rs"]
mod ibl;
#[path = "bistro/instancing.rs"]
mod instancing;
#[path = "bistro/lifecycle.rs"]
mod lifecycle;
#[path = "bistro/memory.rs"]
mod memory;
#[path = "bistro/output.rs"]
mod output;
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
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
#[path = "bistro/window.rs"]
mod window;
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
#[path = "window_camera.rs"]
#[allow(dead_code, reason = "shared with the smaller windowed example")]
mod window_camera;
#[cfg(all(
    windows,
    feature = "presentation",
    any(feature = "vulkan", feature = "d3d12")
))]
#[path = "window_input.rs"]
#[allow(dead_code, reason = "shared with the smaller windowed example")]
mod window_input;

use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};

type Error = Box<dyn std::error::Error>;

const DEFAULT_API: &str = if cfg!(all(windows, feature = "d3d12", not(feature = "vulkan"))) {
    "d3d12"
} else {
    "vulkan"
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PipelineMode {
    Forward,
    ForwardPlus,
    ForwardReference,
    Deferred,
}

impl PipelineMode {
    fn name(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::ForwardPlus => "forward-plus",
            Self::ForwardReference => "forward-reference",
            Self::Deferred => "deferred",
        }
    }

    fn uses_local_lights(self) -> bool {
        matches!(self, Self::ForwardPlus | Self::ForwardReference)
    }

    fn forward_program(self, blend: bool) -> renderer::program::ManagedRasterProgram {
        if self.uses_local_lights() {
            program::forward_plus::raster_program(self == Self::ForwardPlus, blend)
        } else if blend {
            program::blend_program()
        } else {
            program::opaque_program()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubmissionMode {
    Direct,
    Packed,
    Indirect,
}

impl SubmissionMode {
    fn name(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Packed => "packed",
            Self::Indirect => "indirect",
        }
    }
}

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
    capture_bloom_bypass: bool,
    windowed: bool,
    window_smoke: bool,
    frame_limit: Option<usize>,
    vsync: bool,
    resource_checks: bool,
    bloom: bloom::Bloom,
    sample_count: renderer::graph::SampleCount,
    pipeline: PipelineMode,
    instances: instancing::Mode,
    submission: SubmissionMode,
    #[cfg(feature = "development-validation")]
    omit_material: Option<usize>,
    #[cfg(feature = "development-validation")]
    offset_shadow_receiver: bool,
    aa: bool,
    shadows: bool,
    reuse_shadows: bool,
    capture_shadows: bool,
    #[cfg(feature = "tracy")]
    capture_handshake: bool,
    output: PathBuf,
}

impl Options {
    fn sdr_output(&self) -> output::SdrOutput {
        use output::{SdrEncoding, SdrOutput};
        SdrOutput {
            encoding: if self.windowed {
                SdrEncoding::Linear
            } else {
                SdrEncoding::Srgb
            },
            fxaa: self.aa,
        }
    }

    fn validate_lighting_extent(&self) -> Result<(), Error> {
        if self.pipeline.uses_local_lights() {
            program::forward_plus::active_tile_count(self.width, self.height)?;
        }
        Ok(())
    }

    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, Error> {
        let mut result = Self {
            api: DEFAULT_API.into(),
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
            capture_bloom_bypass: false,
            windowed: false,
            window_smoke: false,
            frame_limit: None,
            vsync: true,
            resource_checks: false,
            bloom: bloom::Bloom::Off,
            sample_count: renderer::graph::SampleCount::X1,
            pipeline: PipelineMode::Forward,
            instances: instancing::Mode::Off,
            submission: SubmissionMode::Direct,
            #[cfg(feature = "development-validation")]
            omit_material: None,
            #[cfg(feature = "development-validation")]
            offset_shadow_receiver: false,
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
                "--capture-bloom-bypass" => result.capture_bloom_bypass = true,
                "--windowed" => result.windowed = true,
                "--window-smoke" => {
                    result.windowed = true;
                    result.window_smoke = true;
                }
                "--vsync" => {
                    result.vsync = match args.next().as_deref() {
                        Some("on") => true,
                        Some("off") => false,
                        _ => return Err("--vsync needs on or off".into()),
                    };
                }
                #[cfg(feature = "development-validation")]
                "--omit-material" => {
                    result.omit_material =
                        Some(args.next().ok_or("missing material index")?.parse()?);
                }
                #[cfg(feature = "development-validation")]
                "--offset-shadow-receiver" => result.offset_shadow_receiver = true,
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
                "--pipeline" => {
                    result.pipeline = match args.next().as_deref() {
                        Some("forward") => PipelineMode::Forward,
                        Some("forward-plus") => PipelineMode::ForwardPlus,
                        Some("forward-reference") => PipelineMode::ForwardReference,
                        Some("deferred") => PipelineMode::Deferred,
                        _ => return Err(
                            "--pipeline needs forward, forward-plus, forward-reference or deferred"
                                .into(),
                        ),
                    };
                }
                "--submission" => {
                    result.submission = match args.next().as_deref() {
                        Some("direct") => SubmissionMode::Direct,
                        Some("packed") => SubmissionMode::Packed,
                        Some("indirect") => SubmissionMode::Indirect,
                        _ => return Err("--submission needs direct, packed or indirect".into()),
                    };
                }
                "--msaa" => {
                    use renderer::graph::SampleCount;
                    result.sample_count = match args.next().as_deref() {
                        Some("1") => SampleCount::X1,
                        Some("4") => SampleCount::X4,
                        _ => return Err("--msaa needs 1 or 4".into()),
                    };
                }
                "--instances" => {
                    result.instances = match args.next().as_deref() {
                        Some("off") => instancing::Mode::Off,
                        Some("individual") => instancing::Mode::Individual,
                        Some("instanced") => instancing::Mode::Instanced,
                        _ => return Err("--instances needs off, individual or instanced".into()),
                    };
                }
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
                "--frames" => {
                    result.frames = args.next().ok_or("missing frames")?.parse()?;
                    result.frame_limit = Some(result.frames);
                }
                "--warmup" => result.warmup = args.next().ok_or("missing warmup")?.parse()?,
                "--inflight" => result.inflight = args.next().ok_or("missing inflight")?.parse()?,
                "--output" => result.output = args.next().ok_or("missing output directory")?.into(),
                "--alpha-override" => {
                    if args.next().as_deref() != Some("opaque") {
                        return Err("only --alpha-override opaque is supported".into());
                    }
                    result.force_opaque = true;
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
        if result.pipeline == PipelineMode::Deferred
            && result.sample_count != renderer::graph::SampleCount::X1
        {
            return Err("the deferred pipeline requires --msaa 1".into());
        }
        if result.instances.enabled()
            && (result.pipeline != PipelineMode::Forward
                || result.submission != SubmissionMode::Direct
                || result.alpha_only
                || result.force_opaque
                || result.resource_checks)
        {
            return Err("--instances requires Forward, direct submission and original material inputs without resource probes".into());
        }
        #[cfg(feature = "development-validation")]
        if result.instances.enabled() && result.omit_material.is_some() {
            return Err("--instances requires all original materials".into());
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
        result.validate_lighting_extent()?;
        if result.capture_route_samples && result.fixture {
            return Err("route samples require a cooked Bistro scene".into());
        }
        if result.capture_bloom_bypass && result.bloom == bloom::Bloom::Off {
            return Err("Bloom bypass capture requires an active Bloom mode".into());
        }
        if result.windowed
            && (result.capture_all_views
                || result.capture_route_samples
                || result.capture_bloom_bypass
                || result.capture_shadows
                || result.resource_checks
                || result.steady_state_cpu
                || result.alpha_only
                || result.force_opaque)
        {
            return Err("offscreen capture and resource probes require offscreen mode".into());
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

fn print_help() {
    println!(
        "bistro (--fixture | --manifest FILE --ibl-manifest FILE) [OPTIONS]

Scenes and output
  --api vulkan|d3d12             Default: {DEFAULT_API}
  --output DIR                   Default: .tools/bistro-runtime
  --view street|overview|material-detail|glass-detail|label-detail|street-mid|street-end|route
  --width N --height N
  --windowed | --window-smoke    Requires Windows and the presentation feature
  --vsync on|off                 Default: on

Rendering
  --pipeline forward|forward-plus|forward-reference|deferred
  --msaa 1|4
  --bloom off|tent|box
  --instances off|individual|instanced
  --submission direct|packed|indirect
  --aa-off --shadows-off --redraw-shadows

Measurement and captures
  --frames N --warmup N --inflight 1..4
  --gpu-timing
  --steady-state-cpu             Requires warmup >= inflight
  --resource-checks              Requires memory-profiling and inflight >= 2
  --capture-all-views --capture-route-samples
  --capture-bloom-bypass --capture-shadow-maps
  --alpha-only --alpha-override opaque"
    );
    #[cfg(feature = "development-validation")]
    println!("  --omit-material INDEX --offset-shadow-receiver");
    #[cfg(feature = "tracy")]
    println!("  --capture-handshake");
}

fn require_backend(api: &str) -> Result<(), Error> {
    if (cfg!(feature = "vulkan") && api == "vulkan")
        || (cfg!(all(windows, feature = "d3d12")) && api == "d3d12")
    {
        Ok(())
    } else {
        Err("selected backend was not compiled into this example".into())
    }
}

pub fn main() -> Result<(), Error> {
    let mut arguments = std::env::args().skip(1).peekable();
    if matches!(arguments.peek().map(String::as_str), Some("--help" | "-h")) {
        print_help();
        return Ok(());
    }
    let options = Options::parse(arguments)?;
    #[cfg(feature = "tracy")]
    let _client = tracy_client::Client::start();
    #[cfg(feature = "tracy")]
    tracy_client::set_thread_name!("Bistro main");
    if options.windowed {
        #[cfg(all(
            windows,
            feature = "presentation",
            any(feature = "vulkan", feature = "d3d12")
        ))]
        {
            require_backend(&options.api)?;
            return window::execute(options);
        }
        #[cfg(not(all(
            windows,
            feature = "presentation",
            any(feature = "vulkan", feature = "d3d12")
        )))]
        return Err("windowed Bistro requires Windows and the presentation feature".into());
    }
    require_backend(&options.api)?;
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    return run::execute(&options);
    #[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options(values: &[&str]) -> Result<Options, Error> {
        Options::parse(values.iter().map(|value| value.to_string()))
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub(super) fn build_material_test_frame(
        scene: &asset::Scene,
        environment: &ibl::Environment,
        options: &Options,
        camera: camera::Camera,
        uniform: &[u8; 128],
        background: [f32; 3],
    ) -> Result<
        (
            renderer::runtime::Runtime,
            pipeline::Resident,
            pipeline::Built,
        ),
        Error,
    > {
        let budget = pipeline::Budget::new(scene, environment, options)?;
        let mut backend = backend::create(options, &budget)?;
        let resident =
            pipeline::Resident::upload(&mut backend, scene, environment, &budget, uniform)?;
        let mut pool = renderer::graph::GraphPool::new(budget.graph, 2)?;
        let mut workspace = pipeline::Workspace::new(scene);
        let built = pipeline::build(
            &mut pool,
            &mut workspace,
            &resident,
            scene,
            options,
            camera,
            uniform,
            background,
            true,
        )?;
        Ok((backend, resident, built))
    }

    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    #[test]
    fn default_api_is_compiled_into_the_example() {
        let defaults = options(&["--fixture"]).unwrap();
        require_backend(&defaults.api).unwrap();
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
    fn msaa_is_selected_independently_of_fxaa() {
        let defaults = options(&["--fixture"]).unwrap();
        assert_eq!(defaults.sample_count.as_u32(), 1);
        assert!(defaults.aa);
        let msaa = options(&["--fixture", "--msaa", "4", "--aa-off"]).unwrap();
        assert_eq!(msaa.sample_count.as_u32(), 4);
        assert!(!msaa.aa);
        assert!(options(&["--fixture", "--msaa", "2"]).is_err());
    }

    #[test]
    fn instance_comparison_is_opt_in_and_runs_in_multisampled_forward() {
        assert_eq!(options(&["--fixture"]).unwrap().instances.draws(), 0);
        for (mode, draws) in [("individual", instancing::COUNT), ("instanced", 1)] {
            let chosen = options(&["--fixture", "--instances", mode, "--msaa", "4"]).unwrap();
            assert_eq!(chosen.instances.draws(), draws);
            assert_eq!(chosen.instances.name(), mode);
            assert!(
                options(&["--fixture", "--instances", mode, "--pipeline", "deferred"]).is_err()
            );
        }
        assert!(options(&["--fixture", "--instances", "wrong"]).is_err());
    }

    #[test]
    fn submission_modes_are_explicit() {
        assert_eq!(
            options(&["--fixture"]).unwrap().submission,
            SubmissionMode::Direct
        );
        assert_eq!(
            options(&["--fixture", "--submission", "packed"])
                .unwrap()
                .submission,
            SubmissionMode::Packed
        );
        assert_eq!(
            options(&["--fixture", "--submission", "indirect"])
                .unwrap()
                .submission,
            SubmissionMode::Indirect
        );
        assert!(options(&["--fixture", "--submission", "unknown"]).is_err());
        assert!(
            options(&[
                "--fixture",
                "--submission",
                "packed",
                "--instances",
                "instanced"
            ])
            .is_err()
        );
    }

    #[test]
    fn deferred_requires_single_sample_attachments() {
        assert_eq!(
            options(&["--fixture"]).unwrap().pipeline,
            PipelineMode::Forward
        );
        let deferred = options(&["--fixture", "--pipeline", "deferred"]).unwrap();
        assert_eq!(deferred.pipeline, PipelineMode::Deferred);
        let error = options(&["--fixture", "--msaa", "4", "--pipeline", "deferred"]).unwrap_err();
        assert_eq!(error.to_string(), "the deferred pipeline requires --msaa 1");
    }

    #[test]
    fn forward_local_lights_support_both_sample_counts_and_check_tile_capacity() {
        for mode in ["forward-plus", "forward-reference"] {
            for samples in ["1", "4"] {
                let opts = options(&["--fixture", "--pipeline", mode, "--msaa", samples]).unwrap();
                assert_eq!(opts.pipeline.name(), mode);
                assert!(opts.pipeline.uses_local_lights());
            }
            assert!(
                options(&[
                    "--fixture",
                    "--pipeline",
                    mode,
                    "--width",
                    "8192",
                    "--height",
                    "8192",
                ])
                .is_err()
            );
        }
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
