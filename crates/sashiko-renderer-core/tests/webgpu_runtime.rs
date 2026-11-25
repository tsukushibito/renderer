//! Browser integration tests for the same public Graph/Runtime interfaces as native hosts.
//! Run with wasm-bindgen-test-runner and a WebGPU-capable browser (see development guide).
#![cfg(all(feature = "webgpu", target_arch = "wasm32", target_os = "unknown"))]

use std::{
    pin::pin,
    task::{Context, Waker},
    time::Duration,
};

use sashiko_renderer_core::{
    capacity::{ResourceClass, RuntimeCapacityKind, WorkingSet},
    geometry::GeometryData,
    graph::{
        Attachment, Binding, BindingResource, BufferDesc, Graph, GraphCapacity, GraphPool,
        ImageDesc, LoadOp, RasterAttachments, RasterDraw, RasterParameters, RasterRect, StoreOp,
    },
    program::{ManagedProgram, ManagedRasterProgram, SamplerDescriptor},
    runtime::{ExecutionErrorKind, Runtime, RuntimeLimits, WebGpuConfig},
    webgpu::{CanvasTarget, wgpu},
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::*;
use web_sys::HtmlCanvasElement;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen(inline_js = "
export function newCanvas() { return document.createElement('canvas'); }
export function nextTurn() { return new Promise(resolve => setTimeout(resolve, 0)); }
")]
extern "C" {
    #[wasm_bindgen(js_name = newCanvas)]
    fn new_canvas() -> HtmlCanvasElement;
    #[wasm_bindgen(js_name = nextTurn)]
    async fn next_turn();
}

async fn target() -> CanvasTarget {
    CanvasTarget::attach(new_canvas(), Default::default(), Default::default())
        .await
        .expect("WebGPU device for browser Runtime acceptance")
}

async fn runtime(target: &CanvasTarget, set: WorkingSet<'_>) -> Runtime {
    Runtime::from_webgpu_device(
        target.device(),
        target.queue(),
        WebGpuConfig {
            upload_bytes: 16384,
            readback_bytes: 16384,
        },
        RuntimeLimits {
            graph: GraphCapacity {
                draws: 8,
                bindings: 16,
                passes: 24,
                ..Default::default()
            },
            frames: 2,
            results: 2,
            ..Default::default()
        },
        set,
    )
    .await
    .unwrap()
}

fn buffer(size: u64) -> ResourceClass {
    ResourceClass::Buffer {
        desc: BufferDesc { size },
        count: 1,
    }
}

#[wasm_bindgen_test]
#[ignore = "requires browser WebGPU"]
async fn device_failures_reach_the_canvas_and_each_attached_runtime() {
    for expected in [ExecutionErrorKind::Native, ExecutionErrorKind::DeviceLost] {
        let target = target().await;
        let mut first = runtime(&target, WorkingSet::default()).await;
        let mut second = runtime(&target, WorkingSet::default()).await;
        if expected == ExecutionErrorKind::DeviceLost {
            target.device().destroy();
        } else {
            // A mapping buffer may only combine MAP_READ with COPY_DST.
            drop(target.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("intentional uncaptured validation error"),
                size: 16,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            }));
        }
        for _ in 0..2500 {
            if target.check_device().is_err() {
                break;
            }
            next_turn().await;
        }
        assert!(target.check_device().is_err());
        assert_eq!(first.poll_frame().err().unwrap().kind(), expected);
        assert_eq!(second.poll_frame().err().unwrap().kind(), expected);
    }
}

#[wasm_bindgen_test]
#[ignore = "requires browser WebGPU"]
async fn scheduled_exports_survive_early_drops_and_stale_revisions_are_rejected() {
    let target = target().await;
    let image = ImageDesc::rgba8(3, 2);
    let classes = [
        ResourceClass::Buffer {
            desc: BufferDesc { size: 16 },
            count: 2,
        },
        ResourceClass::Image {
            desc: image,
            count: 1,
        },
    ];
    let set = WorkingSet {
        resources: &classes,
        compute: &[ManagedProgram::uniform_readback()],
        ..Default::default()
    };
    let mut rt = runtime(&target, set).await;
    let mut other = runtime(&target, WorkingSet::default()).await;
    let mut pool = GraphPool::new(rt.limits().graph, 3).unwrap();
    let mut before = pool.begin().unwrap();
    let b = before.buffer("counter", BufferDesc { size: 16 }).unwrap();
    before.upload_buffer("seed", b, 0, &[7; 16]).unwrap();
    before.export_buffer(b).unwrap();
    let pixels: [u8; 24] = std::array::from_fn(|i| (3 * i) as u8);
    let tex = before
        .upload_texture_for_export("3x2 packed rows", image, &pixels)
        .unwrap();
    let (seed, owner) = rt
        .submit_with_image(&before.compile().unwrap(), tex)
        .unwrap();
    let old = seed.export_buffer(b).unwrap();
    let image_key = owner.snapshot().key();
    let first = seed.id();
    drop(seed);
    assert!(rt.wait_for_frame(first, Duration::ZERO).unwrap().is_none());
    assert_eq!(
        rt.wait_for_frame(first, Duration::from_millis(1))
            .err()
            .unwrap()
            .kind(),
        ExecutionErrorKind::Unsupported
    );

    let mut update = pool.begin().unwrap();
    let b = update.import_buffer("counter", &old).unwrap();
    let tex = update
        .import_image("seeded image", owner.snapshot())
        .unwrap();
    update
        .upload_buffer("change prefix", b, 0, &[42; 4])
        .unwrap();
    update.export_buffer(b).unwrap();
    let output = update
        .buffer("compute output", BufferDesc { size: 16 })
        .unwrap();
    let bindings = [(0, b), (1, output)].map(|(group, buffer)| Binding {
        group,
        binding: 0,
        resource: BindingResource::Buffer {
            buffer,
            offset: 0,
            size: 16,
        },
    });
    update
        .dispatch(
            "copy uniform to storage",
            &ManagedProgram::uniform_readback(),
            &bindings,
        )
        .unwrap();
    let image_read = update.readback_image("packed rows", tex).unwrap();
    let buffer_read = update
        .readback_buffer("computed contents", output, 0, 16)
        .unwrap();
    let receipt = rt.submit(&update.compile().unwrap()).unwrap();
    let new = receipt.export_buffer(b).unwrap();
    let second = receipt.id();
    drop((owner, receipt));
    rt.collect_released_images().unwrap();
    assert_eq!(
        rt.refresh_image(image_key).unwrap_err().kind(),
        ExecutionErrorKind::StaleResource
    );
    assert_eq!(rt.refresh_buffer(old.key()).unwrap(), new);
    assert_ne!(old, new);

    let mut reader = pool.begin().unwrap();
    let latest = reader.import_buffer("current revision", &new).unwrap();
    reader
        .readback_buffer("current prefix", latest, 0, 4)
        .unwrap();
    let read_plan = reader.compile().unwrap();
    assert_eq!(
        rt.submit(&read_plan)
            .err()
            .unwrap()
            .capacity()
            .unwrap()
            .resource,
        RuntimeCapacityKind::ExecutionSlots
    );
    // Cancelling a pending wait must leave the frame available to a later waiter.
    {
        let wait = pin!(rt.wait_for_webgpu_frame(second));
        assert!(
            wait.poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
    }
    // The first queue operation's completion remains independently retrievable.
    let mut result = rt.wait_for_webgpu_frame(second).await.unwrap();
    assert_eq!(result.id(), second);
    let mut packed = [0; 24];
    assert!(result.readback_into(image_read, &mut packed[..4]).is_err());
    result.readback_into(image_read, &mut packed).unwrap();
    assert_eq!(packed, pixels);
    let mut counter = [0; 16];
    result.readback_into(buffer_read, &mut counter).unwrap();
    assert_eq!(
        counter,
        [42, 42, 42, 42, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7]
    );
    assert_eq!(rt.wait_for_webgpu_frame(first).await.unwrap().id(), first);
    let third = rt.submit(&read_plan).unwrap().id();
    assert_eq!(
        rt.submit(&read_plan)
            .err()
            .unwrap()
            .capacity()
            .unwrap()
            .resource,
        RuntimeCapacityKind::ResultSlots
    );
    drop(result);
    let fourth = rt.submit(&read_plan).unwrap().id();
    drop(rt.wait_for_webgpu_frame(third).await.unwrap());
    drop(rt.wait_for_webgpu_frame(fourth).await.unwrap());
    drop(read_plan);
    assert!(rt.poll_frame().unwrap().is_none());

    let mut reader = pool.begin().unwrap();
    let stale = reader.import_buffer("old revision", &old).unwrap();
    reader.readback_buffer("stale read", stale, 0, 4).unwrap();
    let plan = reader.compile().unwrap();
    assert_eq!(
        rt.submit(&plan).err().unwrap().kind(),
        ExecutionErrorKind::StaleResource
    );
    assert_eq!(
        other.submit(&plan).err().unwrap().kind(),
        ExecutionErrorKind::WrongOwner
    );
    assert_eq!(rt.pending_frames(), 0);
    rt.delete(new.key()).unwrap();
    assert_eq!(
        rt.refresh_buffer(new.key()).unwrap_err().kind(),
        ExecutionErrorKind::StaleResource
    );
    target.check_device().unwrap();
}

fn draw(
    graph: &mut Graph,
    program: &ManagedRasterProgram,
    vertices: sashiko_renderer_core::graph::BufferHandle,
    indices: sashiko_renderer_core::graph::BufferHandle,
    offset: sashiko_renderer_core::graph::BufferHandle,
    color: [u8; 4],
    depth: f32,
) -> RasterDraw {
    let tex = graph.image("albedo", ImageDesc::rgba8(1, 1)).unwrap();
    graph.upload_image("upload albedo", tex, &color).unwrap();
    let view = graph.image_view(tex, 0, 0).unwrap();
    let bindings = graph
        .raster_bindings(
            program,
            &[
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::Buffer {
                        buffer: offset,
                        offset: 0,
                        size: 16,
                    },
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: BindingResource::ImageView(view),
                },
                Binding {
                    group: 1,
                    binding: 1,
                    resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                },
            ],
        )
        .unwrap();
    let mut model = *RasterParameters::default().transform();
    model[3][2] = depth;
    RasterDraw {
        vertices,
        indices,
        secondary_vertices: None,
        bindings: Some(bindings),
        first_index: 0,
        index_count: 3,
        instance_count: 1,
        parameters: RasterParameters::new(model, [1.; 4]).unwrap().into(),
    }
}

#[wasm_bindgen_test]
#[ignore = "requires browser WebGPU"]
async fn textured_triangles_preserve_the_nearest_layer_and_read_back_packed_rows() {
    let target = target().await;
    let program = ManagedRasterProgram::textured_mesh();
    let raster = [program.clone().into()];
    let color_desc = ImageDesc::rgba8(5, 3);
    let depth_desc = ImageDesc::depth32(5, 3);
    let classes = [
        buffer(3 * 28),
        buffer(3 * 4),
        buffer(16),
        ResourceClass::Image {
            desc: ImageDesc::rgba8(1, 1),
            count: 2,
        },
        ResourceClass::Image {
            desc: color_desc,
            count: 1,
        },
        ResourceClass::Image {
            desc: depth_desc,
            count: 1,
        },
    ];
    let mut rt = runtime(
        &target,
        WorkingSet {
            resources: &classes,
            raster: &raster,
            samplers: &[SamplerDescriptor::NearestClamp],
            ..Default::default()
        },
    )
    .await;
    let mut pool = GraphPool::new(rt.limits().graph, 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let vertex_bytes: Vec<_> = [[-1f32, -1.], [3., -1.], [-1., 3.]]
        .into_iter()
        .flat_map(|[x, y]| [x, y, 0., 1., 1., 1., 1.])
        .flat_map(f32::to_le_bytes)
        .collect();
    let mesh = GeometryData::new(28, &vertex_bytes, &[0, 1, 2]).unwrap();
    let vertices = graph.buffer("triangle", BufferDesc { size: 84 }).unwrap();
    let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
    mesh.upload_vertices(&mut graph, "vertices", vertices)
        .unwrap();
    mesh.upload_indices(&mut graph, "indices", indices).unwrap();
    let offset = graph
        .buffer("vertex offset", BufferDesc { size: 16 })
        .unwrap();
    graph
        .upload_buffer("zero offset", offset, 0, &[0; 16])
        .unwrap();
    let blue = [16, 64, 192, 255];
    let red = [224, 48, 8, 255];
    let near = draw(&mut graph, &program, vertices, indices, offset, blue, 0.25);
    let far = draw(&mut graph, &program, vertices, indices, offset, red, 0.75);
    let color = graph.image("surface color", color_desc).unwrap();
    let depth = graph.image("depth", depth_desc).unwrap();
    let attachments = [Attachment {
        view: graph.image_view(color, 0, 0).unwrap(),
        load: LoadOp::Clear([0.; 4]),
        store: StoreOp::Store,
    }];
    let depth_attachment = Attachment {
        view: graph.image_view(depth, 0, 0).unwrap(),
        load: LoadOp::Clear(1.),
        store: StoreOp::Store,
    };
    graph
        .raster(
            "overlapping triangles",
            RasterAttachments {
                colors: &attachments,
                color_resolves: &[],
                depth: Some(depth_attachment),
            },
            |pass| {
                pass.set_scissor(Some(RasterRect::new(0, 0, 2, 3)))?;
                pass.draw(&program, [near])?;
                pass.set_scissor(None)?;
                pass.draw(&program, [far])
            },
        )
        .unwrap();
    let color_read = graph.readback_image("color", color).unwrap();
    let depth_read = graph.readback_image("depth", depth).unwrap();
    let plan = graph.compile().unwrap();
    assert!(
        pool.begin().is_err(),
        "the single Graph slot is owned by the compiled plan"
    );
    let id = rt.submit(&plan).unwrap().id();
    drop(plan);
    let mut result = rt.wait_for_webgpu_frame(id).await.unwrap();
    let mut colors = [0; 5 * 3 * 4];
    let mut depths = [0; 5 * 3 * 4];
    result.readback_into(color_read, &mut colors).unwrap();
    result.readback_into(depth_read, &mut depths).unwrap();
    for (i, (rgba, z)) in colors
        .chunks_exact(4)
        .zip(depths.chunks_exact(4))
        .enumerate()
    {
        let left = i % 5 < 2;
        assert_eq!(rgba, if left { &blue } else { &red });
        assert_eq!(
            f32::from_le_bytes(z.try_into().unwrap()),
            if left { 0.25 } else { 0.75 }
        );
    }
    drop(result);
    assert!(pool.begin().is_ok());
    target.check_device().unwrap();
}
