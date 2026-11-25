//! Orthographic Canvas preview of the same simulation. Pack rectangles once per redraw.
use super::{VIEWS, access};
use crate::{Game, MAX_DROPS, MAX_ENEMIES, MAX_SHOTS};
use sashiko::{input::PadState, math::Vec2};

pub(super) struct CanvasScene(Vec<[f32; 5]>);
impl CanvasScene {
    pub(super) fn new(obstacles: usize) -> Self {
        Self(Vec::with_capacity(
            MAX_ENEMIES + MAX_DROPS + MAX_SHOTS + obstacles + 3,
        ))
    }
    fn update(&mut self, game: &Game) {
        self.0.clear();
        let mut rect = |pos: Vec2, half: Vec2, color: u8| {
            self.0
                .push([pos.x(), pos.y(), half.x(), half.y(), color.into()]);
        };
        rect(Vec2::ZERO, game.arena.half_extent, 0);
        for b in &game.arena.obstacles {
            rect(b.center, b.half_extent, 1);
        }
        rect(game.arena.marker, Vec2::ONE * 0.4, 2);
        for e in &game.enemies {
            rect(e.pos, Vec2::ONE * 0.28, if e.windup { 4 } else { 3 });
        }
        for d in &game.drops {
            rect(d.pos, Vec2::ONE * 0.14, 5);
        }
        for s in &game.shots {
            rect(s.pos, Vec2::ONE * 0.09, 6);
        }
        rect(game.player, Vec2::ONE * 0.32, 7);
    }
}

#[unsafe(no_mangle)]
extern "C" fn survivor_pad(
    id: u32,
    buttons: u32,
    lx: f32,
    ly: f32,
    rx: f32,
    ry: f32,
    lt: f32,
    rt: f32,
) -> i32 {
    access(id, |s| {
        s.controls.gamepad(Some(PadState {
            buttons,
            left: Vec2::new(lx, ly),
            right: Vec2::new(rx, ry),
            triggers: [lt, rt],
        }));
        Ok(0)
    })
}

#[unsafe(no_mangle)]
extern "C" fn survivor_scene(id: u32) -> i32 {
    access(id, |s| {
        s.scene.update(&s.app.game);
        Ok(s.scene.0.len() as i32)
    })
}
#[unsafe(no_mangle)]
extern "C" fn survivor_rects(id: u32) -> *const [f32; 5] {
    VIEWS.with_borrow(|v| v.get(&id).map_or(std::ptr::null(), |s| s.scene.0.as_ptr()))
}
