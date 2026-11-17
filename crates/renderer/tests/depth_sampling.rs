//! Alpha-discard depth-only shaders and mixed image/sampler policies use the
//! same public Graph and immutable Program contracts on both native backends.

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

use renderer::ImageFormat;
use renderer::graph::{
    Attachment, Binding, BindingResource, BufferDesc, CompiledGraph, GraphCapacity, GraphPool,
    ImageDesc, LoadOp, PassHandle, RasterAttachments, RasterConstants, RasterDraw, StoreOp,
};
use renderer::program::{
    BindingLayout, BindingType, BlendState, CompareOp, DepthState, ManagedRasterProgram,
    RasterParameterLayout, RasterProgramDescriptor, SampledImageSamplerPair, SamplerDescriptor,
    ShaderStages, VertexAttribute, VertexFormat,
};
use renderer::{
    D3D12BufferClass, D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig,
    D3D12ValidationMode, FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode,
    VulkanRuntime,
};
use serde_json::json;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(30);
const IMAGE: BindingLayout = BindingLayout {
    group: 0,
    binding: 0,
    kind: BindingType::SampledImage,
    stages: ShaderStages::FRAGMENT,
    min_size: 0,
};
const SAMPLER: BindingLayout = BindingLayout {
    binding: 1,
    kind: BindingType::Sampler,
    ..IMAGE
};
const COLOR_PAIR: SampledImageSamplerPair = SampledImageSamplerPair {
    image_group: 0,
    image_binding: 0,
    sampler_group: 0,
    sampler_binding: 1,
};
static MASK: RasterProgramDescriptor = RasterProgramDescriptor {
    entry: "main",
    vertex_stride: 8,
    vertex_attributes: &[VertexAttribute {
        location: 0,
        offset: 0,
        format: VertexFormat::Float32x2,
    }],
    parameters: RasterParameterLayout {
        size: 0,
        stages: ShaderStages::EMPTY,
        dxil_register: 0,
        dxil_space: 0,
    },
    color_outputs: &[],
    bindings: &[IMAGE, SAMPLER],
    binding_bytes: &[0, 0],
    sampling_pairs: &[COLOR_PAIR],
    blend: BlendState::Replace,
    depth: DepthState::Required {
        compare: CompareOp::Less,
        write: true,
        require_load: false,
    },
    vertex_spirv: include_bytes!("support/shaders/depth_sampling.vert.spv"),
    fragment_spirv: include_bytes!("support/shaders/depth_sampling_mask.frag.spv"),
    vertex_dxil: include_bytes!("support/shaders/dxil/depth_sampling.vert.dxil"),
    fragment_dxil: include_bytes!("support/shaders/dxil/depth_sampling_mask.frag.dxil"),
};
static SAMPLE: RasterProgramDescriptor = RasterProgramDescriptor {
    parameters: RasterParameterLayout {
        size: 16,
        stages: ShaderStages::FRAGMENT,
        dxil_register: 7,
        dxil_space: 0,
    },
    color_outputs: &[ImageFormat::Rgba16Float],
    bindings: &[
        IMAGE,
        SAMPLER,
        BindingLayout {
            binding: 2,
            ..IMAGE
        },
        BindingLayout {
            binding: 3,
            ..SAMPLER
        },
    ],
    binding_bytes: &[0, 0, 0, 0],
    sampling_pairs: &[
        COLOR_PAIR,
        SampledImageSamplerPair {
            image_binding: 2,
            sampler_binding: 3,
            ..COLOR_PAIR
        },
    ],
    depth: DepthState::Disabled,
    fragment_spirv: include_bytes!("support/shaders/depth_sampling_color.frag.spv"),
    fragment_dxil: include_bytes!("support/shaders/dxil/depth_sampling_color.frag.dxil"),
    ..MASK
};

fn programs() -> [ManagedRasterProgram; 2] {
    [&MASK, &SAMPLE].map(|descriptor| {
        // SAFETY: Pinned shader binaries/reflection implement these exact 2D
        // image/sampler and vertex ABIs. MASK only samples alpha and discards;
        // SAMPLE reads precisely the declared color-linear/depth-nearest pairs.
        // LOD and normalized coordinates are safe for every accepted View.
        // Neither shader writes depth, storage, or any undeclared output.
        unsafe { ManagedRasterProgram::from_trusted_descriptor(descriptor) }
    })
}

fn source_desc() -> ImageDesc {
    ImageDesc {
        mip_levels: 2,
        ..ImageDesc::rgba8(2, 2)
    }
}
fn output_desc() -> ImageDesc {
    ImageDesc::rgba16_float(4, 4)
}
fn capacity() -> GraphCapacity {
    GraphCapacity {
        resources: 8,
        passes: 16,
        accesses: 48,
        draws: 4,
        bindings: 16,
        attachments: 4,
        dependencies: 32,
        initial_ranges: 32,
        name_bytes: 1024,
        upload_bytes: 128,
        edges: 128,
        segments: 64,
        readers: 128,
    }
}

struct Capture {
    pass: PassHandle,
    name: &'static str,
    bytes: Vec<u8>,
}
fn half_eighths(value: u16) -> u16 {
    [
        0, 0x3000, 0x3400, 0x3600, 0x3800, 0x3900, 0x3a00, 0x3b00, 0x3c00,
    ][usize::from(value)]
}
fn expected_color(repeat_u: bool, mip_blend: bool) -> Vec<u8> {
    let r = if repeat_u { [6, 2, 6, 2] } else { [0, 2, 8, 8] };
    let g = [0, 2, 8, 8];
    let mut bytes = Vec::with_capacity(128);
    for g in g {
        for (x, r) in r.into_iter().enumerate() {
            let r = if mip_blend { (r + 8) / 2 } else { r };
            let g = if mip_blend { g / 2 } else { g };
            let depth = if x < 2 { 8 } else { 2 };
            bytes.extend(
                [r, g, depth, 8]
                    .map(half_eighths)
                    .into_iter()
                    .flat_map(u16::to_le_bytes),
            );
        }
    }
    bytes
}
fn build() -> (GraphPool, CompiledGraph, Vec<Capture>) {
    let mut pool = GraphPool::new(capacity(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let vertices = graph
        .buffer("quad vertices", BufferDesc { size: 32 })
        .unwrap();
    let indices = graph
        .buffer("quad indices", BufferDesc { size: 24 })
        .unwrap();
    let vertex_bytes: Vec<_> = [-1_f32, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    let index_bytes: Vec<_> = [0_u32, 1, 2, 2, 1, 3]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    graph
        .upload_buffer("upload vertices", vertices, 0, &vertex_bytes)
        .unwrap();
    graph
        .upload_buffer("upload indices", indices, 0, &index_bytes)
        .unwrap();
    let source = graph.image("color and alpha", source_desc()).unwrap();
    graph
        .upload_image(
            "upload all source mips",
            source,
            &[
                0, 0, 0, 0, 255, 0, 0, 255, 0, 255, 0, 0, 255, 255, 0, 255, 255, 0, 255, 255,
            ],
        )
        .unwrap();
    let depth = graph
        .image("masked depth", ImageDesc::depth32(4, 4))
        .unwrap();
    let [mask, sample] = programs();
    let source_view = graph.sampled_image_view(source, 0, 2).unwrap();
    let mask_bindings = graph
        .raster_bindings(
            &mask,
            &[
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::ImageView(source_view),
                },
                Binding {
                    group: 0,
                    binding: 1,
                    resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                },
            ],
        )
        .unwrap();
    let draw = RasterDraw {
        vertices,
        indices,
        index_count: 6,
        bindings: Some(mask_bindings),
        parameters: RasterConstants::default(),
    };
    graph
        .raster(
            "alpha discard with no color outputs",
            &mask,
            [draw],
            RasterAttachments {
                colors: &[],
                depth: Some(Attachment {
                    view: graph.image_view(depth, 0, 0).unwrap(),
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
            },
        )
        .unwrap();
    let depth_bytes: Vec<_> = (0..16)
        .flat_map(|pixel| if pixel % 4 < 2 { 1_f32 } else { 0.25_f32 }.to_le_bytes())
        .collect();
    let mut captures = vec![Capture {
        pass: graph.readback_image("read masked depth", depth).unwrap(),
        name: "alpha holes",
        bytes: depth_bytes,
    }];
    for (policy, lod, name, repeat_u, mip_blend) in [
        (
            SamplerDescriptor::LinearClamp,
            0_f32,
            "linear clamp",
            false,
            false,
        ),
        (
            SamplerDescriptor::LinearRepeatUClampV,
            0_f32,
            "repeat U clamp V",
            true,
            false,
        ),
        (
            SamplerDescriptor::LinearRepeatUClampV,
            0.5_f32,
            "fractional mip",
            true,
            true,
        ),
    ] {
        let output = graph.image(name, output_desc()).unwrap();
        let depth_view = graph.sampled_image_view(depth, 0, 1).unwrap();
        let bindings = graph
            .raster_bindings(
                &sample,
                &[
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::ImageView(source_view),
                    },
                    Binding {
                        group: 0,
                        binding: 1,
                        resource: BindingResource::Sampler(policy),
                    },
                    Binding {
                        group: 0,
                        binding: 2,
                        resource: BindingResource::ImageView(depth_view),
                    },
                    Binding {
                        group: 0,
                        binding: 3,
                        resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                    },
                ],
            )
            .unwrap();
        let constants = [lod.to_bits(), 0, 0, 0];
        graph
            .raster(
                name,
                &sample,
                [RasterDraw {
                    bindings: Some(bindings),
                    parameters: RasterConstants::new(&constants).unwrap(),
                    ..draw
                }],
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(output, 0, 0).unwrap(),
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    depth: None,
                },
            )
            .unwrap();
        captures.push(Capture {
            pass: graph.readback_image(name, output).unwrap(),
            name,
            bytes: expected_color(repeat_u, mip_blend),
        });
    }
    (pool, graph.compile().unwrap(), captures)
}

fn read(captures: &[Capture], mut read: impl FnMut(PassHandle, &mut [u8])) -> Vec<Vec<u8>> {
    captures
        .iter()
        .map(|capture| {
            let mut bytes = vec![0; capture.bytes.len()];
            read(capture.pass, &mut bytes);
            bytes
        })
        .collect()
}

fn check(captures: &[Capture], actual: &[Vec<u8>], backend: &str) {
    for (capture, actual) in captures.iter().zip(actual) {
        if capture.name == "alpha holes" {
            assert_eq!(actual, &capture.bytes, "{backend}: {}", capture.name);
            continue;
        }
        for (index, (actual, expected)) in actual
            .as_chunks::<2>()
            .0
            .iter()
            .zip(capture.bytes.as_chunks::<2>().0.iter())
            .enumerate()
        {
            let actual = u16::from_le_bytes(*actual);
            let expected = u16::from_le_bytes(*expected);
            // Native texture filtering has implementation precision limits:
            // https://docs.vulkan.org/spec/latest/chapters/textures.html#textures-texel-filtering
            // This fixed fixture's <=1 half-ULP bound on [0,1] is stricter than
            // tools/bistro/compare_runtime.py's established 1/1024 absolute
            // fixture bound. It is a target-hardware regression threshold, not
            // an API-wide filtering precision guarantee. The pilot observed
            // 0.75 stored as 0x39ff; its internal source is not established.
            // Allow one half-precision ULP only in interpolated R/G.
            // Axis-clamped endpoints, depth and alpha remain exact. Values are
            // all nonnegative finite, so ordered half bits give the ULP delta.
            let limit = u16::from(index % 4 < 2 && expected != 0 && expected != 0x3c00);
            assert!(
                actual.abs_diff(expected) <= limit,
                "{backend}: {} component {index}: actual half {actual:#06x}, expected {expected:#06x}, ULP limit {limit}",
                capture.name
            );
        }
    }
}

fn check_cross_api(captures: &[Capture], vk: &[Vec<u8>], dx: &[Vec<u8>]) {
    for ((capture, vk), dx) in captures.iter().zip(vk).zip(dx) {
        if capture.name == "alpha holes" {
            assert_eq!(vk, dx, "Vulkan/D3D12 depth must be bit exact");
            continue;
        }
        for (index, ((vk, dx), expected)) in vk
            .as_chunks::<2>()
            .0
            .iter()
            .zip(dx.as_chunks::<2>().0.iter())
            .zip(capture.bytes.as_chunks::<2>().0.iter())
            .enumerate()
        {
            let vk = u16::from_le_bytes(*vk);
            let dx = u16::from_le_bytes(*dx);
            let expected = u16::from_le_bytes(*expected);
            let limit = if index % 4 < 2 && expected != 0 && expected != 0x3c00 {
                2
            } else {
                0
            };
            assert!(
                vk.abs_diff(dx) <= limit,
                "Vulkan/D3D12: {} component {index}: {vk:#06x}/{dx:#06x}, ULP limit {limit}",
                capture.name
            );
        }
    }
}

fn run(strict: bool) {
    let mut vk = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 1,
            resource_capacity: 8,
            max_shader_bindings: 4,
            staging_bytes: 8192,
            staging_ranges: 16,
        },
        if strict {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )
    .expect("Vulkan hardware/validation prerequisites");
    let mut dx = D3D12Runtime::new(D3D12RuntimeConfig {
        validation: if strict {
            D3D12ValidationMode::Required
        } else {
            D3D12ValidationMode::Disabled
        },
        capacity: D3D12FrameCapacity {
            executions: 1,
            results: 1,
            resources: 8,
            resource_capacity: 8,
            operations: 16,
            buffers: 2,
            buffer_bytes: 32,
            upload_bytes: 8192,
            readback_bytes: 8192,
            scratch_bytes: 8192,
            ..Default::default()
        },
    })
    .expect("D3D12 hardware/validation prerequisites");
    let vk_info = vk.device_info();
    let dx_info = dx.device_info();
    assert_ne!(vk_info.device_type, "CPU", "hardware Vulkan required");
    assert_eq!(
        vk_info.device_luid,
        Some(dx_info.luid),
        "same hardware adapter required"
    );
    let vk_monitor = vk.validation_monitor();
    let dx_monitor = dx.validation_monitor();
    let images = [
        (source_desc(), 1),
        (ImageDesc::depth32(4, 4), 1),
        (output_desc(), 3),
    ];
    let mut resources = vec![
        ResourceClass::Buffer {
            desc: BufferDesc { size: 32 },
            count: 1,
        },
        ResourceClass::Buffer {
            desc: BufferDesc { size: 24 },
            count: 1,
        },
    ];
    resources.extend(images.map(|(desc, count)| ResourceClass::Image { desc, count }));
    let native_capacity = FrameCapacity {
        graph: GraphCapacity {
            bindings: 1,
            ..capacity()
        },
        work_slots: 1,
        result_slots: 1,
        metadata_slots: 1,
        ..Default::default()
    };
    vk.prepare_working_set(native_capacity, &resources).unwrap();
    vk.prepare_raster_programs(&programs()).unwrap();
    dx.prepare_graph_working_set(
        Some(&[
            D3D12BufferClass { size: 32, count: 1 },
            D3D12BufferClass { size: 24, count: 1 },
        ]),
        &images.map(|(desc, count)| D3D12ImageClass { desc, count }),
        &programs(),
        &[],
    )
    .unwrap();
    let (pool, plan, captures) = build();
    let error = vk
        .submit(&plan)
        .err()
        .expect("undersized binding metadata must fail");
    assert_eq!(error.kind(), renderer::RuntimeErrorKind::CapacityExhausted);
    let metadata = error.capacity().expect("typed binding metadata capacity");
    assert_eq!(
        metadata.resource,
        renderer::capacity::RuntimeCapacityKind::GraphMetadata
    );
    assert_eq!(
        metadata.recovery,
        renderer::capacity::CapacityRecovery::Reconfigure
    );
    assert_eq!(metadata.limit, None);
    assert_eq!(metadata.required, None);
    vk.prepare_working_set(
        FrameCapacity {
            graph: capacity(),
            ..native_capacity
        },
        &resources,
    )
    .unwrap();
    let mut vk_pixels = Vec::with_capacity(2);
    let mut dx_pixels = Vec::with_capacity(2);
    for _ in 0..2 {
        let mut pending = vk.submit(&plan).unwrap();
        let mut result = pending.wait(WAIT).unwrap().expect("Vulkan completion");
        vk_pixels.push(read(&captures, |pass, bytes| {
            result.readback_into(pass, bytes).unwrap()
        }));
        drop((result, pending));
        let mut pending = dx.submit(&plan).unwrap();
        let mut result = pending.wait(WAIT).unwrap().expect("D3D12 completion");
        dx_pixels.push(read(&captures, |pass, bytes| {
            result.readback_into(pass, bytes).unwrap()
        }));
        drop((result, pending));
    }
    drop((plan, pool, vk));
    dx.shutdown().unwrap();
    let vk_report = vk_monitor.report();
    let dx_report = dx_monitor.snapshot();
    let cross_api_max_half_ulp = vk_pixels[0]
        .iter()
        .zip(&dx_pixels[0])
        .skip(1)
        .flat_map(|(vk, dx)| {
            vk.as_chunks::<2>()
                .0
                .iter()
                .zip(dx.as_chunks::<2>().0.iter())
        })
        .map(|(vk, dx)| u16::from_le_bytes(*vk).abs_diff(u16::from_le_bytes(*dx)))
        .max()
        .unwrap_or(0);
    let report = json!({
        "scope":"depth-discard-and-mixed-sampling", "strict":strict,
        "same_graph":true,"native_runs_sequential":true,"runs_per_api":2,
        "configuration":{"development_validation":cfg!(feature="development-validation"),
            "vulkan_diagnostics":cfg!(feature="vulkan-diagnostics"),
            "d3d12_diagnostics":cfg!(feature="d3d12-diagnostics"),
            "memory_profiling":cfg!(feature="memory-profiling"),
            "debug_assertions":cfg!(debug_assertions),"external_layer_injection_checked":false},
        "binding_capacity_rejection_and_recovery":true,
        "cases":["alpha holes", "linear clamp", "repeat U clamp V", "fractional mip"],
        "compared_bytes_per_run":448,
        "interpolated_rg_half_ulp_tolerance":1,"depth_alpha_and_clamp_endpoints_bit_exact":true,
        "cross_api_max_half_ulp":cross_api_max_half_ulp,
        "cross_api_content_checked":true,
        "cross_api_bit_exact":vk_pixels[0] == dx_pixels[0],
        "adapter":{"name":vk_info.name,"luid":dx_info.luid},
        "vulkan":{"warnings":vk_report.warnings,"errors":vk_report.errors,
            "core":vk_report.core_validation,"sync":vk_report.synchronization_validation,
            "callback_failures":vk_report.callback_failures,"problems":vk_report.problems},
        "d3d12":{"warnings":dx_report.warnings,"errors":dx_report.errors,
            "corruption":dx_report.corruption,"callback_failures":dx_report.callback_failures,
            "teardown_checked":dx_report.teardown_checked,
            "retained_message_count":dx_report.messages.len(),
            "dropped_messages":dx_report.dropped_messages,
            "problems":format!("{:?}",dx_report.messages.iter().filter(|message| message.severity <= 2).collect::<Vec<_>>())},
    });
    eprintln!("DEPTH_SAMPLING_REPORT={report}");
    assert!(vk_report.is_clean(), "{report}");
    assert!(dx_report.is_clean(), "{report}");
    check_cross_api(&captures, &vk_pixels[0], &dx_pixels[0]);
    assert_eq!(
        vk_pixels[0], vk_pixels[1],
        "Vulkan repeat must be bit exact"
    );
    assert_eq!(dx_pixels[0], dx_pixels[1], "D3D12 repeat must be bit exact");
    for pixels in &vk_pixels {
        check(&captures, pixels, "Vulkan");
    }
    for pixels in &dx_pixels {
        check(&captures, pixels, "D3D12");
    }
    if strict {
        assert_eq!(vk_report.core_validation, Some(true), "{report}");
        assert_eq!(vk_report.synchronization_validation, Some(true), "{report}");
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
#[ignore = "requires same-adapter Vulkan Core/SyncVal and D3D12 Debug Layer/GBV hardware"]
fn depth_discard_and_mixed_sampling_strict() {
    run(true);
}

#[cfg(not(any(
    feature = "development-validation",
    feature = "d3d12-diagnostics",
    feature = "vulkan-diagnostics",
    feature = "graph-diagnostics"
)))]
#[test]
#[ignore = "requires same-adapter Vulkan/D3D12 hardware; optimized diagnostics-excluded build"]
fn depth_discard_and_mixed_sampling_shipping() {
    if cfg!(debug_assertions) {
        panic!("shipping acceptance requires --release");
    }
    run(false);
}
