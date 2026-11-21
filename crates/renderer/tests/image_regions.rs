use renderer::graph::{GraphCapacity, GraphErrorKind, GraphPool, ImageDesc, ImageRegion};

fn atlas() -> ImageDesc {
    ImageDesc {
        mip_levels: 4,
        ..ImageDesc::rgba8(8, 4)
    }
}

#[test]
fn subrectangle_requires_prior_mip_contents_and_preserves_its_producer() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let desc = atlas();
    let image = graph.image("atlas", desc).unwrap();
    let patch = ImageRegion {
        mip_level: 1,
        origin: [1, 0],
        extent: [2, 2],
    };
    graph
        .upload_image_region("patch", image, patch, &[7; 16])
        .unwrap();
    graph.export_image_mips(image, 1..2).unwrap();
    assert_eq!(
        graph.compile().unwrap_err().kind(),
        GraphErrorKind::UninitializedRead
    );

    let mut graph = pool.begin().unwrap();
    let image = graph.image("atlas", desc).unwrap();
    graph
        .upload_image_region("base", image, ImageRegion::mip(desc, 1).unwrap(), &[1; 32])
        .unwrap();
    graph
        .upload_image_region("patch", image, patch, &[7; 16])
        .unwrap();
    assert!(
        graph
            .upload_image_region(
                "outside",
                image,
                ImageRegion {
                    origin: [3, 0],
                    ..patch
                },
                &[7; 16]
            )
            .is_err()
    );
    assert!(
        graph
            .upload_image_region("short", image, patch, &[7; 15])
            .is_err()
    );
    graph.export_image_mips(image, 1..2).unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["base", "patch"]
    );
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a real Vulkan GPU and the requested validation layers"]
fn vulkan_streams_mips_and_copies_atlas_regions() {
    streams_mips_and_copies_atlas_regions(renderer::runtime::RuntimeConfig::Vulkan(
        Default::default(),
    ));
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a real D3D12 GPU and the requested validation layers"]
fn d3d12_streams_mips_and_copies_atlas_regions() {
    streams_mips_and_copies_atlas_regions(renderer::runtime::RuntimeConfig::D3D12(
        Default::default(),
    ));
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn streams_mips_and_copies_atlas_regions(config: renderer::runtime::RuntimeConfig) {
    use renderer::{
        capacity::{ResourceClass, WorkingSet},
        runtime::{Runtime, RuntimeLimits},
    };
    use std::time::Duration;
    let atlas = atlas();
    let target = ImageDesc::rgba8(5, 3);
    let bc_atlas = ImageDesc {
        format: renderer::ImageFormat::Bc1RgbaUnorm,
        mip_levels: 4,
        ..ImageDesc::rgba8(16, 16)
    };
    let bc_tile = ImageDesc {
        format: renderer::ImageFormat::Bc1RgbaUnorm,
        ..ImageDesc::rgba8(4, 4)
    };
    let wait = Duration::from_secs(10);
    let mut rt = Runtime::new(
        config,
        RuntimeLimits::default(),
        WorkingSet {
            resources: &[
                ResourceClass::Image {
                    desc: bc_atlas,
                    count: 2,
                },
                ResourceClass::Image {
                    desc: bc_tile,
                    count: 1,
                },
                ResourceClass::Image {
                    desc: atlas,
                    count: 1,
                },
                ResourceClass::Image {
                    desc: target,
                    count: 1,
                },
            ],
            ..Default::default()
        },
    )
    .unwrap();
    #[cfg(feature = "vulkan")]
    let vk = rt.as_vulkan().map(|v| v.validation_monitor());
    #[cfg(all(windows, feature = "d3d12"))]
    let dx = rt.as_d3d12().map(|v| v.validation_monitor());
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut g = pool.begin().unwrap();
    let image = g.image("atlas", atlas).unwrap();
    let data: Vec<u8> = (0..8_u8).flat_map(|v| [v; 4]).collect();
    g.upload_image_region(
        "base mip 1",
        image,
        ImageRegion::mip(atlas, 1).unwrap(),
        &data,
    )
    .unwrap();
    g.export_image_mips(image, 1..2).unwrap();
    g.upload_image_region(
        "base mip 3",
        image,
        ImageRegion::mip(atlas, 3).unwrap(),
        &[11; 4],
    )
    .unwrap();
    g.export_image_mips(image, 3..4).unwrap();
    let sent = rt.submit(&g.compile().unwrap()).unwrap();
    let retained = sent.export_image(image).unwrap();
    let id = sent.id();
    drop(sent);
    drop(rt.wait_for_frame(id, wait).unwrap().unwrap());
    assert_eq!(
        [0, 1, 2, 3].map(|m| retained.is_mip_initialized(m)),
        [false, true, false, true]
    );
    let mut g = pool.begin().unwrap();
    let incomplete = g.import_image("partial atlas", &retained).unwrap();
    g.export_image(incomplete).unwrap();
    assert_eq!(
        g.compile().unwrap_err().kind(),
        GraphErrorKind::UninitializedRead
    );
    let mut g = pool.begin().unwrap();
    let image = g.import_image("resident atlas", &retained).unwrap();
    let patch = ImageRegion {
        mip_level: 1,
        origin: [1, 1],
        extent: [2, 1],
    };
    g.upload_image_region("patch", image, patch, &[91, 91, 91, 91, 92, 92, 92, 92])
        .unwrap();
    for level in [0, 2] {
        let mip = ImageRegion::mip(atlas, level).unwrap();
        let bytes = vec![3; (mip.extent[0] * mip.extent[1] * 4) as usize];
        g.upload_image_region("stream another mip", image, mip, &bytes)
            .unwrap();
    }
    g.export_image(image).unwrap();
    let dst = g.image("dst", target).unwrap();
    g.upload_image("background", dst, &[255; 60]).unwrap();
    let source = ImageRegion {
        mip_level: 1,
        origin: [1, 0],
        extent: [3, 2],
    };
    g.copy_image_region("atlas subrect", image, source, dst, 0, [2, 1])
        .unwrap();
    let read = g.readback_image("result", dst).unwrap();
    let sent = rt.submit(&g.compile().unwrap()).unwrap();
    let complete = sent.export_image(image).unwrap();
    assert!((0..4).all(|m| complete.is_mip_initialized(m)));
    let id = sent.id();
    drop(sent);
    let mut result = rt.wait_for_frame(id, wait).unwrap().unwrap();
    let mut bytes = [0; 60];
    result.readback_into(read, &mut bytes).unwrap();
    let expected = [
        255, 255, 255, 255, 255, 255, 255, 1, 2, 3, 255, 255, 91, 92, 7,
    ];
    for (rgba, value) in bytes.chunks_exact(4).zip(expected) {
        assert_eq!(rgba, [value; 4]);
    }
    drop(result);
    rt.delete(complete.key()).unwrap();
    let mut g = pool.begin().unwrap();
    let src = g.image("BC atlas", bc_atlas).unwrap();
    let tail = g.image("BC mip tail", bc_atlas).unwrap();
    let tile = g.image("BC tile", bc_tile).unwrap();
    let block = ImageRegion {
        mip_level: 1,
        origin: [4, 4],
        extent: [4, 4],
    };
    g.upload_image_region(
        "allocate BC mip",
        src,
        ImageRegion::mip(bc_atlas, 1).unwrap(),
        &[1; 32],
    )
    .unwrap();
    g.upload_image_region("update BC block", src, block, &[74; 8])
        .unwrap();
    g.copy_image_region("copy BC block", src, block, tile, 0, [0, 0])
        .unwrap();
    let mip_tail = ImageRegion::mip(bc_atlas, 3).unwrap();
    g.upload_image_region("upload tiny BC mip", src, mip_tail, &[99; 8])
        .unwrap();
    g.copy_image_region("copy tiny BC mip", src, mip_tail, tail, 3, [0, 0])
        .unwrap();
    g.export_image_mips(tail, 3..4).unwrap();
    let read = g.readback_image("read BC block", tile).unwrap();
    let sent = rt.submit(&g.compile().unwrap()).unwrap();
    let retained = sent.export_image(tail).unwrap();
    let id = sent.id();
    drop(sent);
    let mut result = rt.wait_for_frame(id, wait).unwrap().unwrap();
    let mut bytes = [0; 8];
    result.readback_into(read, &mut bytes).unwrap();
    assert_eq!(bytes, [74; 8]);
    drop(result);
    rt.delete(retained.key()).unwrap();
    drop(rt);
    #[cfg(feature = "vulkan")]
    if let Some(vk) = vk {
        let report = vk.report();
        assert!(report.is_clean(), "{report:?}");
        if cfg!(feature = "development-validation") {
            assert_eq!(report.core_validation, Some(true));
            assert_eq!(report.synchronization_validation, Some(true));
        }
    }
    #[cfg(all(windows, feature = "d3d12"))]
    if let Some(dx) = dx {
        let report = dx.snapshot();
        assert!(report.is_clean(), "{report:?}");
        if cfg!(feature = "development-validation") {
            assert!(
                report.debug_layer_enabled
                    && report.gpu_based_validation_enabled
                    && report.teardown_checked
            );
        }
    }
}
