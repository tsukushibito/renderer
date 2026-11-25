//! Public-format contracts and encoded-mip sampling through both runtimes.

use sashiko_renderer_core::ImageFormat;
use sashiko_renderer_core::graph::{
    Access, AccessKind, AccessPurpose, AccessRange, GraphCapacity, GraphPool, ImageAspect,
    ImageDesc, PassKind, SampleCount,
};

#[test]
fn compressed_mips_round_up_independently_and_are_sampled_only() {
    // 12x8, 6x4, 3x2, 1x1 use 6 + 2 + 1 + 1 blocks.
    let desc = ImageDesc::texture_2d(12, 8, ImageFormat::Bc1RgbaUnorm).with_mip_levels(4);
    assert_eq!(desc.byte_len(), Some(80));
    assert_eq!(ImageFormat::Bc7RgbaSrgb.packed_byte_len(5, 3), Some(32));
    assert_eq!(desc.with_sample_count(SampleCount::X4).byte_len(), None);
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let handle = graph.image("texture", desc).unwrap();
    assert!(
        graph
            .upload_image("short final block", handle, &[0; 79])
            .is_err()
    );
    graph
        .upload_image("all mip blocks", handle, &[0; 80])
        .unwrap();
    let single = graph.image("single mip", desc.with_mip_levels(1)).unwrap();
    let access = Access {
        resource: single.into(),
        range: AccessRange::Image {
            mip: 0,
            layer: 0,
            aspect: ImageAspect::Color,
        },
        kind: AccessKind::Write,
        purpose: AccessPurpose::StorageWrite,
    };
    assert!(
        graph
            .declare_analysis_pass("invalid storage image", PassKind::Compute, &[access], true)
            .is_err()
    );
    let color = graph.image_view(single, 0, 0).unwrap();
    assert!(
        graph
            .attachment_pass(
                "invalid target",
                sashiko_renderer_core::graph::RasterAttachments {
                    colors: &[sashiko_renderer_core::graph::Attachment {
                        view: color,
                        load: sashiko_renderer_core::graph::LoadOp::Clear([0.; 4]),
                        store: sashiko_renderer_core::graph::StoreOp::Store
                    }],
                    color_resolves: &[],
                    depth: None,
                }
            )
            .is_err()
    );
}

#[cfg(all(windows, feature = "vulkan", feature = "d3d12"))]
mod native {
    use super::*;
    use sashiko_renderer_core::graph::{Binding, BindingResource, BufferDesc};
    use sashiko_renderer_core::program::{ManagedProgram, SamplerDescriptor};
    use sashiko_renderer_core::runtime::{
        ResourceClass, Runtime, RuntimeConfig, RuntimeLimits, WorkingSet,
    };
    use std::time::Duration;
    const FORMATS: [ImageFormat; 8] = [
        ImageFormat::Bc1RgbaUnorm,
        ImageFormat::Bc1RgbaSrgb,
        ImageFormat::Bc3RgbaUnorm,
        ImageFormat::Bc3RgbaSrgb,
        ImageFormat::Bc5RgUnorm,
        ImageFormat::Bc7RgbaUnorm,
        ImageFormat::Bc7RgbaSrgb,
        ImageFormat::Rgba32Float,
    ];

    fn tail(format: ImageFormat) -> (Vec<u8>, [f32; 4]) {
        use ImageFormat::*;
        // BC1 and BC3 choose RGB565 endpoint 0x8000; BC3 selects alpha endpoint 100.
        // BC5 uses R/G endpoints 100/200. BC7 mode 6 has two identical endpoints
        // (128,64,0,254) and zero selectors/p-bits.
        let (bytes, rgba) = match format {
            Bc1RgbaUnorm | Bc1RgbaSrgb => (vec![0, 128, 0, 0, 0, 0, 0, 0], [132, 0, 0, 255]),
            Bc3RgbaUnorm | Bc3RgbaSrgb => (
                vec![100, 0, 0, 0, 0, 0, 0, 0, 0, 128, 0, 0, 0, 0, 0, 0],
                [132, 0, 0, 100],
            ),
            Bc5RgUnorm => (
                vec![100, 0, 0, 0, 0, 0, 0, 0, 200, 0, 0, 0, 0, 0, 0, 0],
                [100, 200, 0, 255],
            ),
            Bc7RgbaUnorm | Bc7RgbaSrgb => (
                vec![64, 32, 16, 4, 2, 0, 254, 127, 0, 0, 0, 0, 0, 0, 0, 0],
                [128, 64, 0, 254],
            ),
            Rgba32Float => {
                let color = [-7.125f32, 0.5, 19., 1.];
                return (
                    color.into_iter().flat_map(f32::to_le_bytes).collect(),
                    color,
                );
            }
            _ => unreachable!(),
        };
        let mut expected = rgba.map(|n| n as f32 / 255.);
        if matches!(format, Bc1RgbaSrgb | Bc3RgbaSrgb | Bc7RgbaSrgb) {
            for c in &mut expected[..3] {
                *c = if *c <= 0.04045 {
                    *c / 12.92
                } else {
                    ((*c + 0.055) / 1.055).powf(2.4)
                };
            }
        }
        (bytes, expected)
    }

    fn sample(api: RuntimeConfig) -> [u8; 8] {
        const OUTPUT: BufferDesc = BufferDesc { size: 64 };
        let desc = |format| ImageDesc::texture_2d(12, 8, format).with_mip_levels(4);
        let mut resources: Vec<_> = FORMATS
            .map(|format| ResourceClass::Image {
                desc: desc(format),
                count: 1,
            })
            .into();
        let copy_desc = ImageDesc::texture_2d(12, 8, ImageFormat::Bc3RgbaUnorm);
        resources.push(ResourceClass::Image {
            desc: copy_desc,
            count: 2,
        });
        resources.push(ResourceClass::Buffer {
            desc: OUTPUT,
            count: 1,
        });
        let program = ManagedProgram::sampled_float_readback();
        let mut runtime = Runtime::new(
            api,
            RuntimeLimits::default(),
            WorkingSet {
                resources: &resources,
                compute: std::slice::from_ref(&program),
                samplers: &[SamplerDescriptor::NearestClamp],
                ..Default::default()
            },
        )
        .unwrap();
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        for format in FORMATS {
            let d = desc(format);
            let (encoded, expected) = tail(format);
            let mut bytes = vec![0; d.byte_len().unwrap() as usize - encoded.len()];
            bytes.extend_from_slice(&encoded);
            let mut g = pool.begin().unwrap();
            let texture = g.image("mips", d).unwrap();
            g.upload_image("packed blocks", texture, &bytes).unwrap();
            let view = g.image_view(texture, 3, 0).unwrap();
            let result = g.buffer("samples", OUTPUT).unwrap();
            g.dispatch(
                "sample tail mip",
                &program,
                &[
                    Binding {
                        group: 0,
                        binding: 0,
                        resource: BindingResource::ImageView(view),
                    },
                    Binding {
                        group: 0,
                        binding: 1,
                        resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
                    },
                    Binding {
                        group: 1,
                        binding: 0,
                        resource: BindingResource::Buffer {
                            buffer: result,
                            offset: 0,
                            size: 64,
                        },
                    },
                ],
            )
            .unwrap();
            let read = g.readback_buffer("pixels", result, 0, 64).unwrap();
            let receipt = runtime.submit(&g.compile().unwrap()).unwrap();
            let mut completed = runtime
                .wait_for_frame(receipt.id(), Duration::from_secs(15))
                .unwrap()
                .unwrap();
            let mut pixels = [0; 64];
            completed.readback_into(read, &mut pixels).unwrap();
            for pixel in pixels.as_chunks::<16>().0 {
                for (word, val) in pixel.as_chunks::<4>().0.iter().zip(expected) {
                    let actual = f32::from_le_bytes(*word);
                    assert!(
                        (actual - val).abs() < 0.002,
                        "{format:?}: sample {actual}, expected {val}"
                    );
                }
            }
        }
        // Preserve two rows of three distinct BC3 blocks across padded native rows.
        let mut g = pool.begin().unwrap();
        let src = g.image("encoded source", copy_desc).unwrap();
        let dst = g.image("encoded destination", copy_desc).unwrap();
        let bytes: Vec<u8> = (0..96).collect();
        g.upload_image("encoded upload", src, &bytes).unwrap();
        g.copy_image("copy blocks", src, dst).unwrap();
        let read = g.readback_image("encoded rows", dst).unwrap();
        let receipt = runtime.submit(&g.compile().unwrap()).unwrap();
        {
            let mut frame = runtime
                .wait_for_frame(receipt.id(), Duration::from_secs(15))
                .unwrap()
                .unwrap();
            let mut packed = [0; 96];
            frame.readback_into(read, &mut packed).unwrap();
            assert_eq!(&packed[..], &bytes);
        }
        let strict = cfg!(feature = "development-validation");
        if let Some(device) = runtime.as_vulkan() {
            assert_ne!(device.device_info().device_type, "CPU");
            let luid = device
                .device_info()
                .device_luid
                .expect("hardware adapter LUID");
            let monitor = device.validation_monitor();
            drop(runtime);
            let report = monitor.report();
            assert!(report.is_clean(), "{report:?}");
            if strict {
                assert_eq!(report.core_validation, Some(true));
                assert_eq!(report.synchronization_validation, Some(true));
            }
            luid
        } else {
            let device = runtime.as_d3d12().unwrap();
            let luid = device.device_info().luid;
            let monitor = device.validation_monitor();
            drop(runtime);
            let report = monitor.snapshot();
            assert!(report.is_clean(), "{report:?}");
            if strict {
                assert!(
                    report.debug_layer_enabled
                        && report.gpu_based_validation_enabled
                        && report.teardown_checked
                );
            }
            luid
        }
    }

    #[test]
    #[ignore = "requires real Vulkan and D3D12 hardware; development-validation requires both native validation layers"]
    fn compressed_and_float32_mip_sampling() {
        assert_eq!(
            sample(RuntimeConfig::Vulkan(Default::default())),
            sample(RuntimeConfig::D3D12(Default::default()))
        );
    }
}
