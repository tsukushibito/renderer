//! Standard command-line options for interactive, replay and measurement hosts.
use clap::{Arg, ArgAction, Command, value_parser};
use std::path::PathBuf;

/// Extend a game command with the options understood by Sashiko game hosts.
/// Games add their content selectors and settings to the returned command.
pub fn game_command(command: Command) -> Command {
    let flag = |name| Arg::new(name).long(name).action(ArgAction::SetTrue);
    let path = |name| {
        Arg::new(name)
            .long(name)
            .value_parser(value_parser!(PathBuf))
    };
    command
        .arg(
            flag("headless")
                .help("Run fixed simulation ticks without a GPU")
                .conflicts_with("window"),
        )
        .arg(flag("window").help("Play or measure the Windows game"))
        .arg(
            Arg::new("replay-from")
                .long("replay-from")
                .requires("replay")
                .conflicts_with("record")
                .value_parser(value_parser!(usize))
                .help("Simulate this many recorded inputs before starting the requested segment"),
        )
        .arg(
            path("memory-samples")
                .requires("frames")
                .help("Write prepared, active and drained memory observations"),
        )
        .arg(path("replay").help("Consume one recorded input per tick"))
        .arg(
            path("record")
                .help("Record the starting configuration and per-tick inputs as JSON")
                .conflicts_with("headless"),
        )
        .arg(
            Arg::new("backend")
                .long("backend")
                .value_parser(["vulkan", "d3d12"]),
        )
        .arg(
            Arg::new("ticks")
                .long("ticks")
                .value_parser(value_parser!(usize))
                .requires("headless"),
        )
        .arg(
            Arg::new("frames")
                .long("frames")
                .value_parser(value_parser!(usize))
                .conflicts_with("headless")
                .help("Number of successfully presented frames after warmup"),
        )
        .arg(
            Arg::new("warmup")
                .long("warmup")
                .value_parser(value_parser!(usize))
                .requires("frames"),
        )
        .arg(
            Arg::new("width")
                .long("width")
                .value_parser(value_parser!(u32).range(1..)),
        )
        .arg(
            Arg::new("height")
                .long("height")
                .value_parser(value_parser!(u32).range(1..)),
        )
        .arg(
            path("metrics")
                .requires("frames")
                .help("Write per-frame CSV"),
        )
        .arg(
            path("capture")
                .requires("frames")
                .help("Write a PPM after measurement"),
        )
        .arg(flag("gpu-profile").requires("metrics"))
}
