use std::{error::Error, path::PathBuf};

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: renderer_core::measurement::CountingAllocator<std::alloc::System> =
    renderer_core::measurement::CountingAllocator::new(std::alloc::System);

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mut api = None;
    let mut mode = String::from("headless");
    let mut output = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--api" => api = Some(args.next().ok_or("expected vulkan or d3d12")?),
            "--mode" => mode = args.next().ok_or("expected a mode")?,
            "--output" => {
                output = Some(PathBuf::from(
                    args.next().ok_or("expected output directory")?,
                ))
            }
            "-h" | "--help" => {
                println!(
                    "training-yard --api vulkan|d3d12 --mode headless|host|window|interactive [--output DIRECTORY]\nInteractive keys: 1 armor glow, 2 shared color, 3 restore armor, 4/5 pose left/right, 6 rest, Space step equipment replay, Esc close."
                );
                return Ok(());
            }
            _ => return Err(format!("unknown option {arg}").into()),
        }
    }
    run(
        api.as_deref().ok_or("select --api vulkan or --api d3d12")?,
        &mode,
        output,
    )
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn run(api: &str, mode: &str, output: Option<PathBuf>) -> Result<(), Box<dyn Error>> {
    use renderer_core::runtime::RuntimeConfig;
    let config = match api {
        #[cfg(feature = "vulkan")]
        "vulkan" => RuntimeConfig::Vulkan(Default::default()),
        #[cfg(all(windows, feature = "d3d12"))]
        "d3d12" => RuntimeConfig::D3D12(Default::default()),
        _ => return Err(format!("backend {api} is unavailable in this build").into()),
    };
    match mode {
        "headless" => training_yard::standalone::headless(config, output.as_deref())?,
        _ if output.is_some() => return Err("use --output with --mode headless".into()),
        "host" => {
            training_yard::replay::host(config)?;
            training_yard::targets::run(config)?;
        }
        #[cfg(all(windows, feature = "presentation"))]
        "window" | "interactive" => training_yard::window::run(config, mode == "interactive")?,
        _ => return Err(format!("unknown mode {mode}").into()),
    }
    println!("{api} {mode} replay passed");
    Ok(())
}

#[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
fn run(_: &str, _: &str, _: Option<PathBuf>) -> Result<(), Box<dyn Error>> {
    Err("enable the vulkan or d3d12 Cargo feature to run the training yard".into())
}
