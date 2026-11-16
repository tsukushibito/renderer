//! Real Windows acceptance. No runtime prerequisite is silently skipped.

use super::super::{
    D3D12FrameCapacity, D3D12ImageClass, D3D12Submission, D3D12SubmissionResult,
    D3D12ValidationMode,
};
use super::*;
use crate::graph::{
    Attachment, GraphCapacity, GraphPool, ImageDesc, LoadOp, PassHandle, RasterAttachments, StoreOp,
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::{pump_events::EventLoopExtPumpEvents, windows::EventLoopBuilderExtWindows},
    window::WindowId,
};

const WAIT: Duration = Duration::from_secs(10);
const EXTENTS: [(u32, u32); 5] = [(128, 96), (160, 112), (176, 120), (144, 104), (192, 128)];
const PIXELS: usize = 192 * 128 * 4;
const IN_FLIGHT: usize = 3;

#[derive(Default)]
struct Host {
    window: Option<Arc<Window>>,
}
impl ApplicationHandler for Host {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        if self.window.is_none() {
            self.window = Some(Arc::new(
                active
                    .create_window(
                        Window::default_attributes()
                            .with_title("Renderer D3D12 presentation acceptance")
                            .with_inner_size(PhysicalSize::new(128, 96)),
                    )
                    .expect("real Win32 window is required"),
            ));
        }
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

#[derive(Default)]
struct Accounting {
    #[cfg(feature = "memory-profiling")]
    caller: crate::measurement::Snapshot,
}
impl Accounting {
    fn caller<T>(&mut self, action: impl FnOnce() -> T) -> T {
        #[cfg(feature = "memory-profiling")]
        let start = crate::measurement::Snapshot::now();
        let result = action();
        #[cfg(feature = "memory-profiling")]
        self.caller
            .accumulate(crate::measurement::Snapshot::now().since(start));
        result
    }
}
fn pump(events: &mut EventLoop<()>, host: &mut Host, account: &mut Accounting) {
    account.caller(|| events.pump_app_events(Some(Duration::ZERO), host));
}

struct Source {
    submission: D3D12Submission,
    image: ImportImage,
    pass: PassHandle,
    expected: [u8; 4],
    presented: usize,
}
fn build(
    runtime: &mut D3D12Runtime,
    pool: &mut GraphPool,
    extent: (u32, u32),
    sequence: usize,
) -> Source {
    let mut graph = pool.begin().unwrap();
    let color = graph
        .image("window.output", ImageDesc::rgba8(extent.0, extent.1))
        .unwrap();
    let value = if sequence.is_multiple_of(2) {
        [1.0, 0.0, 0.0, 1.0]
    } else {
        [0.0, 0.0, 1.0, 1.0]
    };
    graph
        .attachment_pass(
            "window.draw",
            RasterAttachments {
                colors: &[Attachment {
                    view: graph.image_view(color, 0, 0).unwrap(),
                    load: LoadOp::Clear(value),
                    store: StoreOp::Store,
                }],
                depth: None,
            },
        )
        .unwrap();
    graph.export_image(color).unwrap();
    let pass = graph.readback_image("window.verify", color).unwrap();
    let compiled = graph.compile().unwrap();
    let prepared = runtime.prepare(&compiled).unwrap();
    let submission = runtime.submit_prepared(prepared).unwrap();
    let image = submission.export_image(color).unwrap();
    Source {
        submission,
        image,
        pass,
        expected: if sequence.is_multiple_of(2) {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        },
        presented: 0,
    }
}
fn present(runtime: &mut D3D12Runtime, presenter: &mut D3D12Presenter, source: &mut Source) {
    match runtime.present(presenter, &source.image).unwrap() {
        PresentOutcome::Presented { .. } => {
            source.presented += 1;
            assert_eq!(
                runtime.refresh_image(source.image.key()).unwrap(),
                source.image
            );
        }
        PresentOutcome::NotReady => {}
        outcome => panic!("prepared presentation changed unexpectedly: {outcome:?}"),
    }
}
fn verify(
    mut result: D3D12SubmissionResult,
    source: &Source,
    extent: (u32, u32),
    pixels: &mut [u8; PIXELS],
) {
    let len = extent.0 as usize * extent.1 as usize * 4;
    result
        .readback_into(source.pass, &mut pixels[..len])
        .unwrap();
    for pixel in pixels[..len].as_chunks::<4>().0 {
        assert_eq!(*pixel, source.expected);
    }
}
fn finish(
    runtime: &mut D3D12Runtime,
    presenter: &mut D3D12Presenter,
    mut source: Source,
    extent: (u32, u32),
    pixels: &mut [u8; PIXELS],
) {
    let result = source
        .submission
        .wait(WAIT)
        .unwrap()
        .expect("GPU Graph must complete");
    verify(result, &source, extent, pixels);
    runtime.delete(source.image.key()).unwrap();
    drop(source);
    assert!(presenter.wait_idle(WAIT).unwrap());
}

#[test]
#[ignore = "requires real Win32 Window, D3D12 hardware and requested native diagnostics"]
fn d3d12_windowed_resize_cancel_pending_shutdown_and_frame_memory() {
    let strict = cfg!(feature = "development-validation");
    let mut events = EventLoop::builder()
        .with_any_thread(true)
        .build()
        .expect("Windows event loop is required");
    let mut host = Host::default();
    let mut account = Accounting::default();
    pump(&mut events, &mut host, &mut account);
    let window = Arc::clone(
        host.window
            .as_ref()
            .expect("Windows window creation must succeed"),
    );
    #[cfg(feature = "memory-profiling")]
    let setup_started = crate::measurement::Snapshot::now();
    let capacity = D3D12FrameCapacity {
        executions: 3,
        results: 6,
        resources: 8,
        resource_capacity: 64,
        operations: 16,
        provenance_segments_per_resource: 8,
        initialized_ranges_per_resource: 8,
        range_snapshots: 64,
        buffers: 1,
        buffer_bytes: 256,
        upload_bytes: 256,
        readback_bytes: PIXELS as u64,
        scratch_bytes: 256,
    };
    let (mut runtime, mut presenter) = D3D12Runtime::for_window(
        D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            capacity,
        },
        Arc::clone(&window),
        PresentationConfig {
            max_frames: 3,
            ..Default::default()
        },
    )
    .expect("hardware D3D12 and requested diagnostics/tearing support are required");
    let monitor = runtime.validation_monitor();
    if strict {
        let report = monitor.snapshot();
        assert!(
            report.debug_layer_enabled && report.gpu_based_validation_enabled,
            "strict diagnostics must be effective: {report:?}"
        );
    }
    let classes = EXTENTS.map(|(width, height)| D3D12ImageClass {
        desc: ImageDesc::rgba8(width, height),
        count: 2,
    });
    runtime
        .prepare_graph_working_set(&classes, &[], &[])
        .unwrap();
    let mut pool = GraphPool::new(
        GraphCapacity {
            resources: 8,
            passes: 8,
            accesses: 16,
            attachments: 8,
            draws: 1,
            bindings: 0,
            dependencies: 8,
            initial_ranges: 16,
            name_bytes: 512,
            upload_bytes: 256,
            edges: 32,
            segments: 32,
            readers: 32,
        },
        8,
    )
    .unwrap();
    let mut pixels = [0u8; PIXELS];
    // Real repeated Window resize, pending mode changes and zero extent restore.
    for (sequence, extent) in EXTENTS.into_iter().enumerate() {
        account.caller(|| {
            let _ = window.request_inner_size(PhysicalSize::new(extent.0, extent.1));
        });
        presenter.resize(extent.0, extent.1);
        let start = Instant::now();
        loop {
            pump(&mut events, &mut host, &mut account);
            match presenter.prepare().unwrap() {
                Some(PresentOutcome::NotReady) => {}
                _ => break,
            }
            assert!(start.elapsed() < WAIT);
        }
        assert_eq!(presenter.extent(), Some(extent));
        let mut first = build(&mut runtime, &mut pool, extent, sequence);
        presenter.cancel_next_acquire();
        assert_eq!(
            runtime.present(&mut presenter, &first.image).unwrap(),
            PresentOutcome::NotReady
        );
        let mut replacement = build(&mut runtime, &mut pool, extent, sequence + 1);
        let start = Instant::now();
        while replacement.presented == 0 {
            pump(&mut events, &mut host, &mut account);
            present(&mut runtime, &mut presenter, &mut replacement);
            assert!(start.elapsed() < WAIT);
        }
        // A NotReady/cancel caller may choose a different output without knowing
        // which backend has a native image-acquisition primitive.
        assert!(
            presenter
                .inner
                .as_ref()
                .unwrap()
                .slots
                .iter()
                .any(|slot| slot.pending)
        );
        presenter
            .set_vsync(!sequence.is_multiple_of(2))
            .expect("both real DXGI VSync modes are required");
        finish(
            &mut runtime,
            &mut presenter,
            replacement,
            extent,
            &mut pixels,
        );
        let start = Instant::now();
        while first.presented == 0 {
            pump(&mut events, &mut host, &mut account);
            if let Some(PresentOutcome::NotReady) = presenter.prepare().unwrap() {
                continue;
            }
            present(&mut runtime, &mut presenter, &mut first);
            assert!(start.elapsed() < WAIT);
        }
        finish(&mut runtime, &mut presenter, first, extent, &mut pixels);
    }
    account.caller(|| window.set_minimized(true));
    presenter.resize(0, 0);
    assert_eq!(
        presenter.prepare().unwrap(),
        Some(PresentOutcome::Suspended)
    );
    assert_eq!(presenter.extent(), None);
    account.caller(|| window.set_minimized(false));
    let extent = *EXTENTS.last().unwrap();
    presenter.resize(extent.0, extent.1);
    presenter.set_vsync(false).unwrap();
    while matches!(presenter.prepare().unwrap(), Some(PresentOutcome::NotReady)) {
        pump(&mut events, &mut host, &mut account);
    }
    let warmup = build(&mut runtime, &mut pool, extent, 0);
    finish(&mut runtime, &mut presenter, warmup, extent, &mut pixels);

    #[cfg(feature = "memory-profiling")]
    let setup = crate::measurement::Snapshot::now().since(setup_started);
    #[cfg(feature = "memory-profiling")]
    let normal_started = crate::measurement::Snapshot::now();
    #[cfg(feature = "memory-profiling")]
    let caller_started = account.caller;
    let mut normal_presents = 0;
    // Six batches hold three submitted Graphs, three CPU results, and source
    // snapshots independently. Every ordinary lifecycle phase is inside this
    // one continuous window; only synchronous winit caller work is subtracted.
    for batch in 0..6 {
        let mut sources: [Option<Source>; IN_FLIGHT] = std::array::from_fn(|index| {
            Some(build(&mut runtime, &mut pool, extent, batch * 3 + index))
        });
        let mut results: [Option<D3D12SubmissionResult>; IN_FLIGHT] = std::array::from_fn(|_| None);
        let start = Instant::now();
        loop {
            pump(&mut events, &mut host, &mut account);
            presenter.poll().unwrap();
            for (source, result) in sources.iter_mut().zip(&mut results) {
                let source = source.as_mut().unwrap();
                if source.presented < 2 {
                    let before = source.presented;
                    present(&mut runtime, &mut presenter, source);
                    normal_presents += source.presented - before;
                }
                if result.is_none() {
                    *result = source.submission.poll().unwrap();
                }
            }
            if sources.iter().all(|s| s.as_ref().unwrap().presented == 2)
                && results.iter().all(Option::is_some)
            {
                break;
            }
            assert!(start.elapsed() < WAIT, "normal pending work must progress");
        }
        for (source, result) in sources.iter_mut().zip(&mut results) {
            let source = source.take().unwrap();
            verify(result.take().unwrap(), &source, extent, &mut pixels);
            runtime.delete(source.image.key()).unwrap();
            drop(source);
        }
        let start = Instant::now();
        while presenter
            .inner
            .as_ref()
            .unwrap()
            .slots
            .iter()
            .any(|s| s.pending)
        {
            presenter.poll().unwrap();
            assert!(start.elapsed() < WAIT);
        }
        runtime.poll().unwrap();
    }
    #[cfg(feature = "memory-profiling")]
    let normal = {
        let process = crate::measurement::Snapshot::now().since(normal_started);
        let caller = account.caller.since(caller_started);
        let mut normal = process.since(caller);
        normal.native = process.native;
        normal.vma_host = process.vma_host;
        assert!(!normal.overflowed);
        assert!(
            normal.native[..crate::measurement::NativeEvent::Reset as usize]
                .iter()
                .all(|&count| count == 0),
            "normal native creation/destruction: {normal:?}"
        );
        if !strict {
            assert!(
                normal.is_allocation_free(),
                "normal D3D12 windowed frame accounting: {normal:?}"
            );
        }
        assert!(normal.native[crate::measurement::NativeEvent::Reset as usize] > 0);
        normal
    };
    assert_eq!(normal_presents, 36);
    // Profiling is independent. Disabling after acceptance preserves this sample.
    presenter.set_profiling(true).unwrap();
    let mut profiled = build(&mut runtime, &mut pool, extent, 0);
    let start = Instant::now();
    while profiled.presented == 0 {
        present(&mut runtime, &mut presenter, &mut profiled);
        pump(&mut events, &mut host, &mut account);
        assert!(start.elapsed() < WAIT);
    }
    presenter.set_profiling(false).unwrap();
    finish(&mut runtime, &mut presenter, profiled, extent, &mut pixels);
    let sample = presenter
        .take_profile()
        .expect("accepted sample survives disabling");
    assert!(matches!(sample.gpu, GpuTiming::Available { .. }));
    assert!(presenter.take_profile().is_none());

    #[cfg(feature = "memory-profiling")]
    let shutdown_started = crate::measurement::Snapshot::now();
    let mut pending = build(&mut runtime, &mut pool, extent, 1);
    let start = Instant::now();
    while pending.presented == 0 {
        present(&mut runtime, &mut presenter, &mut pending);
        pump(&mut events, &mut host, &mut account);
        assert!(start.elapsed() < WAIT);
    }
    assert!(
        presenter
            .inner
            .as_ref()
            .unwrap()
            .slots
            .iter()
            .any(|slot| slot.pending)
    );
    runtime.delete(pending.image.key()).unwrap();
    drop(pending); // Native work remains retained by runtime/presenter owners.
    drop(runtime);
    assert!(presenter.close(WAIT).unwrap());
    drop(presenter);
    drop(pool);
    drop(window);
    host.window = None;
    #[cfg(feature = "memory-profiling")]
    let shutdown = crate::measurement::Snapshot::now().since(shutdown_started);
    let report = monitor.snapshot();
    assert!(
        report.is_clean(),
        "diagnostics through final native teardown: {report:?}"
    );
    if strict {
        assert!(
            report.teardown_checked,
            "final native destruction must be observed"
        );
    }
    #[cfg(feature = "memory-profiling")]
    {
        assert!(setup.native[crate::measurement::NativeEvent::ObjectCreate as usize] > 0);
        assert!(shutdown.native[crate::measurement::NativeEvent::ObjectDestroy as usize] > 0);
        println!(
            "D3D12 windowed memory {{\"sources\":18,\"presents\":36,\"diagnostic_baseline\":{},\"rust_calls\":{:?},\"rust_bytes\":{:?},\"native\":{:?},\"vma_calls\":{:?},\"overflowed\":{}}}",
            strict,
            normal.rust.map(|v| v.calls),
            normal.rust.map(|v| v.bytes),
            normal.native,
            normal.vma_host.map(|v| v.calls),
            normal.overflowed
        );
    }
    println!(
        "D3D12 windowed acceptance strict={strict} extents={EXTENTS:?} normal_presents={normal_presents} diagnostics={report:?} profile={sample:?}"
    );
}
