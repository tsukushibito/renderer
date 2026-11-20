use std::num::NonZeroUsize;

use renderer::capacity::{GraphRequirements, Preparation};
use renderer::graph::{
    Attachment, BufferDesc, GraphCapacity, GraphCapacityKind, GraphErrorKind, GraphPool, ImageDesc,
    IndexedIndirectTable, IndexedIndirectTemplate, LoadOp, RasterAttachments, RasterParameters,
    StoreOp,
};
use renderer::program::ManagedRasterProgram;
use renderer_pipeline::passes::visibility::{IndexedGeometry, PreparedVisibility, WorldBounds};

fn prepared(counts: &[usize]) -> PreparedVisibility {
    let command = IndexedIndirectTemplate {
        first_index: 0,
        index_count: 3,
    };
    let tables = counts
        .iter()
        .map(|&count| IndexedIndirectTable::new(&vec![command; count]).unwrap())
        .collect::<Vec<_>>();
    PreparedVisibility::new(&tables).unwrap()
}

#[test]
fn bounds_preserve_group_order_and_zero_every_padding_slot() {
    let visibility = prepared(&[1, 65]);
    assert_eq!(visibility.bounds_desc().size, 3 * 64 * 32);
    assert_eq!(visibility.mask_desc().size, 3 * 64 * 4);
    assert_eq!(visibility.template_transfer_bytes(), 3 * 64 * 8);
    assert_eq!(visibility.template_transfer_count(), 2);

    let mut bytes = vec![255; visibility.bounds_desc().size as usize];
    let mut commands = 0;
    visibility
        .encode_bounds(&mut bytes, |group, command| {
            commands += 1;
            WorldBounds {
                min: [group as f32, command as f32, 0.0],
                max: [3.0, 100.0, 1.0],
                enabled: command != 1,
            }
        })
        .unwrap();
    assert_eq!(commands, 66);
    let word = |slot: usize, word: usize| {
        let offset = slot * 32 + word * 4;
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    };
    assert_eq!(word(0, 3), 1);
    assert_eq!(word(64, 0), 1.0_f32.to_bits());
    assert_eq!(word(65, 3), 0);
    assert_eq!(word(128, 1), 64.0_f32.to_bits());
    assert_eq!(word(128, 7), 0);
    assert!(bytes[32..64 * 32].iter().all(|&byte| byte == 0));
    assert!(bytes[129 * 32..].iter().all(|&byte| byte == 0));
    assert!(
        visibility
            .encode_bounds(&mut bytes[1..], |_, _| panic!("invalid destination"))
            .is_err()
    );
}

#[test]
fn prepared_visibility_connects_live_indirect_groups_and_clears_reused_scratch() {
    let visibility = prepared(&[1, 65]);
    let mut preparation = Preparation::new(NonZeroUsize::MIN);
    visibility.reserve(&mut preparation).unwrap();
    preparation
        .add_graph_requirements(GraphRequirements {
            resources: 5,
            passes: 6,
            accesses: 11,
            draws: 2,
            attachments: 1,
            name_bytes: 128,
            ..GraphRequirements::default()
        })
        .unwrap();
    let capacity = preparation
        .graph_capacity(GraphCapacity {
            resources: 0,
            passes: 0,
            accesses: 0,
            draws: 0,
            bindings: 0,
            attachments: 0,
            name_bytes: 0,
            ..GraphCapacity::default()
        })
        .unwrap();
    let mut pool = GraphPool::new(capacity, 1).unwrap();
    let mut arguments = [None, None];

    for [first, second] in [[true, true], [false, true], [false, false]] {
        let mut graph = pool.begin().unwrap();
        let camera = graph.buffer("camera", BufferDesc { size: 64 }).unwrap();
        let bounds = graph.buffer("bounds", visibility.bounds_desc()).unwrap();
        let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
        let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
        let mut bytes = vec![0; visibility.bounds_desc().size as usize];
        visibility
            .encode_bounds(&mut bytes, |_, _| WorldBounds {
                enabled: true,
                ..Default::default()
            })
            .unwrap();
        for (name, buffer, bytes) in [
            ("camera data", camera, &[0; 64][..]),
            ("bounds data", bounds, &bytes),
            ("vertex data", vertices, &[0; 84][..]),
            ("index data", indices, &[0; 12][..]),
        ] {
            graph.upload_buffer(name, buffer, 0, bytes).unwrap();
        }
        let frame = visibility
            .add_passes(&mut graph, camera, bounds, &mut arguments, |group| {
                [first, second][group].then_some(IndexedGeometry { vertices, indices })
            })
            .unwrap();
        assert_eq!(
            [frame.arguments[0].is_some(), frame.arguments[1].is_some()],
            [first, second]
        );
        let output = graph.image("output", ImageDesc::rgba8(2, 2)).unwrap();
        let view = graph.image_view(output, 0, 0).unwrap();
        graph
            .raster(
                "draw visible commands",
                RasterAttachments {
                    colors: &[Attachment {
                        view,
                        load: LoadOp::Clear([0.0; 4]),
                        store: StoreOp::Store,
                    }],
                    color_resolves: &[],
                    depth: None,
                },
                |pass| {
                    for &args in frame.arguments.iter().flatten() {
                        pass.draw_indirect(
                            &ManagedRasterProgram::mesh(),
                            args,
                            None,
                            RasterParameters::default().into(),
                        )?;
                    }
                    Ok(())
                },
            )
            .unwrap();
        graph.readback_image("pixels", output).unwrap();
        let plan = graph.compile().unwrap();
        let names = plan.ordered_pass_names().collect::<Vec<_>>();
        assert_eq!(
            names
                .iter()
                .filter(|&&name| name == "encode visible indexed draws")
                .count(),
            usize::from(first) + usize::from(second)
        );
        if let Some(cull) = names
            .iter()
            .position(|&name| name == "cull indexed draw bounds")
        {
            assert!(second);
            let bounds = names
                .iter()
                .position(|&name| name == "bounds data")
                .unwrap();
            let encode = names
                .iter()
                .position(|&name| name == "encode visible indexed draws")
                .unwrap();
            let draw = names
                .iter()
                .position(|&name| name == "draw visible commands")
                .unwrap();
            assert!(bounds < cull && cull < encode && encode < draw);
        } else {
            assert!(!second);
        }
    }
}

#[test]
fn missing_argument_slots_and_foreign_inputs_fail_before_declaration() {
    let visibility = prepared(&[1]);
    let mut pool = GraphPool::new(
        GraphCapacity {
            resources: 1,
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let old_camera = pool
        .begin()
        .unwrap()
        .buffer("old camera", BufferDesc { size: 64 })
        .unwrap();
    let mut graph = pool.begin().unwrap();
    let input = graph.buffer("local", visibility.bounds_desc()).unwrap();
    let short = visibility
        .add_passes(&mut graph, input, input, &mut [], |_| None)
        .err()
        .unwrap();
    assert_eq!(short.capacity().unwrap().kind, GraphCapacityKind::Draws);
    let foreign = visibility
        .add_passes(&mut graph, old_camera, input, &mut [None], |_| None)
        .err()
        .unwrap();
    assert_eq!(foreign.kind(), GraphErrorKind::WrongOwner);
}
