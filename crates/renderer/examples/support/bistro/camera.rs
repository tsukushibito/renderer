//! Caller-owned right-handed camera. Matrices are column-major, depth is [0,1].

use super::Error;
use serde_json::Value;

#[derive(Clone, Copy, Debug)]
pub(super) struct Camera {
    pub position: [f32; 3],
    pub target: [f32; 3],
    pub fov: f32,
}

pub(super) struct Conditions {
    pub views: Vec<(String, Camera)>,
    pub route: Vec<(usize, Camera)>,
    pub light_direction: [f32; 3],
    pub light_color: [f32; 3],
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
            background: vector(&value["output"]["background_linear_rgb"])?,
            near,
            far,
        })
    }

    pub fn camera(&self, view: &str, frame: usize) -> Result<Camera, Error> {
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
fn normalize(a: [f32; 3]) -> Result<[f32; 3], Error> {
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
    pub fn uniform(
        self,
        aspect: f32,
        near: f32,
        far: f32,
        light: [f32; 3],
        color: [f32; 3],
    ) -> Result<[u8; 128], Error> {
        let matrix = self.matrix(aspect, near, far)?;
        let mut words = [0.0f32; 32];
        words[..16].copy_from_slice(matrix.as_flattened());
        words[16..19].copy_from_slice(&self.position);
        words[19] = near;
        words[20..23].copy_from_slice(&normalize(light)?);
        words[23] = far;
        words[24..27].copy_from_slice(&color);
        // The frame builder fills the environment and shadow appendix from
        // prepared immutable assets; this helper only emits the camera prefix.
        let mut bytes = [0; 128];
        for (out, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
            *out = value.to_le_bytes();
        }
        Ok(bytes)
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
    fn conditions_route_returns_named_endpoints_and_loops() {
        let data: Value =
            serde_json::from_str(include_str!("../../../../../tools/bistro/conditions.json"))
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
        let mut data: Value =
            serde_json::from_str(include_str!("../../../../../tools/bistro/conditions.json"))
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
