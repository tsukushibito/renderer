//! Compose two standard recipes with an application pass, then inspect the
//! compiled Graph on the CPU.

use std::num::NonZeroUsize;

use renderer::graph::{
    CompiledGraph, Graph, GraphCapacity, GraphError, GraphPool, ImageDesc, ImageHandle,
};
use renderer_pipeline::standard::{
    graphs::{ForwardConfig, PreparedForward, PreparedOutput},
    passes::{SdrEncoding, SdrOutput},
};
use renderer_pipeline::{GraphRequirements, Preparation};

const COPY_TARGET: &str = "effect output";
const COPY_PASS: &str = "application HDR copy";
const READBACK: &str = "capture SDR";

fn copy_hdr(
    graph: &mut Graph,
    input: ImageHandle,
    desc: ImageDesc,
) -> Result<ImageHandle, GraphError> {
    let output = graph.image(COPY_TARGET, desc)?;
    graph.copy_image(COPY_PASS, input, output)?;
    Ok(output)
}

fn build(use_effect: bool) -> Result<CompiledGraph, GraphError> {
    let (width, height) = (8, 8);
    let color = ImageDesc::rgba16_float(width, height);
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    let scene = PreparedForward::prepare(
        &mut preparation,
        "forward",
        ForwardConfig {
            color,
            clear_color: [0.3, 0.6, 1.2, 1.0],
            depth: true,
        },
        0,
    )?;
    // Reserve the optional pass during preparation so frames can select it.
    preparation.reserve_image(color, 1)?;
    preparation.add_graph_requirements(GraphRequirements {
        resources: 1,
        passes: 2, // optional copy and readback
        accesses: 3,
        name_bytes: COPY_TARGET.len() + COPY_PASS.len() + READBACK.len(),
        ..GraphRequirements::default()
    })?;
    let output = PreparedOutput::prepare(
        &mut preparation,
        [width, height],
        SdrOutput {
            encoding: SdrEncoding::Srgb,
            fxaa: false,
        },
    )?;

    // An application with a GPU also passes preparation.working_set() into
    // Runtime at this point. The CPU example only compiles the frame.
    let mut pool = GraphPool::new(preparation.graph_capacity(GraphCapacity::default())?, 1)?;
    let mut graph = pool.begin()?;
    let scene_color = scene.add_passes(&mut graph, &[])?.color;
    let hdr = if use_effect {
        copy_hdr(&mut graph, scene_color, color)?
    } else {
        scene_color
    };
    let hdr_view = graph.image_view(hdr, 0, 0)?;
    let outputs = output.add_passes(&mut graph, hdr_view, 1.0)?;
    graph.readback_image(READBACK, outputs.sdr)?;
    graph.compile()
}

fn main() -> Result<(), GraphError> {
    for pass in build(true)?.ordered_pass_names() {
        println!("{pass}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_can_select_the_input_to_a_prepared_recipe() {
        let with_effect = build(true).unwrap();
        let direct = build(false).unwrap();
        assert_eq!(
            with_effect.ordered_pass_names().collect::<Vec<_>>(),
            ["forward", COPY_PASS, "tone_map", "write_sdr", READBACK]
        );
        assert_eq!(
            direct.ordered_pass_names().collect::<Vec<_>>(),
            ["forward", "tone_map", "write_sdr", READBACK]
        );
    }
}
