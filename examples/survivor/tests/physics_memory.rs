//! Measure the fixed update while a load changes from rest to sliding contact.
#![cfg(all(windows, feature = "memory-profiling"))]
use sashiko::math::Vec2;
use sashiko_renderer::core::measurement::{CountingAllocator, Snapshot};
use survivor::{Arena, Game, Input, Scenario};

#[global_allocator]
static HEAP: CountingAllocator<std::alloc::System> = CountingAllocator::new(std::alloc::System);
#[test]
#[ignore = "run in isolation to measure process-wide allocations"]
fn push_cargo_after_warmup() {
    use sashiko::{
        math::{Pose3, Vec3},
        physics::{BodyKind, PhysicsWorld},
    };
    let mut world = PhysicsWorld::new(Vec3::Y * -9.81, 1. / 60.);
    world.add_box(
        Pose3::from_translation(-Vec3::Y),
        Vec3::new(32., 1., 32.),
        BodyKind::Fixed,
    );
    let (body, _) = world.add_box(
        Pose3::from_translation(Vec3::Y * 0.5),
        Vec3::ONE * 0.5,
        BodyKind::Dynamic {
            lock_rotation: true,
        },
    );
    for _ in 0..240 {
        world.step();
    }
    for push in [false, true] {
        let clock = std::time::Instant::now();
        let start = Snapshot::now();
        for _ in 0..90 {
            if push {
                world.impulse(body, Vec3::X * 0.12).unwrap();
            }
            world.step();
        }
        let delta = Snapshot::now().since(start);
        println!(
            "90 rigid-body steps push={push}: {:?}, heap {delta:?}",
            clock.elapsed()
        );
    }
    let mut game = Game::new(Arena::default(), Scenario::Early, 506);
    for _ in 0..60 {
        game.step(Input::default());
    }
    let start = game.cargo_position().unwrap();
    let movement = Input {
        movement: Vec2::new(1., 0.),
        ..Input::default()
    };
    let clock = std::time::Instant::now();
    let before = Snapshot::now();
    for _ in 0..90 {
        game.step(movement);
    }
    let changes = Snapshot::now().since(before);
    println!(
        "90 fixed updates {:?}, heap events {changes:?}",
        clock.elapsed()
    );
    assert!(game.cargo_position().unwrap().x() > start.x() + 1.);
}
