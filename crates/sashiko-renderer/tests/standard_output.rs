use sashiko_renderer::passes::output::{SdrOutput, tone_map_neutral, write_sdr};
use sashiko_renderer_core::graph::{
    GraphCapacity, GraphErrorKind, GraphPool, ImageDesc, SampleCount,
};

const SDR: SdrOutput = SdrOutput::capture(false);

#[test]
fn invalid_output_inputs_preserve_capacity_for_a_retry() {
    let mut pool = GraphPool::new(
        GraphCapacity {
            draws: 2,
            bindings: 4,
            raster_constant_words: Some(1),
            ..GraphCapacity::default()
        },
        1,
    )
    .unwrap();
    let mut graph = pool.begin().unwrap();
    let hdr_desc = ImageDesc::rgba16_float(2, 2);
    let hdr = graph.image("HDR", hdr_desc).unwrap();
    let display = graph.image("display linear", hdr_desc).unwrap();
    let wrong_extent = graph
        .image("wrong extent", ImageDesc::rgba16_float(3, 2))
        .unwrap();
    let msaa = graph
        .image("MSAA", hdr_desc.with_sample_count(SampleCount::X4))
        .unwrap();
    let sdr = graph.image("SDR", ImageDesc::rgba8(2, 2)).unwrap();
    let source = graph.image_view(hdr, 0, 0).unwrap();
    for exposure in [
        0.0,
        -1.0,
        f32::NAN,
        f32::INFINITY,
        f32::MAX,
        f32::from_bits(1),
    ] {
        let error = tone_map_neutral(&mut graph, "tone", source, display, exposure).unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
        assert_eq!(error.pass_name(), Some("tone"));
    }
    for destination in [hdr, wrong_extent, msaa, sdr] {
        let error = tone_map_neutral(&mut graph, "tone", source, destination, 1.0).unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    }
    let error = write_sdr(&mut graph, "output", source, display, SDR).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    graph.upload_image("HDR input", hdr, &[0; 32]).unwrap();
    tone_map_neutral(&mut graph, "tone", source, display, f32::MAX / 65536.0).unwrap();
    let display_view = graph.image_view(display, 0, 0).unwrap();
    let output = write_sdr(&mut graph, "output", display_view, sdr, SDR).unwrap();
    assert!(output.display().is_none());
    output
        .capture()
        .unwrap()
        .readback(&mut graph, "read SDR")
        .unwrap();
    let plan = graph.compile().unwrap();
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        ["HDR input", "tone", "output", "read SDR"]
    );
}

#[test]
fn sdr_output_reads_the_selected_srgb_mip_and_checks_its_owner() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 2).unwrap();
    let mut graph = pool.begin().unwrap();
    let mut foreign_graph = pool.begin().unwrap();
    let desc = ImageDesc {
        mip_levels: 2,
        ..ImageDesc::rgba8_srgb(3, 3)
    };
    let foreign = foreign_graph.image("foreign texture", desc).unwrap();
    let texture = graph.image("texture", desc).unwrap();
    let output = graph.image("SDR", ImageDesc::rgba8(1, 1)).unwrap();
    let foreign_mip = foreign_graph.image_view(foreign, 1, 0).unwrap();
    let error = write_sdr(&mut graph, "output", foreign_mip, output, SDR).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
    let whole_chain = graph.sampled_image_view(texture, 0, 2).unwrap();
    let error = write_sdr(&mut graph, "output", whole_chain, output, SDR).unwrap_err();
    assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
    let mip = graph.image_view(texture, 1, 0).unwrap();
    graph
        .upload_image("texture mips", texture, &[0; 40])
        .unwrap();
    write_sdr(&mut graph, "output", mip, output, SDR).unwrap();
    graph.readback_image("read SDR", output).unwrap();
    let plan = graph.compile().unwrap();
    assert_eq!(
        plan.ordered_pass_names().collect::<Vec<_>>(),
        ["texture mips", "output", "read SDR"]
    );
}
