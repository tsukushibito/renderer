use renderer::graph::{
    AccessRange, Binding, BufferDesc, BufferHandle, Graph, GraphCapacity, GraphCapacityKind,
    GraphErrorKind, GraphPool,
};
use renderer::program::ManagedProgram;

fn buffer(graph: &mut Graph, name: &str, size: u64) -> BufferHandle {
    graph.buffer(name, BufferDesc { size }).unwrap()
}

#[test]
fn sampled_mip_chain_requires_every_level_and_full_upload_initializes_it() {
    use renderer::graph::{
        Attachment, BindingResource, ImageDesc, LoadOp, RasterAttachments, StoreOp,
    };
    use renderer::program::SamplerDescriptor;

    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for complete in [false, true] {
        let mut graph = pool.begin().unwrap();
        let image = graph
            .image(
                "two mip levels",
                ImageDesc {
                    mip_levels: 2,
                    ..ImageDesc::rgba8(2, 2)
                },
            )
            .unwrap();
        let view = graph.sampled_image_view(image, 0, 2).unwrap();
        assert_eq!(view.mip_level_count(), 2);
        if complete {
            // Sixteen base-level bytes followed by the four-byte 1x1 level.
            graph
                .upload_image("complete chain", image, &[17; 20])
                .unwrap();
        } else {
            graph
                .attachment_pass(
                    "base level only",
                    RasterAttachments {
                        colors: &[Attachment {
                            view: graph.image_view(image, 0, 0).unwrap(),
                            load: LoadOp::Clear([0.0; 4]),
                            store: StoreOp::Store,
                        }],
                        depth: None,
                    },
                )
                .unwrap();
        }
        let output = buffer(&mut graph, "sample output", 16);
        graph
            .dispatch(
                "sample chain",
                &ManagedProgram::sampled_readback(),
                &[
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::ImageView(view),
                    },
                    Binding {
                        group: 0,
                        binding: 1,
                        resource: BindingResource::Sampler(SamplerDescriptor::LinearRepeat),
                    },
                    Binding {
                        group: 1,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: output,
                            offset: 0,
                            size: 16,
                        },
                    },
                ],
                [1; 3],
            )
            .unwrap();
        graph.readback_buffer("result", output, 0, 16).unwrap();
        if complete {
            assert_eq!(graph.compile().unwrap().pass_count(), 3);
        } else {
            let error = graph.compile().unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
            assert_eq!(error.resource(), Some(image.into()));
        }
    }
}

#[test]
fn sampled_view_range_checks_do_not_change_transfer_or_attachment_contracts() {
    use renderer::graph::{Attachment, ImageDesc, LoadOp, RasterAttachments, StoreOp};
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let image = graph
        .image(
            "chain",
            ImageDesc {
                mip_levels: 3,
                ..ImageDesc::rgba8(4, 2)
            },
        )
        .unwrap();
    for (base, count) in [(0, 0), (3, 1), (2, 2), (1, u32::MAX)] {
        assert_eq!(
            graph
                .sampled_image_view(image, base, count)
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput
        );
    }
    let view = graph.sampled_image_view(image, 1, 2).unwrap();
    assert_eq!(view.mip_level(), 1);
    assert_eq!(view.mip_level_count(), 2);
    assert!(
        graph
            .attachment_pass(
                "not an attachment view",
                RasterAttachments {
                    colors: &[Attachment {
                        view,
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store
                    }],
                    depth: None,
                }
            )
            .is_err()
    );
    assert!(
        graph
            .upload_image("base bytes only", image, &[0; 32])
            .is_err()
    );
    assert!(
        graph
            .readback_image("whole chain readback is unsupported", image)
            .is_err()
    );
    let srgb = graph
        .image(
            "sRGB sampled color",
            ImageDesc {
                format: renderer::ImageFormat::Rgba8Srgb,
                ..ImageDesc::rgba8(1, 1)
            },
        )
        .unwrap();
    let srgb_view = graph.image_view(srgb, 0, 0).unwrap();
    assert!(
        graph
            .attachment_pass(
                "sRGB is sampled-only",
                RasterAttachments {
                    colors: &[Attachment {
                        view: srgb_view,
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store
                    }],
                    depth: None,
                }
            )
            .is_err()
    );
    graph
        .upload_image("sRGB bytes are preserved", srgb, &[0, 128, 255, 17])
        .unwrap();
    graph.readback_image("sRGB component bits", srgb).unwrap();
    graph.compile().unwrap();
}

fn bindings(input: BufferHandle, output: BufferHandle) -> [Binding; 2] {
    [
        Binding {
            group: 0,
            binding: 0,
            resource: renderer::graph::BindingResource::Buffer {
                buffer: input,
                offset: 0,
                size: 16,
            },
        },
        Binding {
            group: 1,
            binding: 0,
            resource: renderer::graph::BindingResource::Buffer {
                buffer: output,
                offset: 0,
                size: 16,
            },
        },
    ]
}

fn set_buffer(
    binding: &mut Binding,
    buffer: Option<BufferHandle>,
    offset: Option<u64>,
    size: Option<u64>,
) {
    let renderer::graph::BindingResource::Buffer {
        buffer: current_buffer,
        offset: current_offset,
        size: current_size,
    } = &mut binding.resource
    else {
        panic!("expected a buffer test input")
    };
    if let Some(value) = buffer {
        *current_buffer = value;
    }
    if let Some(value) = offset {
        *current_offset = value;
    }
    if let Some(value) = size {
        *current_size = value;
    }
}

#[test]
fn short_lived_bindings_build_a_two_dispatch_producer_chain() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let source = buffer(&mut graph, "uniform", 48);
    let intermediate = buffer(&mut graph, "intermediate", 32);
    let output = buffer(&mut graph, "output", 48);
    graph
        .upload_buffer("uniform bytes", source, 16, &[7; 16])
        .unwrap();
    {
        let mut input = bindings(source, intermediate);
        set_buffer(&mut input[0], None, Some(16), None);
        set_buffer(&mut input[0], None, None, Some(32));
        set_buffer(&mut input[1], None, None, Some(32));
        input.swap(0, 1);
        graph
            .dispatch(
                "producer",
                &ManagedProgram::uniform_readback(),
                &input,
                [1; 3],
            )
            .unwrap();
        // Neither the caller's slot ordering nor its subsequent edits belong
        // to the frozen plan.
        let overwritten = input[0];
        input.fill(overwritten);
        let mut reader = bindings(intermediate, output);
        set_buffer(&mut reader[0], None, None, Some(32));
        set_buffer(&mut reader[1], None, Some(16), None);
        set_buffer(&mut reader[1], None, None, Some(32));
        graph
            .dispatch(
                "reader",
                &ManagedProgram::storage_readback(),
                &reader,
                [1; 3],
            )
            .unwrap();
    }
    graph.readback_buffer("result", output, 16, 16).unwrap();
    let plan = graph.compile().unwrap();
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        ["uniform bytes", "producer", "reader", "result"]
    );
}

#[test]
fn descriptor_tail_is_neither_read_nor_initialized_by_the_shader() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    for read_tail in [false, true] {
        let mut graph = pool.begin().unwrap();
        let source = buffer(&mut graph, "uniform", 32);
        let output = buffer(&mut graph, "output", 32);
        graph
            .upload_buffer("prefix only", source, 0, &[9; 16])
            .unwrap();
        let mut input = bindings(source, output);
        set_buffer(&mut input[0], None, None, Some(32));
        set_buffer(&mut input[1], None, None, Some(32));
        graph
            .dispatch(
                "write prefix",
                &ManagedProgram::uniform_readback(),
                &input,
                [1; 3],
            )
            .unwrap();
        let offset = if read_tail { 16 } else { 0 };
        let readback = graph.readback_buffer("result", output, offset, 16).unwrap();
        if read_tail {
            let error = graph.compile().unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
            assert_eq!(error.pass(), Some(readback));
            assert_eq!(error.resource(), Some(output.into()));
            assert_eq!(error.range(), Some(&AccessRange::Buffer(16..32)));
        } else {
            assert_eq!(graph.compile().unwrap().pass_count(), 3);
        }
    }

    let mut graph = pool.begin().unwrap();
    let source = buffer(&mut graph, "uniform", 16);
    let intermediate = buffer(&mut graph, "intermediate", 32);
    let output = buffer(&mut graph, "output", 16);
    graph.upload_buffer("seed", source, 0, &[0; 16]).unwrap();
    let mut producer = bindings(source, intermediate);
    set_buffer(&mut producer[1], None, None, Some(32));
    graph
        .dispatch(
            "producer",
            &ManagedProgram::uniform_readback(),
            &producer,
            [1; 3],
        )
        .unwrap();
    let mut reader = bindings(intermediate, output);
    set_buffer(&mut reader[0], None, Some(16), None);
    let read = graph
        .dispatch(
            "read unwritten tail",
            &ManagedProgram::storage_readback(),
            &reader,
            [1; 3],
        )
        .unwrap();
    graph.readback_buffer("result", output, 0, 16).unwrap();
    let error = graph.compile().unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
    assert_eq!(error.pass(), Some(read));
    assert_eq!(error.resource(), Some(intermediate.into()));
    assert_eq!(error.range(), Some(&AccessRange::Buffer(16..32)));
}

#[test]
fn overlapping_descriptors_with_disjoint_shader_footprints_are_supported() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let storage = buffer(&mut graph, "storage", 48);
    graph.upload_buffer("seed", storage, 0, &[3; 16]).unwrap();
    let mut input = bindings(storage, storage);
    set_buffer(&mut input[0], None, None, Some(32));
    set_buffer(&mut input[1], None, Some(16), None);
    set_buffer(&mut input[1], None, None, Some(32));
    graph
        .dispatch(
            "copy prefix",
            &ManagedProgram::storage_readback(),
            &input,
            [1; 3],
        )
        .unwrap();
    graph.readback_buffer("result", storage, 16, 16).unwrap();
    assert_eq!(graph.compile().unwrap().pass_count(), 3);
}

#[test]
fn a_reader_keeps_only_the_latest_declared_producer() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let first = buffer(&mut graph, "first", 16);
    let second = buffer(&mut graph, "second", 16);
    let intermediate = buffer(&mut graph, "intermediate", 16);
    let output = buffer(&mut graph, "output", 16);
    for (name, source) in [("old", first), ("current", second)] {
        graph.upload_buffer(name, source, 0, &[0; 16]).unwrap();
        graph
            .dispatch(
                name,
                &ManagedProgram::uniform_readback(),
                &bindings(source, intermediate),
                [1; 3],
            )
            .unwrap();
    }
    graph
        .dispatch(
            "reader",
            &ManagedProgram::storage_readback(),
            &bindings(intermediate, output),
            [1; 3],
        )
        .unwrap();
    graph.readback_buffer("result", output, 0, 16).unwrap();
    let plan = graph.compile().unwrap();
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        ["current", "current", "reader", "result"]
    );
    #[cfg(feature = "graph-diagnostics")]
    assert_eq!(plan.culled_pass_names().collect::<Vec<_>>(), ["old", "old"]);
}

#[test]
fn explicit_reordering_cannot_reverse_live_dispatch_hazards() {
    for hazard in ["RAW", "WAR", "WAW"] {
        for reverse in [false, true] {
            let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
            let mut graph = pool.begin().unwrap();
            let source = buffer(&mut graph, "uniform", 16);
            let shared = buffer(&mut graph, "shared", 16);
            let output = buffer(&mut graph, "output", 16);
            graph.upload_buffer("seed", source, 0, &[0; 16]).unwrap();
            let first = graph
                .dispatch(
                    "first write",
                    &ManagedProgram::uniform_readback(),
                    &bindings(source, shared),
                    [1; 3],
                )
                .unwrap();
            let reader = if hazard != "WAW" {
                Some(
                    graph
                        .dispatch(
                            "read",
                            &ManagedProgram::storage_readback(),
                            &bindings(shared, output),
                            [1; 3],
                        )
                        .unwrap(),
                )
            } else {
                None
            };
            let (before, after) = if hazard == "RAW" {
                graph.readback_buffer("result", output, 0, 16).unwrap();
                (first, reader.unwrap())
            } else {
                let last = graph
                    .dispatch(
                        "last write",
                        &ManagedProgram::uniform_readback(),
                        &bindings(source, shared),
                        [1; 3],
                    )
                    .unwrap();
                let result = graph.readback_buffer("result", shared, 0, 16).unwrap();
                if let Some(reader) = reader {
                    graph.readback_buffer("read result", output, 0, 16).unwrap();
                    (reader, last)
                } else {
                    // Both writers must be live before testing their WAW edge.
                    graph.depends_on(result, first).unwrap();
                    (first, last)
                }
            };
            if reverse {
                graph.depends_on(before, after).unwrap();
                let error = graph.compile().unwrap_err();
                assert_eq!(error.kind(), GraphErrorKind::DependencyCycle, "{hazard}");
                assert!(error.cycle_passes().contains(&before), "{hazard}");
                assert!(error.cycle_passes().contains(&after), "{hazard}");
            } else {
                let plan = graph.compile().unwrap();
                let names = plan.ordered_pass_names().collect::<Vec<_>>();
                let before_name = if hazard == "WAR" {
                    "read"
                } else {
                    "first write"
                };
                let after_name = if hazard == "RAW" {
                    "read"
                } else {
                    "last write"
                };
                assert!(
                    names
                        .iter()
                        .position(|&name| name == before_name)
                        .expect("the earlier dispatch is live")
                        < names
                            .iter()
                            .position(|&name| name == after_name)
                            .expect("the later dispatch is live"),
                    "{hazard}"
                );
            }
        }
    }
}

#[test]
fn invalid_bindings_are_rejected_transactionally_with_slot_context() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            bindings: 2,
            ..GraphCapacity::default()
        },
        2,
    )
    .unwrap();
    let mut foreign = pool.begin().unwrap();
    let foreign_buffer = buffer(&mut foreign, "foreign", 64);
    let mut graph = pool.begin().unwrap();
    let source = buffer(&mut graph, "uniform", 64);
    let output = buffer(&mut graph, "output", 64);
    let valid = bindings(source, output);
    let error = graph
        .dispatch(
            "missing count",
            &ManagedProgram::uniform_readback(),
            &valid[..1],
            [1; 3],
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    assert_eq!(error.binding(), None);
    for (name, input, expected_kind, slot) in [
        (
            "duplicate",
            [valid[0], valid[0]],
            GraphErrorKind::InvalidInput,
            (0, 0),
        ),
        (
            "unknown slot",
            [
                valid[0],
                Binding {
                    group: 9,
                    binding: 7,
                    ..valid[1]
                },
            ],
            GraphErrorKind::InvalidInput,
            (1, 0),
        ),
        (
            "zero range",
            [valid[0], {
                let mut binding = valid[1];
                set_buffer(&mut binding, None, None, Some(0));
                binding
            }],
            GraphErrorKind::InvalidInput,
            (1, 0),
        ),
        (
            "undersized ABI",
            [valid[0], {
                let mut binding = valid[1];
                set_buffer(&mut binding, None, None, Some(8));
                binding
            }],
            GraphErrorKind::InvalidInput,
            (1, 0),
        ),
        (
            "out of bounds",
            [valid[0], {
                let mut binding = valid[1];
                set_buffer(&mut binding, None, Some(60), None);
                binding
            }],
            GraphErrorKind::InvalidInput,
            (1, 0),
        ),
        (
            "overflow",
            [valid[0], {
                let mut binding = valid[1];
                set_buffer(&mut binding, None, Some(u64::MAX - 7), None);
                binding
            }],
            GraphErrorKind::InvalidInput,
            (1, 0),
        ),
        (
            "foreign owner",
            [valid[0], {
                let mut binding = valid[1];
                set_buffer(&mut binding, Some(foreign_buffer), None, None);
                binding
            }],
            GraphErrorKind::WrongOwner,
            (1, 0),
        ),
        (
            "aliased footprints",
            [valid[0], {
                let mut binding = valid[1];
                set_buffer(&mut binding, Some(source), None, None);
                binding
            }],
            GraphErrorKind::InvalidInput,
            (0, 0),
        ),
    ] {
        let error = graph
            .dispatch(name, &ManagedProgram::uniform_readback(), &input, [1; 3])
            .unwrap_err();
        assert_eq!(error.kind(), expected_kind, "{name}");
        assert_eq!(error.binding(), Some(slot), "{name}");
        assert_eq!(error.pass_name(), Some(name));
    }
    for groups in [[0, 1, 1], [2, 1, 1]] {
        assert_eq!(
            graph
                .dispatch(
                    "unreviewed groups",
                    &ManagedProgram::uniform_readback(),
                    &valid,
                    groups
                )
                .unwrap_err()
                .kind(),
            GraphErrorKind::Unsupported
        );
    }
    for program in [
        ManagedProgram::sampled_readback(),
        ManagedProgram::sampled_float_readback(),
    ] {
        assert_eq!(
            graph
                .dispatch("sampled asset missing bindings", &program, &[], [1; 3])
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput
        );
    }
    // Every rejected call must have returned its partially appended binding
    // operands and accesses, leaving exactly enough room for this dispatch.
    graph.upload_buffer("seed", source, 0, &[0; 16]).unwrap();
    graph
        .dispatch("valid", &ManagedProgram::uniform_readback(), &valid, [1; 3])
        .unwrap();
    graph.readback_buffer("result", output, 0, 16).unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["seed", "valid", "result"]
    );
}

#[test]
fn late_capacity_failure_rolls_back_operands_before_retry() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            bindings: 2,
            accesses: 4,
            name_bytes: 64,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let source = buffer(&mut graph, "uniform", 16);
    let output = buffer(&mut graph, "output", 16);
    graph.upload_buffer("seed", source, 0, &[0; 16]).unwrap();
    let input = bindings(source, output);
    let error = graph
        .dispatch(
            &"x".repeat(64),
            &ManagedProgram::uniform_readback(),
            &input,
            [1; 3],
        )
        .unwrap_err();
    assert_eq!(error.capacity().unwrap().kind, GraphCapacityKind::NameBytes);
    graph
        .dispatch("retry", &ManagedProgram::uniform_readback(), &input, [1; 3])
        .unwrap();
    let error = graph
        .dispatch(
            "no room",
            &ManagedProgram::uniform_readback(),
            &input,
            [1; 3],
        )
        .unwrap_err();
    let capacity = error.capacity().unwrap();
    assert_eq!(capacity.kind, GraphCapacityKind::Bindings);
    assert_eq!((capacity.limit, capacity.required), (2, Some(4)));
    graph.readback_buffer("result", output, 0, 16).unwrap();
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        ["seed", "retry", "result"]
    );
}

#[test]
fn held_plan_pins_owned_operands_and_reuse_rejects_old_handles() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            bindings: 2,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let source = buffer(&mut graph, "uniform", 16);
    let output = buffer(&mut graph, "output", 16);
    graph.upload_buffer("seed", source, 0, &[0; 16]).unwrap();
    graph
        .dispatch(
            "dispatch",
            &ManagedProgram::uniform_readback(),
            &bindings(source, output),
            [1; 3],
        )
        .unwrap();
    graph.readback_buffer("result", output, 0, 16).unwrap();
    let plan = graph.compile().unwrap();
    let held = plan.clone();
    drop(plan);
    assert_eq!(
        pool.begin().unwrap_err().capacity().unwrap().kind,
        GraphCapacityKind::Slots
    );
    assert_eq!(held.pass_count(), 3);
    drop(held);

    let mut graph = pool.begin().unwrap();
    let replacement = buffer(&mut graph, "new uniform", 16);
    let output = buffer(&mut graph, "new output", 16);
    let error = graph
        .dispatch(
            "stale",
            &ManagedProgram::uniform_readback(),
            &bindings(source, output),
            [1; 3],
        )
        .unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
    assert_eq!(error.binding(), Some((0, 0)));
    graph
        .upload_buffer("new seed", replacement, 0, &[0; 16])
        .unwrap();
    graph
        .dispatch(
            "new dispatch",
            &ManagedProgram::uniform_readback(),
            &bindings(replacement, output),
            [1; 3],
        )
        .unwrap();
    graph.readback_buffer("new result", output, 0, 16).unwrap();
    assert_eq!(graph.compile().unwrap().pass_count(), 3);
}
