use super::*;

/// A piecewise-linear map of one animation cycle to corresponding markers in another.
/// Construction matches unique marker IDs in cyclic order.
pub struct PhaseMap {
    knots: Vec<(f64, f64)>,
}

impl PhaseMap {
    /// A missing or unmatched track permits the caller to use normalized phase.
    pub fn between(a: &ClipTimeline, b: &ClipTimeline) -> Option<Self> {
        let from = &a.sync;
        let to = &b.sync;
        if from.is_empty() || from.len() != to.len() || !a.looping || !b.looping {
            return None;
        }
        let offset = to.iter().position(|m| m.id == from[0].id)?;
        let mut knots = Vec::with_capacity(from.len());
        for (i, m) in from.iter().enumerate() {
            let j = (offset + i) % to.len();
            if to[j].id != m.id {
                return None;
            }
            knots.push((
                m.frame as f64 / a.frames as f64,
                to[j].frame as f64 / b.frames as f64 + f64::from(j < offset),
            ));
        }
        Some(Self { knots })
    }

    /// Map an unwrapped cycle position; differences preserve crossings over the seam.
    pub fn map(&self, cycles: f64) -> f64 {
        let phase = cycles.rem_euclid(1.);
        let i = self.knots.partition_point(|&(x, _)| x <= phase);
        let (x0, y0) = if i == 0 {
            let (x, y) = self.knots[self.knots.len() - 1];
            (x - 1., y - 1.)
        } else {
            self.knots[i - 1]
        };
        let (x1, y1) = if i == self.knots.len() {
            let (x, y) = self.knots[0];
            (x + 1., y + 1.)
        } else {
            self.knots[i]
        };
        cycles.floor() + y0 + (y1 - y0) * ((phase - x0) / (x1 - x0))
    }
}
impl ClipTimeline {
    pub fn with_sync_markers(mut self, mut markers: Vec<Marker>) -> Result<Self, AnimationError> {
        markers.sort_by_key(|m| m.frame);
        let mut ids = std::collections::HashSet::new();
        if markers
            .iter()
            .any(|m| m.frame >= self.frames || !ids.insert(m.id))
            || markers.windows(2).any(|p| p[0].frame == p[1].frame)
        {
            return Err(Invalid("invalid sync markers"));
        }
        self.sync = markers;
        Ok(self)
    }
    pub fn sync_markers(&self) -> &[Marker] {
        &self.sync
    }
}
