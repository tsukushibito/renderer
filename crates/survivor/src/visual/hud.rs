use renderer::{
    canvas::{Layer, Paint},
    core::graph::RasterRect,
};
use survivor::{Game, Phase};

const PALE: [f32; 4] = [0.79, 0.90, 0.91, 1.];
const PANEL: [f32; 4] = [0.018, 0.031, 0.039, 1.];
const CYAN: [f32; 4] = [0.10, 0.84, 0.70, 1.];
const AMBER: [f32; 4] = [1.0, 0.40, 0.09, 1.];

fn rect(out: &mut Vec<Layer>, extent: [u32; 2], x: u32, y: u32, w: u32, h: u32, color: [f32; 4]) {
    let w = w.min(extent[0].saturating_sub(x));
    let h = h.min(extent[1].saturating_sub(y));
    if w > 0 && h > 0 {
        out.push(Layer {
            paint: Paint::Color(color),
            rect: RasterRect::new(x, y, w, h),
            clip: None,
        })
    }
}

fn glyph(c: char) -> [u8; 5] {
    match c {
        '1' => [2, 6, 2, 2, 7],
        '2' => [7, 1, 7, 4, 7],
        '3' => [7, 1, 3, 1, 7],
        'A' => [2, 5, 7, 5, 5],
        'C' => [7, 4, 4, 4, 7],
        'D' => [6, 5, 5, 5, 6],
        'E' => [7, 4, 6, 4, 7],
        'F' => [7, 4, 6, 4, 4],
        'H' => [5, 5, 7, 5, 5],
        'I' => [7, 2, 2, 2, 7],
        'L' => [4, 4, 4, 4, 7],
        'O' => [7, 5, 5, 5, 7],
        'P' => [6, 5, 6, 4, 4],
        'R' => [6, 5, 6, 5, 5],
        'S' => [7, 4, 7, 1, 7],
        'T' => [7, 2, 2, 2, 2],
        'U' => [5, 5, 5, 5, 7],
        'X' => [5, 5, 2, 5, 5],
        'Y' => [5, 5, 2, 2, 2],
        _ => [0; 5],
    }
}

fn label(out: &mut Vec<Layer>, size: [u32; 2], text: &str, x: u32, y: u32, scale: u32) {
    for (letter, c) in text.chars().enumerate() {
        for (row, mask) in glyph(c).into_iter().enumerate() {
            let mut col = 0;
            while col < 3 {
                if mask & (4 >> col) == 0 {
                    col += 1;
                    continue;
                }
                let start = col;
                while col < 3 && mask & (4 >> col) != 0 {
                    col += 1
                }
                rect(
                    out,
                    size,
                    x + (letter as u32 * 4 + start) * scale,
                    y + row as u32 * scale,
                    (col - start) * scale,
                    scale,
                    PALE,
                );
            }
        }
    }
}

pub(super) fn paint(out: &mut Vec<Layer>, game: &Game, size: [u32; 2]) {
    out.clear();
    let unit = (size[1] / 320).clamp(1, 4);
    let bar_width = (size[0] / 7).clamp(40, 320);
    let x = 6 * unit;
    let y = 6 * unit;
    let box_width = bar_width + 16 * unit;
    rect(out, size, x, y, box_width, 22 * unit, PANEL);
    label(out, size, "HP", x + 2 * unit, y + 3 * unit, unit);
    label(out, size, "XP", x + 2 * unit, y + 13 * unit, unit);
    for (dy, value, cap, color) in [
        (3, game.hp, Game::MAX_HP, AMBER),
        (13, game.xp, game.xp_required(), CYAN),
    ] {
        let bx = x + 12 * unit;
        let by = y + dy * unit;
        rect(
            out,
            size,
            bx,
            by,
            bar_width,
            5 * unit,
            [0.10, 0.14, 0.16, 1.],
        );
        rect(
            out,
            size,
            bx,
            by,
            (bar_width as u64 * u64::from(value.min(cap)) / u64::from(cap.max(1))) as u32,
            5 * unit,
            color,
        );
    }
    match game.phase {
        Phase::Choosing => {
            let card_width = 40 * unit;
            let left = size[0].saturating_sub(3 * card_width + 4 * unit) / 2;
            let top = size[1].saturating_sub(28 * unit);
            for (i, text) in ["1 RAPID", "2 PIERCE", "3 PULSE"].iter().enumerate() {
                let cx = left + i as u32 * (card_width + 2 * unit);
                rect(out, size, cx, top, card_width, 15 * unit, PANEL);
                rect(out, size, cx, top, card_width, unit, CYAN);
                label(out, size, text, cx + 3 * unit, top + 5 * unit, unit);
            }
        }
        Phase::Defeated => {
            let left = size[0].saturating_sub(44 * unit) / 2;
            let top = size[1] / 2;
            rect(out, size, left, top, 44 * unit, 20 * unit, PANEL);
            label(out, size, "DEFEAT", left + 4 * unit, top + 3 * unit, unit);
            label(out, size, "R RETRY", left + 4 * unit, top + 12 * unit, unit);
        }
        Phase::Playing => {}
    }
}
