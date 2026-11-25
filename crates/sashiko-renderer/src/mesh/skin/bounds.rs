use super::{Bounds, GraphError, Skin, Transform};

/// Bind-space intervals over the vertices actually influenced by each joint.
pub(super) struct JointBounds(Vec<Option<Bounds>>);

impl JointBounds {
    pub(super) fn from_skin(skin: &Skin) -> Self {
        let mut ranges = vec![([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]); skin.joints];
        for (vertex, influence) in &skin.vertices {
            for (j, w) in influence.joints.into_iter().zip(influence.weights) {
                if w == 0. {
                    continue;
                }
                let (low, high) = &mut ranges[usize::from(j)];
                for (axis, p) in vertex.position().into_iter().enumerate() {
                    low[axis] = low[axis].min(p);
                    high[axis] = high[axis].max(p);
                }
            }
        }
        Self(
            ranges
                .into_iter()
                .map(|(low, high)| {
                    (low[0] <= high[0])
                        .then(|| Bounds::new(low, high).expect("finite bind positions"))
                })
                .collect(),
        )
    }

    pub(super) fn transform(&self, palette: &[Transform]) -> Result<Bounds, GraphError> {
        let mut low = [f64::INFINITY; 3];
        let mut high = [f64::NEG_INFINITY; 3];
        for (range, joint) in self.0.iter().zip(palette) {
            let Some(range) = range else { continue };
            let cols = joint.columns();
            for axis in 0..3 {
                let mut min = f64::from(cols[3][axis]);
                let mut max = min;
                let mut magnitude = min.abs();
                let mut flush = f64::from(f32::MIN_POSITIVE);
                for (column, (a, b)) in cols.iter().zip(range.min().into_iter().zip(range.max())) {
                    let c = f64::from(column[axis]);
                    let (a, b) = (f64::from(a), f64::from(b));
                    let (x, y) = (a * c, b * c);
                    min += x.min(y);
                    max += x.max(y);
                    magnitude += x.abs().max(y.abs());
                    flush += f64::from(f32::MIN_POSITIVE) * (c.abs() + a.abs().max(b.abs()));
                }
                // Four weighted dots, rounded normalized weights, and shader
                // implementations that flush subnormal inputs/intermediates.
                let error = 32. * (f64::from(f32::EPSILON) * magnitude + flush);
                if magnitude + error > f64::from(f32::MAX) {
                    return Err(GraphError::invalid_input(
                        "skin position exceeds shader precision",
                    ));
                }
                low[axis] = low[axis].min(min - error);
                high[axis] = high[axis].max(max + error);
            }
        }
        Bounds::new(low.map(|v| v as f32), high.map(|v| v as f32))
    }
}
