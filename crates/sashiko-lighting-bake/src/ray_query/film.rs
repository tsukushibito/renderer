//! Shared, instanced triangle traversal for offline film samples.
use super::{
    Acceleration, Buffer, Commands, Device, Memory, Pipeline, Result, Usage, barrier, geometry,
};
use ash::vk;
use sashiko_asset::{
    ObjectStore,
    scene::{FilmRay, ScenePackage},
};
use sashiko_renderer::core::vulkan::{ValidationMode, ValidationMonitor, VulkanRuntime};
use std::{path::Path, sync::Arc};
mod camera;
pub use camera::FilmSampler;
mod pack;
mod sky;

const FILM_FILTER_BINS: usize = 2048;
const PIXELS_PER_DISPATCH: usize = 2048;
const BATCH: usize = 65536;
const WORKGROUP: usize = 64;

/// Select paths by the number of scattering events before they reach the sky.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum FilmLighting {
    Combined,
    Direct,
    Indirect,
    /// Direct lighting at the first visible surface with shadow rays unobstructed.
    Unshadowed,
}

pub struct FilmIntersection {
    pub distance: f32,
    /// Original indices in the cooked package, independent of BLAS sharing.
    pub placement: u32,
    pub primitive: u32,
    pub triangle: u32,
}

pub struct FilmTracer {
    shaded: bool,
    pipeline: Pipeline,
    commands: Commands,
    input: Buffer,
    output: Buffer,
    _tlas: Acceleration,
    _blas: Vec<Acceleration>,
    _world: Buffer,
    dev: Arc<Device>,
    monitor: ValidationMonitor,
    validation: ValidationMode,
}

impl FilmTracer {
    /// Prepare immutable cooked textures, one BLAS per unique mesh, and one TLAS
    /// instance per placement. Separate offline device; all batches retire synchronously.
    pub fn open(package: &Path, validation: ValidationMode) -> Result<Self> {
        Self::prepare(package, validation, None)
    }

    /// Prepare full-resolution physical materials and the authored HDR light.
    pub fn for_radiance(
        package: &Path,
        validation: ValidationMode,
        lighting: FilmLighting,
    ) -> Result<Self> {
        Self::prepare(package, validation, Some(lighting))
    }

    fn prepare(
        package: &Path,
        validation: ValidationMode,
        lighting: Option<FilmLighting>,
    ) -> Result<Self> {
        let shaded = lighting.is_some();
        let scene = ScenePackage::open(package)?;
        let store = ObjectStore::new(package);
        let runtime = VulkanRuntime::try_new_ray_query(Default::default(), validation)?
            .ok_or("film tracing requires Vulkan acceleration structures and ray queries")?;
        let monitor = runtime.validation_monitor();
        let dev = Device::new(runtime.external_device())?;
        drop(runtime);
        // SAFETY: The physical device belongs to the instance retained by dev.
        let limits = unsafe {
            dev.host
                .instance()
                .get_physical_device_properties(dev.host.physical_device())
        }
        .limits;
        let mut commands = Commands::new(&dev)?;
        let pack::World {
            mut bytes,
            meshes,
            parts,
        } = pack::pack(
            &scene,
            &store,
            u64::from(limits.max_storage_buffer_range),
            shaded,
        )?;
        if let Some(lighting) = lighting {
            bytes[24..28].copy_from_slice(&(lighting as u32).to_le_bytes());
        }
        let world = Buffer::new(
            &dev,
            bytes.len() as u64,
            Usage::STORAGE_BUFFER
                | Usage::TRANSFER_DST
                | Usage::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
                | Usage::SHADER_DEVICE_ADDRESS,
            Memory::empty(),
        )?;
        let chunk_size = bytes.len().min(32 << 20);
        let mut staging = Buffer::new(
            &dev,
            chunk_size as u64,
            Usage::TRANSFER_SRC,
            Memory::HOST_ACCESS_SEQUENTIAL_WRITE,
        )?;
        for (i, chunk) in bytes.chunks(chunk_size).enumerate() {
            staging.write(chunk)?;
            commands.run(|cmd| {
                // SAFETY: Chunks partition the checked world allocation. The staging
                // buffer is at least one chunk and is rewritten only after the fence.
                unsafe {
                    dev.raw().cmd_copy_buffer(
                        cmd,
                        staging.handle,
                        world.handle,
                        &[vk::BufferCopy::default()
                            .dst_offset((i * chunk_size) as u64)
                            .size(chunk.len() as u64)],
                    )
                };
                barrier(
                    &dev,
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::PipelineStageFlags::COMPUTE_SHADER
                        | vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR,
                    vk::AccessFlags::SHADER_READ | vk::AccessFlags::ACCELERATION_STRUCTURE_READ_KHR,
                );
            })?;
        }
        drop(staging);
        drop(bytes);
        let (tlas, blas) =
            geometry::placed_scene(&dev, &mut commands, &world, &meshes, &parts, &scene)?;
        let input = Buffer::new(
            &dev,
            if shaded {
                (96 + WORKGROUP * 16 + (FILM_FILTER_BINS + 1) * 4) as u64
            } else {
                (BATCH * 32) as u64
            },
            Usage::STORAGE_BUFFER,
            Memory::HOST_ACCESS_SEQUENTIAL_WRITE,
        )?;
        let output = Buffer::new(
            &dev,
            if shaded {
                (PIXELS_PER_DISPATCH * 16) as u64
            } else {
                (BATCH * 16) as u64
            },
            Usage::STORAGE_BUFFER,
            Memory::HOST_ACCESS_RANDOM,
        )?;
        let pipeline = Pipeline::new(
            &dev,
            [&world, &input, &output],
            &tlas,
            if shaded {
                include_bytes!("../../shaders/film_radiance.comp.spv")
            } else {
                include_bytes!("../../shaders/film_primary.comp.spv")
            },
        )?;
        Ok(Self {
            shaded,
            pipeline,
            commands,
            input,
            output,
            _tlas: tlas,
            _blas: blas,
            _world: world,
            dev,
            monitor,
            validation,
        })
    }

    /// Trace camera samples against original triangles. Fractional alpha uses a
    /// stable random decision per ray and hit distance, independent of traversal order.
    pub fn primary(
        &mut self,
        rays: &[(FilmRay, u32)],
        out: &mut [Option<FilmIntersection>],
    ) -> Result<()> {
        if self.shaded {
            return Err("visibility requires the primary-ray pipeline".into());
        }
        if rays.len() != out.len() {
            return Err("ray and result counts differ".into());
        }
        let mut bytes = Vec::with_capacity(rays.len().min(BATCH).next_multiple_of(WORKGROUP) * 32);
        for (samples, result) in rays.chunks(BATCH).zip(out.chunks_mut(BATCH)) {
            bytes.clear();
            for (ray, seed) in samples {
                if !ray.origin.is_finite() || !ray.direction.is_normalized() {
                    return Err("invalid film ray".into());
                }
                for v in ray.origin.to_array() {
                    bytes.extend(v.to_le_bytes());
                }
                bytes.extend(seed.to_le_bytes());
                for v in ray.direction.to_array() {
                    bytes.extend(v.to_le_bytes());
                }
                bytes.extend(0u32.to_le_bytes());
            }
            let groups = samples.len().div_ceil(WORKGROUP);
            // Inactive tail lanes read a nonzero marker before constructing a ray.
            bytes.resize(groups * WORKGROUP * 32, 255);
            self.input.write(&bytes)?;
            self.dispatch(groups as u32)?;
            for (slot, record) in result
                .iter_mut()
                .zip(self.output.read(samples.len() * 16)?.as_chunks::<16>().0)
            {
                let [distance, placement, primitive, triangle] = std::array::from_fn(|i| {
                    u32::from_le_bytes(record[i * 4..i * 4 + 4].try_into().unwrap())
                });
                *slot = (placement != u32::MAX).then_some(FilmIntersection {
                    distance: f32::from_bits(distance),
                    placement,
                    primitive,
                    triangle,
                });
            }
        }
        Ok(())
    }

    fn dispatch(&mut self, groups: u32) -> Result<()> {
        let p = &self.pipeline;
        self.commands.run(|cmd| {
            // SAFETY: All sets and buffers remain live, and dispatch fits the
            // bound records including padding. run waits for completion.
            unsafe {
                self.dev
                    .raw()
                    .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, p.pipeline);
                self.dev.raw().cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    p.layout,
                    0,
                    &[p.set],
                    &[],
                );
                self.dev.raw().cmd_dispatch(cmd, groups, 1, 1);
            }
            barrier(
                &self.dev,
                cmd,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::AccessFlags::SHADER_WRITE,
                vk::PipelineStageFlags::HOST,
                vk::AccessFlags::HOST_READ,
            );
        })
    }

    pub fn finish(self) -> Result<()> {
        let monitor = self.monitor.clone();
        let required = matches!(self.validation, ValidationMode::Required);
        drop(self);
        let report = monitor.report();
        if !report.is_clean()
            || (required
                && (report.core_validation != Some(true)
                    || report.synchronization_validation != Some(true)))
        {
            return Err(format!("Vulkan film traversal validation: {report:?}").into());
        }
        Ok(())
    }
}
