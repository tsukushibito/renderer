//! Independent numeric probes for the caller's actual PBR/resolve connection.
//! These test-owned inputs are separate from the convenient CLI fixture.
//!
//! The reference uses f64 and Smith's slope/lambda formulation instead of the
//! shader's factored visibility expression. It models the glTF channel meanings,
//! OPAQUE/MASK/BLEND coverage and sRGB transfer explicitly. This is directional
//! light with IBL/shadows/AA explicitly disabled. SDR is independently computed
//! through B3's fixed exposure, Neutral transform, half storage and sRGB encode.

use super::asset::{Material, Pixels, Primitive, Scene, TextureImage, fallback_images, material};
use super::camera::Camera;
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use super::{backend, tests::build_material_test_frame};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 256;
const NEAR: f32 = 0.02;
const FAR: f32 = 500.0;
const LIGHT: [f32; 3] = [0.2, 0.6, 0.7745967];
const LIGHT_COLOR: [f32; 3] = [1.0, 0.9, 0.8];
const BACKGROUND: [f32; 3] = [0.18; 3];
const ABSOLUTE: f64 = 1.0 / 1024.0;
const RELATIVE: f64 = 1.0 / 1024.0;
const DEPTH_ABSOLUTE: f64 = 1.0 / 1_048_576.0;

type V3 = [f64; 3];

#[derive(Clone)]
struct Case {
    name: &'static str,
    center: [f64; 2],
    probe_offset: [f64; 2],
    half_size: f64,
    normal: V3,
    tangent: V3,
    sign: f64,
    normal_map: Option<[u8; 4]>,
    normal_scale: f64,
    base: [f64; 4],
    base_map: [u8; 4],
    base_pattern: Option<[[u8; 4]; 4]>,
    metallic: f64,
    roughness: f64,
    emissive: V3,
    emissive_map: [u8; 4],
    alpha_mode: u32,
    back: bool,
    double_sided: bool,
}

fn dot(a: V3, b: V3) -> f64 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn unit(v: V3) -> V3 {
    let length = dot(v, v).sqrt();
    assert!(length > 1e-12 && length.is_finite());
    v.map(|x| x / length)
}
fn srgb_decode(byte: u8) -> f64 {
    srgb_linear(f64::from(byte) / 255.)
}
fn srgb_linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
fn srgb_encode(value: f64) -> f64 {
    let value = value.clamp(0., 1.);
    if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    }
}

fn cases() -> Vec<Case> {
    let names = [
        "dielectric",
        "metallic-only",
        "roughness-low",
        "roughness-high",
        "normal-scale-zero",
        "normal-y-positive",
        "normal-y-negative-handedness",
        "rotated-tangent",
        "emissive-only",
        "single-sided-back",
        "double-sided-back",
        "mask-zero",
        "blend-zero",
        "blend-half",
        "base-srgb",
        "emissive-srgb",
    ];
    let mut output: Vec<_> = names
        .into_iter()
        .enumerate()
        .map(|(i, name)| Case {
            name,
            center: [(i % 4) as f64 * 0.9 - 1.35, 1.35 - (i / 4) as f64 * 0.9],
            probe_offset: [0.; 2],
            half_size: 0.34,
            normal: [0., 0., 1.],
            tangent: [1., 0., 0.],
            sign: 1.,
            normal_map: None,
            normal_scale: 1.,
            base: [0.8, 0.4, 0.2, 1.],
            base_map: [255; 4],
            base_pattern: None,
            metallic: 0.,
            roughness: 0.6,
            emissive: [0.; 3],
            emissive_map: [255; 4],
            alpha_mode: 0,
            back: false,
            double_sided: false,
        })
        .collect();
    output[1].metallic = 1.;
    output[2].roughness = 0.2;
    output[3].roughness = 0.8;
    for case in &mut output[4..8] {
        case.normal_map = Some([128, 220, 220, 255]);
    }
    output[4].normal_scale = 0.;
    output[6].sign = -1.;
    output[7].tangent = [0., 1., 0.];
    output[8].emissive = [0.15, 0.05, 0.025];
    output[9].back = true;
    output[10].back = true;
    output[10].double_sided = true;
    output[11].alpha_mode = 1;
    output[11].base_map = [255, 128, 64, 0];
    output[12].alpha_mode = 2;
    output[12].base_map = [255, 128, 64, 0];
    output[13].alpha_mode = 2;
    output[13].base = [0.1, 0.4, 0.8, 0.5];
    output[14].base_map = [128, 192, 64, 255];
    output[14].base_pattern = Some([
        [16, 32, 64, 255],
        [224, 48, 96, 255],
        [64, 192, 24, 255],
        [160, 112, 240, 255],
    ]);
    output[14].probe_offset = [0.15, 0.1];
    output[15].emissive = [0.3, 0.2, 0.1];
    output[15].emissive_map = [128, 64, 200, 255];
    // A tiny disjoint panel in the central grid gap exercises the legitimate
    // GGX peak above binary16 max. Its normal bisects the actual probe's V/L.
    let mut peak = output[0].clone();
    peak.name = "finite-half-peak";
    peak.center = [0., 0.];
    peak.half_size = 0.06;
    peak.base = [1.; 4];
    peak.metallic = 1.;
    peak.roughness = 0.;
    let (_, position, _) = probe(&peak);
    let view = unit([-position[0], -position[1], 4.]);
    let light = unit(LIGHT.map(f64::from));
    peak.normal = unit(std::array::from_fn(|i| view[i] + light[i]));
    peak.tangent = unit(std::array::from_fn(|i| {
        f64::from(i == 0) - peak.normal[0] * peak.normal[i]
    }));
    output.push(peak);
    output
}

/// Use the actual caller Camera matrix to pick a raster pixel, then solve the
/// planar projection for that pixel center. This is not a quad-center proxy:
/// the shader's interpolated world position and view vector vary per pixel.
fn probe(case: &Case) -> ([u32; 2], V3, f64) {
    let m = Camera::fixture()
        .matrix(WIDTH as f32 / HEIGHT as f32, NEAR, FAR)
        .unwrap()
        .map(|col| col.map(f64::from));
    let p = [
        case.center[0] + case.probe_offset[0],
        case.center[1] + case.probe_offset[1],
        0.,
        1.,
    ];
    let clip: [f64; 4] = std::array::from_fn(|r| (0..4).map(|c| m[c][r] * p[c]).sum());
    let pixel = [
        ((clip[0] / clip[3] + 1.) * f64::from(WIDTH) * 0.5).floor() as u32,
        ((clip[1] / clip[3] + 1.) * f64::from(HEIGHT) * 0.5).floor() as u32,
    ];
    assert!(pixel[0] < WIDTH && pixel[1] < HEIGHT);
    assert_eq!((m[0][3], m[1][3]), (0., 0.));
    let ndc = [
        2. * (f64::from(pixel[0]) + 0.5) / f64::from(WIDTH) - 1.,
        2. * (f64::from(pixel[1]) + 0.5) / f64::from(HEIGHT) - 1.,
    ];
    let position = [
        (ndc[0] * clip[3] - m[3][0]) / m[0][0],
        (ndc[1] * clip[3] - m[3][1]) / m[1][1],
        0.,
    ];
    assert!((position[0] - case.center[0]).abs() < case.half_size);
    assert!((position[1] - case.center[1]).abs() < case.half_size);
    (pixel, position, clip[2] / clip[3])
}

fn image(images: &mut Vec<TextureImage>, bytes: [u8; 4], srgb: bool) -> usize {
    let id = images.len();
    images.push(TextureImage {
        width: 1,
        height: 1,
        srgb,
        mips: std::iter::once(0..4).collect(),
        pixels: Pixels::Owned(bytes.to_vec()),
    });
    id
}

fn base_image(images: &mut Vec<TextureImage>, case: &Case) -> usize {
    let Some(pattern) = case.base_pattern else {
        return image(images, case.base_map, true);
    };
    let mut bytes: Vec<u8> = pattern.into_iter().flatten().collect();
    for channel in 0..3 {
        let mean = pattern
            .iter()
            .map(|pixel| srgb_decode(pixel[channel]))
            .sum::<f64>()
            / 4.;
        bytes.push((srgb_encode(mean) * 255.).round() as u8);
    }
    bytes.push(255);
    let id = images.len();
    images.push(TextureImage {
        width: 2,
        height: 2,
        srgb: true,
        mips: vec![0..16, 16..20],
        pixels: Pixels::Owned(bytes),
    });
    id
}

fn uv(case: &Case, position: V3) -> [f64; 2] {
    [
        0.5 + (position[0] - case.center[0]) / (2. * case.half_size),
        0.5 - (position[1] - case.center[1]) / (2. * case.half_size),
    ]
}

/// Magnified normalized 2x2 sampling: texel centers at .25/.75, repeat on both
/// axes. Decode each source texel before interpolation, as a native sRGB View
/// must do. This deliberately does not approximate decode-after-filter.
fn sample_pattern(pattern: [[u8; 4]; 4], uv: [f64; 2], decode_first: bool) -> V3 {
    let coordinate = uv.map(|v| v * 2. - 0.5);
    let origin = coordinate.map(|v| v.floor() as i32);
    let fraction: [f64; 2] = std::array::from_fn(|i| coordinate[i] - coordinate[i].floor());
    std::array::from_fn(|channel| {
        let mut sum = 0.;
        for y in 0..2 {
            for x in 0..2 {
                let index = (origin[1] + y).rem_euclid(2) * 2 + (origin[0] + x).rem_euclid(2);
                let encoded = f64::from(pattern[index as usize][channel]) / 255.;
                let value = if decode_first {
                    srgb_linear(encoded)
                } else {
                    encoded
                };
                sum += value
                    * if x == 0 {
                        1. - fraction[0]
                    } else {
                        fraction[0]
                    }
                    * if y == 0 {
                        1. - fraction[1]
                    } else {
                        fraction[1]
                    };
            }
        }
        if decode_first { sum } else { srgb_linear(sum) }
    })
}

fn scene(cases: &[Case], force_opaque: bool) -> Scene {
    let mut images = Vec::new();
    let defaults = fallback_images(&mut images);
    let mut materials: Vec<Material> = Vec::new();
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut primitives = Vec::new();
    for (id, case) in cases.iter().enumerate() {
        let base_image = base_image(&mut images, case);
        let normal_image = image(
            &mut images,
            case.normal_map.unwrap_or([128, 128, 255, 255]),
            false,
        );
        let emissive_image = image(&mut images, case.emissive_map, true);
        let record = serde_json::json!({"id":id,"base_factor":case.base,"emissive":case.emissive,
            "normal_scale":case.normal_scale,"metallic":case.metallic,"roughness":case.roughness,
        "occlusion_strength":0.,"alpha_cutoff":0.5,"alpha_mode":(["OPAQUE","MASK","BLEND"][case.alpha_mode as usize]),
            "double_sided":case.double_sided,"base_color_texture":0,"normal_texture":case.normal_map.map(|_|1),
            "orm_texture":null,"occlusion_texture":null,"emissive_texture":2});
        materials.push(
            material(
                &record,
                id,
                &[base_image, normal_image, emissive_image],
                &images,
                defaults,
                force_opaque,
            )
            .unwrap(),
        );
        let start = vertices.len();
        let index_start = indices.len();
        for (x, y, uv) in [
            (-1., -1., [0., 1.]),
            (1., -1., [1., 1.]),
            (1., 1., [1., 0.]),
            (-1., 1., [0., 0.]),
        ] {
            let words = [
                case.center[0] + x * case.half_size,
                case.center[1] + y * case.half_size,
                0.,
                case.normal[0],
                case.normal[1],
                case.normal[2],
                case.tangent[0],
                case.tangent[1],
                case.tangent[2],
                case.sign,
                uv[0],
                uv[1],
            ];
            for value in words {
                vertices.extend_from_slice(&(value as f32).to_le_bytes());
            }
        }
        for index in if case.back {
            [0_u32, 2, 1, 0, 3, 2]
        } else {
            [0, 1, 2, 0, 2, 3]
        } {
            indices.extend_from_slice(&index.to_le_bytes());
        }
        for vertex in vertices[start..].as_chunks::<48>().0 {
            super::asset::validate_vertex(vertex).unwrap();
        }
        primitives.push(Primitive {
            surface: None,
            lightmap: None,
            material: id,
            vertices: start..vertices.len(),
            indices: index_start..indices.len(),
            index_count: 6,
            center: [case.center[0] as f32, case.center[1] as f32, 0.],
        });
    }
    Scene {
        lightmap_uv: Vec::new(),
        vertices,
        indices,
        primitives,
        images,
        materials,
        mesh_count: cases.len(),
        source: serde_json::json!({"fixture":"independent-material-probes-v2"}),
    }
}

/// Analytic outgoing linear RGB before blending/storage. Unlike the shader's
/// optimized expression, Smith visibility is reconstructed from slope terms.
fn surface(case: &Case, position: V3) -> V3 {
    let sampled = case.base_pattern.map_or_else(
        || std::array::from_fn(|i| srgb_decode(case.base_map[i])),
        |pattern| sample_pattern(pattern, uv(case, position), true),
    );
    surface_with_base(case, position, sampled)
}

fn surface_with_base(case: &Case, position: V3, sampled_base: V3) -> V3 {
    let geometric = unit(case.normal);
    let t = unit(case.tangent);
    let b = cross(geometric, t).map(|x| x * case.sign);
    let mapped = case.normal_map.map_or([0., 0., 1.], |bytes| {
        [
            (f64::from(bytes[0]) / 127.5 - 1.) * case.normal_scale,
            (f64::from(bytes[1]) / 127.5 - 1.) * case.normal_scale,
            f64::from(bytes[2]) / 127.5 - 1.,
        ]
    });
    let mut n = unit(std::array::from_fn(|i| {
        t[i] * mapped[0] + b[i] * mapped[1] + geometric[i] * mapped[2]
    }));
    if case.back {
        n = n.map(|x| -x);
    }
    let v = unit([-position[0], -position[1], 4. - position[2]]);
    let l = unit(LIGHT.map(f64::from));
    let h = unit(std::array::from_fn(|i| v[i] + l[i]));
    let nl = dot(n, l).max(0.);
    let nv = dot(n, v).max(0.);
    let nh = dot(n, h).max(0.);
    let vh = dot(v, h).max(0.);
    let alpha = (case.roughness * case.roughness).max(1e-3);
    let a2 = alpha * alpha;
    let d = a2 / (std::f64::consts::PI * (1. + nh * nh * (a2 - 1.)).powi(2));
    let visibility = if nl > 0. && nv > 0. {
        let g = 2.
            / ((1. + a2 * (1. - nl * nl) / (nl * nl)).sqrt()
                + (1. + a2 * (1. - nv * nv) / (nv * nv)).sqrt());
        g / (4. * nl * nv)
    } else {
        0.
    };
    std::array::from_fn(|i| {
        let base = case.base[i] * sampled_base[i];
        let f0 = 0.04 * (1. - case.metallic) + base * case.metallic;
        let fresnel = f0 + (1. - f0) * (1. - vh).clamp(0., 1.).powi(5);
        let diffuse_color = base * (1. - case.metallic);
        let diffuse = (1. - fresnel) * diffuse_color / std::f64::consts::PI;
        ((diffuse + fresnel * d * visibility) * f64::from(LIGHT_COLOR[i]) * nl
            + case.emissive[i] * srgb_decode(case.emissive_map[i]))
        .clamp(0., 65504.)
    })
}

fn half(bits: u16) -> f64 {
    let sign = if bits & 0x8000 == 0 { 1. } else { -1. };
    let exponent = (bits >> 10) & 31;
    let fraction = f64::from(bits & 1023) / 1024.;
    match exponent {
        0 => sign * fraction * 2_f64.powi(-14),
        31 => {
            if fraction == 0. {
                sign * f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => sign * (1. + fraction) * 2_f64.powi(i32::from(exponent) - 15),
    }
}
fn stored_half(value: f64) -> f64 {
    assert!((0.0..=65504.0).contains(&value));
    if value == 0. {
        return 0.;
    }
    let step = if value < 2_f64.powi(-14) {
        2_f64.powi(-24)
    } else {
        2_f64.powi(value.log2().floor() as i32 - 10)
    };
    (value / step).round_ties_even() * step
}

struct Expected {
    hdr: [f64; 4],
    sdr: [u8; 4],
    depth: f64,
}

/// f64 reference for the fixed display transform, distinct from the shader's
/// FP32 execution and binary16 intermediate. The compressed peak is solved
/// first and used to reconstruct RGB/chroma, rather than sampling GPU output.
fn neutral_rgb(hdr: V3, exposure: f64) -> V3 {
    let exposed = hdr.map(|x| x.max(0.) * exposure);
    let low = exposed.into_iter().fold(f64::INFINITY, f64::min);
    let black_lift = if low < 0.08 {
        low - 6.25 * low * low
    } else {
        0.04
    };
    let shifted = exposed.map(|x| x - black_lift);
    let peak = shifted.into_iter().fold(0., f64::max);
    if peak < 0.76 {
        return shifted;
    }
    let white = 1. - 0.0576 / (peak - 0.52);
    let chroma = 1. / (1. + 0.15 * (peak - white));
    shifted.map(|x| white * (1. + chroma * (x / peak - 1.)))
}

fn display_rgb(hdr: V3) -> [u8; 3] {
    neutral_rgb(hdr, 2.)
        .map(|linear| (srgb_encode(stored_half(linear.clamp(0., 1.))) * 255.).round() as u8)
}
fn expected(case: &Case, force_opaque: bool) -> Expected {
    let (_, position, geometry_depth) = probe(case);
    let alpha_mode = if force_opaque { 0 } else { case.alpha_mode };
    let alpha = if force_opaque {
        1.
    } else {
        case.base[3] * f64::from(case.base_map[3]) / 255.
    };
    let absent = case.back && !case.double_sided
        || alpha_mode == 1 && alpha < 0.5
        || alpha_mode == 2 && alpha == 0.;
    let background = BACKGROUND.map(|v| stored_half(f64::from(v)));
    let rgb = if absent {
        background
    } else {
        let color = surface(case, position);
        if alpha_mode == 2 {
            std::array::from_fn(|i| stored_half(color[i] * alpha + background[i] * (1. - alpha)))
        } else {
            color.map(stored_half)
        }
    };
    let display = display_rgb(rgb);
    Expected {
        hdr: [rgb[0], rgb[1], rgb[2], 1.],
        sdr: [display[0], display[1], display[2], 255],
        depth: if absent || alpha_mode == 2 {
            1.
        } else {
            geometry_depth
        },
    }
}

fn close(actual: f64, expected: f64) -> bool {
    actual.is_finite() && (actual - expected).abs() <= ABSOLUTE + RELATIVE * expected.abs()
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn capture_scene(api: &str, scene: &Scene, width: u32, height: u32) -> [Vec<u8>; 3] {
    capture_lighting(api, scene, width, height, &Lighting::direct()).main
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
struct Lighting {
    environment: super::ibl::Environment,
    light_color: [f32; 3],
    aa: bool,
    shadows: bool,
    capture_shadows: bool,
}
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
impl Lighting {
    fn direct() -> Self {
        let mut environment = super::ibl::Environment::fixture();
        environment.nits_per_unit = 0.;
        Self {
            environment,
            light_color: LIGHT_COLOR,
            aa: false,
            shadows: false,
            capture_shadows: false,
        }
    }
}
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
struct Capture {
    main: [Vec<u8>; 3],
    shadows: Option<[Vec<u8>; 4]>,
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn capture_lighting(
    api: &str,
    scene: &Scene,
    width: u32,
    height: u32,
    lighting: &Lighting,
) -> Capture {
    let mut options = super::Options::parse(
        [
            "--fixture",
            "--frames",
            "1",
            "--warmup",
            "0",
            "--inflight",
            "1",
        ]
        .map(str::to_owned),
    )
    .unwrap();
    options.api = api.into();
    options.width = width;
    options.height = height;
    options.aa = lighting.aa;
    options.shadows = lighting.shadows;
    options.capture_shadows = lighting.capture_shadows;
    let camera = Camera::fixture();
    let uniform = camera
        .frame_input(
            width as f32 / height as f32,
            NEAR,
            FAR,
            LIGHT,
            lighting.light_color,
        )
        .unwrap();
    let (mut backend, resident, built) =
        build_material_test_frame(scene, &lighting.environment, &options, &uniform, BACKGROUND)
            .unwrap();
    assert_eq!(built.draws, Some(scene.primitives.len()));
    let plan = built.graph.compile().unwrap();
    let pending = backend.submit(&plan).unwrap();
    let mut completed = backend::wait_for(&mut backend, pending.id()).unwrap();
    let captures = built.captures.unwrap();
    let mut hdr = vec![0; width as usize * height as usize * 8];
    let mut sdr = vec![0; width as usize * height as usize * 4];
    let mut depth = vec![0; width as usize * height as usize * 4];
    backend::readback(&mut completed, captures.hdr, &mut hdr).unwrap();
    backend::readback(&mut completed, captures.sdr, &mut sdr).unwrap();
    backend::readback(&mut completed, captures.depth.unwrap(), &mut depth).unwrap();
    let shadows = built.shadow_captures.map(|passes| {
        let resolution = resident.shadow_parameters.resolution as usize;
        passes.map(|pass| {
            let mut bytes = vec![0; resolution * resolution * 4];
            backend::readback(&mut completed, pass, &mut bytes).unwrap();
            for word in bytes.as_chunks::<4>().0 {
                let depth = f32::from_le_bytes(*word);
                assert!(depth.is_finite() && (0.0..=1.0).contains(&depth));
            }
            bytes
        })
    });
    drop((completed, pending, plan, resident));
    let validation = backend::close(backend).unwrap();
    assert_eq!(validation["clean"], true);
    Capture {
        main: [hdr, sdr, depth],
        shadows,
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(super) fn native_acceptance(api: &str) {
    let cases = cases();
    for force_opaque in [false, true] {
        let scene = scene(&cases, force_opaque);
        let [hdr, sdr, depth] = capture_scene(api, &scene, WIDTH, HEIGHT);
        for bits in hdr.as_chunks::<2>().0 {
            let value = half(u16::from_le_bytes(*bits));
            assert!(
                value.is_finite() && (0.0..=65504.0).contains(&value),
                "non-finite or unrepresentable linear target"
            );
        }
        for case in &cases {
            let (pixel, _, _) = probe(case);
            let index = (pixel[1] * WIDTH + pixel[0]) as usize;
            let expected = expected(case, force_opaque);
            let actual_rgb = std::array::from_fn(|channel| {
                let offset = index * 8 + channel * 2;
                half(u16::from_le_bytes(
                    hdr[offset..offset + 2].try_into().unwrap(),
                ))
            });
            for channel in 0..4 {
                let offset = index * 8 + channel * 2;
                let actual = half(u16::from_le_bytes(
                    hdr[offset..offset + 2].try_into().unwrap(),
                ));
                // Near the regularized ideal-mirror singularity, one FP32
                // normal-dot rounding step changes GGX's peak substantially.
                // That panel gates representability and resolve consistency;
                // ordinary panels retain the frozen analytic shade tolerance.
                let peak = case.name == "finite-half-peak" && channel < 3;
                if !peak {
                    assert!(
                        close(actual, expected.hdr[channel]),
                        "{} override={force_opaque} HDR[{channel}]: {actual} != {} at{pixel:?}",
                        case.name,
                        expected.hdr[channel]
                    );
                }
                let expected_sdr = if peak {
                    display_rgb(actual_rgb)[channel]
                } else {
                    expected.sdr[channel]
                };
                let actual = sdr[index * 4 + channel];
                if channel == 3 {
                    assert_eq!(actual, 255, "resolve alpha must be one");
                } else {
                    assert!(
                        actual.abs_diff(expected_sdr) <= 1,
                        "{} override={force_opaque} SDR[{channel}]: {actual} != {expected_sdr}",
                        case.name
                    );
                }
            }
            let actual = f32::from_le_bytes(depth[index * 4..index * 4 + 4].try_into().unwrap());
            assert!(
                (f64::from(actual) - expected.depth).abs() <= DEPTH_ABSOLUTE,
                "{} override={force_opaque} depth: {actual} != {}",
                case.name,
                expected.depth
            );
        }
    }
}

/// Format conversion is distinct from shader arithmetic: Vulkan permits either
/// adjacent finite half value (rounding mode unspecified), while Direct3D's
/// published float-format narrowing rules specify round-to-zero. In particular
/// .18f32 need not become the CPU round-to-nearest-even half value.
/// https://docs.vulkan.org/spec/latest/chapters/fundamentals.html#fundamentals-fpconvert
/// https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm#3.2.2
fn clear_neighbors(value: f32) -> [f64; 2] {
    let value = f64::from(value);
    let step = if value < 2_f64.powi(-14) {
        2_f64.powi(-24)
    } else {
        2_f64.powi(value.log2().floor() as i32 - 10)
    };
    [(value / step).floor() * step, (value / step).ceil() * step]
}

fn validate_clear_control(captures: &[Vec<u8>; 3], width: u32, height: u32) {
    let pixels = width as usize * height as usize;
    for (bytes, stride) in captures.iter().zip([8, 4, 4]) {
        assert_eq!(bytes.len(), pixels * stride);
    }
    for (pixel, encoded) in captures[0]
        .as_chunks::<8>()
        .0
        .iter()
        .zip(captures[1].as_chunks::<4>().0)
    {
        let actual_rgb = std::array::from_fn(|channel| {
            half(u16::from_le_bytes(
                pixel[channel * 2..channel * 2 + 2].try_into().unwrap(),
            ))
        });
        let expected_display = display_rgb(actual_rgb);
        for channel in 0..4 {
            let value = half(u16::from_le_bytes(
                pixel[channel * 2..channel * 2 + 2].try_into().unwrap(),
            ));
            if channel == 3 {
                assert_eq!(value, 1., "clear-only alpha must be one");
                assert_eq!(encoded[channel], 255);
            } else {
                assert!(
                    clear_neighbors(BACKGROUND[channel]).contains(&value),
                    "clear-only conversion escaped adjacent representable values"
                );
                let expected = expected_display[channel];
                assert!(
                    encoded[channel].abs_diff(expected) <= 1,
                    "clear-only resolve differs from stored HDR"
                );
            }
        }
    }
    for depth in captures[2].as_chunks::<4>().0 {
        assert_eq!(
            f32::from_le_bytes(*depth),
            1.,
            "clear-only depth must be one"
        );
    }
}

/// Compare with a separate zero-PBR-draw run using the same API, clear,
/// attachments, resolve and readback. This preserves exact no-write checks
/// without confusing a permitted clear conversion with alpha coverage.
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(super) fn alpha_background(api: &str, report: &serde_json::Value) {
    assert_eq!(
        report["draw_count"].as_u64(),
        Some(2),
        "both original alpha-zero materials must be submitted"
    );
    let width = u32::try_from(report["captures"]["hdr"]["width"].as_u64().unwrap()).unwrap();
    let height = u32::try_from(report["captures"]["hdr"]["height"].as_u64().unwrap()).unwrap();
    let names = ["hdr", "sdr", "depth"];
    let actual: [Vec<u8>; 3] = names.map(|name| {
        assert_eq!(
            report["captures"][name]["width"].as_u64(),
            Some(u64::from(width))
        );
        assert_eq!(
            report["captures"][name]["height"].as_u64(),
            Some(u64::from(height))
        );
        std::fs::read(report["captures"][name]["path"].as_str().unwrap()).unwrap()
    });
    let empty = Scene {
        lightmap_uv: Vec::new(),
        vertices: Vec::new(),
        indices: Vec::new(),
        primitives: Vec::new(),
        images: Vec::new(),
        materials: Vec::new(),
        mesh_count: 0,
        source: serde_json::json!({"fixture":"independent-clear-only-control"}),
    };
    assert!(empty.primitives.is_empty() && empty.materials.is_empty());
    let control = capture_scene(api, &empty, width, height);
    validate_clear_control(&control, width, height);
    for ((actual, control), name) in actual.iter().zip(control.iter()).zip(names) {
        assert_eq!(actual.len(), control.len());
        if let Some(offset) = actual.iter().zip(control).position(|(a, b)| a != b) {
            panic!(
                "alpha-zero changed {name} at byte {offset}: {} != independent clear-only {}",
                actual[offset], control[offset]
            );
        }
    }
}

#[test]
fn clear_control_accepts_only_adjacent_half_conversions() {
    assert_eq!(clear_neighbors(0.18), [0.179931640625, 0.1800537109375]);
    for bits in [0x31c2_u16, 0x31c3] {
        let value = half(bits);
        let mut pixel = Vec::new();
        for word in [bits, bits, bits, 0x3c00] {
            pixel.extend_from_slice(&word.to_le_bytes());
        }
        let encoded = display_rgb([value; 3])[0];
        let captures = [
            pixel,
            vec![encoded, encoded, encoded, 255],
            1_f32.to_le_bytes().to_vec(),
        ];
        validate_clear_control(&captures, 1, 1);
        let mut wrong = captures.clone();
        wrong[0][..2].copy_from_slice(&0x31c1_u16.to_le_bytes());
        assert!(std::panic::catch_unwind(|| validate_clear_control(&wrong, 1, 1)).is_err());
    }
}

#[allow(
    clippy::duplicate_mod,
    reason = "run the independent lighting reference against each route"
)]
#[path = "material_tests/lighting_tests.rs"]
mod lighting_tests;

#[test]
fn analytic_probes_reject_shared_material_mutations() {
    let cases = cases();
    let differs = |a: &Case, b: &Case| {
        let a = expected(a, false);
        let b = expected(b, false);
        (0..3).any(|i| !close(a.hdr[i], b.hdr[i]))
    };
    for (index, mutation) in [
        (1, "metallic"),
        (2, "roughness"),
        (3, "roughness"),
        (4, "normal-scale-zero"),
        (5, "normal"),
        (6, "handedness"),
        (7, "rotation"),
        (8, "emissive"),
        (14, "base-srgb"),
        (15, "emissive-map"),
    ] {
        let mut wrong = cases[index].clone();
        match mutation {
            "metallic" => wrong.metallic = 0.,
            "roughness" => wrong.roughness = 0.6,
            "normal" => wrong.normal_scale = 0.,
            "normal-scale-zero" => wrong.normal_scale = 1.,
            "handedness" => wrong.sign = 1.,
            "rotation" => wrong.tangent = [1., 0., 0.],
            "emissive" => wrong.emissive = [0.; 3],
            "base-srgb" => {
                wrong.base_map = [255; 4];
                wrong.base_pattern = None;
            }
            "emissive-map" => wrong.emissive_map = [255; 4],
            _ => unreachable!(),
        }
        assert!(
            differs(&cases[index], &wrong),
            "probe is insensitive to {mutation}"
        );
    }
    let mut wrong = cases[9].clone();
    wrong.double_sided = true;
    assert!(
        differs(&cases[9], &wrong),
        "single-sided backface rejection"
    );
    let mut wrong = cases[10].clone();
    wrong.back = false;
    assert!(differs(&cases[10], &wrong), "double-sided normal reversal");
    for case in &cases[11..13] {
        let opaque = expected(case, true);
        let expected = expected(case, false);
        assert_eq!(expected.depth, 1.);
        assert!(opaque.depth < 1.);
        assert!(
            (0..3).any(|i| !close(expected.hdr[i], opaque.hdr[i])),
            "opaque sensitivity for {}",
            case.name
        );
    }
    assert!(expected(&cases[16], false).hdr[..3].contains(&65504.));
    assert!(
        cases
            .iter()
            .any(|case| !close(expected(case, false).hdr[0], 0.2)),
        "constant output would fail"
    );
}

#[test]
fn varying_srgb_probe_rejects_constant_swapped_and_flipped_uvs() {
    let cases = cases();
    let case = &cases[14];
    let (_, position, _) = probe(case);
    let coordinates = uv(case, position);
    let pattern = case.base_pattern.unwrap();
    let expected = expected(case, false);
    for (name, sampled) in [
        ("constant-zero", sample_pattern(pattern, [0., 0.], true)),
        ("constant-center", sample_pattern(pattern, [0.5, 0.5], true)),
        (
            "swapped",
            sample_pattern(pattern, [coordinates[1], coordinates[0]], true),
        ),
        (
            "flipped-v",
            sample_pattern(pattern, [coordinates[0], 1. - coordinates[1]], true),
        ),
        (
            "decode-after-filter",
            sample_pattern(pattern, coordinates, false),
        ),
    ] {
        let wrong = surface_with_base(case, position, sampled).map(stored_half);
        assert!(
            (0..3).any(|channel| !close(wrong[channel], expected.hdr[channel])),
            "UV/color probe cannot reject {name}"
        );
    }
    let scene = scene(&cases, false);
    let image = &scene.images[scene.materials[14].textures[0]];
    assert_eq!((image.width, image.height), (2, 2));
    assert_eq!(image.mips, [0..16, 16..20]);
    assert_eq!(image.load_bytes().unwrap().len(), 20);
    // The panel spans dozens of pixels while its base image has only two
    // texels. The reference therefore exercises magnification/base mip, not
    // an unmodeled trilinear blend with the generated 1x1 level.
    let pixels_per_side =
        2. * case.half_size / (8. * (30_f64.to_radians()).tan()) * f64::from(WIDTH);
    assert!(pixels_per_side > 16.);
}

#[test]
fn fixture_publication_matches_declared_basis_and_material_offsets() {
    let cases = cases();
    let scene = scene(&cases, false);
    assert_eq!(scene.primitives.len(), 17);
    assert_eq!(
        scene
            .materials
            .iter()
            .filter(|material| material.alpha_zero)
            .count(),
        2
    );
    for (index, (case, material)) in cases.iter().zip(scene.materials.iter()).enumerate() {
        assert_eq!(
            u32::from_le_bytes(material.uniform[48..52].try_into().unwrap()),
            case.alpha_mode
        );
        assert_eq!(
            u32::from_le_bytes(material.uniform[52..56].try_into().unwrap()),
            u32::from(case.double_sided)
        );
        for vertex in scene.vertices[scene.primitives[index].vertices.clone()]
            .as_chunks::<48>()
            .0
        {
            super::asset::validate_vertex(vertex).unwrap();
        }
    }
    assert_eq!(half(0x3c00), 1.);
    assert_eq!(half(0x7bff), 65504.);
    assert!(half(0x7c00).is_infinite());
    assert_eq!(stored_half(0.18), 0.1800537109375);
}
