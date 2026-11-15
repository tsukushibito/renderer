//! Owned construction with consumer-defined intermediate passes, using real
//! reviewed shader contracts. This example compiles a CPU plan; general native
//! graphics and dispatch execution are separate backend work.

use renderer::graph::{
    Attachment, Binding, BindingResource, BufferDesc, Graph, GraphCapacity, GraphError, GraphPool,
    ImageDesc, ImageHandle, LoadOp, RasterAttachments, RasterDraw, RasterParameters, StoreOp,
};
use renderer::program::{ManagedRasterProgram, SamplerDescriptor};

fn sampled_pass(
    graph: &mut Graph,
    name: &str,
    draw: RasterDraw<'_>,
    uniform: renderer::graph::BufferHandle,
    source: ImageHandle,
    destination: ImageHandle,
) -> Result<(), GraphError> {
    // These arrays expire on return. Graph owns their canonical operands.
    let bindings = [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::Buffer {
                buffer: uniform,
                offset: 0,
                size: 16,
            },
        },
        Binding {
            group: 1,
            binding: 0,
            resource: BindingResource::ImageView(graph.image_view(source, 0, 0)?),
        },
        Binding {
            group: 1,
            binding: 1,
            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
        },
    ];
    graph.raster(
        name,
        &ManagedRasterProgram::textured_mesh(),
        &[RasterDraw {
            bindings: &bindings,
            ..draw
        }],
        RasterAttachments {
            colors: &[Attachment {
                view: graph.image_view(destination, 0, 0)?,
                load: LoadOp::Clear([0.0; 4]),
                store: StoreOp::Store,
            }],
            depth: None,
        },
    )?;
    Ok(())
}

fn sampled_chain(
    graph: &mut Graph,
    draw: RasterDraw<'_>,
    uniform: renderer::graph::BufferHandle,
    input: ImageHandle,
    enabled: bool,
) -> Result<ImageHandle, GraphError> {
    if !enabled {
        return Ok(input);
    }
    // Resource creation and pass composition belong to this consumer module.
    // No effect identifier or independent read/write list is sent to Renderer.
    let intermediate = graph.image("module intermediate", ImageDesc::rgba8(2, 2))?;
    let output = graph.image("module output", ImageDesc::rgba8(2, 2))?;
    sampled_pass(graph, "sample first", draw, uniform, input, intermediate)?;
    sampled_pass(graph, "sample second", draw, uniform, intermediate, output)?;
    Ok(output)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1)?;
    let mut graph = pool.begin()?;
    let vertices = graph.buffer("vertices", BufferDesc { size: 84 })?;
    let indices = graph.buffer("indices", BufferDesc { size: 12 })?;
    let uniform = graph.buffer("vertex offset", BufferDesc { size: 16 })?;
    let mut vertex_bytes = [0; 84];
    let attributes: [f32; 21] = [
        -1.0, -1.0, 0.5, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, 0.5, 1.0, 1.0, 1.0, 1.0, 0.0, 1.0, 0.5,
        1.0, 1.0, 1.0, 1.0,
    ];
    for (bytes, value) in vertex_bytes
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(attributes)
    {
        bytes.copy_from_slice(&value.to_le_bytes());
    }
    graph.upload_buffer("vertex bytes", vertices, 0, &vertex_bytes)?;
    graph.upload_buffer(
        "index bytes",
        indices,
        0,
        &[0, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0],
    )?;
    graph.upload_buffer("offset bytes", uniform, 0, &[0; 16])?;
    let input = graph.image("input", ImageDesc::rgba8(2, 2))?;
    graph.upload_image("input pixels", input, &[255; 16])?;
    let output = sampled_chain(
        &mut graph,
        RasterDraw {
            vertices,
            indices,
            index_count: 3,
            parameters: RasterParameters::default(),
            bindings: &[],
        },
        uniform,
        input,
        true,
    )?;
    graph.readback_image("result", output)?;
    let plan = graph.compile()?;
    for name in plan.ordered_pass_names() {
        println!("{name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_module_returns_input_and_does_not_keep_draw_inputs_alive() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let vertices = graph
            .buffer("unused vertices", BufferDesc { size: 84 })
            .unwrap();
        let indices = graph
            .buffer("unused indices", BufferDesc { size: 12 })
            .unwrap();
        let uniform = graph
            .buffer("unused uniform", BufferDesc { size: 16 })
            .unwrap();
        let input = graph.image("input", ImageDesc::rgba8(2, 2)).unwrap();
        graph
            .upload_image("input pixels", input, &[255; 16])
            .unwrap();
        let output = sampled_chain(
            &mut graph,
            RasterDraw {
                vertices,
                indices,
                index_count: 3,
                parameters: RasterParameters::default(),
                bindings: &[],
            },
            uniform,
            input,
            false,
        )
        .unwrap();
        assert_eq!(input, output);
        graph.readback_image("result", output).unwrap();
        assert_eq!(
            graph
                .compile()
                .unwrap()
                .ordered_pass_names()
                .collect::<Vec<_>>(),
            ["input pixels", "result"]
        );
    }
}
