use crate::Vec3;

/// Closed, axis-aligned box. Zero half-extents represent flat boxes or points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb3 {
    center: Vec3,
    half_extents: Vec3,
}

impl Aabb3 {
    /// Panics if any component is non-finite or a half-extent is negative.
    pub fn from_center_half_extents(center: Vec3, half_extents: Vec3) -> Self {
        assert!(
            center.0.is_finite()
                && half_extents.0.is_finite()
                && half_extents.0.cmpge(glam::Vec3::ZERO).all()
        );
        Self {
            center,
            half_extents,
        }
    }

    pub const fn center(self) -> Vec3 {
        self.center
    }

    pub const fn half_extents(self) -> Vec3 {
        self.half_extents
    }

    pub fn min(self) -> Vec3 {
        self.center - self.half_extents
    }

    pub fn max(self) -> Vec3 {
        self.center + self.half_extents
    }

    pub fn contains(self, point: Vec3) -> bool {
        self.min().0.cmple(point.0).all() && point.0.cmple(self.max().0).all()
    }

    /// Touching at a face, edge, or corner counts as overlap.
    pub fn overlaps(self, other: Self) -> bool {
        self.min().0.cmple(other.max().0).all() && other.min().0.cmple(self.max().0).all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_bounds_include_the_corner_and_touching_boxes() {
        let bounds = Aabb3::from_center_half_extents(Vec3::new(2., 3., 4.), Vec3::new(1., 2., 3.));
        assert_eq!(bounds.min(), Vec3::ONE);
        assert_eq!(bounds.max(), Vec3::new(3., 5., 7.));
        assert!(bounds.contains(bounds.max()));
        assert!(bounds.overlaps(Aabb3::from_center_half_extents(bounds.max(), Vec3::ZERO)));
        assert!(!bounds.overlaps(Aabb3::from_center_half_extents(
            bounds.max() + Vec3::X,
            Vec3::ZERO
        )));
    }
}
