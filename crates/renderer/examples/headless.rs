//! Offscreen indexed mesh -> two dependent Compute passes -> sampled composite.
//! Run with an output path, for example `.tools/m2.ppm`. No window is created.

use std::{error::Error, io::Write, time::Duration};

use renderer::{
    RuntimeConfig, VulkanRuntime,
    graph::{AttachmentOps, BufferDesc, ComputeTransform, Graph, ImageDesc},
};

fn main() -> Result<(), Box<dyn Error>> {
    let output = std::env::args_os()
        .nth(1)
        .ok_or("pass an output PPM path")?;
    let mut graph = Graph::new();
    // Near red triangle is submitted before the overlapping far green triangle.
    // Conventional Z and LESS depth testing keep the near triangle visible.
    let mut vertices = Vec::new();
    for (z, color) in [
        (0.25_f32, [1.0_f32, 0.0, 0.0, 1.0]),
        (0.75, [0.0, 1.0, 0.0, 1.0]),
    ] {
        for [x, y] in [[-0.75_f32, -0.75], [0.75, -0.75], [0.0, 0.75]] {
            for value in [x, y, z, color[0], color[1], color[2], color[3]] {
                vertices.extend_from_slice(&value.to_ne_bytes());
            }
        }
    }
    let indices = [0_u32, 1, 2, 3, 4, 5]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect::<Vec<_>>();
    let vertex_buffer = graph.buffer(
        "mesh vertices",
        BufferDesc {
            size: vertices.len() as u64,
        },
    )?;
    let index_buffer = graph.buffer(
        "mesh indices",
        BufferDesc {
            size: indices.len() as u64,
        },
    )?;
    graph.upload_buffer("upload vertices", vertex_buffer, 0, vertices)?;
    graph.upload_buffer("upload indices", index_buffer, 0, indices)?;
    let desc = ImageDesc::rgba8(128, 96);
    let color = graph.image("mesh color", desc)?;
    let depth = graph.image("mesh depth", ImageDesc::depth32(desc.width, desc.height))?;
    let inverted = graph.image("inverted", desc)?;
    let transformed = graph.image("transformed", desc)?;
    let final_image = graph.image("final composite", desc)?;
    graph.raster(
        "indexed mesh and depth",
        vertex_buffer,
        index_buffer,
        6,
        color,
        depth,
        AttachmentOps::CLEAR_STORE,
        AttachmentOps::CLEAR_STORE,
        [0.0, 0.0, 0.0, 1.0],
        1.0,
    )?;
    graph.compute("invert", color, inverted, ComputeTransform::Invert)?;
    graph.compute(
        "swap red and blue",
        inverted,
        transformed,
        ComputeTransform::SwapRedBlue,
    )?;
    graph.composite("final composite", transformed, final_image)?;
    graph.export_image(final_image)?;
    let readback = graph.readback_image("final readback", final_image)?;
    // This valid branch has no observable output and must be culled.
    let unused = graph.image("unused image", ImageDesc::rgba8(1, 1))?;
    graph.upload_image("unused upload", unused, vec![1, 2, 3, 255])?;
    let plan = graph.compile()?;
    #[cfg(feature = "graph-diagnostics")]
    println!("{}", plan.dump());
    let mut runtime = VulkanRuntime::new(RuntimeConfig::default())?;
    let mut submission = runtime.submit(&plan)?;
    let mut result = submission
        .wait(Duration::from_secs(10))?
        .ok_or("GPU completion timed out")?;
    let rgba = result.readback(readback)?;
    let center = (desc.height as usize / 2 * desc.width as usize + desc.width as usize / 2) * 4;
    if rgba[center..center + 4] != [255, 255, 0, 255] || rgba[..4] != [255, 255, 255, 255] {
        return Err("unexpected mesh/depth/Compute/composite output".into());
    }
    let mut file = std::fs::File::create(output)?;
    write!(file, "P6\n{} {}\n255\n", desc.width, desc.height)?;
    for pixel in rgba.as_chunks::<4>().0 {
        file.write_all(&pixel[..3])?;
    }
    println!(
        "Verified {}x{} output; {:?}",
        desc.width,
        desc.height,
        runtime.stats()
    );
    Ok(())
}
