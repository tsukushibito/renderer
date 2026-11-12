//! The same small scene and external Graph extension for headless and display.

use renderer::{
    engine::{Draw, EngineError, IDENTITY, MaterialHandle, MeshData, MeshHandle, Vertex, View},
    graph::{ComputeTransform, Graph, GraphError, ImageDesc, ImageHandle},
};

pub fn triangle(color: [f32; 4]) -> Result<MeshData, EngineError> {
    MeshData::new(
        &[
            Vertex {
                position: [-0.75, -0.75, 0.25],
                color,
            },
            Vertex {
                position: [0.75, -0.75, 0.25],
                color,
            },
            Vertex {
                position: [0.0, 0.75, 0.25],
                color,
            },
        ],
        &[0, 1, 2],
    )
}

pub fn view(width: u32, height: u32) -> View {
    View {
        width,
        height,
        clip_from_world: IDENTITY,
        clear_color: [0.0, 0.0, 0.0, 1.0],
    }
}

pub fn draw(mesh: MeshHandle, material: MaterialHandle, translation_x: f32) -> Draw {
    let mut world_from_model = IDENTITY;
    world_from_model[3][0] = translation_x;
    Draw {
        mesh,
        material,
        world_from_model,
    }
}

pub fn custom_pass(
    graph: &mut Graph,
    color: ImageHandle,
    width: u32,
    height: u32,
    enabled: bool,
) -> Result<ImageHandle, GraphError> {
    if !enabled {
        return Ok(color);
    }
    let output = graph.image("external custom output", ImageDesc::rgba8(width, height))?;
    graph.compute(
        "external custom SwapRedBlue",
        color,
        output,
        ComputeTransform::SwapRedBlue,
    )?;
    Ok(output)
}
