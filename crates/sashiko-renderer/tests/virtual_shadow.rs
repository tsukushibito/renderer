#![cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]

use sashiko_renderer::{
    passes::virtual_shadow::{
        self, LEVELS, PAGES_PER_AXIS, PHYSICAL_PAGES, REQUEST_BYTES, TABLE_BYTES,
    },
    pbr::{Bounds, Projection, ShadowSettings, Sun, ViewInput},
};
use sashiko_renderer_core::{
    capacity::Preparation,
    graph::{Attachment, GraphCapacity, GraphPool, ImageDesc, LoadOp, RasterAttachments, StoreOp},
    runtime::{Runtime, RuntimeConfig, RuntimeLimits},
};
use std::{num::NonZeroUsize, time::Duration};

fn check_requests(mut config: RuntimeConfig) {
    // The sole pixel projects to the intersection of four pages at every level.
    let identity = std::array::from_fn(|col| std::array::from_fn(|row| f32::from(row == col)));
    let camera = ViewInput::new(
        identity,
        [0., 0., -0.01],
        [0., 0., 1.],
        [0.01, 1.01],
        Projection::Orthographic,
    )
    .unwrap();
    let sun = Sun::from_lux([0., 0., 1.], [1.; 3], 1.).unwrap();
    let bounds = Bounds::new([-1.; 3], [1.; 3]).unwrap();
    let settings = |distance| ShadowSettings {
        distance,
        ..Default::default()
    };
    let mut prep = Preparation::new(NonZeroUsize::MIN);
    virtual_shadow::reserve_requests(&mut prep).unwrap();
    virtual_shadow::reserve_table(&mut prep).unwrap();
    virtual_shadow::reserve_atlas(&mut prep).unwrap();
    virtual_shadow::reserve_visibility(&mut prep, [1, 1]).unwrap();
    config
        .fit_preparation(&prep, 128 * 1024, 65 * 1024 * 1024)
        .unwrap();
    let depth = ImageDesc::depth32(1, 1);
    prep.reserve_transient_image("depth", depth).unwrap();
    let limits = prep
        .runtime_limits(RuntimeLimits {
            graph: GraphCapacity::default(),
            ..Default::default()
        })
        .unwrap();
    let mut runtime = Runtime::new(config, limits, prep.working_set()).unwrap();
    let mut pool = GraphPool::new(limits.graph, 1).unwrap();
    for (z, distance, marked) in [(0.5, 60., true), (1., 60., false), (0.5, 0.1, false)] {
        let mut graph = pool.begin().unwrap();
        let depth = graph.image("depth", depth).unwrap();
        let view = graph.image_view(depth, 0, 0).unwrap();
        graph
            .raster(
                "camera depth",
                RasterAttachments {
                    colors: &[],
                    color_resolves: &[],
                    depth: Some(Attachment {
                        view,
                        load: LoadOp::Clear(z),
                        store: StoreOp::Store,
                    }),
                },
                |_| Ok(()),
            )
            .unwrap();
        let requests = virtual_shadow::request_pages(
            &mut graph,
            view,
            camera,
            sun,
            bounds,
            settings(distance),
        )
        .unwrap();
        let pages = virtual_shadow::assign_pages(&mut graph, requests, None).unwrap();
        let capture = graph
            .readback_buffer("table entries", pages.table, 0, TABLE_BYTES as u64)
            .unwrap();
        let plan = graph.compile().unwrap();
        let submitted = runtime.submit(&plan).unwrap().id();
        let mut bits = vec![255; TABLE_BYTES];
        runtime
            .wait_for_frame(submitted, Duration::from_secs(30))
            .unwrap()
            .unwrap()
            .readback_into(capture, &mut bits)
            .unwrap();
        let mut expected = vec![0u32; TABLE_BYTES / 4];
        if marked {
            let mut slot = 1;
            for level in std::iter::once(LEVELS - 1).chain(0..LEVELS - 1) {
                for y in [63, 64] {
                    for x in [63, 64] {
                        expected[(level * PAGES_PER_AXIS + y) * PAGES_PER_AXIS + x] = slot;
                        slot += 1;
                    }
                }
            }
        }
        assert!(
            bits.as_chunks::<4>()
                .0
                .iter()
                .map(|b| u32::from_le_bytes(*b))
                .eq(expected),
            "unexpected virtual pages for depth {z}, distance {distance}"
        );
    }
    // Flood the nearest level but reserve one coarse page. It must survive
    // exhaustion, and every physical tile can have at most one owner.
    let mut graph = pool.begin().unwrap();
    let mask_buffer = graph
        .buffer(
            "pressure",
            sashiko_renderer_core::graph::BufferDesc {
                size: REQUEST_BYTES as u64,
            },
        )
        .unwrap();
    let mut mask = vec![0; REQUEST_BYTES];
    mask[..REQUEST_BYTES / LEVELS].fill(255);
    mask[REQUEST_BYTES - 1] = 128;
    graph
        .upload_buffer("all fine and one coarse", mask_buffer, 0, &mask)
        .unwrap();
    let camera_bytes = 272;
    let uniform = graph
        .buffer(
            "zero origins",
            sashiko_renderer_core::graph::BufferDesc { size: camera_bytes },
        )
        .unwrap();
    graph
        .upload_buffer("origins", uniform, 0, &[0; 272])
        .unwrap();
    let requests = virtual_shadow::PageRequests {
        mask: mask_buffer,
        camera: uniform,
    };
    let pages = virtual_shadow::assign_pages(&mut graph, requests, None).unwrap();
    let capture = graph
        .readback_buffer("table", pages.table, 0, TABLE_BYTES as u64)
        .unwrap();
    let plan = graph.compile().unwrap();
    let submitted = runtime.submit(&plan).unwrap().id();
    let mut bytes = vec![0; TABLE_BYTES];
    runtime
        .wait_for_frame(submitted, Duration::from_secs(30))
        .unwrap()
        .unwrap()
        .readback_into(capture, &mut bytes)
        .unwrap();
    let entries = bytes.as_chunks::<4>().0;
    for (i, &entry) in entries.iter().enumerate() {
        let expected = if i == entries.len() - 1 {
            1
        } else if i < PHYSICAL_PAGES - 1 {
            i as u32 + 2
        } else {
            0
        };
        assert_eq!(u32::from_le_bytes(entry), expected);
    }
    drop(plan);
    // Slot zero already owns the coarse page (-1,-1). Other requested
    // pages must be cleared, adjacent unallocated and matching slots retained.
    for mode in 0..3 {
        let mut graph = pool.begin().unwrap();
        let depth = graph.image("depth", depth).unwrap();
        let view = graph.image_view(depth, 0, 0).unwrap();
        graph
            .raster(
                "a receiver",
                RasterAttachments {
                    colors: &[],
                    color_resolves: &[],
                    depth: Some(Attachment {
                        view,
                        load: LoadOp::Clear(0.5),
                        store: StoreOp::Store,
                    }),
                },
                |_| Ok(()),
            )
            .unwrap();
        let requests =
            virtual_shadow::request_pages(&mut graph, view, camera, sun, bounds, settings(60.))
                .unwrap();
        if mode != 0 {
            // Keep four coarse tiles; optionally keep only the centre fine tile.
            // The top-left PCF tap must then find the retained, dark coarse tile.
            let mut sparse = vec![0u8; REQUEST_BYTES];
            for (level, x, y) in [
                (7, 63, 63),
                (7, 63, 64),
                (7, 64, 63),
                (7, 64, 64),
                (0, 64, 64),
            ] {
                if level == 0 && mode == 2 {
                    continue;
                }
                let index = (level * 128 + y) * 128 + x;
                sparse[index / 8] |= 1 << (index % 8);
            }
            graph
                .upload_buffer("force parent fallback", requests.mask, 0, &sparse)
                .unwrap();
        }
        let prior = graph
            .buffer(
                "prior owners",
                sashiko_renderer_core::graph::BufferDesc {
                    size: virtual_shadow::SLOT_BYTES as u64,
                },
            )
            .unwrap();
        let mut prior_bytes = vec![0; virtual_shadow::SLOT_BYTES];
        prior_bytes[..16].copy_from_slice(
            [8u32, u32::MAX, u32::MAX, 1]
                .map(u32::to_le_bytes)
                .as_flattened(),
        );
        graph
            .upload_buffer("seed owners", prior, 0, &prior_bytes)
            .unwrap();
        let pages = virtual_shadow::assign_pages(&mut graph, requests, Some(prior)).unwrap();
        let atlas = graph.image("atlas", virtual_shadow::ATLAS_DESC).unwrap();
        let atlas_view = graph.image_view(atlas, 0, 0).unwrap();
        graph
            .raster(
                "previous caster depth",
                RasterAttachments {
                    colors: &[],
                    color_resolves: &[],
                    depth: Some(Attachment {
                        view: atlas_view,
                        load: LoadOp::Clear(0.25),
                        store: StoreOp::Store,
                    }),
                },
                |_| Ok(()),
            )
            .unwrap();
        virtual_shadow::draw_atlas(&mut graph, atlas, &pages, true, |_| Ok(())).unwrap();
        let visibility =
            virtual_shadow::resolve_visibility(&mut graph, view, requests, &pages, atlas).unwrap();
        let visible = graph.readback_image("filtered sun", visibility).unwrap();
        let capture =
            (mode == 0).then(|| graph.readback_image("atlas after update", atlas).unwrap());
        let plan = graph.compile().unwrap();
        let submitted = runtime.submit(&plan).unwrap().id();
        let mut frame = runtime
            .wait_for_frame(submitted, Duration::from_secs(30))
            .unwrap()
            .unwrap();
        let mut result = [0u8; 8];
        frame.readback_into(visible, &mut result).unwrap();
        let red = u16::from_le_bytes([result[0], result[1]]);
        if mode == 0 {
            assert_eq!(red, 0x3c00);
        } else {
            // 8/9 rounded to binary16, allowing either adjacent value.
            assert!(
                (0x3b1c..=0x3b1d).contains(&red),
                "mode {mode}, got {red:#06x}"
            );
        }
        let Some(capture) = capture else {
            continue;
        };
        let mut pixels = vec![0u8; 4096 * 4096 * 4];
        frame.readback_into(capture, &mut pixels).unwrap();
        for (y, row) in pixels.as_chunks::<{ 4096 * 4 }>().0.iter().enumerate() {
            let split = if y < 128 { 128 } else { 4096 };
            assert!(
                row[..split * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|v| *v == 0.25_f32.to_le_bytes())
            );
            assert!(
                row[split * 4..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|v| *v == 1.0_f32.to_le_bytes())
            );
        }
    }
    match config {
        #[cfg(feature = "vulkan")]
        RuntimeConfig::Vulkan(_) => {
            let monitor = runtime.as_vulkan().unwrap().validation_monitor();
            drop(runtime);
            let report = monitor.report();
            assert!(report.is_clean(), "{report:?}");
            if cfg!(feature = "development-validation") {
                assert_eq!(report.core_validation, Some(true));
                assert_eq!(report.synchronization_validation, Some(true));
            }
        }
        #[cfg(all(windows, feature = "d3d12"))]
        RuntimeConfig::D3D12(_) => {
            let monitor = runtime.as_d3d12().unwrap().validation_monitor();
            drop(runtime);
            let report = monitor.snapshot();
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
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires a Vulkan GPU"]
fn vulkan_virtual_page_requests() {
    check_requests(RuntimeConfig::Vulkan(Default::default()));
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires a D3D12 GPU"]
fn d3d12_virtual_page_requests() {
    check_requests(RuntimeConfig::D3D12(Default::default()));
}
