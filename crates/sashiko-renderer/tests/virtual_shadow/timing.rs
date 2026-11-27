//! Native timestamp probe for the receiver alone. Run in release without native validation.
use super::*;
use sashiko_renderer_core::GpuTiming;

fn measure(mut config: RuntimeConfig) {
    let extent = [1920, 1080];
    let depth_desc = ImageDesc::depth32(extent[0], extent[1]);
    let mut prep = Preparation::new(NonZeroUsize::MIN);
    virtual_shadow::reserve_requests(&mut prep).unwrap();
    virtual_shadow::reserve_table(&mut prep).unwrap();
    virtual_shadow::reserve_atlas(&mut prep, 0).unwrap();
    virtual_shadow::reserve_visibility(&mut prep, extent).unwrap();
    prep.reserve_transient_image("depth", depth_desc).unwrap();
    config.fit_preparation(&prep, 128 * 1024, 0).unwrap();
    let limits = prep.runtime_limits(RuntimeLimits::default()).unwrap();
    let mut runtime = Runtime::new(config, limits, prep.working_set()).unwrap();
    // The native execution slot may retain the last compiled plan until the
    // next submission. Record the next graph in the second prepared slot.
    let mut pool = GraphPool::new(limits.graph, 2).unwrap();
    let mut setup = pool.begin().unwrap();
    let depth = setup.image("depth", depth_desc).unwrap();
    let receiver = setup.image_view(depth, 0, 0).unwrap();
    setup
        .raster(
            "planar receiver",
            RasterAttachments {
                colors: &[],
                color_resolves: &[],
                depth: Some(Attachment {
                    view: receiver,
                    load: LoadOp::Clear(0.5),
                    store: StoreOp::Store,
                }),
            },
            |_| Ok(()),
        )
        .unwrap();
    let camera = ViewInput::new(
        std::array::from_fn(|c| std::array::from_fn(|r| f32::from(c == r))),
        [0., 0., -0.01],
        [0., 0., 1.],
        [0.01, 1.01],
        Projection::Orthographic,
    )
    .unwrap();
    let requests = virtual_shadow::request_pages(
        &mut setup,
        receiver,
        camera,
        Sun::from_lux([0., 0., 1.], [1.; 3], 1.).unwrap(),
        Bounds::new([-1.; 3], [1.; 3]).unwrap(),
        ShadowSettings::default(),
    )
    .unwrap();
    let pages = virtual_shadow::assign_pages(&mut setup, requests, None).unwrap();
    let atlas = setup.image("atlas", virtual_shadow::ATLAS_DESC).unwrap();
    virtual_shadow::draw_atlas(&mut setup, atlas, &pages, false, |_| Ok(())).unwrap();
    for item in [requests.camera, requests.mask, pages.table, pages.slots] {
        setup.export_buffer(item).unwrap();
    }
    for item in [depth, atlas] {
        setup.export_image(item).unwrap();
    }
    let plan = setup.compile().unwrap();
    let submitted = runtime.submit(&plan).unwrap();
    let buffers = [requests.camera, requests.mask, pages.table, pages.slots]
        .map(|b| submitted.export_buffer(b).unwrap());
    let images = [depth, atlas].map(|i| submitted.export_image(i).unwrap());
    runtime
        .wait_for_frame(submitted.id(), Duration::from_secs(30))
        .unwrap()
        .unwrap();
    drop(submitted);
    drop(plan);

    runtime.set_profiling(true).unwrap();
    let mut samples = Vec::new();
    for iteration in 0..80 {
        let mut graph = pool.begin().unwrap();
        let [camera, mask, table, slots] = buffers
            .each_ref()
            .map(|b| graph.import_buffer("cached", b).unwrap());
        let [depth, atlas] = images
            .each_ref()
            .map(|i| graph.import_image("cached", i).unwrap());
        let mut current = requests;
        current.camera = camera;
        current.mask = mask;
        let depth = graph.image_view(depth, 0, 0).unwrap();
        let output = virtual_shadow::resolve_visibility(
            &mut graph,
            depth,
            current,
            &virtual_shadow::AssignedPages { table, slots },
            atlas,
        )
        .unwrap();
        graph.export_image(output).unwrap();
        let plan = graph.compile().unwrap();
        assert!(
            plan.ordered_pass_names()
                .eq(["resolve virtual shadow visibility"])
        );
        let submission = runtime.submit(&plan).unwrap();
        let output = submission.export_image(output).unwrap();
        let frame = runtime
            .wait_for_frame(submission.id(), Duration::from_secs(30))
            .unwrap()
            .unwrap();
        let GpuTiming::Available { elapsed, .. } = frame.metrics().unwrap().gpu else {
            panic!("GPU timestamp unavailable");
        };
        if iteration >= 16 {
            samples.push(elapsed.as_secs_f64() * 1000.);
        }
        runtime.delete(output.key()).unwrap();
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "1080p planar visibility GPU median_ms={:.4} p95_ms={:.4} samples={}",
        (samples[31] + samples[32]) * 0.5,
        samples[60],
        samples.len()
    );
    for buffer in buffers {
        runtime.delete(buffer.key()).unwrap();
    }
    for image in images {
        runtime.delete(image.key()).unwrap();
    }
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a real Vulkan GPU and timestamps"]
fn vulkan_receiver_timing() {
    measure(RuntimeConfig::Vulkan(Default::default()));
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a real D3D12 GPU and timestamps"]
fn d3d12_receiver_timing() {
    measure(RuntimeConfig::D3D12(Default::default()));
}
