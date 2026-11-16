//! Explicit working-set boundary regressions; normal token return is measured separately.

use super::{Backend, fixture, raster_imports};
use renderer::{
    D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig, D3D12ValidationMode,
    FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode, VulkanRuntime,
    graph::{Binding, BindingResource, BufferDesc, GraphPool, ImageDesc},
};

fn return_without_allocating<T>(value: T) {
    #[cfg(feature = "memory-profiling")]
    let before = renderer::measurement::Snapshot::now();
    drop(value);
    #[cfg(feature = "memory-profiling")]
    {
        let events = renderer::measurement::Snapshot::now().since(before);
        assert!(events.is_allocation_free(), "metadata return: {events:?}");
        println!("WORKING_SET_METADATA_RETURN={events:?}");
    }
}

pub(super) fn metadata(strict: bool) {
    use renderer::D3D12RuntimeErrorKind::CapacityExhausted;

    let mut dx = D3D12Runtime::new(D3D12RuntimeConfig {
        validation: if strict {
            D3D12ValidationMode::Required
        } else {
            D3D12ValidationMode::Disabled
        },
        capacity: D3D12FrameCapacity {
            executions: 1,
            results: 1,
            resources: 4,
            operations: 8,
            buffers: 2,
            buffer_bytes: 16,
            upload_bytes: 64,
            readback_bytes: 64,
            scratch_bytes: 16,
            ..Default::default()
        },
    })
    .unwrap();
    let mut pool = GraphPool::new(fixture::capacity(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let buffer = graph.buffer("persistent", BufferDesc { size: 16 }).unwrap();
    graph.upload_buffer("bytes", buffer, 0, &[7; 16]).unwrap();
    graph.export_buffer(buffer).unwrap();
    let mut pending = dx.submit(&graph.compile().unwrap()).unwrap();
    let exported = pending.export_buffer(buffer).unwrap();
    let exported_clone = exported.clone();
    let refreshed = dx.refresh_buffer(exported.key()).unwrap();
    let refreshed_clone = refreshed.clone();
    drop(<D3D12Runtime as Backend>::wait(&mut pending));
    // Completion alone does not return the submission's export metadata.
    assert_eq!(
        dx.prepare_graph_working_set(None, &[], &[], &[])
            .unwrap_err()
            .kind(),
        CapacityExhausted
    );
    dx.delete(exported.key()).unwrap();
    // Failed replacement must keep the old metadata anchors alive. Returning
    // the completed submission and each last snapshot remains allocation-free.
    for value in [exported, exported_clone, refreshed] {
        assert_eq!(
            dx.prepare_graph_working_set(None, &[], &[], &[])
                .unwrap_err()
                .kind(),
            CapacityExhausted
        );
        return_without_allocating(value);
    }
    return_without_allocating(pending);
    // Only an independently captured refresh token's final clone remains.
    assert_eq!(
        dx.prepare_graph_working_set(None, &[], &[], &[])
            .unwrap_err()
            .kind(),
        CapacityExhausted
    );
    return_without_allocating(refreshed_clone);

    let desc = ImageDesc::rgba8(1, 1);
    let classes = [D3D12ImageClass { desc, count: 1 }];
    dx.prepare_graph_working_set(None, &classes, &[], &[])
        .unwrap();
    let mut graph = pool.begin().unwrap();
    let image = graph.image("persistent image", desc).unwrap();
    graph.upload_image("pixel", image, &[7; 4]).unwrap();
    graph.export_image(image).unwrap();
    let mut pending = dx.submit(&graph.compile().unwrap()).unwrap();
    let snapshot = pending.export_image(image).unwrap();
    drop(<D3D12Runtime as Backend>::wait(&mut pending));
    dx.delete(snapshot.key()).unwrap();
    // Image-only metadata has no ImportRanges token. Its completed submission
    // must independently prevent replacement of the export slot backing.
    assert_eq!(
        dx.prepare_graph_working_set(None, &classes, &[], &[])
            .unwrap_err()
            .kind(),
        CapacityExhausted
    );
    return_without_allocating(pending);
    // An ImportImage is scalar metadata, so it need not be returned. A failed
    // replacement preserves the owner, so its explicitly deleted key stays stale.
    dx.prepare_graph_working_set(None, &classes, &[], &[])
        .unwrap();
    assert_eq!(
        dx.refresh_image(snapshot.key()).unwrap_err().kind(),
        renderer::D3D12RuntimeErrorKind::StaleResource
    );
    dx.shutdown().unwrap();
}

fn persistent_resize<B: Backend>(backend: &mut B) {
    let mut pool = GraphPool::new(fixture::capacity(), 4).unwrap();
    let mut graph = pool.begin().unwrap();
    let offset = graph
        .buffer("resident offset", BufferDesc { size: 16 })
        .unwrap();
    graph
        .upload_buffer("offset", offset, 0, &fixture::vec4_bytes([0.5; 4]))
        .unwrap();
    let source = graph.image("resident source", fixture::hdr()).unwrap();
    let destination = graph.image("resident destination", fixture::hdr()).unwrap();
    for image in [source, destination] {
        graph
            .upload_image(
                "resident pixels",
                image,
                &fixture::image_bytes(&[[2.0; 4]; fixture::PIXELS]),
            )
            .unwrap();
        graph.export_image(image).unwrap();
    }
    graph.export_buffer(offset).unwrap();
    let mut pending = backend.submit(&graph.compile().unwrap()).unwrap();
    let offset = B::buffer(&pending, offset);
    let source = B::image(&pending, source);
    let destination = B::image(&pending, destination);
    let key = offset.key();
    let mut graph = pool.begin().unwrap();
    let image = graph.import_image("unchanged source", &source).unwrap();
    let old_pass = graph.readback_image("old readonly plan", image).unwrap();
    let readonly = graph.compile().unwrap();
    drop(offset);
    drop(B::wait(&mut pending));
    drop(pending);

    // Exactly three registered + three prepared physical resources fit the
    // six-entry registry. Repeated resize cannot append a fresh registry each time.
    for width in [2, 3, 2, 3, 2, 3] {
        let resized = ImageDesc::rgba16_float(width, 2);
        let classes = [
            ResourceClass::Buffer {
                desc: BufferDesc { size: 512 },
                count: 2,
            },
            ResourceClass::Image {
                desc: resized,
                count: 1,
            },
        ];
        // Neither resident HDR image occurs in the new classes. The storage
        // Program is also added only after these images were created.
        backend.reconfigure(&classes).unwrap();
        let mut excessive = classes;
        excessive[1] = ResourceClass::Image {
            desc: resized,
            count: 2,
        };
        assert_eq!(
            backend.reconfigure(&excessive).unwrap_err(),
            "CapacityExhausted"
        );
        assert_eq!(
            backend
                .reconfigure(&[ResourceClass::Image {
                    desc: resized,
                    count: 0,
                }])
                .unwrap_err(),
            "InvalidInput"
        );

        // Rejected preparation preserves the old configuration, registrations
        // and contents. A plan compiled before the first change remains usable.
        let mut pending = backend.submit(&readonly).unwrap();
        let mut result = B::wait(&mut pending);
        let mut pixels = [0; fixture::IMAGE_BYTES];
        B::read(&mut result, old_pass, &mut pixels);
        assert_eq!(pixels, fixture::image_bytes(&[[2.0; 4]; fixture::PIXELS]));
        drop((pending, result));

        let offset = backend.fresh_buffer(key);
        let destination_snapshot = backend.fresh(destination.key());
        let mut graph = pool.begin().unwrap();
        let input = graph.import_image("retained source", &source).unwrap();
        let output = graph
            .import_image("retained destination", &destination_snapshot)
            .unwrap();
        let offset = graph.import_buffer("retained offset", &offset).unwrap();
        graph
            .dispatch(
                "new storage Program",
                &fixture::storage_program(),
                &[
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::ImageView(
                            graph.image_view(input, 0, 0).unwrap(),
                        ),
                    },
                    Binding {
                        group: 0,
                        binding: 1,
                        resource: BindingResource::Buffer {
                            buffer: offset,
                            offset: 0,
                            size: 16,
                        },
                    },
                    Binding {
                        group: 1,
                        binding: 0,
                        resource: BindingResource::ImageView(
                            graph.image_view(output, 0, 0).unwrap(),
                        ),
                    },
                ],
                [1, 2, 1],
            )
            .unwrap();
        let pass = graph
            .readback_image("retained storage output", output)
            .unwrap();
        let resized_image = graph.image("resized transient", resized).unwrap();
        let bytes = [9; 48];
        graph
            .upload_image(
                "resized bytes",
                resized_image,
                &bytes[..resized.byte_len().unwrap() as usize],
            )
            .unwrap();
        let resized_pass = graph
            .readback_image("resized readback", resized_image)
            .unwrap();
        let mut pending = backend.submit(&graph.compile().unwrap()).unwrap();
        let mut result = B::wait(&mut pending);
        B::read(&mut result, pass, &mut pixels);
        assert_eq!(pixels, fixture::image_bytes(&[[2.5; 4]; fixture::PIXELS]));
        let mut actual = [0; 48];
        B::read(
            &mut result,
            resized_pass,
            &mut actual[..resized.byte_len().unwrap() as usize],
        );
        assert_eq!(
            &actual[..resized.byte_len().unwrap() as usize],
            &bytes[..resized.byte_len().unwrap() as usize]
        );
    }
    backend.delete(key);
    backend.delete(source.key());
    backend.delete(destination.key());
    backend
        .reconfigure(&[
            ResourceClass::Buffer {
                desc: BufferDesc { size: 84 },
                count: 1,
            },
            ResourceClass::Buffer {
                desc: BufferDesc { size: 12 },
                count: 1,
            },
            ResourceClass::Image {
                desc: fixture::hdr(),
                count: 1,
            },
        ])
        .unwrap();
    // Registry migration must preserve the CPU-known index proof too.
    raster_imports(backend, true);
}

pub(super) fn reconfiguration(strict: bool) {
    let mut vk = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            resource_capacity: 6,
            ..Default::default()
        },
        if strict {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )
    .unwrap();
    let mut dx = D3D12Runtime::new(D3D12RuntimeConfig {
        validation: if strict {
            D3D12ValidationMode::Required
        } else {
            D3D12ValidationMode::Disabled
        },
        capacity: D3D12FrameCapacity {
            executions: 1,
            resources: 16,
            resource_capacity: 6,
            operations: 32,
            buffers: 2,
            buffer_bytes: 512,
            upload_bytes: 16384,
            readback_bytes: 16384,
            scratch_bytes: 512,
            ..Default::default()
        },
    })
    .unwrap();
    let vm = vk.validation_monitor();
    let dm = dx.validation_monitor();
    vk.prepare_working_set(
        FrameCapacity {
            graph: renderer::graph::GraphCapacity {
                draws: 1,
                attachments: 1,
                ..fixture::capacity()
            },
            ..Default::default()
        },
        &[
            ResourceClass::Buffer {
                desc: BufferDesc { size: 16 },
                count: 1,
            },
            ResourceClass::Image {
                desc: fixture::hdr(),
                count: 2,
            },
        ],
    )
    .unwrap();
    dx.prepare_graph_working_set(
        None,
        &[D3D12ImageClass {
            desc: fixture::hdr(),
            count: 2,
        }],
        &[],
        &[],
    )
    .unwrap();
    persistent_resize(&mut vk);
    persistent_resize(&mut dx);
    drop(vk);
    dx.shutdown().unwrap();
    let vr = vm.report();
    let dr = dm.snapshot();
    assert!(vr.is_clean() && dr.is_clean(), "{vr:?}; {dr:?}");
    if strict {
        assert_eq!(vr.core_validation, Some(true));
        assert_eq!(vr.synchronization_validation, Some(true));
        assert!(dr.debug_layer_enabled && dr.gpu_based_validation_enabled);
    }
    // The outer fixture's native owner stays live; it checks final teardown.
}
