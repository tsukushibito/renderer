//! A UI panel used as a world material, followed by a UI equipment preview
//! and a HUD drawn at screen resolution, all in one Frame submission.
#![cfg(all(
    feature = "graphics",
    any(feature = "vulkan", all(windows, feature = "d3d12"))
))]

use renderer::canvas::{self, Layer, Paint};
use renderer::core::{
    graph::{
        Attachment, Binding, BindingResource, BufferDesc, Graph, GraphError, ImageDesc,
        ImageHandle, LoadOp, RasterAttachments, RasterDraw, RasterParameters, RasterRect, StoreOp,
    },
    program::{ManagedRasterProgram, SamplerDescriptor},
    runtime::{ResourceClass, RuntimeConfig, WorkingSet},
};

const VERTICES: BufferDesc = BufferDesc { size: 8 * 28 };
const INDICES: BufferDesc = BufferDesc { size: 12 * 4 };
const OFFSET: BufferDesc = BufferDesc { size: 16 };
const SCENE: ImageDesc = ImageDesc::rgba8(4, 2);
const DEPTH: ImageDesc = ImageDesc::depth32(4, 2);

const PANEL: ImageDesc = ImageDesc::rgba8(2, 1);
const ICON: ImageDesc = ImageDesc::rgba8(1, 1);

#[path = "../../renderer/examples/support/validation.rs"]
mod validation;

fn world(graph: &mut Graph, panel: ImageHandle) -> Result<ImageHandle, GraphError> {
    let vertices = graph.buffer("world vertices", VERTICES)?;
    let indices = graph.buffer("world indices", INDICES)?;
    let uniform = graph.buffer("vertex offset", OFFSET)?;
    // Draw the near green quad first. Its depth protects the left half of the
    // viewport from the full-screen white textured quad drawn afterwards.
    let quads = [(0., 0.2, [0., 1., 0., 1.]), (1., 0.8, [1.; 4])];
    let vertex_bytes = quads.map(|(right, z, [r, g, b, a])| {
        [[-1., -1.], [right, -1.], [right, 1.], [-1., 1.]]
            .map(|[x, y]| [x, y, z, r, g, b, a].map(f32::to_le_bytes))
    });
    let index_bytes = [0u32, 1, 2, 2, 3, 0, 4, 5, 6, 6, 7, 4].map(u32::to_le_bytes);
    graph.upload_buffer(
        "vertices",
        vertices,
        0,
        vertex_bytes.as_flattened().as_flattened().as_flattened(),
    )?;
    graph.upload_buffer("indices", indices, 0, index_bytes.as_flattened())?;
    graph.upload_buffer("offset", uniform, 0, &[0; 16])?;
    let program = ManagedRasterProgram::textured_mesh();
    let bindings = graph.raster_bindings(
        &program,
        &[
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
                resource: BindingResource::ImageView(graph.image_view(panel, 0, 0)?),
            },
            Binding {
                group: 1,
                binding: 1,
                resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
            },
        ],
    )?;
    let color = graph.image("world color", SCENE)?;
    let depth = graph.image("world depth", DEPTH)?;
    let draw = RasterDraw {
        secondary_vertices: None,
        vertices,
        indices,
        first_index: 0,
        index_count: 6,
        instance_count: 1,
        bindings: None,
        parameters: RasterParameters::default().into(),
    };
    graph.raster(
        "world",
        RasterAttachments {
            colors: &[Attachment {
                view: graph.image_view(color, 0, 0)?,
                load: LoadOp::Clear([0.; 4]),
                store: StoreOp::Store,
            }],
            color_resolves: &[],
            depth: Some(Attachment {
                view: graph.image_view(depth, 0, 0)?,
                load: LoadOp::Clear(1.),
                store: StoreOp::Store,
            }),
        },
        |pass| {
            pass.draw(&ManagedRasterProgram::mesh(), [draw])?;
            pass.draw(
                &program,
                [RasterDraw {
                    first_index: 6,
                    bindings: Some(bindings),
                    ..draw
                }],
            )
        },
    )?;
    Ok(color)
}

fn compose(config: RuntimeConfig) -> Result<(), Box<dyn std::error::Error>> {
    let resources = [ICON, PANEL, SCENE, DEPTH].map(|desc| ResourceClass::Image { desc, count: 1 });
    let buffers = [VERTICES, INDICES, OFFSET].map(|desc| ResourceClass::Buffer { desc, count: 1 });
    let camera = renderer::pipeline::CameraView {
        id: renderer::pipeline::ViewId(1),
        extent: [10, 4],
        camera: renderer::pbr::ViewInput::new(
            renderer::unlit_forward::IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            renderer::pbr::Projection::Orthographic,
        )?,
    };
    let mut meshes = renderer::mesh::Meshes::new(renderer::mesh::MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let mut materials = renderer::material::Materials::default();
    let mut renderer = renderer::Renderer::headless_with(
        config,
        camera,
        renderer::pipeline::SceneAssets::new(&mut meshes, &mut materials, &[]),
        |prep| {
            prep.add_working_set(WorkingSet {
                resources: &[resources.as_slice(), buffers.as_slice()].concat(),
                raster: &[
                    canvas::programs(renderer::core::ImageFormat::Rgba8Unorm)?[0]
                        .clone()
                        .into(),
                    ManagedRasterProgram::mesh().into(),
                    ManagedRasterProgram::textured_mesh().into(),
                ],
                compute: &[],
            })
        },
    )?;
    let mut frame = renderer.begin_frame()?;
    let graph = frame.graph();
    let icon = graph.image("icon", ICON)?;
    graph.upload_image("red", icon, &[255, 0, 0, 255])?;
    let icon = graph.image_view(icon, 0, 0)?;
    let panel = graph.image("terminal screen", PANEL)?;
    canvas::record(
        graph,
        panel,
        LoadOp::Clear([0., 0., 1., 1.]),
        &[Layer {
            paint: Paint::Image(icon),
            rect: RasterRect::new(1, 0, 1, 1),
            clip: None,
        }],
    )?;
    let scene = world(graph, panel)?;
    let scene = graph.image_view(scene, 0, 0)?;
    frame.draw_overlay(&[
        Layer {
            paint: Paint::Image(scene),
            rect: RasterRect::new(1, 0, 8, 4),
            clip: None,
        },
        Layer {
            paint: Paint::Image(icon),
            rect: RasterRect::new(2, 1, 1, 1),
            clip: None,
        },
    ])?;
    let capture = frame.capture()?;
    let mut pixels = [0; 160];
    assert!(renderer.read_capture(&capture, &mut pixels, std::time::Duration::from_secs(15))?);
    for (i, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (i % 10, i / 10);
        let expected = match (x, y) {
            (0 | 9, _) => [0, 0, 0, 255],
            (2, 1) | (5..=8, _) => [255, 0, 0, 255],
            _ => [0, 255, 0, 255],
        };
        assert_eq!(*pixel, expected, "pixel ({x},{y})");
    }
    validation::check_validation(renderer.into_runtime()?)
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Vulkan device"]
fn vulkan_world_panel() -> Result<(), Box<dyn std::error::Error>> {
    compose(RuntimeConfig::Vulkan(Default::default()))
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a D3D12 device"]
fn d3d12_world_panel() -> Result<(), Box<dyn std::error::Error>> {
    compose(RuntimeConfig::D3D12(Default::default()))
}
