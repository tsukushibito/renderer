//! Caller-owned right-handed camera. Matrices are column-major, depth is `[0, 1]`.

use super::{Error, shadow::Bounds};
use serde_json::Value;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Camera {
    pub position: [f32; 3],
    pub target: [f32; 3],
    pub fov: f32,
}

/// Clip planes for the exact FP32 view-projection matrix consumed by PBR.
/// An uncertifiable matrix leaves the original draw list visible.
pub(crate) struct Frustum {
    planes: Option<[ClipPlane; 6]>,
}

struct ClipPlane {
    coefficients: [f64; 4],
    tolerance: f64,
}

impl ClipPlane {
    fn combine(row: [f64; 4], other: [f64; 4], sign: f64, tolerance: f64) -> Self {
        Self {
            coefficients: std::array::from_fn(|i| row[i] + sign * other[i]),
            tolerance,
        }
    }

    fn overlaps(&self, bounds: Bounds) -> bool {
        let mut maximum = self.coefficients[3];
        for (axis, coefficient) in self.coefficients[..3].iter().copied().enumerate() {
            let corner = if coefficient >= 0.0 {
                bounds.max[axis]
            } else {
                bounds.min[axis]
            };
            maximum += coefficient * f64::from(corner);
        }
        maximum >= -self.tolerance || !maximum.is_finite()
    }
}

impl Frustum {
    pub fn new(matrix: [[f32; 4]; 4], scene: Bounds) -> Self {
        let rows: [[f64; 4]; 4] =
            std::array::from_fn(|row| matrix.map(|column| f64::from(column[row])));
        let position_limit: [f64; 3] =
            std::array::from_fn(|i| f64::from(scene.min[i].abs().max(scene.max[i].abs())));
        let errors = rows.map(|row| {
            let size = row[3].abs()
                + row[..3]
                    .iter()
                    .zip(position_limit)
                    .map(|(coefficient, limit)| coefficient.abs() * limit)
                    .sum::<f64>();
            if !size.is_finite() || size >= 0.5 * f64::from(f32::MAX) {
                return None;
            }
            // Bound the three products and three sums in pbr.vert relative
            // to the absolute terms, including cancellation and subnormals.
            let rounding = 32.0 * f64::from(f32::EPSILON) * size.max(1.0);
            let denorm = 8.0
                * f64::from(f32::MIN_POSITIVE)
                * (1.0
                    + position_limit.into_iter().sum::<f64>()
                    + row.into_iter().map(f64::abs).sum::<f64>());
            Some(rounding + denorm)
        });
        let [Some(dx), Some(dy), Some(dz), Some(dw)] = errors else {
            return Self { planes: None };
        };
        let [x, y, z, w] = rows;
        // Work in homogeneous coordinates without dividing by W. Both APIs
        // use [0, W] depth, and their clip-Y flip exchanges the two Y planes.
        Self {
            planes: Some([
                ClipPlane::combine(w, x, 1.0, dw + dx),
                ClipPlane::combine(w, x, -1.0, dw + dx),
                ClipPlane::combine(w, y, 1.0, dw + dy),
                ClipPlane::combine(w, y, -1.0, dw + dy),
                ClipPlane {
                    coefficients: z,
                    tolerance: dz,
                },
                ClipPlane::combine(w, z, -1.0, dw + dz),
            ]),
        }
    }

    pub fn overlaps(&self, bounds: Bounds) -> bool {
        self.planes
            .as_ref()
            .is_none_or(|planes| planes.iter().all(|plane| plane.overlaps(bounds)))
    }
}

/// Offscreen captures use the route's 60 Hz frame indices.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RouteCapture {
    Samples,
    Clip,
}

impl RouteCapture {
    pub fn frames(self) -> Vec<usize> {
        match self {
            Self::Samples => vec![0, 300, 600, 900, 1200, 1500, 1799],
            Self::Clip => (0..120).step_by(2).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    Reference,
    LumberyardDay,
}

impl std::str::FromStr for Look {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "reference" => Ok(Self::Reference),
            "lumberyard-day" => Ok(Self::LumberyardDay),
            _ => Err(format!("unknown Bistro look: {value}").into()),
        }
    }
}

pub(crate) const DAY_IBL_STRENGTH: f32 = 0.92;

impl Look {
    pub fn apply(
        self,
        conditions: &mut Conditions,
        environment: &mut super::ibl::Environment,
    ) -> Value {
        let name = match self {
            Self::Reference => "reference",
            Self::LumberyardDay => {
                conditions.light_direction = BAKE_SCENARIOS[0].direction;
                conditions.light_color = [8.4, 8.0, 7.1];
                conditions.background = [0.55, 0.68, 0.92];
                environment.strength = DAY_IBL_STRENGTH;
                "lumberyard-day"
            }
        };
        serde_json::json!({
            "name": name, "direction_to_light": conditions.light_direction,
            "irradiance_rgb": conditions.light_color, "background_linear_rgb": conditions.background,
            "ibl_strength": environment.strength, "ibl_yaw_degrees": environment.yaw_degrees,
            "exposure_ev": 1
        })
    }
}

pub(crate) struct BakeScenario {
    pub sky: [f32; 3],
    pub direction: [f32; 3],
    pub sun: [f32; 3],
}
pub(crate) const BAKE_SCENARIOS: [BakeScenario; 2] = [
    BakeScenario {
        sky: [0.55, 0.68, 0.92],
        // Fit to the menu board and streetlamp shadows in ORCA exterior plate 1.
        direction: [0.519167, 0.4784949, -0.7081725],
        sun: [8.4, 8.0, 7.1],
    },
    BakeScenario {
        sky: [0.16, 0.13, 0.20],
        direction: [-0.8, 0.22, -0.56],
        sun: [3.8, 1.35, 0.55],
    },
];

impl BakeScenario {
    /// Shared by the runtime sun, background and offline diffuse keyframes.
    pub(crate) fn at(t: f32) -> Self {
        let [day, dusk] = BAKE_SCENARIOS;
        let mix = |a: [f32; 3], b: [f32; 3]| std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t);
        Self {
            direction: mix(day.direction, dusk.direction),
            sun: mix(day.sun, dusk.sun),
            sky: mix(day.sky, dusk.sky),
        }
    }
}

/// Identify the extra, independently integrated midpoint in a daylight sequence.
pub(crate) fn daylight_sequence_identity(base: &str) -> String {
    use sha2::{Digest, Sha256};
    let mid = BakeScenario::at(0.5);
    let mut hash = Sha256::new();
    hash.update(base.as_bytes());
    hash.update(b"daylight-3");
    for v in mid.direction.into_iter().chain(mid.sky).chain(mid.sun) {
        hash.update(v.to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}

/// Changing either the geometry cook or a lighting endpoint invalidates the bake.
pub(crate) fn bake_identity(cook_sha256: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(cook_sha256.as_bytes());
    for scenario in BAKE_SCENARIOS {
        for word in scenario
            .sky
            .into_iter()
            .chain(scenario.sun)
            .chain(scenario.direction)
        {
            hash.update(word.to_le_bytes());
        }
    }
    format!("{:x}", hash.finalize())
}

/// An environment-lit bake must match the source panorama and its world orientation.
pub(crate) fn environment_bake_identity(
    base: &str,
    env: &super::ibl::Environment,
) -> Option<String> {
    use sha2::{Digest, Sha256};
    let manifest = env.identity["manifest_sha256"].as_str()?;
    let mut hash = Sha256::new();
    hash.update(base.as_bytes());
    hash.update(manifest.as_bytes());
    hash.update(env.strength.to_le_bytes());
    hash.update(env.yaw_degrees.to_le_bytes());
    Some(format!("{:x}", hash.finalize()))
}

pub(crate) struct Conditions {
    pub views: Vec<(String, Camera)>,
    pub route: Vec<(usize, Camera)>,
    pub light_direction: [f32; 3],
    pub light_color: [f32; 3],
    pub ambient_occlusion: bool,
    pub background: [f32; 3],
    pub near: f32,
    pub far: f32,
}

fn vector(value: &Value) -> Result<[f32; 3], Error> {
    let values = value
        .as_array()
        .filter(|values| values.len() == 3)
        .ok_or("expected three camera components")?;
    let mut output = [0.0; 3];
    for (out, value) in output.iter_mut().zip(values) {
        *out = value.as_f64().ok_or("expected numeric camera component")? as f32;
        if !out.is_finite() {
            return Err("non-finite camera component".into());
        }
    }
    Ok(output)
}

impl Conditions {
    pub fn frame_input(
        &self,
        camera: Camera,
        width: u32,
        height: u32,
    ) -> Result<CameraInput, Error> {
        camera.frame_input(
            width as f32 / height as f32,
            self.near,
            self.far,
            self.light_direction,
            self.light_color,
        )
    }

    pub fn parse(value: &Value) -> Result<Self, Error> {
        let mut views = Vec::new();
        for view in value["views"].as_array().ok_or("missing fixed views")? {
            let name = view["name"].as_str().ok_or("missing view name")?.to_owned();
            if views.iter().any(|(old, _)| old == &name) {
                return Err("duplicate view name".into());
            }
            let camera = Camera {
                position: vector(&view["position"])?,
                target: vector(&view["target"])?,
                fov: view["vertical_fov_degrees"]
                    .as_f64()
                    .ok_or("missing camera FOV")? as f32,
            };
            camera.matrix(1.0, 0.02, 500.0)?;
            views.push((name, camera));
        }
        if views.is_empty() {
            return Err("at least one fixed camera view is required".into());
        }
        let fps = value["route"]["fps"].as_u64().ok_or("missing route FPS")? as usize;
        let mut route = Vec::new();
        for key in value["route"]["keyframes"]
            .as_array()
            .ok_or("missing route")?
        {
            let seconds = key["seconds"].as_u64().ok_or("invalid route time")? as usize;
            let name = key["view"].as_str().ok_or("invalid route view")?;
            let camera = views
                .iter()
                .find(|(old, _)| old == name)
                .ok_or("route references missing view")?
                .1;
            let frame = seconds.checked_mul(fps).ok_or("route frame overflow")?;
            if route.last().is_some_and(|(previous, _)| *previous >= frame) {
                return Err("route times must increase".into());
            }
            route.push((frame, camera));
        }
        if route.len() < 2 || route[0].0 != 0 {
            return Err("invalid route endpoints".into());
        }
        let near = value["camera"]["near_m"]
            .as_f64()
            .ok_or("missing camera near")? as f32;
        let far = value["camera"]["far_m"]
            .as_f64()
            .ok_or("missing camera far")? as f32;
        views[0].1.matrix(1.0, near, far)?;
        Ok(Self {
            views,
            route,
            light_direction: vector(&value["lighting"]["sun"]["direction_to_light"])?,
            light_color: vector(&value["lighting"]["sun"]["irradiance_rgb"])?,
            ambient_occlusion: value["lighting"]["ambient_occlusion"]["enabled"]
                .as_bool()
                .ok_or("missing ambient occlusion lighting policy")?,
            background: vector(&value["output"]["background_linear_rgb"])?,
            near,
            far,
        })
    }

    pub fn camera(&self, view: &str, frame: usize) -> Result<Camera, Error> {
        // ORCA exterior plate 1, fitted from the scooter, street posts and manhole.
        if view == "bistro-corner" {
            return Ok(Camera {
                position: [-23.01494, 6.42312, -0.15436],
                target: [-13.36295, 3.82032, 0.09966],
                fov: 37.83862,
            });
        }
        if view != "route" {
            return self
                .views
                .iter()
                .find(|(name, _)| name == view)
                .map(|(_, camera)| *camera)
                .ok_or_else(|| format!("unknown camera view: {view}").into());
        }
        let period = self.route.last().unwrap().0;
        let frame = frame % period;
        let end = self.route.partition_point(|(time, _)| *time <= frame);
        let (a, first) = self.route[end - 1];
        let (b, second) = self.route[end];
        Ok(first.interpolate(second, (frame - a) as f32 / (b - a) as f32))
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub(crate) fn normalize(a: [f32; 3]) -> Result<[f32; 3], Error> {
    let length = dot(a, a).sqrt();
    if !length.is_finite() || length < 1e-8 {
        return Err("degenerate camera basis".into());
    }
    Ok(a.map(|value| value / length))
}

impl Camera {
    pub fn fixture() -> Self {
        Self {
            position: [0.0, 0.0, 4.0],
            target: [0.0; 3],
            fov: 60.0,
        }
    }
    pub fn distance_squared(self, position: [f32; 3]) -> f32 {
        let delta = sub(position, self.position);
        dot(delta, delta)
    }
    fn interpolate(self, other: Self, t: f32) -> Self {
        Self {
            position: std::array::from_fn(|i| {
                self.position[i] + (other.position[i] - self.position[i]) * t
            }),
            target: std::array::from_fn(|i| {
                self.target[i] + (other.target[i] - self.target[i]) * t
            }),
            fov: self.fov + (other.fov - self.fov) * t,
        }
    }
    pub fn matrix(self, aspect: f32, near: f32, far: f32) -> Result<[[f32; 4]; 4], Error> {
        if !aspect.is_finite()
            || aspect <= 0.0
            || !near.is_finite()
            || !far.is_finite()
            || near <= 0.0
            || far <= near
            || !self.fov.is_finite()
            || self.fov <= 0.0
            || self.fov >= 179.0
        {
            return Err("invalid perspective camera".into());
        }
        let z = normalize(sub(self.position, self.target))?;
        let x = normalize(cross([0.0, 1.0, 0.0], z))?;
        let y = cross(z, x);
        let view = [
            [x[0], y[0], z[0], 0.0],
            [x[1], y[1], z[1], 0.0],
            [x[2], y[2], z[2], 0.0],
            [
                -dot(x, self.position),
                -dot(y, self.position),
                -dot(z, self.position),
                1.0,
            ],
        ];
        let scale = 1.0 / (self.fov.to_radians() * 0.5).tan();
        // Positive-height Vulkan viewports map negative clip-Y to image top.
        // The generated DXIL applies the matching SPIRV-Cross vertex-Y flip.
        let projection = [
            [scale / aspect, 0.0, 0.0, 0.0],
            [0.0, -scale, 0.0, 0.0],
            [0.0, 0.0, far / (near - far), -1.0],
            [0.0, 0.0, near * far / (near - far), 0.0],
        ];
        Ok(std::array::from_fn(|column| {
            std::array::from_fn(|row| (0..4).map(|i| projection[i][row] * view[column][i]).sum())
        }))
    }
    pub fn frame_input(
        self,
        aspect: f32,
        near: f32,
        far: f32,
        light: [f32; 3],
        color: [f32; 3],
    ) -> Result<CameraInput, Error> {
        let matrix = self.matrix(aspect, near, far)?;
        Ok(CameraInput {
            scenario_blend: 0.,
            matrix,
            camera: self,
            near,
            far,
            direction_to_light: normalize(light)?,
            irradiance: color,
        })
    }
}

pub(crate) struct CameraInput {
    pub scenario_blend: f32,
    pub matrix: [[f32; 4]; 4],
    pub camera: Camera,
    pub near: f32,
    pub far: f32,
    pub direction_to_light: [f32; 3],
    pub irradiance: [f32; 3],
}

impl CameraInput {
    pub fn uniform_bytes(&self) -> [u8; 128] {
        let mut words = [0.0f32; 32];
        words[..16].copy_from_slice(self.matrix.as_flattened());
        words[16..19].copy_from_slice(&self.camera.position);
        words[19] = self.near;
        words[20..23].copy_from_slice(&self.direction_to_light);
        words[23] = self.far;
        words[24..27].copy_from_slice(&self.irradiance);
        // The frame builder fills the environment and shadow appendix from
        // prepared immutable assets; this helper only emits the camera prefix.
        let mut bytes = [0; 128];
        for (out, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *out = value.to_le_bytes();
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project(matrix: [[f32; 4]; 4], p: [f32; 4]) -> [f32; 3] {
        let out: [f32; 4] = std::array::from_fn(|r| (0..4).map(|c| matrix[c][r] * p[c]).sum());
        std::array::from_fn(|i| out[i] / out[3])
    }
    #[test]
    fn camera_preserves_center_up_and_zero_to_one_depth() {
        let camera = Camera::fixture();
        let matrix = camera.matrix(1.0, 0.02, 500.0).unwrap();
        let center = project(matrix, [0.0, 0.0, 0.0, 1.0]);
        assert!(center[0].abs() < 1e-6 && center[1].abs() < 1e-6);
        assert!(project(matrix, [0.0, 1.0, 0.0, 1.0])[1] < 0.0);
        assert!(project(matrix, [0.0, 0.0, 3.98, 1.0])[2].abs() < 1e-4);
        assert!((project(matrix, [0.0, 0.0, -496.0, 1.0])[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn frustum_culls_only_bounds_outside_one_homogeneous_plane() {
        let scene = Bounds::new([-10.0; 3], [10.0; 3]).unwrap();
        let matrix = Camera::fixture().matrix(1.0, 1.0, 10.0).unwrap();
        let frustum = Frustum::new(matrix, scene);
        let visible = |point| frustum.overlaps(Bounds::new(point, point).unwrap());
        assert!(visible([0.0, 0.0, 0.0]));
        assert!(!visible([0.0, 0.0, 3.5]));
        assert!(!visible([0.0, 0.0, 5.0]));
        assert!(!visible([0.0, 0.0, -7.0]));
        assert!(!visible([-4.0, 0.0, 0.0]));
        assert!(!visible([4.0, 0.0, 0.0]));
        assert!(!visible([0.0, -4.0, 0.0]));
        assert!(!visible([0.0, 4.0, 0.0]));
        assert!(frustum.overlaps(Bounds::new([-8.0, -8.0, 0.0], [8.0, 8.0, 5.0]).unwrap()));
    }

    #[test]
    fn frustum_retains_clip_edges_and_falls_back_for_uncertifiable_math() {
        let scene = Bounds::new([-10.0; 3], [10.0; 3]).unwrap();
        let mut matrix = std::array::from_fn(|column| {
            std::array::from_fn(|row| if column == row { 1.0 } else { 0.0 })
        });
        let frustum = Frustum::new(matrix, scene);
        let edge = [1.0 + f32::EPSILON, 0.0, 0.0];
        let outside = [2.0, 2.0, 2.0];
        assert!(frustum.overlaps(Bounds::new(edge, edge).unwrap()));
        assert!(!frustum.overlaps(Bounds::new(outside, outside).unwrap()));
        for coefficient in [f32::NAN, f32::MAX] {
            matrix[0][0] = coefficient;
            assert!(Frustum::new(matrix, scene).overlaps(Bounds::new(outside, outside).unwrap()));
        }
    }
    #[test]
    fn conditions_route_returns_named_endpoints_and_loops() {
        let data: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/bistro/conditions.json"
        )))
        .unwrap();
        let conditions = Conditions::parse(&data).unwrap();
        assert_eq!(
            conditions.camera("route", 0).unwrap().position,
            conditions.camera("street", 0).unwrap().position
        );
        assert_eq!(
            conditions.camera("route", 900).unwrap().position,
            conditions.camera("street-end", 0).unwrap().position
        );
        assert_eq!(
            conditions.camera("route", 1800).unwrap().position,
            conditions.camera("route", 0).unwrap().position
        );
        assert!(conditions.camera("missing", 0).is_err());
    }

    #[test]
    fn empty_views_and_invalid_projection_fov_are_rejected() {
        let mut data: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/bistro/conditions.json"
        )))
        .unwrap();
        data["views"] = serde_json::json!([]);
        assert!(Conditions::parse(&data).is_err());
        for fov in [0.0, -1.0, 179.0, f32::NAN, f32::INFINITY] {
            assert!(
                Camera {
                    fov,
                    ..Camera::fixture()
                }
                .matrix(1.0, 0.02, 500.0)
                .is_err()
            );
        }
    }
}
