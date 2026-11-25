//! Public standard recipes, replacement/addition/bypass and direct parts share
//! the same consumer-owned Compute module. Native coverage extends raster_graph.

#[allow(dead_code)]
#[path = "../fixtures/compute_fixture.rs"]
mod compute_fixture;
#[allow(dead_code)]
#[path = "../fixtures/standard_fixture.rs"]
mod fixture;

use fixture::PostMode;
use sashiko_renderer_core::graph::GraphPool;

#[test]
fn changing_three_route_composition_keeps_consumer_compute_and_culls_unused_modules() {
    let mut pool = GraphPool::new(fixture::capacity(), 1).unwrap();
    for frame in 0..12 {
        let (graph, expected) = fixture::build(&mut pool, frame);
        let plan = graph.compile().unwrap();
        let names = plan.ordered_pass_names().collect::<Vec<_>>();
        let active = fixture::post_mode(frame) != PostMode::Disabled;
        assert_eq!(expected.readbacks.len(), 4);
        assert_eq!(
            expected
                .readbacks
                .iter()
                .map(|entry| entry.len)
                .sum::<usize>(),
            fixture::READBACK_BYTES
        );
        assert_eq!(
            expected.readbacks[0].bytes, expected.readbacks[1].bytes,
            "identity insertion must preserve standard direct output"
        );
        assert_eq!(
            expected.readbacks[2].bytes, expected.readbacks[3].bytes,
            "recipe and independent pass composition use the same input and post semantics"
        );
        if active {
            assert_ne!(expected.readbacks[0].bytes, expected.readbacks[2].bytes);
        } else {
            assert_eq!(expected.readbacks[0].bytes, expected.readbacks[2].bytes);
        }
        assert_eq!(
            names.contains(&"standard vertex upload"),
            fixture::draw_count(frame) != 0
        );
        assert_eq!(names.contains(&"standard GPU offset producer"), active);
        assert_eq!(
            names
                .iter()
                .filter(|&&name| name == "consumer sampled multiply")
                .count(),
            if active { 2 } else { 0 }
        );
        assert_eq!(
            names
                .iter()
                .filter(|&&name| name == "consumer storage add")
                .count(),
            match fixture::post_mode(frame) {
                PostMode::Disabled => 0,
                PostMode::AdditionalAdd => 4,
                _ => 2,
            }
        );
        assert_eq!(
            plan.pass_count(),
            match fixture::post_mode(frame) {
                PostMode::Disabled => 10,
                PostMode::AdditionalAdd => 20,
                _ => 18,
            }
        );
        drop(plan);
        assert!(pool.is_idle());
    }
}
