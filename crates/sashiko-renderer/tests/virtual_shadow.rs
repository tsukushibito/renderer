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

#[path = "virtual_shadow/timing.rs"]
mod timing;

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
    virtual_shadow::reserve_invalidation(&mut prep).unwrap();
    virtual_shadow::reserve_atlas(&mut prep, 1).unwrap();
    virtual_shadow::reserve_camera_depth(&mut prep, [1, 1], 1).unwrap();
    prep.add_working_set(sashiko_renderer_core::capacity::WorkingSet {
        samplers: &[sashiko_renderer_core::program::SamplerDescriptor::LinearRepeat],
        ..Default::default()
    })
    .unwrap();
    virtual_shadow::reserve_visibility(&mut prep, [1, 1]).unwrap();
    prep.reserve_transient_image("coverage", ImageDesc::rgba8(2, 1))
        .unwrap();
    for size in [4 * 48, 24, 64, 64] {
        prep.reserve_buffer(sashiko_renderer_core::graph::BufferDesc { size }, 1)
            .unwrap();
    }
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
    let sky = graph.image("empty view", depth).unwrap();
    let sky = graph.image_view(sky, 0, 0).unwrap();
    graph
        .raster(
            "sky",
            RasterAttachments {
                colors: &[],
                color_resolves: &[],
                depth: Some(Attachment {
                    view: sky,
                    load: LoadOp::Clear(1.),
                    store: StoreOp::Store,
                }),
            },
            |_| Ok(()),
        )
        .unwrap();
    let mut requests =
        virtual_shadow::request_pages(&mut graph, sky, camera, sun, bounds, settings(60.)).unwrap();
    requests.mask = mask_buffer;
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
    for mode in 0..4 {
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
            // An absent fine centre must resolve its whole footprint in the coarse tile.
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
            if mode == 3 {
                for x in [68, 80] {
                    let i = 64 * PAGES_PER_AXIS + x;
                    sparse[i / 8] |= 1 << (i % 8);
                }
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
        if mode == 2 {
            prior_bytes[3 * 16..4 * 16]
                .copy_from_slice([8u32, 0, 0, 1].map(u32::to_le_bytes).as_flattened());
        }
        if mode == 3 {
            for (slot, page_x) in [(4, 0), (5, 4), (6, 16)] {
                prior_bytes[slot * 16..(slot + 1) * 16]
                    .copy_from_slice([1u32, page_x, 0, 0].map(u32::to_le_bytes).as_flattened());
            }
        }
        graph
            .upload_buffer("seed owners", prior, 0, &prior_bytes)
            .unwrap();
        let pages = virtual_shadow::assign_pages(&mut graph, requests, Some(prior)).unwrap();
        let dirty = if mode == 3 {
            // A caster is removed from the old tile's outer corner and added
            // to a second tile's outer corner. A third retained tile is remote.
            let changed = [0., 0.16].map(|x| {
                Some(
                    Bounds::new(
                        [x - 1.5 / 3200., -1.5 / 3200., 0.5],
                        [x - 0.75 / 3200., -0.75 / 3200., 1.0],
                    )
                    .unwrap(),
                )
            });
            virtual_shadow::invalidate_casters(&mut graph, &requests, &pages, &changed).unwrap();
            Some(
                graph
                    .readback_buffer("three fine owners", pages.slots, 4 * 16, 3 * 16)
                    .unwrap(),
            )
        } else {
            None
        };
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
        if let Some(dirty) = dirty {
            let mut bytes = [0u8; 3 * 16];
            frame.readback_into(dirty, &mut bytes).unwrap();
            let flags: Vec<_> = bytes
                .as_chunks::<16>()
                .0
                .iter()
                .map(|slot| u32::from_le_bytes(slot[12..].try_into().unwrap()))
                .collect();
            assert_eq!(flags, [1, 1, 0]);
        }
        let red = u16::from_le_bytes([result[0], result[1]]);
        assert_eq!(red, if mode == 2 { 0 } else { 0x3c00 }, "mode {mode}");
        let Some(capture) = capture else {
            continue;
        };
        let mut pixels = vec![0u8; 4096 * 4096 * 4];
        frame.readback_into(capture, &mut pixels).unwrap();
        for (y, row) in pixels.as_chunks::<{ 4096 * 4 }>().0.iter().enumerate() {
            for (x, value) in row.as_chunks::<4>().0.iter().enumerate() {
                let tile = virtual_shadow::TILE_TEXELS;
                let columns = virtual_shadow::TILES_PER_AXIS;
                let slot = y / tile * columns + x / tile;
                let dirty = x / tile < columns && (1..32).contains(&slot);
                assert_eq!(
                    *value,
                    if dirty { 1.0_f32 } else { 0.25 }.to_le_bytes(),
                    "atlas pixel {x},{y}"
                );
            }
        }
    }
    check_casters(
        &mut runtime,
        &mut pool,
        camera,
        sun,
        Bounds::new([-2.; 3], [2.; 3]).unwrap(),
    );
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

fn check_casters(
    runtime: &mut Runtime,
    pool: &mut GraphPool,
    camera: ViewInput,
    sun: Sun,
    bounds: Bounds,
) {
    use sashiko_renderer::pbr::{
        AlphaMode, GraphMaterial, MaterialFactors, MaterialTextures, MaterialUniform, Transform,
    };
    use sashiko_renderer_core::graph::BufferDesc;
    // An opaque square or an alpha-tested leaf covers the receiver. Reversing
    // its winding tests single-sided, double-sided, and mirrored placements.
    let materials = [
        (AlphaMode::Opaque, [0, 0], false, false, false, 0),
        (AlphaMode::Mask, [0, 0], false, false, false, 0x3c00),
        (AlphaMode::Mask, [255, 255], false, false, false, 0),
        (AlphaMode::Mask, [255, 255], true, false, false, 0x3c00),
        (AlphaMode::Mask, [255, 255], true, true, false, 0),
        (AlphaMode::Mask, [255, 255], false, false, true, 0),
        (AlphaMode::Mask, [0, 255], false, true, false, 0x3800),
        (AlphaMode::Blend, [255, 255], false, true, false, 0x3c00),
    ];
    // A receiver sits 1/8 texel inside an edge or corner. Only its centre
    // page is resident. A square stops 3/4 texel outside the interior, so
    // only the outer gutter texel can store its depth. Its PCF weight is
    // (1/2 - 1/8) / 3 = 1/8 per crossed axis.
    let gutters = [
        [-1, -1],
        [-1, 1],
        [1, -1],
        [1, 1],
        [1, 0],
        [-1, 0],
        [0, 1],
        [0, -1],
    ];
    let cases = materials
        .into_iter()
        .map(|case| (case, [0, 0]))
        .chain(gutters.map(|side| {
            let expected = if side.contains(&0) { 0x3b00 } else { 0x3be0 };
            (
                (AlphaMode::Opaque, [255; 2], false, true, false, expected),
                side,
            )
        }));
    for ((mode, alpha, back, double_sided, mirror, expected), side) in cases {
        let camera = if side == [0, 0] {
            camera
        } else {
            let [x, y] = side.map(|v| v as f32 * 0.125 / 3200.);
            let mut clip = camera.clip_from_world();
            clip[3][0] -= x;
            clip[3][1] -= y;
            ViewInput::new(
                clip,
                [x, y, -0.01],
                [0., 0., 1.],
                [0.01, 1.01],
                Projection::Orthographic,
            )
            .unwrap()
        };
        let mut graph = pool.begin().unwrap();
        let vertices = graph.buffer("square", BufferDesc { size: 4 * 48 }).unwrap();
        let mut words = [0f32; 48];
        for (v, (x, y)) in words.as_chunks_mut::<12>().0.iter_mut().zip([
            (-0.02, -0.02),
            (0.02, -0.02),
            (0.02, 0.02),
            (-0.02, 0.02),
        ]) {
            v[..3].copy_from_slice(&[x, y, 1.5]);
            v[10] = x * 25. + 0.5;
            v[11] = y * 25. + 0.5;
        }
        graph
            .upload_buffer(
                "positions and UVs",
                vertices,
                0,
                words.map(f32::to_le_bytes).as_flattened(),
            )
            .unwrap();
        let indices = graph.buffer("faces", BufferDesc { size: 24 }).unwrap();
        let order = if back {
            [0, 2, 1, 0, 3, 2]
        } else {
            [0, 1, 2, 0, 2, 3]
        };
        graph
            .upload_buffer(
                "winding",
                indices,
                0,
                order.map(u32::to_le_bytes).as_flattened(),
            )
            .unwrap();
        let texture = graph.image("coverage", ImageDesc::rgba8(2, 1)).unwrap();
        graph
            .upload_image(
                "alpha texels",
                texture,
                &[255, 255, 255, alpha[0], 255, 255, 255, alpha[1]],
            )
            .unwrap();
        let texture = graph.image_view(texture, 0, 0).unwrap();
        let factors = MaterialUniform::upload(
            &mut graph,
            "factors",
            MaterialFactors {
                alpha_mode: mode,
                double_sided,
                ..Default::default()
            },
        )
        .unwrap();
        let material = GraphMaterial::new(factors, MaterialTextures::uniform(texture));
        let mesh = sashiko_renderer::pipeline::Mesh {
            bounds: Bounds::new([-0.02, -0.02, 1.5], [0.02, 0.02, 1.5]).unwrap(),
            vertex_stride: 48,
            vertices,
            indices,
            first_index: 0,
            index_count: 6,
        };
        let projection = virtual_shadow::camera_matrix(&mut graph, camera).unwrap();
        let receiver_factors = MaterialUniform::upload(
            &mut graph,
            "receiver surface",
            MaterialFactors {
                double_sided: true,
                ..Default::default()
            },
        )
        .unwrap();
        let receiver = GraphMaterial::new(receiver_factors, MaterialTextures::uniform(texture));
        let receiver_pose =
            Transform::new([[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., -1.]]).unwrap();
        let receiver =
            virtual_shadow::bind_receiver(&mut graph, projection, mesh, receiver_pose, receiver)
                .unwrap()
                .unwrap();
        let depth = virtual_shadow::draw_camera_depth(&mut graph, [1, 1], &[receiver]).unwrap();
        let requests = virtual_shadow::request_pages(
            &mut graph,
            depth,
            camera,
            sun,
            bounds,
            ShadowSettings::default(),
        )
        .unwrap();
        if side != [0, 0] {
            let mut center_only = vec![0; REQUEST_BYTES];
            let index = 64 * PAGES_PER_AXIS + 64;
            center_only[index / 8] = 1 << (index % 8);
            graph
                .upload_buffer("only centre page", requests.mask, 0, &center_only)
                .unwrap();
        }
        let pages = virtual_shadow::assign_pages(&mut graph, requests, None).unwrap();
        let atlas = graph.image("atlas", virtual_shadow::ATLAS_DESC).unwrap();
        let transform = if side != [0, 0] {
            let [x, y] = side.map(|v| -v as f32 * (0.02 + 0.75 / 3200.));
            Transform::new([[1., 0., 0., x], [0., 1., 0., y], [0., 0., 1., 0.]]).unwrap()
        } else if mirror {
            Transform::new([[-1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]]).unwrap()
        } else {
            Transform::IDENTITY
        };
        let caster =
            virtual_shadow::bind_caster(&mut graph, &requests, &pages, mesh, transform, material)
                .unwrap();
        virtual_shadow::draw_atlas(&mut graph, atlas, &pages, false, |pass| {
            if let Some(caster) = caster {
                caster.record(pass)?;
            }
            Ok(())
        })
        .unwrap();
        let image =
            virtual_shadow::resolve_visibility(&mut graph, depth, requests, &pages, atlas).unwrap();
        let capture = graph.readback_image("shadow", image).unwrap();
        let plan = graph.compile().unwrap();
        let id = runtime.submit(&plan).unwrap().id();
        let mut pixel = [0u8; 8];
        runtime
            .wait_for_frame(id, Duration::from_secs(30))
            .unwrap()
            .unwrap()
            .readback_into(capture, &mut pixel)
            .unwrap();
        assert_eq!(
            u16::from_le_bytes([pixel[0], pixel[1]]),
            expected,
            "{mode:?} alpha {alpha:?} back {back} double {double_sided} mirror {mirror} gutter {side:?}"
        );
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
