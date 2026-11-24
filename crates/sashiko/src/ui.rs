//! Allocation-free layout and menu focus, plus a small bitmap face for screen overlays.
//! Reserve the layer list when preparing the screen. Coordinates are output pixels.
use renderer::canvas::{Layer, Paint};
pub use renderer::core::graph::RasterRect as Rect;
use serde::{Deserialize, Serialize};

pub type Color = [f32; 4];

/// Menu gestures captured once per game update, independently of the physical keys.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MenuInput {
    /// Negative moves to the previous entry, positive to the next.
    pub direction: i8,
    pub activate: bool,
}
impl MenuInput {
    pub fn is_idle(&self) -> bool {
        self.direction == 0 && !self.activate
    }
}

/// Keyboard/controller focus for a row or column of selectable entries.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus(usize);
impl Focus {
    pub fn index(self) -> usize {
        self.0
    }
    pub fn update<'a, T>(&mut self, input: MenuInput, entries: &'a [T]) -> Option<&'a T> {
        let count = entries.len();
        if count == 0 {
            self.0 = 0;
            return None;
        }
        self.0 = self.0.min(count - 1);
        self.0 = match input.direction.signum() {
            -1 if self.0 == 0 => count - 1,
            -1 => self.0 - 1,
            1 => (self.0 + 1) % count,
            _ => self.0,
        };
        input.activate.then(|| &entries[self.0])
    }
}

/// A single-line pixel-aligned display font. ASCII letters share capital glyphs;
/// unsupported characters use a visible replacement mark. Runs of adjacent pixels
/// are emitted as a single Canvas rectangle.
#[derive(Debug, Clone, Copy)]
pub struct DisplayFont {
    scale: u32,
}
impl DisplayFont {
    pub fn new(scale: u32) -> Self {
        Self {
            scale: scale.clamp(1, u32::MAX / 5),
        }
    }
    pub fn measure(self, text: &str) -> [u32; 2] {
        let count = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        let width = count
            .saturating_mul(4)
            .saturating_sub(1)
            .saturating_mul(self.scale);
        [width, 5 * self.scale]
    }
}

fn glyph(c: char) -> [u8; 5] {
    match c.to_ascii_uppercase() {
        ' ' => [0; 5],
        '0' | 'O' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [7, 1, 7, 4, 7],
        '3' => [7, 1, 3, 1, 7],
        '4' => [5, 5, 7, 1, 1],
        '5' | 'S' => [7, 4, 7, 1, 7],
        '6' => [7, 4, 7, 5, 7],
        '7' => [7, 1, 2, 2, 2],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 7],
        'A' => [2, 5, 7, 5, 5],
        'B' => [6, 5, 6, 5, 6],
        'C' => [7, 4, 4, 4, 7],
        'D' => [6, 5, 5, 5, 6],
        'E' => [7, 4, 6, 4, 7],
        'F' => [7, 4, 6, 4, 4],
        'G' => [7, 4, 5, 5, 7],
        'H' => [5, 5, 7, 5, 5],
        'I' => [7, 2, 2, 2, 7],
        'J' => [1, 1, 1, 5, 7],
        'K' => [5, 5, 6, 5, 5],
        'L' => [4, 4, 4, 4, 7],
        'M' => [5, 7, 7, 5, 5],
        'N' => [5, 7, 7, 7, 5],
        'P' => [6, 5, 6, 4, 4],
        'Q' => [7, 5, 5, 7, 1],
        'R' => [6, 5, 6, 5, 5],
        'T' => [7, 2, 2, 2, 2],
        'U' => [5, 5, 5, 5, 7],
        'V' => [5, 5, 5, 5, 2],
        'W' => [5, 5, 7, 7, 5],
        'X' => [5, 5, 2, 5, 5],
        'Y' => [5, 5, 2, 2, 2],
        'Z' => [7, 1, 2, 4, 7],
        '-' => [0, 0, 7, 0, 0],
        '.' => [0, 0, 0, 0, 2],
        '/' => [1, 1, 2, 4, 4],
        ':' => [0, 2, 0, 2, 0],
        _ => [7, 1, 2, 0, 2],
    }
}

/// Pixel-aligned layout writing into caller-owned Canvas layers.
pub struct Overlay<'a> {
    layers: &'a mut Vec<Layer>,
    extent: [u32; 2],
}
impl<'a> Overlay<'a> {
    pub fn new(layers: &'a mut Vec<Layer>, extent: [u32; 2]) -> Self {
        layers.clear();
        Self { layers, extent }
    }
    pub fn fill(&mut self, rect: Rect, color: Color) {
        let width = rect.width.min(self.extent[0].saturating_sub(rect.x));
        let height = rect.height.min(self.extent[1].saturating_sub(rect.y));
        if width > 0 && height > 0 {
            self.layers.push(Layer {
                paint: Paint::Color(color),
                rect: Rect::new(rect.x, rect.y, width, height),
                clip: None,
            })
        }
    }
    pub fn label(&mut self, font: DisplayFont, text: &str, [mut x, y]: [u32; 2], color: Color) {
        let unit = font.scale;
        for letter in text.chars() {
            if x >= self.extent[0] || y >= self.extent[1] {
                break;
            }
            for (row, mask) in glyph(letter).into_iter().enumerate() {
                let mut col = 0;
                while col < 3 {
                    if mask & (4 >> col) == 0 {
                        col += 1;
                        continue;
                    }
                    let start = col;
                    while col < 3 && mask & (4 >> col) != 0 {
                        col += 1;
                    }
                    self.fill(
                        Rect::new(
                            x.saturating_add(start * unit),
                            y.saturating_add(row as u32 * unit),
                            (col - start) * unit,
                            unit,
                        ),
                        color,
                    );
                }
            }
            x = x.saturating_add(4 * unit);
        }
    }
    pub fn meter(&mut self, rect: Rect, value: u32, max: u32, background: Color, fill: Color) {
        self.fill(rect, background);
        let width =
            (u64::from(rect.width) * u64::from(value.min(max)) / u64::from(max.max(1))) as u32;
        self.fill(Rect { width, ..rect }, fill);
    }
    /// Equal-width items packed in a horizontal row centered in the viewport.
    pub fn centered_row(
        &self,
        count: u32,
        size: [u32; 2],
        gap: u32,
        top: u32,
    ) -> impl Iterator<Item = Rect> + use<> {
        let stride = size[0].saturating_add(gap);
        let width = size[0]
            .saturating_mul(count)
            .saturating_add(gap.saturating_mul(count.saturating_sub(1)));
        let left = self.extent[0].saturating_sub(width) / 2;
        (0..count).map(move |index| {
            Rect::new(
                left.saturating_add(index.saturating_mul(stride)),
                top,
                size[0],
                size[1],
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_wraps_and_handles_a_changed_or_empty_menu() {
        let mut focus = Focus::default();
        let previous = MenuInput {
            direction: -1,
            activate: true,
        };
        assert_eq!(focus.update(previous, &[10, 20, 30]), Some(&30));
        assert_eq!(
            focus.update(
                MenuInput {
                    activate: true,
                    ..Default::default()
                },
                &[10]
            ),
            Some(&10)
        );
        assert_eq!(focus.update(previous, &[] as &[u8]), None);
        assert_eq!(focus.index(), 0);
    }
    #[test]
    fn text_is_clipped_and_extreme_coordinates_do_not_wrap() {
        let font = DisplayFont::new(2);
        assert_eq!(font.measure("HP"), [14, 10]);
        let mut layers = Vec::with_capacity(16);
        let mut overlay = Overlay::new(&mut layers, [17, 13]);
        overlay.label(font, "HP", [15, 11], [1.; 4]);
        overlay.label(
            DisplayFont::new(u32::MAX),
            "A",
            [u32::MAX, u32::MAX],
            [1.; 4],
        );
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].rect, Rect::new(15, 11, 2, 2));
    }
}
