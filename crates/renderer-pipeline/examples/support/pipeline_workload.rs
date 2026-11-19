use std::{error::Error, fs, path::Path, time::Duration};

use renderer::{
    D3D12ValidationMode, ValidationMode,
    graph::{
        BindingResource, BufferDesc, Graph, GraphError, GraphPool, ImageDesc, RasterDraw,
        RasterParameters,
    },
    runtime::{
        D3D12Config, ResourceClass, Runtime, RuntimeConfig, RuntimeLimits, VulkanConfig, WorkingSet,
    },
};
use renderer_pipeline::{
    DrawCandidate, FrameInputs, FrameMaterial, FrameResource, FrameView, MaterialBinding,
    PrepareOptions, PreparedPipeline,
};

use super::pipeline_shaders::{self, Backend};

#[cfg(feature = "memory-profiling")]
use renderer::measurement::{CountingAllocator, Snapshot};

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: CountingAllocator<std::alloc::System> =
    CountingAllocator::new(std::alloc::System);

const WAIT: Duration = Duration::from_secs(30);
const WIDTH: u32 = 9;
const HEIGHT: u32 = 7;
const IMAGE_BYTES: usize = WIDTH as usize * HEIGHT as usize * 4;
const FRAME_VARIANTS: usize = 3;
const WARMUP: usize = 6;
const MEASURED_FRAMES: usize = 24;

const VERTICES: BufferDesc = BufferDesc { size: 112 };
const INDICES: BufferDesc = BufferDesc { size: 24 };
const OFFSET: BufferDesc = BufferDesc { size: 16 };
const MATERIAL_TYPES: [&str; 3] = ["replace", "additive", "replace"];
const MATERIAL_COLORS: [[u8; 4]; 3] = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]];

fn target_desc() -> ImageDesc {
    ImageDesc::rgba8(WIDTH, HEIGHT)
}
const COLORS: [[u8; 4]; FRAME_VARIANTS] = [[0, 0, 0, 255], [0, 255, 0, 255], [255, 0, 0, 255]];

#[derive(Clone, Copy, Debug)]
enum Route {
    DepthPrepass,
    Invert,
    SwapRedBlue,
    MaterialTemplate,
}

impl Route {
    fn expected_color(self, variant: usize) -> [u8; 4] {
        let [r, g, b, a] = COLORS[variant];
        match self {
            Self::DepthPrepass => [r, g, b, a],
            Self::Invert => [255 - r, 255 - g, 255 - b, a],
            Self::SwapRedBlue => [b, g, r, a],
            // Replace red, add green, then replace with blue. Regrouping the
            // draws by type would put additive last and end with cyan.
            Self::MaterialTemplate => {
                [[255, 0, 0, 255], [255, 255, 0, 255], [0, 0, 255, 255]][variant]
            }
        }
    }
}

type Pixels = [u8; IMAGE_BYTES];
type Frames = [Pixels; FRAME_VARIANTS];
const ROUTES: [Route; 4] = [
    Route::DepthPrepass,
    Route::Invert,
    Route::SwapRedBlue,
    Route::MaterialTemplate,
];
type Output = [Frames; ROUTES.len()];

pub(super) fn run() -> Result<(), Box<dyn Error>> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/pipelines");
    let depth_prepass = fs::read_to_string(directory.join("depth_prepass.yaml"))?;
    let invert = fs::read_to_string(directory.join("compute_post.yaml"))?;
    let swap_red_blue = invert.replace("post/invert", "post/swap-red-blue");
    let material = fs::read_to_string(directory.join("material_template.yaml"))?;
    let definitions = [
        depth_prepass.as_str(),
        invert.as_str(),
        swap_red_blue.as_str(),
        material.as_str(),
    ];
    let (vk_luid, vk_output) = run_backend(Backend::Vulkan, definitions)?;
    let (dx_luid, dx_output) = run_backend(Backend::D3D12, definitions)?;
    assert_eq!(
        vk_luid, dx_luid,
        "use the same physical adapter for both backends"
    );
    assert_eq!(vk_output, dx_output, "different backend output");
    Ok(())
}

fn run_backend(
    backend: Backend,
    definitions: [&str; ROUTES.len()],
) -> Result<([u8; 8], Output), Box<dyn Error>> {
    let strict = cfg!(feature = "development-validation");
    let config = match backend {
        Backend::Vulkan => RuntimeConfig::Vulkan(VulkanConfig {
            validation: if strict {
                ValidationMode::Required
            } else {
                ValidationMode::Disabled
            },
            ..Default::default()
        }),
        Backend::D3D12 => RuntimeConfig::D3D12(D3D12Config {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            ..Default::default()
        }),
    };
    let limits = RuntimeLimits {
        frames: 1,
        results: 1,
        ..Default::default()
    };
    let prepare = |yaml: &str| {
        PreparedPipeline::from_yaml(
            yaml,
            PrepareOptions {
                frame_extent: None,
                frames_in_flight: limits.frames,
                max_draws_per_list: 3,
                max_materials: 3,
            },
            |shader| pipeline_shaders::resolve(backend, shader),
        )
    };
    let mut pipeline = prepare(definitions[0])?;
    let mut runtime = with_app_resources(&pipeline, |working_set| {
        Runtime::new(config, limits, working_set)
    })?;
    let mut pool = GraphPool::new(limits.graph, limits.frames)?;
    let mut outputs = [[[0; IMAGE_BYTES]; FRAME_VARIANTS]; ROUTES.len()];
    for (index, (route, definition)) in ROUTES.into_iter().zip(definitions).enumerate() {
        if index > 0 {
            let replacement = prepare(definition)?;
            with_app_resources(&replacement, |working_set| {
                runtime.prepare_working_set(working_set)
            })?;
            pipeline = replacement;
        }
        for warmup in 0..WARMUP {
            run_frame(&mut pipeline, &mut runtime, &mut pool, warmup, route)?;
        }
        #[cfg(feature = "memory-profiling")]
        let before = Snapshot::now();
        for frame in 0..MEASURED_FRAMES {
            outputs[index][frame % FRAME_VARIANTS] =
                run_frame(&mut pipeline, &mut runtime, &mut pool, frame, route)?;
        }
        #[cfg(feature = "memory-profiling")]
        {
            let delta = Snapshot::now().since(before);
            assert!(
                delta.is_allocation_free(),
                "{backend:?} {route:?}: {delta:?}"
            );
            println!("{backend:?} {route:?}: {MEASURED_FRAMES} frames, {delta:?}");
        }
        #[cfg(not(feature = "memory-profiling"))]
        println!("{backend:?} {route:?}: {MEASURED_FRAMES} frames matched");
    }
    let id = match backend {
        Backend::Vulkan => {
            let device = runtime.as_vulkan().expect("selected Vulkan");
            let info = device.device_info();
            assert_ne!(info.device_type, "CPU");
            println!("Vulkan adapter: {}", info.name);
            let id = info
                .device_luid
                .ok_or("Vulkan adapter has no Windows LUID")?;
            let monitor = device.validation_monitor();
            drop(runtime);
            let report = monitor.report();
            assert!(report.is_clean(), "{report:?}");
            if strict {
                assert_eq!(report.core_validation, Some(true));
                assert_eq!(report.synchronization_validation, Some(true));
            } else {
                assert_eq!(report.requested, ValidationMode::Disabled);
            }
            id
        }
        Backend::D3D12 => {
            let device = runtime.as_d3d12().expect("selected D3D12");
            let info = device.device_info();
            println!("D3D12 adapter: {}", info.name);
            let id = info.luid;
            let monitor = device.validation_monitor();
            drop(runtime);
            let report = monitor.snapshot();
            assert!(report.is_clean(), "{report:?}");
            assert_eq!(report.debug_layer_enabled, strict);
            assert_eq!(report.gpu_based_validation_enabled, strict);
            if strict {
                assert!(report.teardown_checked);
            }
            id
        }
    };
    Ok((id, outputs))
}

fn with_app_resources<T>(
    pipeline: &PreparedPipeline,
    prepare: impl FnOnce(WorkingSet<'_>) -> T,
) -> T {
    let pipeline_set = pipeline.working_set();
    let mut resources = pipeline_set.resources.to_vec();
    resources.extend([
        ResourceClass::Buffer {
            desc: VERTICES,
            count: 1,
        },
        ResourceClass::Buffer {
            desc: INDICES,
            count: 1,
        },
        ResourceClass::Buffer {
            desc: OFFSET,
            count: 1,
        },
        ResourceClass::Image {
            desc: target_desc(),
            count: 1,
        },
        ResourceClass::Image {
            desc: ImageDesc::rgba8(1, 1),
            count: MATERIAL_TYPES.len(),
        },
    ]);
    prepare(WorkingSet {
        resources: &resources,
        ..pipeline_set
    })
}

fn run_frame(
    pipeline: &mut PreparedPipeline,
    runtime: &mut Runtime,
    pool: &mut GraphPool,
    frame: usize,
    route: Route,
) -> Result<Pixels, Box<dyn Error>> {
    let mut graph = pool.begin()?;
    let candidates = geometry(&mut graph)?;
    let (offset, bindings) = material_resources(&mut graph)?;
    let materials: [_; MATERIAL_TYPES.len()] = std::array::from_fn(|index| FrameMaterial {
        material_type: MATERIAL_TYPES[index],
        bindings: std::slice::from_ref(&bindings[index]),
    });
    let target = graph.image("application target", target_desc())?;
    let variant = frame % FRAME_VARIANTS;
    let target_resource = FrameResource {
        name: "final",
        resource: target.into(),
    };
    let view = FrameView {
        name: "main",
        candidates: &candidates[..(variant + 1) * 2],
    };
    let (mut graph, output) = pipeline.build(
        graph,
        FrameInputs {
            resources: &[target_resource, offset],
            views: &[view],
            materials: &materials,
        },
    )?;
    let output = output.image();
    assert_eq!(output, target);
    let readback = graph.readback_image("application output readback", output)?;
    let plan = graph.compile()?;
    let passes = || plan.ordered_pass_names();
    assert!(passes().any(|pass| pass == "final_image"));
    assert_eq!(
        passes().any(|pass| pass == "depth_prepass"),
        matches!(route, Route::DepthPrepass),
        "{route:?}"
    );
    assert_eq!(
        passes().any(|pass| pass == "filter"),
        matches!(route, Route::Invert | Route::SwapRedBlue),
        "{route:?}"
    );

    let submission = runtime.submit(&plan)?;
    let id = submission.id();
    drop(submission);
    let mut completion = runtime.wait_for_frame(id, WAIT)?.ok_or("GPU timeout")?;
    let mut pixels = [0; IMAGE_BYTES];
    completion.readback_into(readback, &mut pixels)?;
    let expected = route.expected_color(variant);
    for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(*pixel, expected, "{route:?}, frame {frame}, pixel {index}");
    }
    drop(completion);
    drop(plan);
    assert!(pool.is_idle(), "result and graph slots returned");
    Ok(pixels)
}

fn geometry(graph: &mut Graph) -> Result<[DrawCandidate<'static>; 6], GraphError> {
    let vertices = graph.buffer("quad vertices", VERTICES)?;
    let indices = graph.buffer("quad indices", INDICES)?;
    let vertex_bytes = [[-1.0_f32, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]]
        .map(|[x, y]| [x, y, 0.0, 1.0, 1.0, 1.0, 1.0].map(f32::to_le_bytes));
    let index_bytes = [0_u32, 1, 2, 2, 1, 3].map(u32::to_le_bytes);
    graph.upload_buffer(
        "quad vertex bytes",
        vertices,
        0,
        vertex_bytes.as_flattened().as_flattened(),
    )?;
    graph.upload_buffer("quad index bytes", indices, 0, index_bytes.as_flattened())?;
    let candidate = |depth, tint, queue, material| {
        let mut transform = *RasterParameters::default().transform();
        transform[3][2] = depth;
        Ok::<_, GraphError>(DrawCandidate {
            queue,
            material,
            draw: RasterDraw {
                bindings: None,
                vertices,
                indices,
                first_index: 0,
                index_count: 6,
                instance_count: 1,
                parameters: RasterParameters::new(transform, tint)?.into(),
            },
        })
    };
    // Each frame adds one pair. The YAML chooses the queue to draw from the
    // same view, so the application's inputs stay identical across routes.
    Ok([
        candidate(0.1, [0.0, 0.0, 1.0, 1.0], "transparent", None)?,
        candidate(0.5, [1.0; 4], "layered", Some(0))?,
        candidate(0.75, [0.0, 1.0, 0.0, 1.0], "opaque", None)?,
        candidate(0.5, [1.0; 4], "layered", Some(1))?,
        candidate(0.25, [1.0, 0.0, 0.0, 1.0], "opaque", None)?,
        candidate(0.5, [1.0; 4], "layered", Some(2))?,
    ])
}

fn material_resources(
    graph: &mut Graph,
) -> Result<(FrameResource<'static>, [MaterialBinding<'static>; 3]), GraphError> {
    let offset = graph.buffer("material vertex offset", OFFSET)?;
    graph.upload_buffer("upload vertex offset", offset, 0, &[0; 16])?;
    let offset = FrameResource {
        name: "vertex_offset",
        resource: offset.into(),
    };
    let mut color_texture = |pixels: [u8; 4]| {
        let image = graph.image("material color", ImageDesc::rgba8(1, 1))?;
        graph.upload_image("upload material color", image, &pixels)?;
        Ok::<_, GraphError>(MaterialBinding {
            name: "color",
            resource: BindingResource::ImageView(graph.image_view(image, 0, 0)?),
        })
    };
    let [red, green, blue] = MATERIAL_COLORS;
    Ok((
        offset,
        [
            color_texture(red)?,
            color_texture(green)?,
            color_texture(blue)?,
        ],
    ))
}
