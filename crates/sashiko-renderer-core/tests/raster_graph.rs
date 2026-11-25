//! The same owned Raster plan executed on both native APIs. Hardware and
//! validation prerequisites are failures, never successful skips.

#![cfg(all(
    windows,
    feature = "vulkan",
    feature = "d3d12",
    any(
        all(feature = "d3d12-diagnostics", feature = "vulkan-diagnostics"),
        not(any(
            feature = "development-validation",
            feature = "d3d12-diagnostics",
            feature = "vulkan-diagnostics",
            feature = "graph-diagnostics"
        ))
    )
))]

use sashiko_renderer_core::ImageFormat;
use std::time::Duration;

#[allow(dead_code)]
#[path = "../fixtures/compute_fixture.rs"]
mod compute_fixture;
#[allow(dead_code)]
#[path = "../fixtures/standard_fixture.rs"]
mod standard_fixture;

use sashiko_renderer::{passes, unlit_forward};
use sashiko_renderer_core::graph::{
    Attachment, Binding, BindingResource, BufferDesc, CompiledGraph, Graph, GraphCapacity,
    GraphPool, ImageDesc, ImageHandle, LoadOp, PassHandle, RasterAttachments, RasterDraw,
    RasterParameters, RasterRect, StoreOp,
};
use sashiko_renderer_core::program::{ManagedRasterProgram, SamplerDescriptor};
use sashiko_renderer_core::{
    d3d12::{
        D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
        D3D12RuntimeErrorKind, D3D12ValidationMode,
    },
    vulkan::{
        FrameCapacity, ResourceClass, RuntimeConfig, RuntimeErrorKind, ValidationMode,
        VulkanRuntime,
    },
};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(30);

struct Expected {
    pass: PassHandle,
    name: &'static str,
    bytes: Vec<u8>,
}

fn hdr(width: u32, height: u32) -> ImageDesc {
    ImageDesc {
        format: ImageFormat::Rgba16Float,
        ..ImageDesc::rgba8(width, height)
    }
}

fn float_bytes(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn half_bytes(values: &[[u16; 4]]) -> Vec<u8> {
    values
        .iter()
        .flatten()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn pixels<T: Copy, const N: usize>(mut pixel: impl FnMut(usize, usize) -> [T; N]) -> Vec<T> {
    let mut output = Vec::with_capacity(16 * N);
    for y in 0..4 {
        for x in 0..4 {
            output.extend_from_slice(&pixel(x, y));
        }
    }
    output
}

fn quad(graph: &mut Graph, invalid_last_index: bool) -> RasterDraw {
    let vertices = graph
        .buffer("quad vertices", BufferDesc { size: 112 })
        .unwrap();
    let indices = graph
        .buffer("quad indices", BufferDesc { size: 24 })
        .unwrap();
    let values: Vec<f32> = [[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]]
        .into_iter()
        .flat_map(|[x, y]| [x, y, 0.5, 1.0, 1.0, 1.0, 1.0])
        .collect();
    graph
        .upload_buffer("vertex contents", vertices, 0, &float_bytes(&values))
        .unwrap();
    let values = [0_u32, 1, 2, 2, 1, if invalid_last_index { 4 } else { 3 }];
    let bytes: Vec<u8> = values.into_iter().flat_map(u32::to_le_bytes).collect();
    graph
        .upload_buffer("index contents", indices, 0, &bytes)
        .unwrap();
    RasterDraw {
        bindings: None,
        secondary_vertices: None,
        vertices,
        indices,
        first_index: 0,
        index_count: 6,
        instance_count: 1,
        parameters: RasterParameters::default().into(),
    }
}

fn placed(mut draw: RasterDraw, left: bool, depth: f32, tint: [f32; 4]) -> RasterDraw {
    let mut transform = *RasterParameters::default().transform();
    transform[2][2] = 0.0;
    transform[3][2] = depth;
    if left {
        transform[0][0] = 0.5;
        transform[3][0] = -0.5;
    }
    draw.parameters = RasterParameters::new(transform, tint).unwrap().into();
    draw
}

fn color_attachment(
    graph: &Graph,
    image: ImageHandle,
    load: LoadOp<[f32; 4]>,
) -> Attachment<[f32; 4]> {
    Attachment {
        view: graph.image_view(image, 0, 0).unwrap(),
        load,
        store: StoreOp::Store,
    }
}

fn depth_attachment(graph: &Graph, image: ImageHandle, load: LoadOp<f32>) -> Attachment<f32> {
    Attachment {
        view: graph.image_view(image, 0, 0).unwrap(),
        load,
        store: StoreOp::Store,
    }
}

fn observe(graph: &mut Graph, image: ImageHandle, name: &'static str, bytes: Vec<u8>) -> Expected {
    Expected {
        pass: graph.readback_image(name, image).unwrap(),
        name,
        bytes,
    }
}

fn prepass_continuations(graph: &mut Graph, quad: RasterDraw, expected: &mut Vec<Expected>) {
    let depth = graph
        .image("prepass depth", ImageDesc::depth32(4, 4))
        .unwrap();
    let depth_clear = depth_attachment(graph, depth, LoadOp::Clear(1.0));
    passes::depth_prepass(
        graph,
        "prepass geometry",
        [
            placed(quad, false, 0.5, [1.0; 4]),
            placed(quad, true, 0.25, [1.0; 4]),
        ],
        depth_clear,
    )
    .unwrap();
    for (desc, same_name, near_name, colors, near) in [
        (
            ImageDesc::rgba8(4, 4),
            "prepassed RGBA8 equal geometry",
            "prepassed RGBA8 nearer overlay",
            pixels(|x, _| {
                if x < 2 {
                    [0, 255, 0, 255]
                } else {
                    [255, 0, 0, 255]
                }
            }),
            pixels(|_, _| [0, 0, 255, 255]),
        ),
        (
            hdr(4, 4),
            "prepassed HDR equal geometry",
            "prepassed HDR nearer overlay",
            half_bytes(
                pixels(|x, _| {
                    if x < 2 {
                        [0, 0x4400, 0, 0x3c00]
                    } else {
                        [0x4000, 0, 0, 0x3c00]
                    }
                })
                .as_chunks::<4>()
                .0,
            ),
            half_bytes(&[[0, 0, 0x3c00, 0x3c00]; 16]),
        ),
    ] {
        let color = graph.image(same_name, desc).unwrap();
        let hdr = desc.format == ImageFormat::Rgba16Float;
        let color_clear = color_attachment(graph, color, LoadOp::Clear([0.0; 4]));
        let depth_load = depth_attachment(graph, depth, LoadOp::Load);
        passes::opaque(
            graph,
            same_name,
            [
                placed(
                    quad,
                    false,
                    0.5,
                    [if hdr { 2.0 } else { 1.0 }, 0.0, 0.0, 1.0],
                ),
                placed(
                    quad,
                    true,
                    0.25,
                    [0.0, if hdr { 4.0 } else { 1.0 }, 0.0, 1.0],
                ),
                // A disabled depth test would replace both equal-depth draws.
                placed(quad, false, 0.75, [1.0, 1.0, 0.0, 1.0]),
            ],
            color_clear,
            Some(depth_load),
            passes::OpaqueDepth::Prepassed,
        )
        .unwrap();
        expected.push(observe(graph, color, same_name, colors));
        let color_load = color_attachment(graph, color, LoadOp::Load);
        passes::opaque(
            graph,
            near_name,
            [placed(quad, false, 0.125, [0.0, 0.0, 1.0, 1.0])],
            color_load,
            Some(depth_load),
            passes::OpaqueDepth::Prepassed,
        )
        .unwrap();
        expected.push(observe(graph, color, near_name, near));
    }
    // The nearer fragments were shaded but must not replace the prepass depth.
    expected.push(observe(
        graph,
        depth,
        "prepassed readonly depth",
        float_bytes(&pixels(|x, _| [if x < 2 { 0.25 } else { 0.5 }])),
    ));
}

fn standard_recipes(graph: &mut Graph, quad: RasterDraw, expected: &mut Vec<Expected>) {
    let config = unlit_forward::ForwardConfig {
        color: ImageDesc::rgba8(4, 4),
        clear_color: [1.0, 0.0, 0.0, 1.0],
        depth: false,
    };
    let empty = unlit_forward::forward(
        graph,
        "standard empty",
        std::iter::empty::<RasterDraw>(),
        config,
    )
    .unwrap();
    assert_eq!(empty.depth, None);
    expected.push(observe(
        graph,
        empty.color,
        "standard empty clear",
        pixels(|_, _| [255, 0, 0, 255]),
    ));
    let scene = unlit_forward::forward(
        graph,
        "standard forward",
        [placed(quad, false, 0.5, [1.0, 0.0, 0.0, 1.0])],
        config,
    )
    .unwrap();
    let a = graph.image("standard post a", config.color).unwrap();
    let post = graph.image("standard post b", config.color).unwrap();
    graph
        .copy_image("standard post first", scene.color, a)
        .unwrap();
    graph.copy_image("standard post second", a, post).unwrap();
    expected.push(observe(
        graph,
        post,
        "standard two-node post",
        pixels(|_, _| [255, 0, 0, 255]),
    ));
    let hdr = unlit_forward::forward(
        graph,
        "standard HDR",
        [placed(quad, false, 0.5, [2.0, -0.5, 0.25, 1.0])],
        unlit_forward::ForwardConfig {
            color: ImageDesc::rgba16_float(4, 4),
            ..config
        },
    )
    .unwrap();
    let target = graph
        .image("standard HDR output", graph.image_desc(hdr.color).unwrap())
        .unwrap();
    graph
        .copy_image("standard HDR copy", hdr.color, target)
        .unwrap();
    expected.push(observe(
        graph,
        target,
        "standard HDR output",
        half_bytes(&[[0x4000, 0xb800, 0x3400, 0x3c00]; 16]),
    ));
}

fn shared_graph(invalid_last_index: bool) -> (GraphPool, CompiledGraph, Vec<Expected>) {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let quad = quad(&mut graph, invalid_last_index);
    let mut expected = Vec::new();

    let order = graph.image("color-only", ImageDesc::rgba8(4, 4)).unwrap();
    graph
        .raster(
            "ordered overlap without depth",
            RasterAttachments {
                color_resolves: &[],
                colors: &[color_attachment(&graph, order, LoadOp::Clear([0.0; 4]))],
                depth: None,
            },
            |raster| {
                raster.draw(
                    &ManagedRasterProgram::mesh(),
                    [
                        RasterDraw {
                            // The failure fixture reaches a valid first draw before
                            // discovering the bad sixth index of the second draw.
                            index_count: if invalid_last_index { 3 } else { 6 },
                            ..placed(quad, false, 0.5, [1.0, 0.0, 0.0, 1.0])
                        },
                        placed(quad, true, 0.5, [0.0, 1.0, 0.0, 1.0]),
                    ],
                )
            },
        )
        .unwrap();
    expected.push(observe(
        &mut graph,
        order,
        "color-only order",
        pixels(|x, _| {
            if x < 2 {
                [0, 255, 0, 255]
            } else {
                [255, 0, 0, 255]
            }
        }),
    ));
    let mut right_overlay = placed(quad, true, 0.5, [0.0, 0.0, 1.0, 1.0]);
    let mut words: [u32; 20] = right_overlay.parameters.words().try_into().unwrap();
    words[12] = 0.5_f32.to_bits();
    right_overlay.parameters = sashiko_renderer_core::graph::RasterConstants::new(&words).unwrap();
    graph
        .raster(
            "load existing color and replace right half",
            RasterAttachments {
                color_resolves: &[],
                colors: &[color_attachment(&graph, order, LoadOp::Load)],
                depth: None,
            },
            |raster| raster.draw(&ManagedRasterProgram::mesh(), [right_overlay]),
        )
        .unwrap();
    expected.push(observe(
        &mut graph,
        order,
        "color Load preservation",
        pixels(|x, _| {
            if x < 2 {
                [0, 255, 0, 255]
            } else {
                [0, 0, 255, 255]
            }
        }),
    ));

    graph
        .raster(
            "UI panels",
            RasterAttachments {
                colors: &[color_attachment(&graph, order, LoadOp::Clear([0.0; 4]))],
                color_resolves: &[],
                depth: None,
            },
            |raster| {
                let program = ManagedRasterProgram::mesh();
                let red = placed(quad, false, 0.5, [1.0, 0.0, 0.0, 1.0]);
                let green = placed(quad, false, 0.5, [0.0, 1.0, 0.0, 1.0]);
                raster.set_viewport(Some(RasterRect::new(1, 1, 2, 2)))?;
                raster.draw(&program, [red])?;
                raster.set_viewport(None)?;
                raster.set_scissor(Some(RasterRect::new(2, 0, 1, 2)))?;
                raster.draw(&program, [green])?;
                raster.set_scissor(Some(RasterRect::new(0, 0, 0, 4)))?;
                raster.draw(&program, [red])
            },
        )
        .unwrap();
    expected.push(observe(
        &mut graph,
        order,
        "viewport and scissor",
        pixels(|x, y| {
            if x == 2 && y < 2 {
                [0, 255, 0, 255]
            } else if (1..3).contains(&x) && (1..3).contains(&y) {
                [255, 0, 0, 255]
            } else {
                [0; 4]
            }
        }),
    ));

    let color = graph
        .image("depth-tested color", ImageDesc::rgba8(4, 4))
        .unwrap();
    let depth = graph
        .image("depth-tested depth", ImageDesc::depth32(4, 4))
        .unwrap();
    graph
        .raster(
            "near far near sequence",
            RasterAttachments {
                color_resolves: &[],
                colors: &[color_attachment(&graph, color, LoadOp::Clear([0.0; 4]))],
                depth: Some(depth_attachment(&graph, depth, LoadOp::Clear(1.0))),
            },
            |raster| {
                raster.draw(
                    &ManagedRasterProgram::mesh(),
                    [
                        placed(quad, false, 0.25, [1.0, 0.0, 0.0, 1.0]),
                        placed(quad, false, 0.75, [0.0, 1.0, 0.0, 1.0]),
                        placed(quad, true, 0.125, [0.0, 0.0, 1.0, 1.0]),
                    ],
                )
            },
        )
        .unwrap();
    expected.push(observe(
        &mut graph,
        color,
        "depth-tested color",
        pixels(|x, _| {
            if x < 2 {
                [0, 0, 255, 255]
            } else {
                [255, 0, 0, 255]
            }
        }),
    ));
    expected.push(observe(
        &mut graph,
        depth,
        "depth-tested D32",
        float_bytes(&pixels(|x, _| [if x < 2 { 0.125 } else { 0.25 }])),
    ));

    let shadow = graph
        .image("shadow depth", ImageDesc::depth32(4, 4))
        .unwrap();
    graph
        .raster(
            "depth-only shadow",
            RasterAttachments {
                color_resolves: &[],
                colors: &[],
                depth: Some(depth_attachment(&graph, shadow, LoadOp::Clear(1.0))),
            },
            |raster| {
                raster.draw(
                    &ManagedRasterProgram::depth_only(),
                    [
                        placed(quad, false, 0.5, [1.0; 4]),
                        placed(quad, true, 0.25, [1.0; 4]),
                    ],
                )
            },
        )
        .unwrap();
    expected.push(observe(
        &mut graph,
        shadow,
        "depth-only D32",
        float_bytes(&pixels(|x, _| [if x < 2 { 0.25 } else { 0.5 }])),
    ));
    graph
        .raster(
            "load shadow and reject farther geometry",
            RasterAttachments {
                color_resolves: &[],
                colors: &[],
                depth: Some(depth_attachment(&graph, shadow, LoadOp::Load)),
            },
            |raster| {
                raster.draw(
                    &ManagedRasterProgram::depth_only(),
                    [placed(quad, false, 0.75, [1.0; 4])],
                )
            },
        )
        .unwrap();
    expected.push(observe(
        &mut graph,
        shadow,
        "depth Load preservation",
        float_bytes(&pixels(|x, _| [if x < 2 { 0.25 } else { 0.5 }])),
    ));

    let uniform = graph
        .buffer("three clip offsets", BufferDesc { size: 768 })
        .unwrap();
    let mut uniform_bytes = vec![0; 768];
    uniform_bytes[..16].copy_from_slice(&float_bytes(&[-0.5, 0.0, 0.0, 0.0]));
    uniform_bytes[256..272].copy_from_slice(&float_bytes(&[0.5, 0.0, 0.0, 0.0]));
    graph
        .upload_buffer("clip offsets", uniform, 0, &uniform_bytes)
        .unwrap();
    let source_a = graph.image("source a", ImageDesc::rgba8(2, 2)).unwrap();
    let source_b = graph.image("source b", ImageDesc::rgba8(2, 2)).unwrap();
    let colors_a = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
    ];
    let colors_b = [
        [0, 255, 255, 255],
        [255, 0, 255, 255],
        [255, 255, 255, 255],
        [0, 0, 0, 255],
    ];
    graph
        .upload_image("texture a", source_a, &colors_a.concat())
        .unwrap();
    graph
        .upload_image("texture b", source_b, &colors_b.concat())
        .unwrap();
    let bindings = |graph: &Graph, source, offset| {
        [
            Binding {
                group: 0,
                binding: 0,
                resource: BindingResource::Buffer {
                    buffer: uniform,
                    offset,
                    size: 32,
                },
            },
            Binding {
                group: 1,
                binding: 0,
                resource: BindingResource::ImageView(graph.image_view(source, 0, 0).unwrap()),
            },
            Binding {
                group: 1,
                binding: 1,
                resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
            },
        ]
    };
    let left = bindings(&graph, source_a, 0);
    let right = bindings(&graph, source_b, 256);
    let left = graph
        .raster_bindings(&ManagedRasterProgram::textured_mesh(), &left)
        .unwrap();
    let right = graph
        .raster_bindings(&ManagedRasterProgram::textured_mesh(), &right)
        .unwrap();
    let output = graph
        .image("two textured panels", ImageDesc::rgba8(4, 4))
        .unwrap();
    let mut panel = quad;
    let mut words: [u32; 20] = panel.parameters.words().try_into().unwrap();
    words[0] = 0.5_f32.to_bits();
    panel.parameters = sashiko_renderer_core::graph::RasterConstants::new(&words).unwrap();
    graph
        .raster(
            "per-draw descriptor ownership",
            RasterAttachments {
                color_resolves: &[],
                colors: &[color_attachment(&graph, output, LoadOp::Clear([0.0; 4]))],
                depth: None,
            },
            |raster| {
                raster.draw(
                    &ManagedRasterProgram::textured_mesh(),
                    [
                        RasterDraw {
                            bindings: Some(left),
                            ..panel
                        },
                        RasterDraw {
                            bindings: Some(right),
                            ..panel
                        },
                    ],
                )
            },
        )
        .unwrap();
    expected.push(observe(
        &mut graph,
        output,
        "per-draw textures",
        pixels(|x, y| {
            let source = if x < 2 { colors_a } else { colors_b };
            source[(y / 2) * 2 + x % 2]
        }),
    ));

    let output = graph.image("mesh HDR", hdr(4, 4)).unwrap();
    graph
        .raster(
            "signed HDR vertex color",
            RasterAttachments {
                color_resolves: &[],
                colors: &[color_attachment(&graph, output, LoadOp::Clear([0.0; 4]))],
                depth: None,
            },
            |raster| {
                raster.draw(
                    &ManagedRasterProgram::mesh_hdr(),
                    [
                        placed(quad, false, 0.5, [2.0, -0.5, 0.25, 1.0]),
                        placed(quad, true, 0.5, [0.5, 4.0, -1.0, 1.0]),
                    ],
                )
            },
        )
        .unwrap();
    let mesh_halves: Vec<_> = (0..4)
        .flat_map(|_| {
            (0..4).map(|x| {
                if x < 2 {
                    [0x3800, 0x4400, 0xbc00, 0x3c00]
                } else {
                    [0x4000, 0xb800, 0x3400, 0x3c00]
                }
            })
        })
        .collect();
    expected.push(observe(
        &mut graph,
        output,
        "mesh RGBA16F",
        half_bytes(&mesh_halves),
    ));

    let source = graph.image("HDR source", hdr(2, 2)).unwrap();
    let source_halves = [
        [0x4000, 0xb800, 0x3400, 0x3c00],
        [0x3800, 0x4400, 0xbc00, 0x3c00],
        [0x4200, 0x0000, 0x3c00, 0x3800],
        [0xb400, 0x3c00, 0x4000, 0x3c00],
    ];
    graph
        .upload_image("HDR texels", source, &half_bytes(&source_halves))
        .unwrap();
    let hdr_bindings = bindings(&graph, source, 512);
    let hdr_bindings = graph
        .raster_bindings(&ManagedRasterProgram::textured_mesh_hdr(), &hdr_bindings)
        .unwrap();
    let output = graph.image("textured HDR", hdr(4, 4)).unwrap();
    graph
        .raster(
            "HDR sampling",
            RasterAttachments {
                color_resolves: &[],
                colors: &[color_attachment(&graph, output, LoadOp::Clear([0.0; 4]))],
                depth: None,
            },
            |raster| {
                raster.draw(
                    &ManagedRasterProgram::textured_mesh_hdr(),
                    [RasterDraw {
                        bindings: Some(hdr_bindings),
                        ..quad
                    }],
                )
            },
        )
        .unwrap();
    let expanded: Vec<_> = (0..4)
        .flat_map(|y| (0..4).map(move |x| source_halves[(y / 2) * 2 + x / 2]))
        .collect();
    expected.push(observe(
        &mut graph,
        output,
        "textured RGBA16F",
        half_bytes(&expanded),
    ));

    prepass_continuations(&mut graph, quad, &mut expected);
    standard_recipes(&mut graph, quad, &mut expected);
    // Keep the existing RGBA8/HDR fixtures and add all four post selections to
    // the same plan, adapter checks, sequential runs and teardown diagnostics.
    for frame in 0..standard_fixture::CASES {
        let added = standard_fixture::append(&mut graph, frame);
        expected.extend(added.readbacks.map(|entry| Expected {
            pass: entry.pass,
            name: entry.name,
            bytes: entry.bytes[..entry.len].to_vec(),
        }));
    }
    (pool, graph.compile().unwrap(), expected)
}

fn programs() -> [ManagedRasterProgram; 7] {
    [
        ManagedRasterProgram::mesh(),
        ManagedRasterProgram::mesh_hdr(),
        ManagedRasterProgram::textured_mesh(),
        ManagedRasterProgram::textured_mesh_hdr(),
        ManagedRasterProgram::depth_only(),
        ManagedRasterProgram::mesh_prepassed(),
        ManagedRasterProgram::mesh_hdr_prepassed(),
    ]
}

fn image_classes() -> [(ImageDesc, usize); 7] {
    [
        (ImageDesc::rgba8(4, 4), 8),
        (ImageDesc::rgba8(2, 2), 2),
        (ImageDesc::depth32(4, 4), 3),
        (hdr(4, 4), 5),
        (hdr(2, 2), 1),
        (
            standard_fixture::hdr(),
            standard_fixture::HDR_IMAGES * standard_fixture::CASES,
        ),
        (
            ImageDesc::depth32(standard_fixture::WIDTH, standard_fixture::HEIGHT),
            standard_fixture::DEPTH_IMAGES * standard_fixture::CASES,
        ),
    ]
}

fn checksum(bytes: &[u8]) -> String {
    format!(
        "{:016x}",
        bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| (hash
            ^ u64::from(*byte))
        .wrapping_mul(0x100000001b3))
    )
}

fn compare(
    expected: &[Expected],
    mut read: impl FnMut(PassHandle, &mut [u8]),
    backend: &str,
) -> Vec<Value> {
    expected
        .iter()
        .map(|entry| {
            let mut actual = vec![0; entry.bytes.len()];
            read(entry.pass, &mut actual);
            assert_eq!(actual, entry.bytes, "{backend}: {}", entry.name);
            json!({"name":entry.name,"bytes":actual.len(),"checksum_fnv1a64":checksum(&actual)})
        })
        .collect()
}

fn run(strict: bool) {
    let mut vulkan = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 1,
            resource_capacity: 128,
            max_shader_bindings: 256,
            ..Default::default()
        },
        if strict {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )
    .expect("Vulkan hardware/validation prerequisites");
    let mut d3d12 = D3D12Runtime::new(D3D12RuntimeConfig {
        validation: if strict {
            D3D12ValidationMode::Required
        } else {
            D3D12ValidationMode::Disabled
        },
        capacity: D3D12FrameCapacity {
            executions: 1,
            buffers: 3 + standard_fixture::BUFFER_SIZES.len() * standard_fixture::CASES,
            ..Default::default()
        },
    })
    .expect("D3D12 hardware/validation prerequisites");
    let vk_info = vulkan.device_info();
    let dx_info = d3d12.device_info();
    assert_ne!(vk_info.device_type, "CPU", "hardware Vulkan required");
    assert_eq!(
        vk_info.device_luid,
        Some(dx_info.luid),
        "same hardware adapter required"
    );
    assert_eq!(vk_info.vendor_id, dx_info.vendor_id);
    assert_eq!(vk_info.device_id, dx_info.device_id);
    let vk_monitor = vulkan.validation_monitor();
    let dx_monitor = d3d12.validation_monitor();
    vulkan
        .prepare_samplers(&[SamplerDescriptor::NearestClamp])
        .unwrap();
    d3d12
        .prepare_samplers(&[SamplerDescriptor::NearestClamp])
        .unwrap();
    let mut resources: Vec<_> = [112, 24, 768]
        .into_iter()
        .map(|size| ResourceClass::Buffer {
            desc: BufferDesc { size },
            count: 1,
        })
        .collect();
    resources.extend(
        image_classes()
            .into_iter()
            .map(|(desc, count)| ResourceClass::Image { desc, count }),
    );
    resources.extend(
        standard_fixture::BUFFER_SIZES.map(|size| ResourceClass::Buffer {
            desc: BufferDesc { size },
            count: standard_fixture::CASES,
        }),
    );
    vulkan
        .prepare_working_set(
            FrameCapacity {
                work_slots: 1,
                ..Default::default()
            },
            &resources,
        )
        .expect("Vulkan prepared resource classes");
    vulkan
        .prepare_raster_programs(&programs())
        .expect("Vulkan prepared graphics pipelines");
    vulkan
        .prepare_compute_programs(&standard_fixture::programs())
        .expect("Vulkan prepared consumer Compute pipelines");
    let images: Vec<_> = image_classes()
        .into_iter()
        .map(|(desc, count)| D3D12ImageClass { desc, count })
        .collect();
    d3d12
        .prepare_graph_working_set(
            None,
            &images,
            &programs().map(Into::into),
            &standard_fixture::programs(),
        )
        .expect("D3D12 prepared graphics resources");

    let (invalid_pool, invalid, _) = shared_graph(true);
    assert_eq!(
        vulkan
            .submit(&invalid)
            .err()
            .expect("late invalid index rejected")
            .kind(),
        RuntimeErrorKind::InvalidInput
    );
    assert_eq!(
        d3d12
            .prepare(&invalid)
            .err()
            .expect("late invalid index rejected")
            .kind(),
        D3D12RuntimeErrorKind::InvalidInput
    );
    drop((invalid, invalid_pool));

    let (_pool, plan, expected) = shared_graph(false);
    let recording = d3d12.prepare(&plan).expect("record without submitting");
    assert_eq!(
        d3d12
            .prepare(&plan)
            .err()
            .expect("sole execution held by recording")
            .kind(),
        D3D12RuntimeErrorKind::CapacityExhausted
    );
    drop(recording);
    drop(
        d3d12
            .prepare(&plan)
            .expect("cancel returns descriptor/image/command leases"),
    );

    // A cancelled or rejected recording must not poison later use. Repeat on
    // the same native capacity before testing escaped native owners below.
    for _ in 0..2 {
        let mut submitted = vulkan.submit(&plan).expect("Vulkan Raster submit");
        let mut result = submitted
            .wait(WAIT)
            .unwrap()
            .expect("Vulkan Raster completion");
        compare(
            &expected,
            |pass, out| result.readback_into(pass, out).unwrap(),
            "Vulkan reuse",
        );
        drop((result, submitted));
        let mut submitted = d3d12.submit(&plan).expect("D3D12 Raster submit");
        let mut result = submitted
            .wait(WAIT)
            .unwrap()
            .expect("D3D12 Raster completion");
        compare(
            &expected,
            |pass, out| result.readback_into(pass, out).unwrap(),
            "D3D12 reuse",
        );
    }

    drop(plan);
    // Separate plans built from identical inputs let each backend lose every
    // public plan handle before observing completion, with native runs still
    // sequential. Only each GraphPool's prepared backing owner remains public.
    let (vk_pool, vk_plan, vk_expected) = shared_graph(false);
    let mut vk_submission = vulkan
        .submit(&vk_plan)
        .expect("Vulkan escaped-owner submit");
    drop(vk_plan);
    drop(vulkan);
    let mut vk_result = vk_submission
        .wait(WAIT)
        .unwrap()
        .expect("Vulkan escaped-owner completion");
    let vk_pixels = compare(
        &vk_expected,
        |pass, out| vk_result.readback_into(pass, out).unwrap(),
        "Vulkan escaped owners",
    );
    drop((vk_result, vk_submission));
    drop(vk_pool);
    let (dx_pool, dx_plan, dx_expected) = shared_graph(false);
    let mut dx_submission = d3d12.submit(&dx_plan).expect("D3D12 escaped-owner submit");
    drop(dx_plan);
    d3d12.shutdown().unwrap();
    let mut dx_result = dx_submission
        .wait(WAIT)
        .unwrap()
        .expect("D3D12 escaped-owner completion");
    let dx_pixels = compare(
        &dx_expected,
        |pass, out| dx_result.readback_into(pass, out).unwrap(),
        "D3D12 escaped owners",
    );
    drop((dx_result, dx_submission));
    drop(dx_pool);

    let vk_report = vk_monitor.report();
    let dx_report = dx_monitor.snapshot();
    let report = json!({
        "scope":"owned-raster-common-graph", "strict":strict,
        "configuration": {"development_validation":cfg!(feature="development-validation"),"graph_diagnostics":cfg!(feature="graph-diagnostics"),"vulkan_diagnostics":cfg!(feature="vulkan-diagnostics"),"d3d12_diagnostics":cfg!(feature="d3d12-diagnostics"),"debug_assertions":cfg!(debug_assertions),"external_layer_injection_checked":false},
        "adapter":{"name":vk_info.name,"luid":dx_info.luid,"vendor_id":dx_info.vendor_id,"device_id":dx_info.device_id,"vulkan_driver":vk_info.driver_info,"d3d12_driver":dx_info.driver_version},
        "compared_bytes_per_backend_per_run":expected.iter().map(|entry|entry.bytes.len()).sum::<usize>(),
        "preserved_raster_readbacks":17,
        "standard_composition":{
            "routes":["standard forward direct","identity post","consumer post selection","standard passes plus the same consumer post"],
            "post_selections":["disabled","multiply then add","replacement: add then multiply","addition: multiply then add then add"],
            "draw_counts":[0,1,4,2],"hdr_extent":[standard_fixture::WIDTH,standard_fixture::HEIGHT],
            "consumer_programs":["reviewed sampled HDR multiply","reviewed storage HDR add","GPU offset producer"],
            "custom_workgroup_size":[4,4,1],"custom_workgroups":[1,2,1],
            "readbacks_per_case":standard_fixture::READBACKS,"cases":standard_fixture::CASES,
            "bytes_per_case":standard_fixture::READBACK_BYTES,
            "unused_branch_per_case":"unconsumed multiply then add module"
        },
        "successful_runs_per_backend":3,"same_compiled_graph_runs_per_backend":2,"lifetime_plan_rebuilt_from_same_inputs":true,"native_runs_sequential":true,"invalid_index_rejected":true,"d3d12_record_cancel_reuse":true,"runtime_and_all_public_plan_handles_dropped_before_completion_observation":true,
        "vulkan_readbacks":vk_pixels,"d3d12_readbacks":dx_pixels,
        "validation":{
            "vulkan":{"requested":format!("{:?}",vk_report.requested),"core":vk_report.core_validation,"sync":vk_report.synchronization_validation,"warnings":vk_report.warnings,"errors":vk_report.errors,"callback_failures":vk_report.callback_failures,"dropped_problems":vk_report.dropped_problems,"dropped_layer_statuses":vk_report.dropped_layer_statuses,"problems":vk_report.problems,"layer_status_messages":vk_report.layer_status_messages},
            "d3d12":{"debug_layer":dx_report.debug_layer_enabled,"gbv":dx_report.gpu_based_validation_enabled,"teardown_checked":dx_report.teardown_checked,"warnings":dx_report.warnings,"errors":dx_report.errors,"corruption":dx_report.corruption,"callback_failures":dx_report.callback_failures,"dropped_messages":dx_report.dropped_messages,"retained_message_count":dx_report.messages.len()}
        }
    });
    eprintln!("RASTER_GRAPH_REPORT={report}");
    assert!(vk_report.is_clean(), "Vulkan diagnostics: {report}");
    assert!(
        dx_report.is_clean(),
        "D3D12 diagnostics: {dx_report:?}; {report}"
    );
    if strict {
        assert_eq!(vk_report.core_validation, Some(true), "{report}");
        assert_eq!(vk_report.synchronization_validation, Some(true), "{report}");
        assert_eq!(vk_report.dropped_layer_statuses, 0, "{report}");
        assert!(
            dx_report.debug_layer_enabled
                && dx_report.gpu_based_validation_enabled
                && dx_report.teardown_checked,
            "{report}"
        );
    } else {
        assert_eq!(vk_report.requested, ValidationMode::Disabled);
        assert_eq!(vk_report.core_validation, None);
        assert_eq!(vk_report.synchronization_validation, None);
        assert!(!dx_report.debug_layer_enabled && !dx_report.gpu_based_validation_enabled);
    }
}

#[cfg(all(feature = "d3d12-diagnostics", feature = "vulkan-diagnostics"))]
#[test]
#[ignore = "requires same-adapter hardware, Vulkan Core/SyncVal and D3D12 Debug Layer/GBV"]
fn owned_raster_common_graph_strict() {
    run(true);
}

#[cfg(not(any(
    feature = "development-validation",
    feature = "d3d12-diagnostics",
    feature = "vulkan-diagnostics",
    feature = "graph-diagnostics"
)))]
#[test]
#[ignore = "requires same-adapter Vulkan/D3D12 hardware; run optimized with diagnostics excluded"]
fn owned_raster_common_graph_shipping() {
    if cfg!(debug_assertions) {
        panic!("shipping acceptance requires --release");
    }
    run(false);
}
