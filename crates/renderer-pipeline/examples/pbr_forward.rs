//! Draw a small offscreen scene with typed PBR vertices and material defaults.
//! Select Forward, Forward+, or Deferred with --technique.
//! The public-recipe setup and Graph construction are in support/pbr_scene.rs.
//! Use `--copy-hdr` to compose a custom Graph pass before SDR conversion.
//! Use `--bloom` to add Bloom and `--reuse-shadows` to draw two frames using
//! the first frame's accepted shadow maps. Use `--msaa 4` to resolve the color
//! from a multisampled scene.

#[cfg(all(not(test), any(feature = "vulkan", all(windows, feature = "d3d12"))))]
#[path = "support/pbr_runtime.rs"]
mod runtime;
#[cfg(all(not(test), any(feature = "vulkan", all(windows, feature = "d3d12"))))]
#[path = "support/pbr_scene.rs"]
mod scene;

#[cfg(all(not(test), any(feature = "vulkan", all(windows, feature = "d3d12"))))]
mod cli {
    use std::{
        error::Error,
        fs::File,
        io::{BufWriter, Write},
        path::{Path, PathBuf},
    };

    use renderer::graph::SampleCount;
    use renderer_pipeline::{passes::bloom::Bloom, pbr::CASCADE_COUNT};

    use super::{
        runtime::{Api, probe, render},
        scene::{EXTENT, Settings, Technique},
    };

    fn write_ppm(path: &Path, rgba: &[u8]) -> Result<(), std::io::Error> {
        let [width, height] = EXTENT;
        let mut file = BufWriter::new(File::create(path)?);
        write!(file, "P6\n{width} {height}\n255\n")?;
        for pixel in rgba.as_chunks::<4>().0 {
            file.write_all(&pixel[..3])?;
        }
        file.flush()
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let mut args = std::env::args_os().skip(1);
        let mut api = None;
        let mut output = None;
        let mut copy_hdr = false;
        let mut samples = SampleCount::X1;
        let mut technique = Technique::Forward;
        let mut bloom = Bloom::Off;
        let mut reuse_shadows = false;
        while let Some(argument) = args.next() {
            match argument.to_str() {
                Some("--help" | "-h") => {
                    println!(
                        "pbr_forward [--api vulkan|d3d12] [--technique forward|forward-plus|deferred] [--msaa 1|4] [--copy-hdr] [--bloom] [--reuse-shadows] [--output FILE.ppm]"
                    );
                    return Ok(());
                }
                Some("--api") => {
                    let name = args.next().ok_or("expected an API name")?;
                    api = Some(Api::parse(name.to_str().ok_or("API name must be UTF-8")?)?);
                }
                Some("--technique") => {
                    technique = match args.next().as_deref().and_then(|value| value.to_str()) {
                        Some("forward") => Technique::Forward,
                        Some("forward-plus") => Technique::ForwardPlus,
                        Some("deferred") => Technique::Deferred,
                        _ => {
                            return Err(
                                "--technique requires forward, forward-plus, or deferred".into()
                            );
                        }
                    };
                }
                Some("--copy-hdr") => copy_hdr = true,
                Some("--msaa") => {
                    samples = match args.next().as_deref().and_then(|value| value.to_str()) {
                        Some("1") => SampleCount::X1,
                        Some("4") => SampleCount::X4,
                        _ => return Err("--msaa requires 1 or 4".into()),
                    };
                }
                Some("--bloom") => bloom = Bloom::Tent,
                Some("--reuse-shadows") => reuse_shadows = true,
                Some("--output") => {
                    output = Some(PathBuf::from(args.next().ok_or("expected an output path")?))
                }
                _ => return Err(format!("unknown argument {argument:?}").into()),
            }
        }
        #[cfg(feature = "vulkan")]
        let default = Api::Vulkan;
        #[cfg(all(not(feature = "vulkan"), windows, feature = "d3d12"))]
        let default = Api::D3D12;
        let api = api.unwrap_or(default);
        let revisions = [[0; CASCADE_COUNT]; 2];
        let revisions = if reuse_shadows { &revisions[..] } else { &[] };
        if matches!(technique, Technique::Deferred) && samples != SampleCount::X1 {
            return Err("deferred uses --msaa 1".into());
        }
        let settings = Settings {
            technique,
            ..Settings::new(samples)
        };
        let rendered = render(api, settings, copy_hdr, bloom, revisions)?;
        if reuse_shadows {
            for (index, frame) in rendered.iter().enumerate() {
                let count = frame.reused.into_iter().filter(|&hit| hit).count();
                println!(
                    "Frame {} reused {count}/{CASCADE_COUNT} shadow maps",
                    index + 1
                );
            }
        }
        let output_frame = &rendered.last().unwrap().output;
        let (rgba, depth) = probe(output_frame)?;
        if let Some(path) = output {
            write_ppm(&path, &output_frame.sdr)?;
        }
        if let Some(depth) = depth {
            println!("{api:?} {technique:?} center SDR {rgba:?} and depth {depth:.6}");
        } else {
            println!("{api:?} {technique:?} center SDR {rgba:?} after 4× MSAA resolve");
        }
        Ok(())
    }
}

#[cfg(all(not(test), any(feature = "vulkan", all(windows, feature = "d3d12"))))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    cli::run()
}

#[cfg(all(
    not(test),
    not(any(feature = "vulkan", all(windows, feature = "d3d12")))
))]
fn main() -> Result<(), &'static str> {
    Err("compile the example with the vulkan feature or, on Windows, the d3d12 feature")
}
