//! Selection geometry published together with one successfully displayed frame.
use crate::ObjectId;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pointer {
    pub view: String,
    pub frame: Option<String>,
    pub point: [f64; 2],
    /// Left, top, width, height of this view in the pointer's coordinate system.
    pub viewport: [f64; 4],
}

#[derive(Clone, Copy)]
struct Shape {
    id: ObjectId,
    center: [f64; 2],
    radius: [f64; 2],
    depth: f32,
}

pub struct PickView {
    name: &'static str,
    frame: Option<u64>,
    tick: u64,
    extent: [u32; 2],
    shapes: Vec<Shape>,
    limit: usize,
}
impl PickView {
    pub fn new(name: &'static str, limit: usize) -> Self {
        Self {
            name,
            frame: None,
            tick: 0,
            extent: [0, 0],
            shapes: Vec::with_capacity(limit),
            limit,
        }
    }
    pub fn begin(&mut self, frame: u64, tick: u64, extent: [u32; 2]) {
        self.shapes.clear();
        self.frame = Some(frame);
        self.tick = tick;
        self.extent = extent;
    }
    pub fn invalidate(&mut self) {
        self.frame = None;
        self.shapes.clear();
    }
    /// Coordinates are normalized to the view, with the origin at the top left.
    pub fn ellipse(
        &mut self,
        id: ObjectId,
        center: [f64; 2],
        radius: [f64; 2],
        depth: f32,
    ) -> Result<(), &'static str> {
        if self.shapes.len() == self.limit {
            return Err("CAPACITY");
        }
        if !center.into_iter().all(f64::is_finite)
            || !radius.into_iter().all(|x| x.is_finite() && x > 0.)
            || !depth.is_finite()
        {
            return Err("BAD_SHAPE");
        }
        self.shapes.push(Shape {
            id,
            center,
            radius,
            depth,
        });
        Ok(())
    }
    pub fn descriptor(&self) -> Value {
        json!({"view":self.name,"frame":self.frame.map(|n|n.to_string()),"tick":self.tick.to_string(),"extent":self.extent})
    }
    pub fn center(&self, id: ObjectId) -> Option<[f64; 2]> {
        self.shapes.iter().find(|s| s.id == id).map(|s| s.center)
    }
    pub fn pick(&self, p: &Pointer) -> Result<Option<ObjectId>, &'static str> {
        let frame = self.frame.ok_or("NO_DISPLAYED_FRAME")?;
        if p.view != self.name {
            return Err("UNKNOWN_VIEW");
        }
        if let Some(value) = &p.frame
            && value.parse::<u64>().map_err(|_| "BAD_FRAME")? != frame
        {
            return Err("FRAME_EXPIRED");
        }
        let [left, top, w, h] = p.viewport;
        if !p.point.into_iter().chain(p.viewport).all(f64::is_finite) || w <= 0. || h <= 0. {
            return Err("BAD_VIEWPORT");
        }
        let pos = [(p.point[0] - left) / w, (p.point[1] - top) / h];
        if !pos.into_iter().all(|x| (0.0..1.0).contains(&x)) {
            return Ok(None);
        }
        Ok(self
            .shapes
            .iter()
            .filter(|s| {
                let x = (pos[0] - s.center[0]) / s.radius[0];
                let y = (pos[1] - s.center[1]) / s.radius[1];
                x * x + y * y <= 1.
            })
            .min_by(|a, b| a.depth.total_cmp(&b.depth).then(a.id.cmp(&b.id)))
            .map(|s| s.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointer_uses_the_displayed_revision_viewport_and_nearest_subject() {
        let mut displayed = PickView::new("main", 2);
        let mut p = Pointer {
            view: "main".into(),
            frame: Some("1".into()),
            point: [150., 70.],
            viewport: [50., 20., 200., 100.],
        };
        displayed.begin(1, 4, [800, 400]);
        displayed.ellipse(3, [0.5, 0.5], [0.2, 0.2], 0.8).unwrap();
        displayed.ellipse(7, [0.5, 0.5], [0.1, 0.1], 0.3).unwrap();
        assert_eq!(displayed.pick(&p), Ok(Some(7)));
        p.point = [52., 24.];
        assert_eq!(displayed.pick(&p), Ok(None));
        displayed.begin(2, 5, [400, 400]);
        displayed.ellipse(7, [0.8, 0.5], [0.1, 0.1], 0.2).unwrap();
        assert_eq!(displayed.pick(&p), Err("FRAME_EXPIRED"));
        p.frame = None;
        p.viewport = [0., 0., 400., 400.];
        p.point = [320., 200.];
        assert_eq!(displayed.pick(&p), Ok(Some(7)));
        displayed.invalidate();
        assert_eq!(displayed.pick(&p), Err("NO_DISPLAYED_FRAME"));
    }
}
