//! Independent numeric probes for the caller's actual PBR/resolve connection.
//! These test-owned inputs are separate from the convenient CLI fixture.
//!
//! The reference uses f64 and Smith's slope/lambda formulation instead of the
//! shader's factored visibility expression. It models the glTF channel meanings,
//! OPAQUE/MASK/BLEND coverage and sRGB transfer explicitly. This is directional
//! light plus B2's constant ambient, not an IBL/shadow/tone-mapping reference.

use super::{Material, Pixels, Primitive, Scene, TextureImage, fallback_images, material};
use crate::bistro::{backend::Backend, camera::Camera, pipeline};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 256;
const NEAR: f32 = 0.02;
const FAR: f32 = 500.0;
const LIGHT: [f32; 3] = [0.2, 0.6, 0.7745967];
const LIGHT_COLOR: [f32; 3] = [1.0, 0.9, 0.8];
const BACKGROUND: [f32; 3] = [0.18; 3];
const ABSOLUTE: f64 = 1.0 / 1024.0;
const RELATIVE: f64 = 1.0 / 1024.0;

type V3 = [f64; 3];

#[derive(Clone)]
struct Case {
    name: &'static str,
    center: [f64; 2],
    half_size: f64,
    normal: V3,
    tangent: V3,
    sign: f64,
    normal_map: Option<[u8; 4]>,
    normal_scale: f64,
    base: [f64; 4],
    base_map: [u8; 4],
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
    let value = f64::from(byte) / 255.;
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
            half_size: 0.34,
            normal: [0., 0., 1.],
            tangent: [1., 0., 0.],
            sign: 1.,
            normal_map: None,
            normal_scale: 1.,
            base: [0.8, 0.4, 0.2, 1.],
            base_map: [255; 4],
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
    let p = [case.center[0], case.center[1], 0., 1.];
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

fn scene(cases: &[Case], force_opaque: bool) -> Scene {
    let mut images = Vec::new();
    let defaults = fallback_images(&mut images);
    let mut materials: Vec<Material> = Vec::new();
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut primitives = Vec::new();
    for (id, case) in cases.iter().enumerate() {
        let base_image = image(&mut images, case.base_map, true);
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
            super::validate_vertex(vertex).unwrap();
        }
        primitives.push(Primitive {
            material: id,
            vertices: start..vertices.len(),
            indices: index_start..indices.len(),
            index_count: 6,
            center: [case.center[0] as f32, case.center[1] as f32, 0.],
        });
    }
    Scene {
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
        let base = case.base[i] * srgb_decode(case.base_map[i]);
        let f0 = 0.04 * (1. - case.metallic) + base * case.metallic;
        let fresnel = f0 + (1. - f0) * (1. - vh).clamp(0., 1.).powi(5);
        let diffuse_color = base * (1. - case.metallic);
        let diffuse = (1. - fresnel) * diffuse_color / std::f64::consts::PI;
        ((diffuse + fresnel * d * visibility) * f64::from(LIGHT_COLOR[i]) * nl
            + 0.03 * diffuse_color
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
    Expected {
        hdr: [rgb[0], rgb[1], rgb[2], 1.],
        sdr: [
            (srgb_encode(rgb[0]) * 255.).round() as u8,
            (srgb_encode(rgb[1]) * 255.).round() as u8,
            (srgb_encode(rgb[2]) * 255.).round() as u8,
            255,
        ],
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

pub(super) fn native_acceptance<B: Backend>() {
    let cases = cases();
    for force_opaque in [false, true] {
        let options = crate::bistro::Options::parse(
            [
                "--fixture",
                "--width",
                "256",
                "--height",
                "256",
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
        let scene = scene(&cases, force_opaque);
        let budget = pipeline::Budget::new(&scene, &options).unwrap();
        let camera = Camera::fixture();
        let uniform = camera.uniform(1., NEAR, FAR, LIGHT, LIGHT_COLOR).unwrap();
        let mut backend = B::create(&options, &budget).unwrap();
        let resident = pipeline::Resident::upload(&mut backend, &scene, &budget, &uniform).unwrap();
        let mut pool = renderer::graph::GraphPool::new(budget.graph, 2).unwrap();
        let mut workspace = pipeline::Workspace::new(&scene);
        let built = pipeline::build(
            &mut pool,
            &mut workspace,
            &resident,
            &scene,
            &options,
            camera,
            &uniform,
            BACKGROUND,
            true,
        )
        .unwrap();
        let plan = built.graph.compile().unwrap();
        let mut pending = backend.submit(&plan).unwrap();
        let mut completed = B::wait(&mut pending, crate::bistro::WAIT).unwrap();
        let captures = built.captures.unwrap();
        let mut hdr = vec![0; WIDTH as usize * HEIGHT as usize * 8];
        let mut sdr = vec![0; WIDTH as usize * HEIGHT as usize * 4];
        let mut depth = vec![0; WIDTH as usize * HEIGHT as usize * 4];
        B::readback(&mut completed, captures[0], &mut hdr).unwrap();
        B::readback(&mut completed, captures[1], &mut sdr).unwrap();
        B::readback(&mut completed, captures[2], &mut depth).unwrap();
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
                    (srgb_encode(actual) * 255.).round() as u8
                } else {
                    expected.sdr[channel]
                };
                let actual = f64::from(sdr[index * 4 + channel]) / 255.;
                let expected = f64::from(expected_sdr) / 255.;
                assert!(
                    close(actual, expected),
                    "{} override={force_opaque} SDR[{channel}]: {actual} != {expected}",
                    case.name
                );
            }
            let actual = f32::from_le_bytes(depth[index * 4..index * 4 + 4].try_into().unwrap());
            assert!(
                (f64::from(actual) - expected.depth).abs() < 1e-6,
                "{} override={force_opaque} depth: {actual} != {}",
                case.name,
                expected.depth
            );
        }
        drop((completed, pending, plan, resident, workspace));
        let validation = backend.close().unwrap();
        assert_eq!(validation["clean"], true);
        drop(pool);
    }
}

/// The CLI alpha-only run contains no other geometry. Check the complete target,
/// so zero color/depth coverage is not inferred from a single panel or digest.
pub(super) fn alpha_background(report: &serde_json::Value) {
    let read =
        |name: &str| std::fs::read(report["captures"][name]["path"].as_str().unwrap()).unwrap();
    let hdr = read("hdr");
    let sdr = read("sdr");
    let depth = read("depth");
    let linear = BACKGROUND.map(|value| stored_half(f64::from(value)));
    let encoded = linear.map(|value| (srgb_encode(value) * 255.).round() as u8);
    for pixel in hdr.as_chunks::<8>().0 {
        for channel in 0..4 {
            let actual = half(u16::from_le_bytes(
                pixel[channel * 2..channel * 2 + 2].try_into().unwrap(),
            ));
            assert!(
                close(actual, if channel == 3 { 1. } else { linear[channel] }),
                "alpha zero changed linear background"
            );
        }
    }
    for pixel in sdr.as_chunks::<4>().0 {
        assert_eq!(
            *pixel,
            [encoded[0], encoded[1], encoded[2], 255],
            "alpha zero changed SDR background"
        );
    }
    for value in depth.as_chunks::<4>().0 {
        assert_eq!(f32::from_le_bytes(*value), 1., "alpha zero wrote depth");
    }
}

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
            "base-srgb" => wrong.base_map = [255; 4],
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
            super::validate_vertex(vertex).unwrap();
        }
    }
    assert_eq!(half(0x3c00), 1.);
    assert_eq!(half(0x7bff), 65504.);
    assert!(half(0x7c00).is_infinite());
    assert_eq!(stored_half(0.18), 0.1800537109375);
}
