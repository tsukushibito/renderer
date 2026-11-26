use crate::{Rotation3, Vec3};
use serde::{Deserialize, Serialize};

/// Rigid transform, applying rotation followed by translation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pose3 {
    pub translation: Vec3,
    pub rotation: Rotation3,
}

impl Pose3 {
    pub const IDENTITY: Self = Self::from_translation(Vec3::ZERO);

    pub const fn new(translation: Vec3, rotation: Rotation3) -> Self {
        Self {
            translation,
            rotation,
        }
    }

    pub const fn from_translation(translation: Vec3) -> Self {
        Self::new(translation, Rotation3::IDENTITY)
    }

    pub fn transform_point(self, point: Vec3) -> Vec3 {
        self.transform_vector(point) + self.translation
    }

    pub fn transform_vector(self, vector: Vec3) -> Vec3 {
        self.rotation.rotate_vector(vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_points_receive_the_translation() {
        let rotation = Rotation3::from_axis_angle(Vec3::Z, std::f32::consts::FRAC_PI_2).unwrap();
        let pose = Pose3::new(Vec3::X, rotation);
        assert!((pose.transform_vector(Vec3::X) - Vec3::Y).length() < 1e-6);
        assert!((pose.transform_point(Vec3::X) - (Vec3::X + Vec3::Y)).length() < 1e-6);
    }
}
