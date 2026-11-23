use crate::{BakeBackend, BakeScene, Lighting, SurfaceSample};
use renderer::core::{
    graph::{Binding, BindingResource, BufferDesc, GraphCapacity, GraphPool, ImageDesc},
    program::{
        BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
        ManagedProgram, ShaderStages,
    },
    runtime::{ResourceClass, Runtime, RuntimeConfig, RuntimeLimits, WorkingSet},
};
use std::{error::Error, time::Duration};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const RECEIVER_BYTES: u64 = 32;
const RESPONSE_BYTES: u64 = 160;
const WAIT: Duration = Duration::from_secs(120);
const fn slot(binding: u32, kind: BindingType, min_size: u64) -> BindingLayout {
    BindingLayout {
        group: 0,
        binding,
        kind,
        min_size,
        stages: ShaderStages::COMPUTE,
    }
}
const PROGRAM: ComputeProgramDescriptor = ComputeProgramDescriptor {
    entry: "main",
    workgroup_size: [32, 1, 1],
    bindings: &[
        slot(0, BindingType::SampledImage, 0),
        slot(1, BindingType::StorageRead, RECEIVER_BYTES),
        slot(2, BindingType::StorageWrite, RESPONSE_BYTES),
    ],
    dispatch: DispatchContract {
        bytes: &[0, RECEIVER_BYTES, RESPONSE_BYTES],
        workgroups: DispatchWorkgroups::BufferGrid2d {
            group: 0,
            binding: 2,
            max_workgroups: [65535, 1],
            read_bindings: &[(0, 1)],
        },
    },
    shaders: renderer::core::program::embedded_shader_binaries!(
        include_bytes!("../../renderer/shaders/bake_transport.comp.spv"),
        include_bytes!("../../renderer/shaders/dxil/bake_transport.comp.dxil")
    ),
};
#[allow(unsafe_code)]
fn program() -> ManagedProgram {
    // SAFETY: one 32-thread group reads its own 32-byte receiver and writes
    // ten distinct vec4s. All image reads are clamped to the complete view.
    // Miss traversal advances monotonically and leaf and sample loops are bounded.
    unsafe { ManagedProgram::from_trusted_descriptor(&PROGRAM) }
}
fn texel(rgb: [f32; 3], w: f32) -> [f32; 4] {
    [rgb[0], rgb[1], rgb[2], w]
}
fn pack<const N: usize>(
    scene: &BakeScene,
    lights: [Lighting; N],
    samples: u32,
) -> Result<(ImageDesc, Vec<u8>)> {
    let counts = [
        scene.bvh.0.len(),
        scene.triangles.len(),
        scene.materials.len(),
        scene.textures.len(),
    ];
    if scene.point_lights.len() > 0xffffff
        || scene.rect_lights.len() > 0xffffff
        || counts.iter().any(|&n| n > 0xffffff)
        || scene
            .textures
            .iter()
            .any(|t| t.width > 16384 || t.height > 16384)
    {
        return Err("scene exceeds GPU transport addressing".into());
    }
    let count = 2
        + 4 * scene.rect_lights.len()
        + 2 * scene.point_lights.len()
        + 3 * N
        + counts[0] * 3
        + counts[1] * 4
        + counts[2] * 3
        + counts[3]
        + scene.textures.iter().map(|t| t.pixels.len()).sum::<usize>();
    let width = count.min(4096);
    let height = count.div_ceil(width);
    if height > 16384 {
        return Err("bake scene exceeds the GPU texture atlas".into());
    }
    let mut bytes = Vec::with_capacity(width * height * 16);
    let mut push = |v: [f32; 4]| {
        for x in v {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
    };
    push(counts.map(|n| n as f32));
    push([
        samples as f32,
        N as f32,
        scene.rect_lights.len() as f32,
        scene.point_lights.len() as f32,
    ]);
    for light in lights {
        push(texel(
            light.sky_rgb_nits,
            light.environment.map_or(-1., |m| m.texture as f32),
        ));
        push(texel(
            light.sun.rgb_lux(),
            light.environment.map_or(0., |m| m.yaw_radians),
        ));
        push(texel(light.sun.direction(), 0.));
    }
    for rect in &scene.rect_lights {
        let [u, v] = rect.half_edges();
        push(texel(rect.center(), rect.range()));
        push(texel(u, rect.area()));
        push(texel(v, 0.));
        push(texel(rect.rgb_nits(), 0.));
    }
    for point in &scene.point_lights {
        push(texel(point.position(), point.range()));
        push(texel(point.rgb_candela(), 0.));
    }
    for n in &scene.bvh.0 {
        push(texel(n.lo, n.start as f32));
        push(texel(n.hi, n.count as f32));
        push([n.escape as f32, 0., 0., 0.]);
    }
    for t in &scene.triangles {
        for (p, uv) in t.positions.into_iter().zip(t.uv) {
            push(texel(p, uv[0]));
        }
        push(texel(t.uv.map(|uv| uv[1]), t.material as f32));
    }
    for m in &scene.materials {
        push(texel(m.diffuse, m.base_texture.map_or(-1., |i| i as f32)));
        push(texel(m.emissive_nits, m.alpha_cutoff.unwrap_or(-1.)));
        push([
            m.opacity,
            m.metallic,
            m.orm_texture.map_or(-1., |i| i as f32),
            m.emission_texture.map_or(-1., |i| i as f32),
        ]);
    }
    let mut start = 0usize;
    for t in &scene.textures {
        push([
            t.width as f32,
            t.height as f32,
            (start % 65536) as f32,
            (start / 65536) as f32,
        ]);
        start += t.pixels.len();
    }
    for t in &scene.textures {
        for &p in &t.pixels {
            push(p);
        }
    }
    bytes.resize(width * height * 16, 0);
    Ok((ImageDesc::rgba32_float(width as u32, height as u32), bytes))
}
/// A single transport scene upload serves all surface pages for these scenarios.
pub(super) struct Transport<const N: usize> {
    runtime: Runtime,
    pool: GraphPool,
    image: renderer::core::runtime::ManagedImage,
    program: ManagedProgram,
    input: BufferDesc,
    output: BufferDesc,
    batch: usize,
}

impl<const N: usize> Transport<N> {
    pub(super) fn prepare(
        choice: BakeBackend,
        scene: &BakeScene,
        lights: [Lighting; N],
        samples: u32,
    ) -> Result<Option<Self>> {
        if choice == BakeBackend::Cpu {
            return Ok(None);
        }
        if samples > 4096 {
            return if choice == BakeBackend::Auto {
                Ok(None)
            } else {
                Err("GPU supports at most 4096 rays per receiver".into())
            };
        }
        let (desc, pixels) = match pack(scene, lights, samples) {
            Ok(ok) => ok,
            Err(_) if choice == BakeBackend::Auto => return Ok(None),
            Err(e) => return Err(e),
        };
        // Limit the longest dispatch. Upload static triangles once for all batches.
        let batch = (65536 / samples as usize).clamp(1, 512);
        let input = BufferDesc {
            size: batch as u64 * RECEIVER_BYTES,
        };
        let output = BufferDesc {
            size: batch as u64 * RESPONSE_BYTES,
        };
        let graph_cap = GraphCapacity {
            upload_bytes: pixels.len().max(input.size as usize),
            ..Default::default()
        };
        let limits = RuntimeLimits {
            graph: graph_cap,
            frames: 1,
            results: 1,
            ..Default::default()
        };
        let programs = [program()];
        let resources = [
            ResourceClass::Image { desc, count: 1 },
            ResourceClass::Buffer {
                desc: input,
                count: 1,
            },
            ResourceClass::Buffer {
                desc: output,
                count: 1,
            },
        ];
        let set = WorkingSet {
            resources: &resources,
            compute: &programs,
            raster: &[],
        };
        let upload =
            (u64::from(desc.width) * 16).next_multiple_of(256) * u64::from(desc.height) + 512;
        let mut selected = None;
        for candidate in [BakeBackend::Vulkan, BakeBackend::D3D12] {
            if choice != BakeBackend::Auto && choice != candidate {
                continue;
            }
            let config = match candidate {
                #[cfg(feature = "vulkan")]
                BakeBackend::Vulkan => {
                    RuntimeConfig::Vulkan(renderer::core::runtime::VulkanConfig {
                        // The dispatch graph owns the receiver upload and its readback concurrently.
                        staging_bytes: upload
                            .max(input.size.next_multiple_of(256) + output.size)
                            .next_multiple_of(65536),
                        ..Default::default()
                    })
                }
                #[cfg(all(windows, feature = "d3d12"))]
                BakeBackend::D3D12 => RuntimeConfig::D3D12(renderer::core::runtime::D3D12Config {
                    upload_bytes: upload.max(input.size),
                    max_buffer_bytes: output.size,
                    readback_bytes: output.size,
                    ..Default::default()
                }),
                _ => continue,
            };
            match Runtime::new(config, limits, set) {
                Ok(runtime) => {
                    eprintln!("Baking on {candidate:?}");
                    selected = Some(runtime);
                    break;
                }
                Err(e) if choice == BakeBackend::Auto => {
                    eprintln!("{candidate:?} bake initialization: {e}")
                }
                Err(e) => return Err(e.into()),
            }
        }
        let Some(mut rt) = selected else {
            return if choice == BakeBackend::Auto {
                Ok(None)
            } else {
                Err("requested GPU backend unavailable".into())
            };
        };
        let mut pool = GraphPool::new(graph_cap, 1)?;
        let (uploaded, image) = rt.upload_texture(&mut pool, "bake world", desc, &pixels)?;
        drop(pixels);
        rt.wait_for_frame(uploaded, WAIT)?
            .ok_or("bake upload timed out")?;
        Ok(Some(Self {
            runtime: rt,
            pool,
            image,
            program: programs.into_iter().next().unwrap(),
            input,
            output,
            batch,
        }))
    }
    pub(super) fn trace(&mut self, jobs: &[SurfaceSample], limit: f32) -> Result<Vec<[f32; 4]>> {
        let Self {
            runtime: rt,
            pool,
            image,
            program,
            input,
            output,
            batch,
        } = self;
        let (input, output, batch) = (*input, *output, *batch);
        let stride = 2 + N * 4;
        let mut result = vec![[0.; 4]; jobs.len() * stride];
        for (chunk, dst) in jobs.chunks(batch).zip(result.chunks_mut(batch * stride)) {
            for first in (0..N).step_by(2) {
                let mut g = pool.begin()?;
                let scene = g.import_image("transport scene", image.snapshot())?;
                let scene = g.image_view(scene, 0, 0)?;
                let receivers = g.buffer("receivers", input)?;
                let responses = g.buffer("irradiance", output)?;
                let bytes = chunk
                    .iter()
                    .flat_map(|s| {
                        texel(s.position, limit)
                            .into_iter()
                            .chain(texel(s.normal, first as f32))
                    })
                    .flat_map(f32::to_le_bytes)
                    .collect::<Vec<_>>();
                g.upload_buffer("upload bake receivers", receivers, 0, &bytes)?;
                let result_size = chunk.len() as u64 * RESPONSE_BYTES;
                let buffer = |binding, buffer, size| Binding {
                    group: 0,
                    binding,
                    resource: BindingResource::Buffer {
                        buffer,
                        offset: 0,
                        size,
                    },
                };
                g.dispatch_buffer_grid(
                    "integrate diffuse transport",
                    program,
                    &[
                        Binding {
                            group: 0,
                            binding: 0,
                            resource: BindingResource::ImageView(scene),
                        },
                        buffer(1, receivers, bytes.len() as u64),
                        buffer(2, responses, result_size),
                    ],
                    [chunk.len() as u32, 1],
                )?;
                let readback = g.readback_buffer("baked samples", responses, 0, result_size)?;
                let receipt = rt.submit(&g.compile()?)?;
                let mut completed = rt
                    .wait_for_frame(receipt.id(), WAIT)?
                    .ok_or("bake dispatch timed out")?;
                let mut bytes = vec![0; result_size as usize];
                completed.readback_into(readback, &mut bytes)?;
                for (raw, record) in bytes
                    .as_chunks::<{ RESPONSE_BYTES as usize }>()
                    .0
                    .iter()
                    .zip(dst.chunks_exact_mut(stride))
                {
                    let source = raw.as_chunks::<16>().0;
                    let decode = |w: &[u8; 16]| {
                        std::array::from_fn(|i| {
                            f32::from_le_bytes(w[i * 4..i * 4 + 4].try_into().unwrap())
                        })
                    };
                    if first == 0 {
                        record[0] = decode(&source[0]);
                        record[1] = decode(&source[1]);
                    }
                    let count = 4 * (N - first).min(2);
                    for (target, word) in record[2 + first * 4..]
                        .iter_mut()
                        .zip(&source[2..2 + count])
                    {
                        *target = decode(word);
                    }
                }
            }
        }
        Ok(result)
    }
    pub(super) fn finish(self) -> Result<()> {
        let Self {
            image, runtime: rt, ..
        } = self;
        drop(image);
        #[cfg(feature = "vulkan")]
        let vk = rt.as_vulkan().map(|r| r.validation_monitor());
        #[cfg(all(windows, feature = "d3d12"))]
        let dx = rt.as_d3d12().map(|r| r.validation_monitor());
        drop(rt);
        #[cfg(feature = "vulkan")]
        if let Some(m) = vk
            && !m.report().is_clean()
        {
            return Err(format!("Vulkan bake validation: {:?}", m.report()).into());
        }
        #[cfg(all(windows, feature = "d3d12"))]
        if let Some(m) = dx
            && !m.snapshot().is_clean()
        {
            return Err(format!("D3D12 bake validation: {:?}", m.snapshot()).into());
        }
        Ok(())
    }
}

pub(super) fn integrate<const N: usize>(
    choice: BakeBackend,
    scene: &BakeScene,
    jobs: &[SurfaceSample],
    lights: [Lighting; N],
    samples: u32,
    limit: f32,
) -> Result<Option<Vec<[f32; 4]>>> {
    if jobs.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let Some(mut gpu) = Transport::prepare(choice, scene, lights, samples)? else {
        return Ok(None);
    };
    let result = gpu.trace(jobs, limit)?;
    gpu.finish()?;
    Ok(Some(result))
}
