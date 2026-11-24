//! Independent behavioral probes for the consumer's actual B3 graph.
//! Numeric comparisons use analytic environments, not the scene's baked IBL.

use super::*;
use crate::shared::ibl;

fn panel(name: &'static str) -> Case {
    let mut case = cases()[0].clone();
    case.name = name;
    case.center = [0.; 2];
    case.half_size = 1.2;
    case.base = [0., 0., 0., 1.];
    case.normal_scale = 0.;
    case.emissive = [0.; 3];
    case
}

fn empty_scene() -> Scene {
    Scene {
        lightmap_uv: Vec::new(),
        vertices: Vec::new(),
        indices: Vec::new(),
        primitives: Vec::new(),
        images: Vec::new(),
        materials: Vec::new(),
        mesh_count: 0,
        source: serde_json::json!({"fixture":"B3 independent clear"}),
    }
}

fn rgb_at(bytes: &[u8], pixel: [u32; 2]) -> V3 {
    let index = (pixel[1] * WIDTH + pixel[0]) as usize;
    std::array::from_fn(|channel| {
        half(u16::from_le_bytes(
            bytes[index * 8 + channel * 2..index * 8 + channel * 2 + 2]
                .try_into()
                .unwrap(),
        ))
    })
}

fn assert_rgb(actual: V3, expected: V3, label: &str) {
    for channel in 0..3 {
        assert!(
            close(actual[channel], expected[channel]),
            "{label} channel {channel}: {} != {}",
            actual[channel],
            expected[channel]
        );
    }
}

fn half_word(value: f64) -> u16 {
    let value = stored_half(value);
    if value == 0. {
        return 0;
    }
    if value < 2_f64.powi(-14) {
        return (value * 2_f64.powi(24)) as u16;
    }
    let exponent = value.log2().floor() as i32;
    let mantissa = ((value / 2_f64.powi(exponent) - 1.) * 1024.).round() as u16;
    ((exponent + 15) as u16) << 10 | mantissa
}

fn image_levels(width: u32, height: u32, levels: &[Vec<[f64; 3]>]) -> ibl::Image {
    let mut bytes = Vec::new();
    let mut mips = Vec::new();
    for (level, pixels) in levels.iter().enumerate() {
        let expected = (width >> level).max(1) as usize * (height >> level).max(1) as usize;
        assert_eq!(pixels.len(), expected);
        let start = bytes.len();
        for rgb in pixels {
            for value in rgb.iter().copied().chain([1.]) {
                bytes.extend_from_slice(&half_word(value).to_le_bytes());
            }
        }
        mips.push(start..bytes.len());
    }
    ibl::Image {
        format: renderer_core::ImageFormat::Rgba16Float,
        width,
        height,
        mips,
        bytes,
    }
}

fn constant_levels(width: u32, height: u32, colors: &[V3]) -> ibl::Image {
    let levels = colors
        .iter()
        .enumerate()
        .map(|(level, &color)| {
            vec![color; (width >> level).max(1) as usize * (height >> level).max(1) as usize]
        })
        .collect::<Vec<_>>();
    image_levels(width, height, &levels)
}

fn environment(diffuse: ibl::Image, specular: ibl::Image) -> ibl::Environment {
    let max_prefilter_lod = (specular.mips.len() - 1) as u32;
    ibl::Environment {
        lightmaps: Vec::new(),
        reflection_probes: None,
        reflection_metadata: None,
        diffuse,
        specular,
        brdf: constant_levels(2, 2, &[[0.5, 0.125, 0.]]),
        nits_per_unit: 1.,
        yaw_degrees: 0.,
        max_prefilter_lod,
        identity: serde_json::json!({"fixture":"analytic IBL; constant BRDF A=.5/B=.125"}),
    }
}

#[test]
fn display_reference_preserves_black_and_compresses_hdr_highlights() {
    assert_eq!(display_rgb([0.; 3]), [0; 3]);
    assert!(
        neutral_rgb([0.18; 3], 2.)
            .iter()
            .all(|x| (x - 0.32).abs() < 1e-15)
    );
    assert_eq!(display_rgb([0.18; 3]), [153; 3]);
    let bright = neutral_rgb([4., 1., 0.25], 2.);
    assert!(bright.iter().all(|x| x.is_finite() && *x >= 0. && *x <= 1.));
    assert!(bright[0] > bright[1] && bright[1] > bright[2]);
    assert!(bright[0] < 1. && bright[0] > 0.98);
}

#[test]
fn display_probes_reject_exposure_srgb_and_early_clamp_errors() {
    for color in [[0.18, 0.1, 0.05], [4., 1., 0.25], [0.005, 0.001, 0.0001]] {
        let expected = display_rgb(color);
        let encode = |rgb: V3| rgb.map(|v| (srgb_encode(v) * 255.).round() as u8);
        for (name, wrong) in [
            ("exposure omitted", encode(neutral_rgb(color, 1.))),
            ("exposure twice", encode(neutral_rgb(color, 4.))),
            (
                "sRGB twice",
                encode(neutral_rgb(color, 2.).map(srgb_encode)),
            ),
        ] {
            assert!(
                (0..3).any(|i| expected[i].abs_diff(wrong[i]) > 1),
                "probe cannot reject {name}: {color:?}"
            );
        }
    }
    let hdr = [4., 1., 0.25];
    let wrong = display_rgb(hdr.map(|v: f64| v.min(1.)));
    assert!((0..3).any(|i| display_rgb(hdr)[i].abs_diff(wrong[i]) > 1));
}

#[test]
fn analytic_fixture_words_round_trip_and_keep_mips_distinct() {
    for value in [0., 0.00001, 0.125, 0.25, 0.5, 0.75, 1., 4., 65504.] {
        assert_eq!(half(half_word(value)), stored_half(value));
    }
    let image = constant_levels(8, 4, &[[1.; 3], [0.5; 3], [0.25; 3], [0.125; 3]]);
    assert_eq!(image.mips, [0..256, 256..320, 320..336, 336..344]);
    for (level, range) in image.mips.iter().enumerate() {
        assert_eq!(
            half(u16::from_le_bytes(
                image.bytes[range.start..range.start + 2]
                    .try_into()
                    .unwrap()
            )),
            2_f64.powi(-(level as i32))
        );
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn hdr_and_display(api: &str) {
    let mut case = panel("HDR emissive");
    case.emissive = [4., 1., 0.25];
    let scene = scene(std::slice::from_ref(&case), false);
    let mut lighting = Lighting::direct();
    lighting.light_color = [0.; 3];
    let captured = capture_lighting(api, &scene, WIDTH, HEIGHT, &lighting);
    let pixel = probe(&case).0;
    let hdr = rgb_at(&captured.main[0], pixel);
    assert_rgb(
        hdr,
        case.emissive.map(stored_half),
        "HDR must retain values above one",
    );
    assert!(hdr[0] > 1.);
    let at = (pixel[1] * WIDTH + pixel[0]) as usize * 4;
    for (actual, expected) in captured.main[1][at..at + 3].iter().zip(display_rgb(hdr)) {
        assert!(
            actual.abs_diff(expected) <= 1,
            "Neutral/exposure/sRGB: {actual} != {expected}"
        );
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn ibl_materials(api: &str) {
    let colors = [
        [1., 0.5, 0.25],
        [0.5, 1., 0.25],
        [0.25, 0.5, 1.],
        [0.125, 0.25, 0.5],
    ];
    let mut lighting = Lighting::direct();
    lighting.light_color = [0.; 3];
    lighting.environment = environment(
        constant_levels(8, 4, &[[1.; 3]]),
        constant_levels(8, 4, &colors),
    );
    let mut probes = cases()[..8].to_vec();
    for (i, case) in probes.iter_mut().enumerate() {
        case.name = "analytic roughness/metallic IBL";
        case.base = [0.8, 0.4, 0.2, 1.];
        case.base_map = [255; 4];
        case.base_pattern = None;
        case.normal = [0., 0., 1.];
        case.tangent = [1., 0., 0.];
        case.normal_map = None;
        case.normal_scale = 0.;
        case.metallic = f64::from(i >= 4);
        case.roughness = (i % 4) as f64 / 3.;
        case.emissive = [0.; 3];
    }
    let mut model = scene(&probes, false);
    let captured = capture_lighting(api, &model, WIDTH, HEIGHT, &lighting);
    for (i, case) in probes.iter().enumerate() {
        let expected = std::array::from_fn(|channel| {
            let f0 = 0.04 * (1. - case.metallic) + case.base[channel] * case.metallic;
            case.base[channel] * (1. - case.metallic) / std::f64::consts::PI
                + colors[i % 4][channel] * (0.5 * f0 + 0.125)
        });
        assert_rgb(
            rgb_at(&captured.main[0], probe(case).0),
            expected.map(stored_half),
            "IBL roughness/F0",
        );
    }
    // An authored AO value of zero removes diffuse environment only. Direct,
    // emissive and specular are not multiplied by this term.
    let ao_image = image(&mut model.images, [0, 255, 255, 255], false);
    lighting.light_color = LIGHT_COLOR;
    for (case, material) in probes.iter_mut().zip(&mut model.materials) {
        case.emissive = [0.02, 0.03, 0.04];
        for (channel, value) in case.emissive.into_iter().enumerate() {
            material.uniform[16 + channel * 4..20 + channel * 4]
                .copy_from_slice(&(value as f32).to_le_bytes());
        }
        material.textures[2] = ao_image;
        material.uniform[40..44].copy_from_slice(&1_f32.to_le_bytes());
    }
    let occluded = capture_lighting(api, &model, WIDTH, HEIGHT, &lighting);
    for (i, case) in probes.iter().enumerate() {
        let expected = std::array::from_fn(|channel| {
            let f0 = 0.04 * (1. - case.metallic) + case.base[channel] * case.metallic;
            surface(case, probe(case).1)[channel] + colors[i % 4][channel] * (0.5 * f0 + 0.125)
        });
        assert_rgb(
            rgb_at(&occluded.main[0], probe(case).0),
            expected.map(stored_half),
            "AO preserves direct, emissive and specular",
        );
    }
}

fn direction_environment() -> (ibl::Environment, Vec<V3>) {
    let pixels = (0..4)
        .flat_map(|y| {
            (0..8).map(move |x| match y {
                0 => [1., 0.125, 0.25],
                3 => [0.125, 0.25, 1.],
                _ => [
                    (x + 1) as f64 / 8.,
                    (y + 1) as f64 / 4.,
                    (8 - x) as f64 / 8.,
                ],
            })
        })
        .collect::<Vec<_>>();
    (
        environment(
            image_levels(8, 4, std::slice::from_ref(&pixels)),
            constant_levels(8, 4, &[[0.; 3]; 4]),
        ),
        pixels,
    )
}

fn bilinear_environment(pixels: &[V3], uv: [f64; 2]) -> V3 {
    let p = [uv[0] * 8. - 0.5, uv[1] * 4. - 0.5];
    let origin = p.map(f64::floor);
    let fraction = [p[0] - origin[0], p[1] - origin[1]];
    std::array::from_fn(|channel| {
        let mut sum = 0.;
        for y in 0..2 {
            for x in 0..2 {
                let column = (origin[0] as i32 + x).rem_euclid(8) as usize;
                let row = (origin[1] as i32 + y).clamp(0, 3) as usize;
                let weight = (if x == 0 {
                    1. - fraction[0]
                } else {
                    fraction[0]
                }) * (if y == 0 {
                    1. - fraction[1]
                } else {
                    fraction[1]
                });
                sum += pixels[row * 8 + column][channel] * weight;
            }
        }
        sum
    })
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn ibl_directions(api: &str) {
    let (environment, pixels) = direction_environment();
    let mut lighting = Lighting::direct();
    lighting.light_color = [0.; 3];
    lighting.environment = environment;
    let normals = [
        [1., 0., 0.],
        [0., 0., 1.],
        [0., 0., -1.],
        [-1., 0., 0.],
        [0., 1., 0.],
        [0., -1., 0.],
    ];
    let uv = [
        [0.5, 0.5],
        [0.75, 0.5],
        [0.25, 0.5],
        [0., 0.5],
        [0.5, 0.],
        [0.5, 1.],
    ];
    let mut probes = cases()[..6].to_vec();
    for (case, normal) in probes.iter_mut().zip(normals) {
        case.name = "IBL axis/seam/pole";
        case.base = [1.; 4];
        case.metallic = 0.;
        case.roughness = 0.5;
        case.base_map = [255; 4];
        case.base_pattern = None;
        case.normal = normal;
        case.tangent = if normal[0] != 0. {
            [0., 1., 0.]
        } else {
            [1., 0., 0.]
        };
        case.normal_map = None;
        case.normal_scale = 0.;
        case.emissive = [0.; 3];
    }
    let model = scene(&probes, false);
    for yaw in [0., 90.] {
        lighting.environment.yaw_degrees = yaw;
        let captured = capture_lighting(api, &model, WIDTH, HEIGHT, &lighting);
        for (case, mut coordinate) in probes.iter().zip(uv) {
            coordinate[0] = (coordinate[0] + f64::from(yaw) / 360.).fract();
            let expected =
                bilinear_environment(&pixels, coordinate).map(|x| x / std::f64::consts::PI);
            assert_rgb(
                rgb_at(&captured.main[0], probe(case).0),
                expected.map(stored_half),
                "IBL direction/yaw",
            );
        }
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn fxaa_behavior(api: &str) {
    let mut lighting = Lighting::direct();
    lighting.light_color = [0.; 3];
    let clear = empty_scene();
    let no_aa = capture_lighting(api, &clear, WIDTH, HEIGHT, &lighting);
    lighting.aa = true;
    let aa = capture_lighting(api, &clear, WIDTH, HEIGHT, &lighting);
    assert_eq!(
        no_aa.main, aa.main,
        "FXAA must preserve a uniform image including viewport edges"
    );
    let mut case = panel("rotated thin emissive rectangle");
    case.emissive = [4.; 3];
    let mut model = scene(std::slice::from_ref(&case), false);
    let angle = 27_f32.to_radians();
    for vertex in model.vertices.as_chunks_mut::<48>().0 {
        let x = f32::from_le_bytes(vertex[0..4].try_into().unwrap());
        let y = f32::from_le_bytes(vertex[4..8].try_into().unwrap()) * 0.025;
        vertex[0..4].copy_from_slice(&(x * angle.cos() - y * angle.sin()).to_le_bytes());
        vertex[4..8].copy_from_slice(&(x * angle.sin() + y * angle.cos()).to_le_bytes());
    }
    lighting.aa = false;
    let no_aa = capture_lighting(api, &model, WIDTH, HEIGHT, &lighting);
    lighting.aa = true;
    let aa = capture_lighting(api, &model, WIDTH, HEIGHT, &lighting);
    assert_eq!(no_aa.main[0], aa.main[0], "AA is after scene HDR");
    assert_eq!(
        no_aa.main[2], aa.main[2],
        "AA must not alter geometry depth"
    );
    let changed = no_aa.main[1]
        .as_chunks::<4>()
        .0
        .iter()
        .zip(aa.main[1].as_chunks::<4>().0)
        .filter(|(a, b)| (0..3).any(|i| a[i].abs_diff(b[i]) > 1))
        .count();
    assert!(
        changed > 10,
        "FXAA must respond to diagonal/thin edges, changed {changed}"
    );
    assert!(
        aa.main[1]
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    );
    let background = display_rgb(BACKGROUND.map(f64::from));
    assert!(
        aa.main[1]
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| (0..3).any(|i| pixel[i].abs_diff(background[i]) > 8)),
        "AA must retain the thin bright geometry"
    );
    for y in [0, 1, HEIGHT - 2, HEIGHT - 1] {
        for x in 0..WIDTH {
            let index = (y * WIDTH + x) as usize * 4;
            assert_eq!(
                aa.main[1][index..index + 4],
                no_aa.main[1][index..index + 4],
                "clamped viewport boundary"
            );
        }
    }
    let mut border = panel("bright geometry clipped by viewport left edge");
    border.center = [-2.3, 0.];
    border.emissive = [4.; 3];
    let mut model = scene(std::slice::from_ref(&border), false);
    for vertex in model.vertices.as_chunks_mut::<48>().0 {
        let x = f32::from_le_bytes(vertex[0..4].try_into().unwrap());
        vertex[0..4].copy_from_slice(&((x + 2.3) * 0.1 - 2.3).to_le_bytes());
    }
    lighting.aa = false;
    let no_aa = capture_lighting(api, &model, WIDTH, HEIGHT, &lighting);
    lighting.aa = true;
    let aa = capture_lighting(api, &model, WIDTH, HEIGHT, &lighting);
    for y in HEIGHT / 2 - 8..HEIGHT / 2 + 8 {
        for x in [0, WIDTH - 1] {
            let index = (y * WIDTH + x) as usize * 4;
            assert!(
                (0..3).all(|i| aa.main[1][index + i].abs_diff(no_aa.main[1][index + i]) <= 1),
                "FXAA must clamp viewport samples instead of wrapping opposite-edge content"
            );
        }
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn transparent_shadows(api: &str) {
    let mut probes = cases()[11..13].to_vec();
    for (i, case) in probes.iter_mut().enumerate() {
        case.center = [i as f64 - 0.5, 0.];
        case.half_size = 0.4;
    }
    let mut lighting = Lighting::direct();
    lighting.shadows = true;
    lighting.capture_shadows = true;
    let transparent = capture_lighting(api, &scene(&probes, false), WIDTH, HEIGHT, &lighting);
    let transparent_maps = transparent.shadows.unwrap();
    assert!(
        transparent_maps.iter().all(|bytes| bytes
            .as_chunks::<4>()
            .0
            .iter()
            .all(|v| f32::from_le_bytes(*v) == 1.)),
        "alpha-zero MASK and BLEND must leave every cascade clear"
    );
    let opaque = capture_lighting(api, &scene(&probes, true), WIDTH, HEIGHT, &lighting);
    let opaque_maps = opaque.shadows.unwrap();
    assert!(
        opaque_maps.iter().any(|bytes| bytes
            .as_chunks::<4>()
            .0
            .iter()
            .any(|v| f32::from_le_bytes(*v) < 1.)),
        "opaque override must create visible shadow-map coverage"
    );
    assert_ne!(
        transparent_maps, opaque_maps,
        "alpha shadow probe must detect the opaque override"
    );
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn shadow_occlusion(api: &str) {
    let mut receiver = panel("shadow receiver");
    receiver.base = [0.8, 0.4, 0.2, 1.];
    let mut caster = panel("sun occluder");
    caster.half_size = 0.25;
    // At z=1 this center lies exactly along the receiver origin's sun ray.
    caster.center = [
        f64::from(LIGHT[0] / LIGHT[2]),
        f64::from(LIGHT[1] / LIGHT[2]),
    ];
    let mut model = scene(&[receiver.clone(), caster], false);
    let caster_range = model.primitives[1].vertices.clone();
    for vertex in model.vertices[caster_range.clone()].as_chunks_mut::<48>().0 {
        vertex[8..12].copy_from_slice(&1_f32.to_le_bytes());
    }
    model.primitives[1].center[2] = 1.;
    let mut lighting = Lighting::direct();
    let no_shadow = capture_lighting(api, &model, WIDTH, HEIGHT, &lighting);
    lighting.shadows = true;
    let shadowed = capture_lighting(api, &model, WIDTH, HEIGHT, &lighting);
    let center = probe(&receiver).0;
    let unoccluded = rgb_at(&no_shadow.main[0], center);
    let occluded = rgb_at(&shadowed.main[0], center);
    assert!(unoccluded[0] > 0.05, "direct-light positive control");
    assert!(
        (0..3).all(|i| occluded[i] <= (unoccluded[i] * 0.1).max(ABSOLUTE)),
        "known shadow center must lose direct light: {occluded:?} versus {unoccluded:?}"
    );
    let mut outside = receiver.clone();
    outside.probe_offset = [-0.75, -0.75];
    let pixel = probe(&outside).0;
    assert_rgb(
        rgb_at(&shadowed.main[0], pixel),
        rgb_at(&no_shadow.main[0], pixel),
        "outside the known shadow remains lit",
    );
    assert_eq!(
        no_shadow.main[2], shadowed.main[2],
        "enabling shadow must preserve camera depth"
    );

    // Move the caster one metre. A fixed/incorrect shadow projection must not
    // keep the old shadow or miss its new analytically known location.
    for vertex in model.vertices[caster_range].as_chunks_mut::<48>().0 {
        let x = f32::from_le_bytes(vertex[0..4].try_into().unwrap()) + 1.;
        vertex[0..4].copy_from_slice(&x.to_le_bytes());
    }
    model.primitives[1].center[0] += 1.;
    let moved = capture_lighting(api, &model, WIDTH, HEIGHT, &lighting);
    assert_rgb(
        rgb_at(&moved.main[0], center),
        unoccluded,
        "old shadow center becomes lit",
    );
    receiver.probe_offset = [1., 0.];
    let new_center = rgb_at(&moved.main[0], probe(&receiver).0);
    assert!(
        new_center.iter().all(|v| *v <= ABSOLUTE),
        "moved shadow reaches its known center: {new_center:?}"
    );
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn native(api: &str) {
    eprintln!("B3 acceptance: HDR and display");
    hdr_and_display(api);
    eprintln!("B3 acceptance: IBL materials and AO");
    ibl_materials(api);
    eprintln!("B3 acceptance: IBL directions");
    ibl_directions(api);
    eprintln!("B3 acceptance: FXAA");
    fxaa_behavior(api);
    eprintln!("B3 acceptance: alpha shadow coverage");
    transparent_shadows(api);
    eprintln!("B3 acceptance: shadow occlusion");
    shadow_occlusion(api);
}

#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires real Vulkan GPU; run strict and shipping explicitly"]
fn vulkan_b3_lighting_display_and_shadows() {
    native("vulkan");
}

#[cfg(all(windows, feature = "d3d12"))]
#[test]
#[ignore = "requires real D3D12 GPU; run strict and shipping explicitly"]
fn d3d12_b3_lighting_display_and_shadows() {
    native("d3d12");
}
