//! Offscreen indexed mesh -> two dependent Compute passes -> sampled composite.
//! Run with an output path, for example `.tools/m2.ppm`. No window is created.

#[path = "../fixtures/image_transform.rs"]
mod image_transform;
use image_transform::Transform;

use sashiko_renderer_core::graph::{
    Attachment, LoadOp, RasterAttachments, RasterDraw, RasterParameters, StoreOp,
};
use std::{error::Error, io::Write, time::Duration};

use sashiko_renderer_core::{
    graph::{BufferDesc, GraphCapacity, GraphPool, ImageDesc},
    vulkan::{FrameCapacity, ResourceClass, RuntimeConfig, VulkanRuntime},
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let output = arguments
        .next()
        .ok_or("pass an output PPM path or --help")?;
    if output == "--help" || output == "-h" {
        println!(
            "headless OUTPUT.ppm\n\nRender an offscreen image with Vulkan and save it as PPM."
        );
        return Ok(());
    }
    if arguments.next().is_some() {
        return Err("pass exactly one output PPM path".into());
    }
    let mut graph_pool = GraphPool::new(GraphCapacity::default(), 1)?;
    let mut graph = graph_pool.begin()?;
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
    let vertex_desc = BufferDesc {
        size: vertices.len() as u64,
    };
    let index_desc = BufferDesc {
        size: indices.len() as u64,
    };
    let vertex_buffer = graph.buffer("mesh vertices", vertex_desc)?;
    let index_buffer = graph.buffer("mesh indices", index_desc)?;
    graph.upload_buffer("upload vertices", vertex_buffer, 0, &vertices)?;
    graph.upload_buffer("upload indices", index_buffer, 0, &indices)?;
    let desc = ImageDesc::rgba8(128, 96);
    let color = graph.image("mesh color", desc)?;
    let depth = graph.image("mesh depth", ImageDesc::depth32(desc.width, desc.height))?;
    let inverted = graph.image("inverted", desc)?;
    let transformed = graph.image("transformed", desc)?;
    let final_image = graph.image("final copy", desc)?;
    graph.raster(
        "indexed mesh and depth",
        RasterAttachments {
            color_resolves: &[],
            colors: &[Attachment {
                view: graph.image_view(color, 0, 0)?,
                load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
                store: StoreOp::Store,
            }],
            depth: Some(Attachment {
                view: graph.image_view(depth, 0, 0)?,
                load: LoadOp::Clear(1.0),
                store: StoreOp::Store,
            }),
        },
        |raster| {
            raster.draw(
                &sashiko_renderer_core::program::ManagedRasterProgram::mesh(),
                [RasterDraw {
                    bindings: None,
                    secondary_vertices: None,
                    vertices: vertex_buffer,
                    indices: index_buffer,
                    first_index: 0,
                    index_count: 6,
                    instance_count: 1,
                    parameters: RasterParameters::default().into(),
                }],
            )
        },
    )?;
    image_transform::dispatch(&mut graph, "invert", color, inverted, Transform::Invert)?;
    image_transform::dispatch(
        &mut graph,
        "swap red and blue",
        inverted,
        transformed,
        Transform::SwapRedBlue,
    )?;
    graph.copy_image("final copy", transformed, final_image)?;
    graph.export_image(final_image)?;
    let readback = graph.readback_image("final readback", final_image)?;
    // This valid branch has no observable output and must be culled.
    let unused = graph.image("unused image", ImageDesc::rgba8(1, 1))?;
    graph.upload_image("unused upload", unused, &[1, 2, 3, 255])?;
    let plan = graph.compile()?;
    #[cfg(feature = "graph-diagnostics")]
    println!("{}", plan.dump());
    let mut runtime = VulkanRuntime::new(RuntimeConfig::default())?;
    runtime.prepare_working_set(
        FrameCapacity {
            work_slots: 1,
            result_slots: 1,
            ..Default::default()
        },
        &[
            ResourceClass::Buffer {
                desc: vertex_desc,
                count: 1,
            },
            ResourceClass::Buffer {
                desc: index_desc,
                count: 1,
            },
            ResourceClass::Image { desc, count: 4 },
            ResourceClass::Image {
                desc: ImageDesc::depth32(desc.width, desc.height),
                count: 1,
            },
        ],
    )?;
    runtime.prepare_compute_programs(&[
        image_transform::program(Transform::Invert),
        image_transform::program(Transform::SwapRedBlue),
    ])?;
    let mut rgba = vec![0; desc.width as usize * desc.height as usize * 4];
    let mut submission = runtime.submit(&plan)?;
    let mut result = submission
        .wait(Duration::from_secs(10))?
        .ok_or("GPU completion timed out")?;
    result.readback_into(readback, &mut rgba)?;
    let center = (desc.height as usize / 2 * desc.width as usize + desc.width as usize / 2) * 4;
    if rgba[center..center + 4] != [255, 255, 0, 255] || rgba[..4] != [255, 255, 255, 255] {
        return Err("unexpected mesh/depth/compute/copy output".into());
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(output)?);
    write!(file, "P6\n{} {}\n255\n", desc.width, desc.height)?;
    for pixel in rgba.as_chunks::<4>().0 {
        file.write_all(&pixel[..3])?;
    }
    file.flush()?;
    println!(
        "Verified {}x{} output; {:?}",
        desc.width,
        desc.height,
        runtime.stats()
    );
    Ok(())
}
