//! Real Win32 WSI acceptance and complete normal-frame ownership accounting.

use super::super::{
    FrameCapacity, FrameId, FrameSubmission, ResourceClass, RuntimeConfig, RuntimeError,
    RuntimeErrorKind, SubmissionResult, VulkanRuntime,
};
use super::measurement::{Measurements, Observations, Phase};
use super::{PresentOutcome, PresentationConfig, Presenter};
use crate::graph::{
    Attachment, BufferDesc, ComputeTransform, GraphCapacity, GraphPool, ImageDesc, ImportImage,
    LoadOp, PassHandle, RasterAttachments, RasterDraw, RasterParameters, ResourceKey, StoreOp,
};
use ash::vk;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::{pump_events::EventLoopExtPumpEvents, windows::EventLoopBuilderExtWindows},
    window::{Window, WindowId},
};

const WAIT: Duration = Duration::from_secs(10);
const IN_FLIGHT: usize = 3;
const NORMAL_SOURCES: usize = 18;
const GRAPH_SLOTS: usize = 12;
const PIXEL_CAPACITY: usize = 192 * 128 * 4;
const EXTENTS: [(u32, u32); 5] = [(128, 96), (160, 112), (176, 120), (144, 104), (192, 128)];

#[derive(Default)]
struct WindowHost {
    window: Option<Arc<Window>>,
    resized: Option<PhysicalSize<u32>>,
}

impl ApplicationHandler for WindowHost {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        if self.window.is_none() {
            self.window = Some(Arc::new(
                active
                    .create_window(
                        Window::default_attributes()
                            .with_title("Renderer Vulkan WSI acceptance")
                            .with_inner_size(PhysicalSize::new(128, 96)),
                    )
                    .expect("real Win32 window is required"),
            ));
        }
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if let WindowEvent::Resized(size) = event {
            self.resized = Some(size);
        }
    }
}

#[derive(Clone, Copy)]
struct PixelCheck {
    id: FrameId,
    pass: PassHandle,
    width: u32,
    height: u32,
    center: [u8; 4],
    corner: [u8; 4],
}

struct Source {
    // Metadata is retained until output registration is returned.
    _frame: FrameSubmission,
    image: ImportImage,
    check: PixelCheck,
    presented: usize,
}

struct HeldResult {
    check: PixelCheck,
    result: SubmissionResult,
}

fn graph_capacity() -> GraphCapacity {
    GraphCapacity {
        bindings: 0,
        attachments: 16,
        resources: 8,
        passes: 8,
        accesses: 32,
        draws: 4,
        dependencies: 16,
        initial_ranges: 16,
        name_bytes: 1024,
        upload_bytes: 256,
        edges: 64,
        segments: 64,
        readers: 128,
    }
}

fn frame_capacity() -> FrameCapacity {
    FrameCapacity {
        graph: graph_capacity(),
        work_slots: 6,
        result_slots: 8,
        metadata_slots: 12,
        range_snapshots: 256,
        initialized_ranges_per_resource: 16,
        provenance_segments_per_resource: 32,
    }
}

fn prepare(runtime: &mut VulkanRuntime) {
    // Three new graphs can coexist with outputs retained only by present.
    // Reserve every explicit resize extent before accepting a frame.
    let classes: [ResourceClass; 12] = std::array::from_fn(|index| match index {
        0 => ResourceClass::Buffer {
            desc: BufferDesc { size: 84 },
            count: 1,
        },
        1 => ResourceClass::Buffer {
            desc: BufferDesc { size: 12 },
            count: 1,
        },
        _ => {
            let (width, height) = EXTENTS[(index - 2) / 2];
            if (index - 2) % 2 == 0 {
                ResourceClass::Image {
                    desc: ImageDesc::rgba8(width, height),
                    count: 15,
                }
            } else {
                ResourceClass::Image {
                    desc: ImageDesc::depth32(width, height),
                    count: IN_FLIGHT,
                }
            }
        }
    });
    runtime
        .prepare_working_set(frame_capacity(), &classes)
        .unwrap();
}

fn mesh(runtime: &mut VulkanRuntime, pool: &mut GraphPool) -> (ResourceKey, ResourceKey) {
    let vertices = [
        [-0.75_f32, -0.75, 0.25, 1.0, 0.0, 0.0, 1.0],
        [0.75, -0.75, 0.25, 1.0, 0.0, 0.0, 1.0],
        [0.0, 0.75, 0.25, 1.0, 0.0, 0.0, 1.0],
    ]
    .into_iter()
    .flatten()
    .flat_map(f32::to_ne_bytes)
    .collect::<Vec<_>>();
    let indices = [0_u32, 1, 2]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect::<Vec<_>>();
    let mut graph = pool.begin().unwrap();
    let vb = graph
        .buffer("wsi.mesh.vertices", BufferDesc { size: 84 })
        .unwrap();
    let ib = graph
        .buffer("wsi.mesh.indices", BufferDesc { size: 12 })
        .unwrap();
    graph
        .upload_buffer("wsi.upload.vertices", vb, 0, &vertices)
        .unwrap();
    graph
        .upload_buffer("wsi.upload.indices", ib, 0, &indices)
        .unwrap();
    graph.export_buffer(vb).unwrap();
    graph.export_buffer(ib).unwrap();
    let submitted = runtime.submit_frame(&graph.compile().unwrap()).unwrap();
    (
        submitted.export_buffer(vb).unwrap().key(),
        submitted.export_buffer(ib).unwrap().key(),
    )
}

fn image_frame(
    runtime: &mut VulkanRuntime,
    pool: &mut GraphPool,
    mesh: (ResourceKey, ResourceKey),
    extent: (u32, u32),
    sequence: usize,
) -> Result<Source, RuntimeError> {
    let (width, height) = extent;
    assert!((width as usize * height as usize * 4) <= PIXEL_CAPACITY);
    let mut graph = pool.begin().unwrap();
    let vertices = graph
        .import_buffer("wsi.vertices", &runtime.refresh_buffer(mesh.0)?)
        .unwrap();
    let indices = graph
        .import_buffer("wsi.indices", &runtime.refresh_buffer(mesh.1)?)
        .unwrap();
    let desc = ImageDesc::rgba8(width, height);
    let color = graph.image("wsi.mesh.color", desc).unwrap();
    let depth = graph
        .image("wsi.mesh.depth", ImageDesc::depth32(width, height))
        .unwrap();
    let mut parameters = RasterParameters::default();
    parameters.transform[3][0] = if sequence.is_multiple_of(2) {
        -0.125
    } else {
        0.125
    };
    parameters.tint[0] = if sequence.is_multiple_of(2) { 1.0 } else { 0.0 };
    let blue = !sequence.is_multiple_of(2);
    graph
        .raster(
            "wsi.standard.mesh",
            RasterDraw {
                vertices,
                indices,
                index_count: 3,
                parameters,
            },
            RasterAttachments {
                color: Attachment {
                    image: color,
                    load: LoadOp::Clear([0.0, 0.0, if blue { 1.0 } else { 0.0 }, 1.0]),
                    store: StoreOp::Store,
                },
                depth: Attachment {
                    image: depth,
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                },
            },
        )
        .unwrap();
    let inversions = sequence % 3;
    let mut sampled = color;
    for index in 0..inversions {
        let next = graph
            .image(
                if index == 0 {
                    "wsi.invert.0"
                } else {
                    "wsi.invert.1"
                },
                desc,
            )
            .unwrap();
        graph
            .compute(
                if index == 0 {
                    "wsi.custom.0"
                } else {
                    "wsi.custom.1"
                },
                sampled,
                next,
                ComputeTransform::Invert,
            )
            .unwrap();
        sampled = next;
    }
    let output = graph.image("wsi.output", desc).unwrap();
    graph
        .composite("wsi.standard.composite", sampled, output)
        .unwrap();
    graph.export_image(output).unwrap();
    let pass = graph
        .readback_image("wsi.offscreen.verify", output)
        .unwrap();
    let frame = runtime.submit_frame(&graph.compile().unwrap())?;
    let image = frame.export_image(output)?;
    let mut center = [if blue { 0 } else { 255 }, 0, 0, 255];
    let mut corner = [0, 0, if blue { 255 } else { 0 }, 255];
    if !inversions.is_multiple_of(2) {
        for pixel in [&mut center, &mut corner] {
            for channel in &mut pixel[..3] {
                *channel = 255 - *channel;
            }
        }
    }
    Ok(Source {
        check: PixelCheck {
            id: frame.id(),
            pass,
            width,
            height,
            center,
            corner,
        },
        _frame: frame,
        image,
        presented: 0,
    })
}

fn verify(mut held: HeldResult, pixels: &mut [u8; PIXEL_CAPACITY]) {
    let check = held.check;
    let length = check.width as usize * check.height as usize * 4;
    held.result
        .readback_into(check.pass, &mut pixels[..length])
        .unwrap();
    let center = (check.height as usize / 2 * check.width as usize + check.width as usize / 2) * 4;
    assert_eq!(
        &pixels[center..center + 4],
        &check.center,
        "mesh, transform, tint and custom passes"
    );
    assert_eq!(
        &pixels[..4],
        &check.corner,
        "defined background after custom passes"
    );
    drop(held);
}

fn pump(event_loop: &mut EventLoop<()>, host: &mut WindowHost, measurements: &mut Measurements) {
    measurements.caller(|| {
        event_loop.pump_app_events(Some(Duration::from_millis(1)), host);
        host.resized.take();
    });
}

fn configure(
    event_loop: &mut EventLoop<()>,
    host: &mut WindowHost,
    window: &Window,
    presenter: &mut Presenter,
    measurements: &mut Measurements,
    extent: (u32, u32),
) {
    measurements.caller(|| {
        let _ = window.request_inner_size(PhysicalSize::new(extent.0, extent.1));
    });
    let start = Instant::now();
    loop {
        pump(event_loop, host, measurements);
        let actual = measurements.caller(|| window.inner_size());
        let ready = measurements.renderer(Phase::Setup, || {
            presenter.resize(actual.width, actual.height);
            match presenter.prepare() {
                Ok(Some(PresentOutcome::Suspended | PresentOutcome::NotReady)) => false,
                Ok(_) => presenter.extent() == Some(extent),
                Err(error) if error.kind() == RuntimeErrorKind::CapacityExhausted => false,
                Err(error) => panic!("Swapchain preparation failed: {error}"),
            }
        });
        if ready && (actual.width, actual.height) == extent {
            break;
        }
        assert!(
            start.elapsed() < WAIT,
            "requested Win32 extent must become available"
        );
    }
}

fn present_source(
    runtime: &mut VulkanRuntime,
    presenter: &mut Presenter,
    source: &mut Source,
    target: usize,
) {
    presenter.poll().unwrap();
    assert_eq!(
        presenter.prepare().unwrap(),
        None,
        "source extent is already prepared"
    );
    match runtime.present(presenter, &source.image) {
        Ok(PresentOutcome::Presented { .. }) => {
            assert_eq!(
                runtime.refresh_image(source.image.key()).unwrap(),
                source.image
            );
            source.presented += 1;
            assert!(source.presented <= target);
        }
        Ok(PresentOutcome::NotReady) => {}
        Err(error) if error.kind() == RuntimeErrorKind::CapacityExhausted => {}
        Ok(outcome) => panic!("fixed prepared source changed during presentation: {outcome:?}"),
        Err(error) => panic!("presentation failed: {error}"),
    }
}

fn finish_setup_source(
    runtime: &mut VulkanRuntime,
    presenter: &mut Presenter,
    source: Source,
    pixels: &mut [u8; PIXEL_CAPACITY],
) {
    let check = source.check;
    runtime.delete(source.image.key()).unwrap();
    drop(source);
    loop {
        let completed = runtime
            .wait_frame(WAIT)
            .unwrap()
            .expect("setup frame must complete");
        if completed.id() == check.id {
            verify(
                HeldResult {
                    check,
                    result: completed.result,
                },
                pixels,
            );
            break;
        }
        // Initial mesh upload has no CPU result payload.
        drop(completed);
    }
    assert!(presenter.wait_idle(WAIT).unwrap());
}

#[allow(
    clippy::too_many_arguments,
    reason = "one setup helper retains the same ordinary builder and Window owners"
)]
fn setup_source(
    runtime: &mut VulkanRuntime,
    presenter: &mut Presenter,
    pool: &mut GraphPool,
    mesh: (ResourceKey, ResourceKey),
    sequence: usize,
    cancel: bool,
    event_loop: &mut EventLoop<()>,
    host: &mut WindowHost,
    measurements: &mut Measurements,
    observations: &mut Observations,
) -> Source {
    let mut source = measurements.renderer(Phase::Setup, || {
        image_frame(runtime, pool, mesh, presenter.extent().unwrap(), sequence).unwrap()
    });
    observations.setup_sources += 1;
    if cancel {
        measurements.renderer(Phase::Setup, || presenter.cancel_next_acquire());
    }
    let start = Instant::now();
    while source.presented != 2 {
        pump(event_loop, host, measurements);
        measurements.renderer(Phase::Setup, || {
            present_source(runtime, presenter, &mut source, 2)
        });
        assert!(start.elapsed() < WAIT, "setup presentation must progress");
    }
    assert!(
        !presenter.inner.as_ref().unwrap().cancel_next_acquire,
        "successful acquire must exercise cancellation"
    );
    source
}

fn set_vsync_pending(presenter: &mut Presenter, enabled: bool) {
    let inner = presenter.inner.as_ref().unwrap();
    assert!(
        !inner.pending.is_empty(),
        "accepted work remains owned at the setting change"
    );
    let old = inner.current.as_ref().unwrap().handle;
    presenter
        .set_vsync(enabled)
        .expect("FIFO and IMMEDIATE are required for VSync acceptance");
    assert_eq!(presenter.vsync(), Some(enabled));
    assert_eq!(
        presenter
            .inner
            .as_ref()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .handle,
        old,
        "setter schedules recreation"
    );
}

fn observe(
    runtime: &mut VulkanRuntime,
    reads: &mut [Option<PixelCheck>; IN_FLIGHT],
    held: &mut [Option<HeldResult>; IN_FLIGHT],
    pixels: &mut [u8; PIXEL_CAPACITY],
    observations: &mut Observations,
) {
    while let Some(completed) = runtime.poll_frame().unwrap() {
        let index = reads
            .iter()
            .position(|entry| entry.is_some_and(|check| check.id == completed.id()))
            .expect("normal frame readback identity");
        let check = reads[index].take().unwrap();
        let slot = held.iter().position(Option::is_none).unwrap_or_else(|| {
            verify(held[0].take().unwrap(), pixels);
            observations.normal_verified += 1;
            0
        });
        held[slot] = Some(HeldResult {
            check,
            result: completed.result,
        });
        observations.held_peak = observations.held_peak.max(held.iter().flatten().count());
    }
}

#[test]
#[ignore = "explicit real Win32 Surface/Swapchain/Graph acceptance; run strict and release shipping separately"]
fn vulkan_presentation_graph_resize_cancel_and_pending_shutdown() {
    let mut measurements = Measurements::new();
    let dispatch = measurements.caller(crate::backends::vulkan::real_vulkan_dispatch);
    let mut observations = Observations::default();
    let config = RuntimeConfig {
        execution_slots: 6,
        resource_capacity: 128,
        ..RuntimeConfig::default()
    };
    let presentation = PresentationConfig {
        max_frames: IN_FLIGHT,
        ..PresentationConfig::default()
    };
    tracing::dispatcher::with_default(&dispatch, || {
        let mut event_loop = measurements.caller(|| {
            let mut builder = EventLoop::builder();
            builder.with_any_thread(true);
            builder.build().expect("Win32 event loop is required")
        });
        let mut host = WindowHost::default();
        pump(&mut event_loop, &mut host, &mut measurements);
        let window = measurements
            .caller(|| Arc::clone(host.window.as_ref().expect("resumed must create a window")));
        let (mut runtime, mut presenter) = measurements.renderer(Phase::Setup, || {
            VulkanRuntime::for_window(config, Arc::clone(&window), presentation)
                .expect("real Surface/maintenance1 presentation support is required")
        });
        assert_ne!(
            runtime.owner.device.device_type(),
            vk::PhysicalDeviceType::CPU
        );
        let diagnostics = runtime.diagnostics.clone();
        #[cfg(feature = "development-validation")]
        measurements.renderer(Phase::Setup, || {
            crate::backends::vulkan::assert_real_validation(&diagnostics);
        });
        #[cfg(not(feature = "development-validation"))]
        #[allow(
            clippy::assertions_on_constants,
            reason = "ignored shipping acceptance also compiles in debug CPU suites"
        )]
        measurements.renderer(Phase::Setup, || {
            assert!(
                !cfg!(debug_assertions),
                "shipping acceptance requires --release"
            );
            assert_eq!(
                runtime.validation_monitor().report().requested,
                crate::backends::vulkan::ValidationMode::Disabled
            );
        });
        let (mut mesh_graphs, mut frame_graphs) = measurements.renderer(Phase::Setup, || {
            prepare(&mut runtime);
            (
                GraphPool::new(graph_capacity(), 1).unwrap(),
                GraphPool::new(graph_capacity(), GRAPH_SLOTS).unwrap(),
            )
        });
        let mesh = measurements.renderer(Phase::Setup, || mesh(&mut runtime, &mut mesh_graphs));
        let mut pixels = [0; PIXEL_CAPACITY];
        measurements.renderer(Phase::Setup, || {
            assert_eq!(presenter.vsync(), Some(true));
            presenter.set_vsync(true).unwrap();
            assert_eq!(
                presenter.prepare().unwrap(),
                None,
                "unchanged VSync does not recreate"
            );
        });

        // Change/acquire-cancel work and all of its delayed retirement are
        // visible before the complete normal-frame accounting window.
        let first = setup_source(
            &mut runtime,
            &mut presenter,
            &mut frame_graphs,
            mesh,
            2,
            false,
            &mut event_loop,
            &mut host,
            &mut measurements,
            &mut observations,
        );
        measurements.renderer(Phase::Setup, || {
            set_vsync_pending(&mut presenter, false);
            assert_eq!(
                presenter.prepare().unwrap(),
                Some(PresentOutcome::Recreated)
            );
            finish_setup_source(&mut runtime, &mut presenter, first, &mut pixels);
        });
        configure(
            &mut event_loop,
            &mut host,
            &window,
            &mut presenter,
            &mut measurements,
            (160, 112),
        );
        let second = setup_source(
            &mut runtime,
            &mut presenter,
            &mut frame_graphs,
            mesh,
            1,
            true,
            &mut event_loop,
            &mut host,
            &mut measurements,
            &mut observations,
        );
        measurements.renderer(Phase::Setup, || {
            set_vsync_pending(&mut presenter, true);
        });
        for extent in [(176, 120), (144, 104), (192, 128)] {
            measurements.caller(|| {
                let _ = window.request_inner_size(PhysicalSize::new(extent.0, extent.1));
            });
            measurements.renderer(Phase::Setup, || presenter.resize(extent.0, extent.1));
            measurements.caller(|| {
                event_loop.pump_app_events(Some(Duration::ZERO), &mut host);
                host.resized.take();
            });
        }
        measurements.renderer(Phase::Setup, || {
            assert!(
                !presenter.inner.as_ref().unwrap().pending.is_empty(),
                "rapid resize retains accepted, unobserved presentation work"
            );
        });
        configure(
            &mut event_loop,
            &mut host,
            &window,
            &mut presenter,
            &mut measurements,
            (192, 128),
        );
        measurements.renderer(Phase::Setup, || {
            finish_setup_source(&mut runtime, &mut presenter, second, &mut pixels);
        });
        let third = setup_source(
            &mut runtime,
            &mut presenter,
            &mut frame_graphs,
            mesh,
            0,
            false,
            &mut event_loop,
            &mut host,
            &mut measurements,
            &mut observations,
        );
        measurements.caller(|| window.set_minimized(true));
        measurements.renderer(Phase::Setup, || {
            presenter.resize(0, 0);
            assert_eq!(
                runtime.present(&mut presenter, &third.image).unwrap(),
                PresentOutcome::Suspended
            );
            presenter.set_vsync(false).unwrap();
            assert_eq!(
                presenter.prepare().unwrap(),
                Some(PresentOutcome::Suspended)
            );
            presenter.set_vsync(true).unwrap();
        });
        measurements.caller(|| window.set_minimized(false));
        configure(
            &mut event_loop,
            &mut host,
            &window,
            &mut presenter,
            &mut measurements,
            (160, 112),
        );
        measurements.renderer(Phase::Setup, || {
            finish_setup_source(&mut runtime, &mut presenter, third, &mut pixels)
        });
        assert_eq!(runtime.pending_frames(), 0);
        assert!(presenter.inner.as_ref().unwrap().idle());
        // Retain only scalar identity: cloning the Arc here would mask a
        // regression that drops eligible generations during ordinary polling.
        let retired_handle = measurements.renderer(Phase::Setup, || {
            let retired = &presenter.inner.as_ref().unwrap().retired;
            assert!(!retired.is_empty(), "resize retains an old generation");
            assert!(retired.iter().all(|chain| Arc::strong_count(chain) == 1));
            observations.normal_retired_swapchains = retired.len();
            retired[0].handle
        });

        let extent = presenter.extent().unwrap();
        observations.normal_extent = Some(extent);
        let mut held: [Option<HeldResult>; IN_FLIGHT] = std::array::from_fn(|_| None);
        measurements.begin_normal();
        for batch in 0..NORMAL_SOURCES / IN_FLIGHT {
            let mut sources: [Option<Source>; IN_FLIGHT] = std::array::from_fn(|_| None);
            let mut reads: [Option<PixelCheck>; IN_FLIGHT] = [None; IN_FLIGHT];
            measurements.renderer(Phase::Normal, || {
                // No completion observation between these accepted submissions.
                for index in 0..IN_FLIGHT {
                    let source = image_frame(
                        &mut runtime,
                        &mut frame_graphs,
                        mesh,
                        extent,
                        batch * IN_FLIGHT + index,
                    )
                    .unwrap();
                    reads[index] = Some(source.check);
                    sources[index] = Some(source);
                    observations.normal_sources += 1;
                }
                observations.pending_peak = observations.pending_peak.max(runtime.pending_frames());
                observations.readback_peak = observations
                    .readback_peak
                    .max(runtime.stats().readback_live_ranges);
                assert_eq!(runtime.pending_frames(), IN_FLIGHT);
            });
            let start = Instant::now();
            while sources.iter().any(Option::is_some) || reads.iter().any(Option::is_some) {
                pump(&mut event_loop, &mut host, &mut measurements);
                measurements.renderer(Phase::Normal, || {
                    observe(
                        &mut runtime,
                        &mut reads,
                        &mut held,
                        &mut pixels,
                        &mut observations,
                    );
                    for source in &mut sources {
                        if let Some(value) = source {
                            let before = value.presented;
                            present_source(&mut runtime, &mut presenter, value, 2);
                            observations.normal_presents += value.presented - before;
                            if value.presented == 2 {
                                runtime.delete(value.image.key()).unwrap();
                                source.take();
                            }
                        }
                    }
                });
                assert!(
                    start.elapsed() < WAIT,
                    "three-source WSI batch must progress"
                );
            }
        }
        measurements.renderer(Phase::Normal, || {
            for result in &mut held {
                if let Some(result) = result.take() {
                    verify(result, &mut pixels);
                    observations.normal_verified += 1;
                }
            }
            assert_eq!(runtime.pending_frames(), 0);
            assert!(presenter.wait_idle(WAIT).unwrap());
            assert_eq!(runtime.stats().readback_live_ranges, 0);
            assert!(
                frame_graphs.is_idle(),
                "normal plans return after GPU/CPU/present completion"
            );
            let retired = &presenter.inner.as_ref().unwrap().retired;
            assert_eq!(retired.len(), observations.normal_retired_swapchains);
            assert!(
                retired.iter().any(|chain| {
                    chain.handle == retired_handle && Arc::strong_count(chain) == 1
                }),
                "ordinary prepare/poll/drain retains the eligible old generation"
            );
        });

        // Keep pending-close coverage without moving any normal return to shutdown.
        measurements.end_normal();
        measurements.renderer(Phase::Setup, || {
            observations.collected_retired_swapchains = presenter.collect_retired_swapchains();
            assert_eq!(
                observations.collected_retired_swapchains,
                observations.normal_retired_swapchains
            );
            assert!(presenter.inner.as_ref().unwrap().retired.is_empty());
            assert_eq!(presenter.collect_retired_swapchains(), 0);
        });
        let mut final_source = measurements.renderer(Phase::Shutdown, || {
            image_frame(&mut runtime, &mut frame_graphs, mesh, extent, 20).unwrap()
        });
        let start = Instant::now();
        while final_source.presented != 1 {
            pump(&mut event_loop, &mut host, &mut measurements);
            measurements.renderer(Phase::Shutdown, || {
                present_source(&mut runtime, &mut presenter, &mut final_source, 1)
            });
            assert!(start.elapsed() < WAIT, "shutdown handoff must progress");
        }
        observations.shutdown_presents = final_source.presented;
        measurements.renderer(Phase::Shutdown, || {
            assert!(!presenter.inner.as_ref().unwrap().pending.is_empty());
            let weak_window = Arc::downgrade(&window);
            host.window = None;
            drop(window);
            assert!(
                weak_window.upgrade().is_some(),
                "Presentation retains its Window owner"
            );
            assert!(
                presenter.close(WAIT).unwrap(),
                "pending presentation shutdown must complete"
            );
            assert_eq!(presenter.vsync(), None);
            assert_eq!(
                presenter.set_vsync(false).unwrap_err().kind(),
                RuntimeErrorKind::InvalidInput
            );
            finish_setup_source(&mut runtime, &mut presenter, final_source, &mut pixels);
            runtime.delete(mesh.0).unwrap();
            runtime.delete(mesh.1).unwrap();
            drop(presenter);
            drop(runtime);
            drop((frame_graphs, mesh_graphs));
            assert!(
                weak_window.upgrade().is_none(),
                "retired Surface releases Window ownership"
            );
            let report = diagnostics.snapshot();
            assert!(
                report.is_clean(),
                "Core/SyncVal including complete WSI shutdown: {report:#?}"
            );
            #[cfg(feature = "vulkan-diagnostics")]
            drop(diagnostics);
        });
        measurements.caller(|| drop((event_loop, host)));
    });
    measurements.caller(|| drop(dispatch));
    measurements.finish(
        observations,
        config,
        presentation,
        frame_capacity(),
        &EXTENTS,
        GRAPH_SLOTS,
    );
}
