//! Narrow adoption experiment, not Renderer's graph compiler or a public API.
//! Command ownership mirrors proposal A; wgpu owns native transitions/lifetimes.
use std::{error::Error, ops::Range, sync::mpsc, time::Duration};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const WIDTH: u32 = 8;
const HEIGHT: u32 = 4;
const ROW: u32 = 256;

#[derive(Clone, Debug, PartialEq)]
struct Draw {
    instances: Range<u32>,
    scissor: [u32; 4],
}

enum Pass {
    Raster {
        pipeline: wgpu::RenderPipeline,
        color: Option<wgpu::TextureView>,
        depth: Option<wgpu::TextureView>,
        draws: Vec<Draw>,
    },
    Compute {
        pipeline: wgpu::ComputePipeline,
        binding: wgpu::BindGroup,
        groups: [u32; 3],
    },
}

#[derive(Default)]
struct Plan(Vec<Pass>);

impl Plan {
    // Closure inputs may borrow a short-lived frame. Only owned operands remain.
    fn add(&mut self, build: impl FnOnce() -> Pass) {
        self.0.push(build());
    }

    fn record(&self, encoder: &mut wgpu::CommandEncoder) {
        for pass in &self.0 {
            match pass {
                Pass::Raster {
                    pipeline,
                    color,
                    depth,
                    draws,
                } => {
                    let colors: Vec<_> = color
                        .iter()
                        .map(|view| {
                            Some(wgpu::RenderPassColorAttachment {
                                view,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                    store: wgpu::StoreOp::Store,
                                },
                            })
                        })
                        .collect();
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("owned-raster"),
                        color_attachments: &colors,
                        depth_stencil_attachment: depth.as_ref().map(|view| {
                            wgpu::RenderPassDepthStencilAttachment {
                                view,
                                depth_ops: Some(wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(1.0),
                                    store: wgpu::StoreOp::Store,
                                }),
                                stencil_ops: None,
                            }
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    pass.set_pipeline(pipeline);
                    for draw in draws {
                        let [x, y, width, height] = draw.scissor;
                        pass.set_scissor_rect(x, y, width, height);
                        pass.draw(0..3, draw.instances.clone());
                    }
                }
                Pass::Compute {
                    pipeline,
                    binding,
                    groups,
                } => {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("caller-compute"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, binding, &[]);
                    pass.dispatch_workgroups(groups[0], groups[1], groups[2]);
                }
            }
        }
    }
}

fn draw_list() -> Vec<Draw> {
    vec![
        Draw {
            instances: 0..1,
            scissor: [0, 0, WIDTH / 2, HEIGHT],
        },
        Draw {
            instances: 1..2,
            scissor: [WIDTH / 2, 0, WIDTH / 2, HEIGHT],
        },
    ]
}

fn image(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fixture"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn raster_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    depth: bool,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(if depth { "depth-only" } else { "color-only" }),
        layout: None,
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: depth.then_some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: (!depth).then_some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn scenario(
    device: &wgpu::Device,
    color: &wgpu::TextureView,
    depth: &wgpu::TextureView,
    output: &wgpu::TextureView,
    effect: &str,
) -> Plan {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fixture-raster"),
        source: wgpu::ShaderSource::Wgsl(include_str!("raster.wgsl").into()),
    });
    let mut plan = Plan::default();
    {
        // This Vec and the shader's CPU handle are gone before native recording.
        let draws = draw_list();
        for (is_depth, attachment) in [(true, depth), (false, color)] {
            let pipeline = raster_pipeline(device, &shader, is_depth);
            plan.add(|| Pass::Raster {
                pipeline,
                color: (!is_depth).then(|| attachment.clone()),
                depth: is_depth.then(|| attachment.clone()),
                draws: draws.clone(),
            });
        }
    }
    // Effect source is a caller input. Replacing it does not add an effect enum.
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("caller-effect"),
        source: wgpu::ShaderSource::Wgsl(effect.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("caller-effect"),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("effect-binding"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(color),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(depth),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(output),
            },
        ],
    });
    plan.add(|| Pass::Compute {
        pipeline,
        binding,
        groups: [WIDTH / 4, HEIGHT / 4, 1],
    });
    plan
}

fn expected() -> Vec<u8> {
    (0..HEIGHT)
        .flat_map(|_| {
            (0..WIDTH).flat_map(|x| {
                if x < WIDTH / 2 {
                    [0, 255, 64, 255]
                } else {
                    [255, 0, 64, 255]
                }
            })
        })
        .collect()
}

fn hexadecimal(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn run(backend: wgpu::Backends, flags: wgpu::InstanceFlags) -> Result<(Vec<u8>, [u32; 2])> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backend;
    descriptor.flags = flags;
    let instance = wgpu::Instance::new(descriptor);
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        })
        .await?;
    let info = adapter.get_info();
    if wgpu::Backends::from(info.backend) != backend {
        return Err(format!("requested {backend:?}, selected {:?}", info.backend).into());
    }
    if !matches!(
        info.device_type,
        wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
    ) {
        return Err(format!("physical GPU required; selected {info:?}").into());
    }
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("foundation-probe"),
            ..Default::default()
        })
        .await?;
    // API validation errors are failures. Native warning collection is outside
    // this probe: flags below are requests, not strict native acceptance proof.
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let color = image(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let depth = image(
        &device,
        wgpu::TextureFormat::Depth32Float,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let output = image(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
    );
    let plan = scenario(
        &device,
        &color.create_view(&Default::default()),
        &depth.create_view(&Default::default()),
        &output.create_view(&Default::default()),
        include_str!("effect.wgsl"),
    );
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(ROW * HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    plan.record(&mut encoder);
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &output,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ROW),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit([encoder.finish()]);
    // Drop all caller ownership immediately after submit. wgpu retains submitted
    // references; mapping is consumed only after the actual submission completes.
    drop((plan, color, depth, output));
    let (tx, rx) = mpsc::sync_channel(1);
    readback.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = tx.send(result);
    });
    device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: Some(Duration::from_secs(10)),
    })?;
    rx.recv_timeout(Duration::from_secs(10))??;
    if let Some(error) = scope.pop().await {
        return Err(error.into());
    }
    let mapped = readback.get_mapped_range(..)?;
    let pixels: Vec<_> = mapped
        .as_chunks::<{ ROW as usize }>()
        .0
        .iter()
        .flat_map(|row| row[..(WIDTH * 4) as usize].iter().copied())
        .collect();
    drop(mapped);
    readback.unmap();
    if pixels != expected() {
        return Err(format!(
            "{} fixture pixels differ: {}",
            info.name,
            hexadecimal(&pixels)
        )
        .into());
    }
    // Negative control: removing native validation flags does not remove
    // wgpu-core's per-draw API validation. Nothing invalid is submitted.
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let control = image(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let control_view = control.create_view(&Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &control_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            ..Default::default()
        });
        pass.draw(0..3, 0..1);
    }
    drop(encoder.finish());
    if !matches!(scope.pop().await, Some(wgpu::Error::Validation { .. })) {
        return Err("negative control did not reject draw without pipeline".into());
    }
    println!(
        "{}",
        serde_json::json!({
            "backend": format!("{:?}", info.backend), "gpu": info.name,
            "vendor": info.vendor, "device": info.device, "driver": info.driver,
            "driver_info": info.driver_info, "wgpu": "30.0.2",
            "requested_flags_bits": flags.bits(), "extent": [WIDTH, HEIGHT],
            "passes": 3, "draws": 4, "dispatches": 1,
            "rgba8_hex": hexadecimal(&pixels), "expected_match": true,
        "valid_fixture_api_validation_errors": 0, "native_validation_effective": null,
        "negative_control_rejected_draw_without_pipeline": true,
            "native_diagnostics_after_destruction": null
        })
    );
    Ok((pixels, [info.vendor, info.device]))
}

async fn compare(flags: wgpu::InstanceFlags) -> Result<()> {
    let vulkan = run(wgpu::Backends::VULKAN, flags).await?;
    let dx12 = run(wgpu::Backends::DX12, flags).await?;
    if vulkan != dx12 {
        return Err("Vulkan/D3D12 pixels or adapter vendor/device differ".into());
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let flags = match args.as_slice() {
        [] => wgpu::InstanceFlags::empty(),
        [arg] if arg == "--native-validation" => {
            wgpu::InstanceFlags::VALIDATION | wgpu::InstanceFlags::DEBUG
        }
        _ => return Err("usage: renderer-foundation-probe [--native-validation]".into()),
    };
    pollster::block_on(compare(flags))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a physical Windows Vulkan+D3D12 GPU; no silent skip"]
    fn two_native_backends_match_expected() -> Result<()> {
        pollster::block_on(compare(wgpu::InstanceFlags::empty()))
    }
}
