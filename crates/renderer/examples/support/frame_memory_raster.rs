//! Owned Raster Graph lifecycle probe with a counting process allocator.
//! All setup, native preparation, warmup and report formatting are separate
//! from the changing-input build/execute/readback/return measurement window.

use super::frame_memory::{Report, counts, features, phase, semantics};
use renderer::ImageFormat;
use renderer::graph::{
    Attachment, Binding, BindingResource, BufferDesc, CompiledGraph, Graph, GraphCapacity,
    GraphPool, ImageDesc, LoadOp, PassHandle, RasterAttachments, RasterDraw, RasterParameters,
    StoreOp,
};
use renderer::measurement::{NativeEvent, RustEvent, Snapshot};
use renderer::program::{ManagedRasterProgram, SamplerDescriptor};
use std::time::Duration;

const IN_FLIGHT: usize = 3;
const GRAPH_SLOTS: usize = IN_FLIGHT * 4;
const WARMUP: usize = 24;
const FRAMES: usize = 24;
const WIDTH: u32 = 16;
const HEIGHT: u32 = 16;
const PIXELS: usize = WIDTH as usize * HEIGHT as usize;
const OUTPUT_BYTES: usize = PIXELS * 8;
const WAIT: Duration = Duration::from_secs(30);

fn capacity() -> GraphCapacity {
    GraphCapacity {
        resources: 16,
        passes: 16,
        accesses: 64,
        draws: 4,
        bindings: 16,
        attachments: 8,
        dependencies: 16,
        initial_ranges: 32,
        name_bytes: 1024,
        upload_bytes: 4096,
        edges: 256,
        segments: 256,
        readers: 256,
    }
}

fn hdr(width: u32, height: u32) -> ImageDesc {
    ImageDesc {
        format: ImageFormat::Rgba16Float,
        ..ImageDesc::rgba8(width, height)
    }
}

fn programs() -> [ManagedRasterProgram; 5] {
    [
        ManagedRasterProgram::mesh(),
        ManagedRasterProgram::mesh_hdr(),
        ManagedRasterProgram::textured_mesh(),
        ManagedRasterProgram::textured_mesh_hdr(),
        ManagedRasterProgram::depth_only(),
    ]
}

// Only these exactly representable binary16 values occur in this probe. This
// is fixture encoding, not a replacement for a general float conversion API.
fn half(value: f32) -> u16 {
    match value {
        0.0 => 0x0000,
        0.125 => 0x3000,
        0.25 => 0x3400,
        0.5 => 0x3800,
        1.0 => 0x3c00,
        2.0 => 0x4000,
        _ => panic!("unexpected exact fixture value"),
    }
}

fn half_pixel(values: [f32; 4]) -> [u8; 8] {
    let mut pixel = [0; 8];
    for (destination, value) in pixel.as_chunks_mut::<2>().0.iter_mut().zip(values) {
        destination.copy_from_slice(&half(value).to_le_bytes());
    }
    pixel
}

#[derive(Clone, Copy)]
struct Readback {
    pass: PassHandle,
    pixel: [u8; 8],
    pixel_bytes: usize,
}

#[derive(Clone, Copy)]
struct Expected {
    readbacks: [Option<Readback>; 2],
}

fn build(pool: &mut GraphPool, frame: usize) -> (Graph, Expected) {
    let mode = frame % 6;
    let float_color = matches!(mode, 3 | 4);
    let textured = matches!(mode, 2 | 4);
    let depth_only = mode == 5;
    let with_depth = matches!(mode, 1 | 2 | 5);
    let draw_count = [1, 3, 2, 0][frame % 4];
    let clear_depth = if (frame / 6).is_multiple_of(2) {
        1.0f32
    } else {
        0.875f32
    };
    let mut graph = pool.begin().unwrap();
    let vertex_buffer = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
    let index_buffer = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
    let uniform = graph.buffer("offsets", BufferDesc { size: 512 }).unwrap();
    let mut vertices = [0; 84];
    // The oversized triangle covers the complete target even when the vertex
    // Uniform translates it. Every texel can be checked without edge tolerances.
    for (destination, position) in vertices.as_chunks_mut::<28>().0.iter_mut().zip([
        [-2.0f32, -2.0, 0.0],
        [6.0, -2.0, 0.0],
        [-2.0, 6.0, 0.0],
    ]) {
        for (component, value) in destination
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(position.into_iter().chain([1.0; 4]))
        {
            component.copy_from_slice(&value.to_le_bytes());
        }
    }
    graph
        .upload_buffer("vertices", vertex_buffer, 0, &vertices)
        .unwrap();
    let indices = [0u32, 1, 2];
    let mut index_bytes = [0; 12];
    for (destination, value) in index_bytes.as_chunks_mut::<4>().0.iter_mut().zip(indices) {
        destination.copy_from_slice(&value.to_le_bytes());
    }
    graph
        .upload_buffer("indices", index_buffer, 0, &index_bytes)
        .unwrap();
    let mut offsets = [0; 512];
    offsets[..4].copy_from_slice(&0.0625f32.to_le_bytes());
    offsets[256..260].copy_from_slice(&(-0.0625f32).to_le_bytes());
    graph
        .upload_buffer("offsets", uniform, 0, &offsets)
        .unwrap();
    let texture_desc = if float_color {
        hdr(1, 1)
    } else {
        ImageDesc::rgba8(1, 1)
    };
    let textures = [
        graph.image("texture A", texture_desc).unwrap(),
        graph.image("texture B", texture_desc).unwrap(),
    ];
    let texture_colors = if float_color {
        [[2.0, 0.5, 0.25, 1.0], [0.25, 2.0, 0.5, 1.0]]
    } else {
        [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]]
    };
    for (image, color) in textures.into_iter().zip(texture_colors) {
        if float_color {
            graph
                .upload_image("texture upload", image, &half_pixel(color))
                .unwrap();
        } else {
            let pixel = color.map(|component| (component * 255.0) as u8);
            graph.upload_image("texture upload", image, &pixel).unwrap();
        }
    }
    let color = (!depth_only).then(|| {
        graph
            .image(
                "color",
                if float_color {
                    hdr(WIDTH, HEIGHT)
                } else {
                    ImageDesc::rgba8(WIDTH, HEIGHT)
                },
            )
            .unwrap()
    });
    let depth = with_depth.then(|| {
        graph
            .image("depth", ImageDesc::depth32(WIDTH, HEIGHT))
            .unwrap()
    });
    let views = textures.map(|image| graph.image_view(image, 0, 0).unwrap());
    let bindings: [[Binding; 3]; 3] = std::array::from_fn(|draw| {
        [
            Binding {
                group: 0,
                binding: 0,
                resource: BindingResource::Buffer {
                    buffer: uniform,
                    offset: (draw % 2) as u64 * 256,
                    size: 16,
                },
            },
            Binding {
                group: 1,
                binding: 0,
                resource: BindingResource::ImageView(views[(draw + frame / 12) % 2]),
            },
            Binding {
                group: 1,
                binding: 1,
                resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
            },
        ]
    });
    let draws: [RasterDraw<'_>; 3] = std::array::from_fn(|draw| {
        let mut transform = *RasterParameters::default().transform();
        transform[3][2] = 0.75 - draw as f32 * 0.25;
        let tint = if textured {
            [
                if !float_color || (frame / 6).is_multiple_of(2) {
                    1.0
                } else {
                    0.5
                },
                1.0,
                1.0,
                1.0,
            ]
        } else {
            texture_colors[(draw + frame / 12) % 2]
        };
        RasterDraw {
            vertices: vertex_buffer,
            indices: index_buffer,
            index_count: 3,
            bindings: if textured { &bindings[draw] } else { &[] },
            parameters: RasterParameters::new(transform, tint).unwrap().into(),
        }
    });
    let program = if depth_only {
        ManagedRasterProgram::depth_only()
    } else if textured && float_color {
        ManagedRasterProgram::textured_mesh_hdr()
    } else if textured {
        ManagedRasterProgram::textured_mesh()
    } else if float_color {
        ManagedRasterProgram::mesh_hdr()
    } else {
        ManagedRasterProgram::mesh()
    };
    let clear = if float_color {
        [0.125, 0.25, 0.5, 1.0]
    } else {
        [0.0, 0.0, 1.0, 1.0]
    };
    let color_attachments = color.map(|image| Attachment {
        view: graph.image_view(image, 0, 0).unwrap(),
        load: LoadOp::Clear(clear),
        store: StoreOp::Store,
    });
    let depth_attachment = depth.map(|image| Attachment {
        view: graph.image_view(image, 0, 0).unwrap(),
        load: LoadOp::Clear(clear_depth),
        store: StoreOp::Store,
    });
    graph
        .raster(
            "owned Raster",
            &program,
            &draws[..draw_count],
            RasterAttachments {
                colors: color_attachments.as_slice(),
                depth: depth_attachment,
            },
        )
        .unwrap();
    let mut expected = Expected {
        readbacks: [None; 2],
    };
    if let Some(color) = color {
        let mut value = if draw_count == 0 {
            clear
        } else if textured {
            texture_colors[(draw_count - 1 + frame / 12) % 2]
        } else {
            std::array::from_fn::<_, 4, _>(|i| {
                f32::from_bits(draws[draw_count - 1].parameters.words()[16 + i])
            })
        };
        if draw_count != 0 && textured {
            for (component, tint) in value.iter_mut().zip(std::array::from_fn::<_, 4, _>(|i| {
                f32::from_bits(draws[draw_count - 1].parameters.words()[16 + i])
            })) {
                *component *= tint;
            }
        }
        let mut pixel = [0; 8];
        if float_color {
            pixel = half_pixel(value);
        } else {
            pixel[..4].copy_from_slice(&value.map(|v| (v * 255.0).round() as u8));
        }
        expected.readbacks[0] = Some(Readback {
            pass: graph.readback_image("color readback", color).unwrap(),
            pixel,
            pixel_bytes: if float_color { 8 } else { 4 },
        });
    }
    if let Some(depth) = depth {
        let mut pixel = [0; 8];
        pixel[..4].copy_from_slice(
            &if draw_count == 0 {
                clear_depth
            } else {
                f32::from_bits(draws[draw_count - 1].parameters.words()[14])
            }
            .to_le_bytes(),
        );
        expected.readbacks[1] = Some(Readback {
            pass: graph.readback_image("depth readback", depth).unwrap(),
            pixel,
            pixel_bytes: 4,
        });
    }
    (graph, expected)
}

// Measurement-only adapters call each public runtime directly. They do not
// publish a new rendering interface or share native implementation details.
trait Backend {
    const RECORDING_CANCEL: bool;
    type Pending;
    type Completed;
    fn cancel_recording(&mut self, graph: &CompiledGraph);
    fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending;
    fn complete(pending: &mut Self::Pending) -> Self::Completed;
    fn readback(result: &mut Self::Completed, pass: PassHandle, output: &mut [u8]);
    fn collect(&mut self);
    fn native_capacity() -> serde_json::Value;
}

fn consume<B: Backend>(mut result: B::Completed, expected: Expected) {
    let mut output = [0; OUTPUT_BYTES];
    for readback in expected.readbacks.into_iter().flatten() {
        let bytes = &mut output[..PIXELS * readback.pixel_bytes];
        B::readback(&mut result, readback.pass, bytes);
        for pixel in bytes.chunks_exact(readback.pixel_bytes) {
            assert_eq!(pixel, &readback.pixel[..readback.pixel_bytes]);
        }
    }
    drop(result);
}

fn recycle_slots(pool: &mut GraphPool) {
    let empty: [Graph; GRAPH_SLOTS] = std::array::from_fn(|_| pool.begin().unwrap());
    drop(empty);
    assert_eq!(pool.leased_slots(), 0);
}

fn frames<B: Backend>(runtime: &mut B, pool: &mut GraphPool, count: usize) -> Report<5> {
    let mut pending: [Option<(B::Pending, Expected)>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut held: [Option<(B::Completed, Expected)>; IN_FLIGHT] = std::array::from_fn(|_| None);
    let mut phases = [Snapshot::default(); 5];
    let before = Snapshot::now();
    for frame in 0..count {
        let slot = frame % IN_FLIGHT;
        phase(&mut phases[4], || {
            if let Some((result, expected)) = held[slot].take() {
                consume::<B>(result, expected);
            }
        });
        phase(&mut phases[3], || {
            if let Some((mut work, expected)) = pending[slot].take() {
                held[slot] = Some((B::complete(&mut work), expected));
                drop(work);
            }
        });
        let (graph, expected) = phase(&mut phases[0], || build(pool, frame));
        let plan = phase(&mut phases[1], || graph.compile().unwrap());
        let work = phase(&mut phases[2], || {
            runtime.cancel_recording(&plan);
            runtime.submit(&plan)
        });
        pending[slot] = Some((work, expected));
        phase(&mut phases[4], || drop(plan));
    }
    for slot in 0..IN_FLIGHT {
        phase(&mut phases[4], || {
            if let Some((result, expected)) = held[slot].take() {
                consume::<B>(result, expected);
            }
        });
        phase(&mut phases[3], || {
            if let Some((mut work, expected)) = pending[slot].take() {
                held[slot] = Some((B::complete(&mut work), expected));
                drop(work);
            }
        });
    }
    phase(&mut phases[4], || {
        for (result, expected) in held.into_iter().flatten() {
            consume::<B>(result, expected);
        }
        drop(pending);
        runtime.collect();
        recycle_slots(pool);
    });
    Report {
        total: Snapshot::now().since(before),
        phases,
    }
}

fn controls(startup: Snapshot, measured: Snapshot, shutdown: Snapshot) {
    assert_ne!(startup.rust[RustEvent::Allocate as usize].calls, 0);
    assert_ne!(shutdown.rust[RustEvent::Deallocate as usize].calls, 0);
    for event in [
        NativeEvent::ObjectCreate,
        NativeEvent::ResourceCreate,
        NativeEvent::MemoryAllocate,
        NativeEvent::DescriptorAllocate,
    ] {
        assert_ne!(
            startup.native[event as usize], 0,
            "native setup hook must be exercised"
        );
    }
    for event in [
        NativeEvent::ObjectDestroy,
        NativeEvent::ResourceDestroy,
        NativeEvent::MemoryFree,
        NativeEvent::DescriptorFree,
    ] {
        assert!(
            [startup, measured, shutdown]
                .into_iter()
                .any(|value| value.native[event as usize] != 0),
            "native destruction hook must be exercised"
        );
    }
}

fn report<B: Backend>(
    name: &str,
    baseline: bool,
    startup: Snapshot,
    shutdown: Snapshot,
    measured: Report<5>,
    adapter: serde_json::Value,
    validation: serde_json::Value,
) {
    let graph = capacity();
    let json = serde_json::json!({
        "scenario": name,
        "mode": if baseline { "baseline" } else { "require_zero" },
        "features": features(),
        "adapter": adapter,
        "validation_after_native_teardown": validation,
        "counter_semantics": semantics(),
        "warmup_frames": WARMUP,
        "measured_frames": FRAMES,
        "pending_capacity": IN_FLIGHT,
        "completed_readbacks_held": IN_FLIGHT,
        "graph_slots": GRAPH_SLOTS,
        "retained_graph_slots_after_window": 0,
        "graph_capacity": {
            "resources": graph.resources, "passes": graph.passes, "accesses": graph.accesses,
            "draws": graph.draws, "bindings": graph.bindings, "attachments": graph.attachments,
            "dependencies": graph.dependencies, "initial_ranges": graph.initial_ranges,
            "name_bytes": graph.name_bytes, "upload_bytes": graph.upload_bytes,
            "edges": graph.edges, "segments": graph.segments, "readers": graph.readers,
        },
        "extent": [WIDTH, HEIGHT],
        "draw_counts": [1, 3, 2, 0],
        "depth_clear_values": [1.0, 0.875],
        "programs": ["mesh", "mesh_hdr", "textured_mesh", "textured_mesh_hdr", "depth_only"],
        "pass_shapes": ["color-only RGBA8", "color+depth RGBA8", "textured color+depth RGBA8", "color-only RGBA16F", "textured color-only RGBA16F", "depth-only D32"],
        "bindings": "per-draw Uniform at byte0/256, alternating sampled images and nearest/clamp sampler; two binary-exact input versions",
        "native_capacity": B::native_capacity(),
        "recording_cancel_each_frame": B::RECORDING_CANCEL,
        "coverage": "continuous ordinary public Graph build/compile/native prepare-record-submit/GPU completion/poll-retire/CPU readback/full result and plan return; all pixels verified; changing draw count, parameters, Program, attachment format/shape and per-draw bindings; no presentation or asset update",
        "caller": "fixed pending/result arrays and 2048-byte stack output, explicit waits on the reused ring; formatting outside measurement",
        "unobserved": "driver/OS/native allocations outside configured hooks; absent Vulkan layer startup in Disabled mode does not disprove external layer injection",
        "startup_positive_control": counts(startup),
        "shutdown_outside_normal_frames": counts(shutdown),
        "total": counts(measured.total),
        "phases_build_compile_submit_complete_return": measured.phases.map(counts),
    });
    println!("FRAME_MEMORY_REPORT={json}");
    assert!(!measured.total.overflowed);
    if !baseline {
        assert!(
            measured.total.is_allocation_free(),
            "Raster lifecycle still allocates or frees: {:?}",
            measured.total
        );
    }
}

#[cfg(feature = "vulkan")]
impl Backend for renderer::VulkanRuntime {
    const RECORDING_CANCEL: bool = false;
    type Pending = renderer::Submission;
    type Completed = renderer::SubmissionResult;
    fn cancel_recording(&mut self, _: &CompiledGraph) {}
    fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending {
        self.submit(graph).unwrap()
    }
    fn complete(pending: &mut Self::Pending) -> Self::Completed {
        pending.wait(WAIT).unwrap().unwrap()
    }
    fn readback(result: &mut Self::Completed, pass: PassHandle, output: &mut [u8]) {
        assert_eq!(result.readback_len(pass).unwrap(), output.len());
        result.readback_into(pass, output).unwrap();
    }
    fn collect(&mut self) {
        assert!(self.poll_frame().unwrap().is_none());
        assert_eq!(self.pending_frames(), 0);
    }
    fn native_capacity() -> serde_json::Value {
        serde_json::json!({
            "execution_slots": IN_FLIGHT, "resource_capacity": 64,
            "staging_bytes": 16384, "staging_ranges": 64, "max_shader_bindings": 32,
            "work_slots": IN_FLIGHT, "result_slots": IN_FLIGHT * 2,
            "metadata_slots": 16, "range_snapshots": 64,
            "initialized_ranges_per_resource": 16, "provenance_segments_per_resource": 16,
            "resource_classes": [
                { "kind": "buffer", "bytes": 84, "count": IN_FLIGHT },
                { "kind": "buffer", "bytes": 12, "count": IN_FLIGHT },
                { "kind": "buffer", "bytes": 512, "count": IN_FLIGHT },
                { "kind": "rgba8", "extent": [WIDTH, HEIGHT], "count": IN_FLIGHT },
                { "kind": "rgba16f", "extent": [WIDTH, HEIGHT], "count": IN_FLIGHT },
                { "kind": "d32", "extent": [WIDTH, HEIGHT], "count": IN_FLIGHT },
                { "kind": "rgba8", "extent": [1, 1], "count": IN_FLIGHT * 2 },
                { "kind": "rgba16f", "extent": [1, 1], "count": IN_FLIGHT * 2 },
            ],
        })
    }
}

#[cfg(feature = "vulkan")]
pub fn vulkan(baseline: bool) {
    if cfg!(debug_assertions) {
        panic!("memory acceptance requires --release");
    }
    let start = Snapshot::now();
    let mut runtime = renderer::VulkanRuntime::new(renderer::RuntimeConfig {
        execution_slots: IN_FLIGHT,
        resource_capacity: 64,
        staging_bytes: 16 * 1024,
        staging_ranges: 64,
        max_shader_bindings: 32,
    })
    .unwrap();
    let info = runtime.device_info();
    assert_ne!(info.device_type, "CPU");
    let monitor = runtime.validation_monitor();
    let classes = [
        renderer::ResourceClass::Buffer {
            desc: BufferDesc { size: 84 },
            count: IN_FLIGHT,
        },
        renderer::ResourceClass::Buffer {
            desc: BufferDesc { size: 12 },
            count: IN_FLIGHT,
        },
        renderer::ResourceClass::Buffer {
            desc: BufferDesc { size: 512 },
            count: IN_FLIGHT,
        },
        renderer::ResourceClass::Image {
            desc: ImageDesc::rgba8(WIDTH, HEIGHT),
            count: IN_FLIGHT,
        },
        renderer::ResourceClass::Image {
            desc: hdr(WIDTH, HEIGHT),
            count: IN_FLIGHT,
        },
        renderer::ResourceClass::Image {
            desc: ImageDesc::depth32(WIDTH, HEIGHT),
            count: IN_FLIGHT,
        },
        renderer::ResourceClass::Image {
            desc: ImageDesc::rgba8(1, 1),
            count: IN_FLIGHT * 2,
        },
        renderer::ResourceClass::Image {
            desc: hdr(1, 1),
            count: IN_FLIGHT * 2,
        },
    ];
    runtime
        .prepare_working_set(
            renderer::FrameCapacity {
                graph: capacity(),
                work_slots: IN_FLIGHT,
                result_slots: IN_FLIGHT * 2,
                metadata_slots: 16,
                range_snapshots: 64,
                initialized_ranges_per_resource: 16,
                provenance_segments_per_resource: 16,
            },
            &classes,
        )
        .unwrap();
    runtime.prepare_raster_programs(&programs()).unwrap();
    let mut pool = GraphPool::new(capacity(), GRAPH_SLOTS).unwrap();
    frames(&mut runtime, &mut pool, WARMUP);
    let startup = Snapshot::now().since(start);
    let measured = frames(&mut runtime, &mut pool, FRAMES);
    assert_eq!(pool.leased_slots(), 0);
    let before_shutdown = Snapshot::now();
    drop((runtime, pool));
    let shutdown = Snapshot::now().since(before_shutdown);
    controls(startup, measured.total, shutdown);
    assert_ne!(
        startup.vma_host[renderer::measurement::HostEvent::Allocate as usize].calls,
        0
    );
    assert_ne!(
        shutdown.vma_host[renderer::measurement::HostEvent::Deallocate as usize].calls,
        0
    );
    let validation = monitor.report();
    assert!(validation.is_clean(), "{validation:#?}");
    if cfg!(feature = "development-validation") {
        assert_eq!(validation.core_validation, Some(true));
        assert_eq!(validation.synchronization_validation, Some(true));
    } else {
        assert_eq!(validation.requested, renderer::ValidationMode::Disabled);
    }
    report::<renderer::VulkanRuntime>(
        "vulkan_owned_raster_full_lifecycle",
        baseline,
        startup,
        shutdown,
        measured,
        serde_json::json!({ "name": info.name, "vendor_id": info.vendor_id, "device_id": info.device_id, "device_luid": info.device_luid, "driver_version_raw": info.driver_version_raw, "driver_name": info.driver_name, "driver_info": info.driver_info, "api_version": info.api_version }),
        serde_json::json!({ "requested": format!("{:?}", validation.requested), "core_validation": validation.core_validation, "synchronization_validation": validation.synchronization_validation, "warnings": validation.warnings, "errors": validation.errors, "callback_failures": validation.callback_failures, "dropped_problems": validation.dropped_problems }),
    );
}

#[cfg(all(feature = "d3d12", windows))]
impl Backend for renderer::D3D12Runtime {
    const RECORDING_CANCEL: bool = true;
    type Pending = renderer::D3D12Submission;
    type Completed = renderer::D3D12SubmissionResult;
    fn cancel_recording(&mut self, graph: &CompiledGraph) {
        drop(self.prepare(graph).unwrap());
    }
    fn submit(&mut self, graph: &CompiledGraph) -> Self::Pending {
        self.submit(graph).unwrap()
    }
    fn complete(pending: &mut Self::Pending) -> Self::Completed {
        pending.wait(WAIT).unwrap().unwrap()
    }
    fn readback(result: &mut Self::Completed, pass: PassHandle, output: &mut [u8]) {
        assert_eq!(result.readback_len(pass).unwrap(), output.len());
        result.readback_into(pass, output).unwrap();
    }
    fn collect(&mut self) {
        assert_eq!(self.poll().unwrap(), 0);
    }
    fn native_capacity() -> serde_json::Value {
        serde_json::json!({
            "executions": IN_FLIGHT, "results": IN_FLIGHT * 2,
            "resources": 16, "operations_and_draws": 32, "provenance_segments_per_resource": 16,
            "buffers": 4, "buffer_bytes": 512,
            "upload_bytes": 16384, "readback_bytes": 16384, "scratch_bytes": 512,
            "image_classes_per_execution": [
                { "kind": "rgba8", "extent": [WIDTH, HEIGHT], "count": 1 },
                { "kind": "rgba16f", "extent": [WIDTH, HEIGHT], "count": 1 },
                { "kind": "d32", "extent": [WIDTH, HEIGHT], "count": 1 },
                { "kind": "rgba8", "extent": [1, 1], "count": 2 },
                { "kind": "rgba16f", "extent": [1, 1], "count": 2 },
            ],
        })
    }
}

#[cfg(all(feature = "d3d12", windows))]
pub fn d3d12(baseline: bool) {
    if cfg!(debug_assertions) {
        panic!("memory acceptance requires --release");
    }
    let start = Snapshot::now();
    let mut config = renderer::D3D12RuntimeConfig::default();
    config.capacity.executions = IN_FLIGHT;
    config.capacity.results = IN_FLIGHT * 2;
    config.capacity.resources = 16;
    config.capacity.operations = 32;
    config.capacity.provenance_segments_per_resource = 16;
    config.capacity.buffers = 4;
    config.capacity.buffer_bytes = 512;
    config.capacity.upload_bytes = 16 * 1024;
    config.capacity.readback_bytes = 16 * 1024;
    config.capacity.scratch_bytes = 512;
    let mut runtime = renderer::D3D12Runtime::new(config).unwrap();
    let info = runtime.device_info();
    let monitor = runtime.validation_monitor();
    runtime
        .prepare_graph_working_set(
            &[
                renderer::D3D12ImageClass {
                    desc: ImageDesc::rgba8(WIDTH, HEIGHT),
                    count: 1,
                },
                renderer::D3D12ImageClass {
                    desc: hdr(WIDTH, HEIGHT),
                    count: 1,
                },
                renderer::D3D12ImageClass {
                    desc: ImageDesc::depth32(WIDTH, HEIGHT),
                    count: 1,
                },
                renderer::D3D12ImageClass {
                    desc: ImageDesc::rgba8(1, 1),
                    count: 2,
                },
                renderer::D3D12ImageClass {
                    desc: hdr(1, 1),
                    count: 2,
                },
            ],
            &programs(),
            &[],
        )
        .unwrap();
    let mut pool = GraphPool::new(capacity(), GRAPH_SLOTS).unwrap();
    frames(&mut runtime, &mut pool, WARMUP);
    let startup = Snapshot::now().since(start);
    let measured = frames(&mut runtime, &mut pool, FRAMES);
    assert_eq!(pool.leased_slots(), 0);
    let before_shutdown = Snapshot::now();
    runtime.shutdown().unwrap();
    drop(pool);
    let shutdown = Snapshot::now().since(before_shutdown);
    controls(startup, measured.total, shutdown);
    let validation = monitor.snapshot();
    assert!(validation.is_clean(), "{validation:#?}");
    if cfg!(feature = "development-validation") {
        assert!(validation.teardown_checked);
    }
    assert_eq!(
        validation.debug_layer_enabled,
        cfg!(feature = "development-validation")
    );
    assert_eq!(
        validation.gpu_based_validation_enabled,
        cfg!(feature = "development-validation")
    );
    report::<renderer::D3D12Runtime>(
        "d3d12_owned_raster_full_lifecycle",
        baseline,
        startup,
        shutdown,
        measured,
        serde_json::json!({ "name": info.name, "vendor_id": info.vendor_id, "device_id": info.device_id, "luid": info.luid, "driver_version": info.driver_version, "shader_model": info.shader_model }),
        serde_json::json!({ "debug_layer_enabled": validation.debug_layer_enabled, "gpu_based_validation_enabled": validation.gpu_based_validation_enabled, "teardown_checked": validation.teardown_checked, "warnings": validation.warnings, "errors": validation.errors, "corruption": validation.corruption, "callback_failures": validation.callback_failures, "dropped_messages": validation.dropped_messages }),
    );
}
