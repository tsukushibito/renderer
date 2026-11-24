//! Inline Vulkan traversal for static surface irradiance. The transport shader is shared
//! with the software path; the native resources are private to one synchronous bake.
#![allow(unsafe_code)]
mod device;
mod geometry;
use crate::{BakeScene, Lighting, SurfaceSample, Vec3};
use ash::vk;
use device::{Buffer, Commands, Device, barrier};
use geometry::Acceleration;
use renderer::core::vulkan::{ValidationMonitor, VulkanRuntime};
use std::{error::Error, sync::Arc};
use vk::BufferUsageFlags as Usage;
use vk_mem::AllocationCreateFlags as Memory;
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const BATCH: usize = 8192;

struct Pipeline {
    dev: Arc<Device>,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
}
impl Drop for Pipeline {
    fn drop(&mut self) {
        let d = self.dev.raw();
        // SAFETY: Exclusive ownership after all submitted work retires. Partial creation uses null handles.
        unsafe {
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_pipeline_layout(self.layout, None);
            d.destroy_descriptor_pool(self.pool, None);
            d.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}
impl Pipeline {
    fn new(dev: &Arc<Device>, buffers: [&Buffer; 3], tlas: &Acceleration) -> Result<Self> {
        let mut p = Self {
            dev: dev.clone(),
            pipeline: vk::Pipeline::null(),
            layout: vk::PipelineLayout::null(),
            set_layout: vk::DescriptorSetLayout::null(),
            pool: vk::DescriptorPool::null(),
            set: vk::DescriptorSet::null(),
        };
        let d = dev.raw();
        let types = [
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::ACCELERATION_STRUCTURE_KHR,
        ];
        let bindings: Vec<_> = types
            .iter()
            .enumerate()
            .map(|(i, &t)| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(i as u32)
                    .descriptor_count(1)
                    .descriptor_type(t)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
            })
            .collect();
        // SAFETY: The checked-in shader and descriptor layouts agree. The owning struct cleans up on every error.
        unsafe {
            p.set_layout = d.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )?;
            p.layout = d.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().set_layouts(&[p.set_layout]),
                None,
            )?;
            let sizes = [
                vk::DescriptorPoolSize {
                    ty: types[0],
                    descriptor_count: 3,
                },
                vk::DescriptorPoolSize {
                    ty: types[3],
                    descriptor_count: 1,
                },
            ];
            p.pool = d.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(1)
                    .pool_sizes(&sizes),
                None,
            )?;
            p.set = d.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(p.pool)
                    .set_layouts(&[p.set_layout]),
            )?[0];
            let infos = buffers.map(|b| {
                [vk::DescriptorBufferInfo::default()
                    .buffer(b.handle)
                    .range(b.size)]
            });
            let handles = [tlas.handle];
            let mut as_info = vk::WriteDescriptorSetAccelerationStructureKHR::default()
                .acceleration_structures(&handles);
            let mut writes: Vec<_> = infos
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    vk::WriteDescriptorSet::default()
                        .dst_set(p.set)
                        .dst_binding(i as u32)
                        .descriptor_type(types[i])
                        .buffer_info(b)
                })
                .collect();
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(p.set)
                    .dst_binding(3)
                    .descriptor_type(types[3])
                    .descriptor_count(1)
                    .push_next(&mut as_info),
            );
            d.update_descriptor_sets(&writes, &[]);
            let bytes = include_bytes!("../../renderer/shaders/bake_surface_rt.comp.spv");
            let words: Vec<_> = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .copied()
                .map(u32::from_le_bytes)
                .collect();
            let module =
                d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)?;
            let stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::COMPUTE)
                .module(module)
                .name(c"main");
            let created = d.create_compute_pipelines(
                vk::PipelineCache::null(),
                &[vk::ComputePipelineCreateInfo::default()
                    .layout(p.layout)
                    .stage(stage)],
                None,
            );
            d.destroy_shader_module(module, None);
            p.pipeline = match created {
                Ok(v) => v[0],
                Err((partial, e)) => {
                    for item in partial {
                        d.destroy_pipeline(item, None);
                    }
                    return Err(e.into());
                }
            };
        }
        Ok(p)
    }
}
pub(super) struct Transport<const N: usize> {
    pub(super) execution: crate::BakeExecution,
    pipeline: Pipeline,
    commands: Commands,
    input: Buffer,
    output: Buffer,
    _world: Buffer,
    _tlas: Acceleration,
    _blas: Acceleration,
    dev: Arc<Device>,
    monitor: ValidationMonitor,
}
impl<const N: usize> Transport<N> {
    pub fn prepare(scene: &BakeScene, lights: [Lighting; N], samples: u32) -> Result<Option<Self>> {
        if samples == 0 || samples > 4096 {
            return Ok(None);
        }
        let Some(runtime) =
            VulkanRuntime::try_new_ray_query(Default::default(), Default::default())?
        else {
            return Ok(None);
        };
        let monitor = runtime.validation_monitor();
        let execution = crate::BakeExecution::vulkan(
            &runtime,
            crate::BakeMethod::VulkanRayQuery,
            include_bytes!("../../renderer/shaders/bake_surface_rt.comp.spv"),
        );
        let dev = Device::new(runtime.external_device())?;
        drop(runtime);
        let Some(alignment) = geometry::scratch_alignment(&dev, scene.triangles.len()) else {
            return Ok(None);
        };
        let mut commands = Commands::new(&dev)?;
        let mut order: Vec<_> = (0..scene.triangles.len()).collect();
        order.sort_by_key(|&i| {
            scene.materials[scene.triangles[i].material]
                .alpha_cutoff
                .is_some()
        });
        let (_, pixels) = crate::gpu::pack(scene, lights, samples, Some(&order))?;
        // SAFETY: Physical device belongs to the retained Vulkan instance.
        let limits = unsafe {
            dev.host
                .instance()
                .get_physical_device_properties(dev.host.physical_device())
        }
        .limits;
        if pixels.len() as u64 > u64::from(limits.max_storage_buffer_range) {
            return Ok(None);
        }
        let size = pixels.len() as u64;
        let world = Buffer::new(
            &dev,
            size,
            Usage::STORAGE_BUFFER | Usage::TRANSFER_DST,
            Memory::empty(),
        )?;
        {
            let mut staging = Buffer::new(
                &dev,
                size,
                Usage::TRANSFER_SRC,
                Memory::HOST_ACCESS_SEQUENTIAL_WRITE,
            )?;
            staging.write(&pixels)?;
            drop(pixels);
            commands.run(|cmd| {
                // SAFETY: Checked copy size spans both live, nonaliasing buffers.
                unsafe {
                    dev.raw().cmd_copy_buffer(
                        cmd,
                        staging.handle,
                        world.handle,
                        &[vk::BufferCopy::default().size(size)],
                    );
                }
                barrier(
                    &dev,
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::AccessFlags::SHADER_READ,
                );
            })?;
        }
        let (tlas, blas) = geometry::scene(&dev, &mut commands, scene, &order, alignment)?;
        let input = Buffer::new(
            &dev,
            BATCH as u64 * 32,
            Usage::STORAGE_BUFFER,
            Memory::HOST_ACCESS_SEQUENTIAL_WRITE,
        )?;
        let output = Buffer::new(
            &dev,
            BATCH as u64 * 32,
            Usage::STORAGE_BUFFER,
            Memory::HOST_ACCESS_RANDOM,
        )?;
        let pipeline = Pipeline::new(&dev, [&world, &input, &output], &tlas)?;
        eprintln!("Baking surface lightmaps with Vulkan Ray Query");
        Ok(Some(Self {
            execution,
            pipeline,
            commands,
            input,
            output,
            _world: world,
            _tlas: tlas,
            _blas: blas,
            dev,
            monitor,
        }))
    }
    pub fn trace(&mut self, jobs: &[SurfaceSample], values: &mut Vec<[Vec3; N]>) -> Result<()> {
        for chunk in jobs.chunks(BATCH) {
            let start = values.len();
            values.resize(start + chunk.len(), [[0.; 3]; N]);
            for first in (0..N).step_by(2) {
                let upload = self
                    .commands
                    .timing
                    .as_ref()
                    .map(|_| std::time::Instant::now());
                let mut bytes = Vec::with_capacity(chunk.len() * 32);
                for job in chunk {
                    for value in job
                        .position
                        .into_iter()
                        .chain([0.])
                        .chain(job.normal)
                        .chain([first as f32])
                    {
                        bytes.extend_from_slice(&value.to_le_bytes());
                    }
                }
                self.input.write(&bytes)?;
                if let (Some(start), Some(t)) = (upload, &mut self.commands.timing) {
                    t.transfer += start.elapsed();
                }
                let p = &self.pipeline;
                self.commands.run(|cmd| {
                    // SAFETY: Descriptors are live; one workgroup per receiver reads/writes within the checked batch.
                    unsafe {
                        self.dev.raw().cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            p.pipeline,
                        );
                        self.dev.raw().cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            p.layout,
                            0,
                            &[p.set],
                            &[],
                        );
                        self.dev.raw().cmd_dispatch(cmd, chunk.len() as u32, 1, 1);
                    }
                    barrier(
                        &self.dev,
                        cmd,
                        vk::PipelineStageFlags::COMPUTE_SHADER,
                        vk::AccessFlags::SHADER_WRITE,
                        vk::PipelineStageFlags::HOST,
                        vk::AccessFlags::HOST_READ,
                    );
                })?;
                let readback = self
                    .commands
                    .timing
                    .as_ref()
                    .map(|_| std::time::Instant::now());
                for (record, raw) in values[start..]
                    .iter_mut()
                    .zip(self.output.read(bytes.len())?.as_chunks::<32>().0)
                {
                    for (key, rgba) in record[first..].iter_mut().zip(raw.as_chunks::<16>().0) {
                        *key = std::array::from_fn(|i| {
                            f32::from_le_bytes(rgba[i * 4..i * 4 + 4].try_into().unwrap())
                        });
                    }
                }
                if let (Some(start), Some(t)) = (readback, &mut self.commands.timing) {
                    t.transfer += start.elapsed();
                }
            }
        }
        Ok(())
    }
    pub fn enable_timing(&mut self) -> Result<()> {
        self.commands.enable_timing()
    }
    pub fn take_timing(&mut self) -> crate::lightmaps::DispatchTiming {
        self.commands
            .timing
            .replace(Default::default())
            .unwrap_or_default()
    }
    pub fn finish(self) -> Result<()> {
        let monitor = self.monitor.clone();
        drop(self);
        if !monitor.report().is_clean() {
            return Err(format!("Vulkan ray query validation: {:?}", monitor.report()).into());
        }
        Ok(())
    }
}
