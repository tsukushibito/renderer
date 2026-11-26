//! Application math. Vectors use component order XY or XYZ; rotations are right-handed.
use serde::{Deserialize, Serialize};
use std::ops::{Add, AddAssign, Div, Index, IndexMut, Mul, Neg, Sub};

macro_rules! vector {
    ($name:ident, $size:literal, $array:literal, $($axis:ident),+) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
        #[serde(from = $array, into = $array)]
        pub struct $name(pub(crate) glam::$name);

        impl $name {
            pub const ZERO: Self = Self(glam::$name::ZERO);
            pub const ONE: Self = Self(glam::$name::ONE);
            pub const fn new($($axis: f32),+) -> Self {
                Self(glam::$name::new($($axis),+))
            }
            $(pub const fn $axis(self) -> f32 {
                self.0.$axis
            })+
            pub const fn from_array(value: [f32; $size]) -> Self {
                Self(glam::$name::from_array(value))
            }
            pub const fn to_array(self) -> [f32; $size] {
                self.0.to_array()
            }
            pub fn dot(self, rhs: Self) -> f32 {
                self.0.dot(rhs.0)
            }
            pub fn distance_squared(self, rhs: Self) -> f32 {
                let delta = self - rhs;
                delta.dot(delta)
            }
            /// Returns zero when the length is zero or non-finite.
            pub fn normalize_or_zero(self) -> Self {
                let length = self.length();
                if length > 0.0 && length.is_finite() {
                    self / length
                } else {
                    Self::ZERO
                }
            }
        }
        impl From<[f32; $size]> for $name {
            fn from(value: [f32; $size]) -> Self {
                Self::from_array(value)
            }
        }
        impl From<$name> for [f32; $size] {
            fn from(value: $name) -> Self {
                value.to_array()
            }
        }
        impl Index<usize> for $name {
            type Output = f32;
            fn index(&self, index: usize) -> &f32 {
                &self.0[index]
            }
        }
        impl IndexMut<usize> for $name {
            fn index_mut(&mut self, index: usize) -> &mut f32 {
                &mut self.0[index]
            }
        }
        impl Add for $name {
            type Output = Self;
            fn add(self, rhs: Self) -> Self {
                Self(self.0 + rhs.0)
            }
        }
        impl AddAssign for $name {
            fn add_assign(&mut self, rhs: Self) {
                self.0 += rhs.0;
            }
        }
        impl Sub for $name {
            type Output = Self;
            fn sub(self, rhs: Self) -> Self {
                Self(self.0 - rhs.0)
            }
        }
        impl Mul<f32> for $name {
            type Output = Self;
            fn mul(self, rhs: f32) -> Self {
                Self(self.0 * rhs)
            }
        }
        impl Div<f32> for $name {
            type Output = Self;
            fn div(self, rhs: f32) -> Self {
                Self(self.0 / rhs)
            }
        }
        impl Neg for $name {
            type Output = Self;
            fn neg(self) -> Self {
                Self(-self.0)
            }
        }
    };
}
vector!(Vec2, 2, "[f32; 2]", x, y);
vector!(Vec3, 3, "[f32; 3]", x, y, z);

impl Vec2 {
    pub fn length(self) -> f32 {
        self.x().hypot(self.y())
    }
}
impl Vec3 {
    pub const X: Self = Self(glam::Vec3::X);
    pub const Y: Self = Self(glam::Vec3::Y);
    pub const Z: Self = Self(glam::Vec3::Z);
    pub fn length(self) -> f32 {
        self.x().hypot(self.y()).hypot(self.z())
    }
    pub fn cross(self, rhs: Self) -> Self {
        Self(self.0.cross(rhs.0))
    }
}

/// A unit quaternion. Serialized components are in XYZW order.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "[f32; 4]", into = "[f32; 4]")]
pub struct Rotation3(glam::Quat);

impl Rotation3 {
    pub const IDENTITY: Self = Self(glam::Quat::IDENTITY);

    pub fn from_axis_angle(axis: Vec3, radians: f32) -> Result<Self, &'static str> {
        let axis = axis
            .0
            .as_dvec3()
            .try_normalize()
            .ok_or("expected a finite nonzero axis")?;
        if !radians.is_finite() {
            return Err("expected a finite angle");
        }
        Ok(Self(
            glam::Quat::from_axis_angle(axis.as_vec3(), radians).normalize(),
        ))
    }
    /// Align +Y with up and +Z with forward projected onto the plane perpendicular to up.
    pub fn from_forward_up(forward: Vec3, up: Vec3) -> Result<Self, &'static str> {
        // Widen before crossing to handle the full range of finite f32 inputs.
        let up = up.0.as_dvec3();
        let right = up
            .cross(forward.0.as_dvec3())
            .try_normalize()
            .ok_or("expected independent finite forward and up vectors")?;
        let up = up.normalize();
        let basis = glam::DMat3::from_cols(right, up, right.cross(up));
        Ok(Self(glam::DQuat::from_mat3(&basis).as_quat().normalize()))
    }
    /// Apply rhs first, then self.
    pub fn compose(self, rhs: Self) -> Self {
        Self((self.0 * rhs.0).normalize())
    }
    pub fn rotate_vector(self, v: Vec3) -> Vec3 {
        Vec3(self.0 * v.0)
    }
    /// Shortest-arc interpolation, with t in the closed interval [0, 1].
    pub fn slerp(self, end: Self, t: f32) -> Result<Self, &'static str> {
        if !(0.0..=1.0).contains(&t) {
            return Err("expected interpolation weight in [0, 1]");
        }
        if t == 0. {
            return Ok(self);
        }
        if t == 1. {
            return Ok(end);
        }
        Ok(Self(self.0.slerp(end.0, t).normalize()))
    }
}
impl Default for Rotation3 {
    fn default() -> Self {
        Self::IDENTITY
    }
}
impl TryFrom<[f32; 4]> for Rotation3 {
    type Error = &'static str;
    fn try_from(value: [f32; 4]) -> Result<Self, Self::Error> {
        let rotation = glam::Quat::from_array(value);
        if !rotation.is_normalized() {
            return Err("expected a finite unit quaternion");
        }
        Ok(Self(rotation.normalize()))
    }
}
impl From<Rotation3> for [f32; 4] {
    fn from(value: Rotation3) -> Self {
        value.0.to_array()
    }
}

/// Parent-relative scale, rotation and translation, applied in that order.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalTransform {
    pub translation: Vec3,
    pub rotation: Rotation3,
    pub scale: Vec3,
}
impl LocalTransform {
    pub const IDENTITY: Self = Self::new(Vec3::ZERO, Rotation3::IDENTITY, Vec3::ONE);
    pub const fn new(translation: Vec3, rotation: Rotation3, scale: Vec3) -> Self {
        Self {
            translation,
            rotation,
            scale,
        }
    }
    pub fn transform_point(self, p: Vec3) -> Vec3 {
        self.transform_vector(p) + self.translation
    }
    pub fn transform_vector(self, v: Vec3) -> Vec3 {
        self.rotation.rotate_vector(Vec3(self.scale.0 * v.0))
    }
}
