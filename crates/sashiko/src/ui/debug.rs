//! Diagnostic geometry in output pixels. Games submit tagged primitives from their own view.
use super::{Color, DisplayFont, Layer, Overlay, Rect};
use crate::inspect::jobs::RenderLayers;

#[derive(Clone, Copy)]
pub enum Category {
    Collision,
    Velocity,
    Ai,
}

pub enum Shape<'a> {
    Line { from: [f32; 2], to: [f32; 2] },
    Ellipse { center: [f32; 2], radius: [f32; 2] },
    Label { at: [f32; 2], text: &'a str },
}

pub struct DebugDraw<'a> {
    canvas: Overlay<'a>,
    visible: RenderLayers,
    limit: usize,
}
impl<'a> DebugDraw<'a> {
    /// Append after the HUD, within the already prepared CPU and draw capacity.
    pub fn new(
        layers: &'a mut Vec<Layer>,
        extent: [u32; 2],
        visible: RenderLayers,
        limit: usize,
    ) -> Self {
        let limit = limit.min(layers.capacity());
        Self {
            canvas: Overlay { layers, extent },
            visible,
            limit,
        }
    }
    fn point(&mut self, [x, y]: [f32; 2], color: Color) {
        if x >= 0. && y >= 0. && self.canvas.layers.len() < self.limit {
            self.canvas.fill(Rect::new(x as u32, y as u32, 2, 2), color);
        }
    }
    /// Long segments use a dotted line to bound the number of Canvas draws.
    pub fn draw(&mut self, category: Category, shape: Shape<'_>, color: Color) {
        let enabled = match category {
            Category::Collision => self.visible.collision,
            Category::Velocity => self.visible.velocity,
            Category::Ai => self.visible.ai,
        };
        if !enabled {
            return;
        }
        match shape {
            Shape::Line { from, to } => {
                let dx = to[0] - from[0];
                let dy = to[1] - from[1];
                let steps = dx.abs().max(dy.abs()).ceil().clamp(1., 48.) as u32;
                for i in 0..=steps {
                    let t = i as f32 / steps as f32;
                    self.point([dx.mul_add(t, from[0]), dy.mul_add(t, from[1])], color);
                }
            }
            Shape::Ellipse { center, radius } => {
                for i in 0..24 {
                    let (s, c) = (std::f32::consts::TAU * i as f32 / 24.).sin_cos();
                    self.point(
                        [
                            radius[0].mul_add(c, center[0]),
                            radius[1].mul_add(s, center[1]),
                        ],
                        color,
                    );
                }
            }
            Shape::Label { at: [x, y], text } => {
                if x >= 0.
                    && y >= 0.
                    && text.len().saturating_mul(10)
                        <= self.limit.saturating_sub(self.canvas.layers.len())
                {
                    self.canvas
                        .label(DisplayFont::new(1), text, [x as u32, y as u32], color);
                }
            }
        }
    }
}
