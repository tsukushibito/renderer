use renderer::canvas::Layer;
use sashiko::ui::{DisplayFont, Overlay, Rect};
use survivor::{Game, Phase};

const PALE: [f32; 4] = [0.79, 0.90, 0.91, 1.];
const PANEL: [f32; 4] = [0.018, 0.031, 0.039, 1.];
const CYAN: [f32; 4] = [0.10, 0.84, 0.70, 1.];
const AMBER: [f32; 4] = [1.0, 0.40, 0.09, 1.];

pub(super) fn paint(out: &mut Vec<Layer>, game: &Game, size: [u32; 2]) {
    let mut ui = Overlay::new(out, size);
    let unit = (size[1] / 320).clamp(1, 4);
    let font = DisplayFont::new(unit);
    let bar_width = (size[0] / 7).clamp(40, 320);
    let x = 6 * unit;
    let y = 6 * unit;
    ui.fill(Rect::new(x, y, bar_width + 16 * unit, 22 * unit), PANEL);
    for (label, dy, value, cap, color) in [
        ("HP", 3, game.hp, Game::MAX_HP, AMBER),
        ("XP", 13, game.xp, game.xp_required(), CYAN),
    ] {
        let by = y + dy * unit;
        ui.label(font, label, [x + 2 * unit, by], PALE);
        ui.meter(
            Rect::new(x + 12 * unit, by, bar_width, 5 * unit),
            value,
            cap,
            [0.10, 0.14, 0.16, 1.],
            color,
        );
    }
    match game.phase {
        Phase::Choosing => {
            let top = size[1].saturating_sub(28 * unit);
            let cards = ui.centered_row(3, [40 * unit, 15 * unit], 2 * unit, top);
            for (i, (rect, text)) in cards.zip(["1 RAPID", "2 PIERCE", "3 PULSE"]).enumerate() {
                ui.fill(rect, PANEL);
                let color = if i == game.upgrade_focus.index() {
                    AMBER
                } else {
                    CYAN
                };
                ui.fill(
                    Rect {
                        height: unit,
                        ..rect
                    },
                    color,
                );
                ui.label(font, text, [rect.x + 3 * unit, top + 5 * unit], PALE);
            }
        }
        Phase::Defeated => {
            let left = size[0].saturating_sub(44 * unit) / 2;
            let top = size[1] / 2;
            ui.fill(Rect::new(left, top, 44 * unit, 20 * unit), PANEL);
            ui.label(font, "DEFEAT", [left + 4 * unit, top + 3 * unit], PALE);
            ui.label(font, "R RETRY", [left + 4 * unit, top + 12 * unit], PALE);
        }
        Phase::Playing => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_hud_screens_fit_the_prepared_layer_list() {
        let mut game = Game::new(Default::default(), survivor::Scenario::Early, 5);
        let mut layers = Vec::with_capacity(256);
        let initial_capacity = layers.capacity();
        for phase in [Phase::Playing, Phase::Choosing, Phase::Defeated] {
            game.phase = phase;
            for size in [[1, 1], [640, 360], [1920, 1080], [4096, 2160]] {
                paint(&mut layers, &game, size);
                assert_eq!(initial_capacity, layers.capacity());
            }
        }
    }
}
