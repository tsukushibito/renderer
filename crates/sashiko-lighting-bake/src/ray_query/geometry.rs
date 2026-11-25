use super::{
    Result,
    device::{Buffer, Commands, Device, barrier},
};
use ash::vk;
use std::sync::Arc;
use vk::BufferUsageFlags as Usage;
use vk_mem::AllocationCreateFlags as Memory;

pub(super) struct Acceleration {
    pub handle: vk::AccelerationStructureKHR,
    dev: Arc<Device>,
    _storage: Buffer,
}
impl Drop for Acceleration {
    fn drop(&mut self) {
        // SAFETY: All commands have retired and backing storage is still live.
        unsafe {
            self.dev
                .acceleration
                .destroy_acceleration_structure(self.handle, None);
        }
    }
}
fn build(
    dev: &Arc<Device>,
    commands: &mut Commands,
    ty: vk::AccelerationStructureTypeKHR,
    geometries: &[vk::AccelerationStructureGeometryKHR],
    counts: &[u32],
    alignment: u64,
) -> Result<Acceleration> {
    let mut info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
        .ty(ty)
        .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
        .geometries(geometries);
    let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
    // SAFETY: One primitive count per geometry, all input formats and build flags match the build.
    unsafe {
        dev.acceleration.get_acceleration_structure_build_sizes(
            vk::AccelerationStructureBuildTypeKHR::DEVICE,
            &info,
            counts,
            &mut sizes,
        );
    }
    let storage = Buffer::new(
        dev,
        sizes.acceleration_structure_size,
        Usage::ACCELERATION_STRUCTURE_STORAGE_KHR | Usage::SHADER_DEVICE_ADDRESS,
        Memory::empty(),
    )?;
    let create = vk::AccelerationStructureCreateInfoKHR::default()
        .ty(ty)
        .buffer(storage.handle)
        .size(storage.size);
    // SAFETY: Live aligned backing buffer with queried size, lifetime owned by Acceleration.
    let handle = unsafe {
        dev.acceleration
            .create_acceleration_structure(&create, None)
    }?;
    let acceleration = Acceleration {
        dev: dev.clone(),
        handle,
        _storage: storage,
    };
    let scratch = Buffer::new(
        dev,
        sizes
            .build_scratch_size
            .checked_add(alignment)
            .ok_or("scratch size overflow")?,
        Usage::STORAGE_BUFFER | Usage::SHADER_DEVICE_ADDRESS,
        Memory::empty(),
    )?;
    info = info
        .dst_acceleration_structure(handle)
        .scratch_data(vk::DeviceOrHostAddressKHR {
            device_address: scratch.address().next_multiple_of(alignment),
        });
    let ranges: Vec<_> = counts
        .iter()
        .map(|&n| vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(n))
        .collect();
    commands.run(|cmd| {
        // SAFETY: Inputs, output, and scratch are live, non-aliasing, and cover the queried ranges.
        unsafe {
            dev.acceleration
                .cmd_build_acceleration_structures(cmd, &[info], &[&ranges]);
        }
        barrier(
            dev,
            cmd,
            vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR,
            vk::AccessFlags::ACCELERATION_STRUCTURE_WRITE_KHR,
            vk::PipelineStageFlags::ACCELERATION_STRUCTURE_BUILD_KHR
                | vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::AccessFlags::ACCELERATION_STRUCTURE_READ_KHR,
        );
    })?;
    Ok(acceleration)
}
pub(super) fn scratch_alignment(dev: &Device, count: usize) -> Option<u64> {
    let mut props = vk::PhysicalDeviceAccelerationStructurePropertiesKHR::default();
    let mut query = vk::PhysicalDeviceProperties2::default().push_next(&mut props);
    // SAFETY: Device enabled the queried extension; the property chain and adapter are live.
    unsafe {
        dev.host
            .instance()
            .get_physical_device_properties2(dev.host.physical_device(), &mut query);
    }
    if count as u64 > props.max_primitive_count
        || props.max_geometry_count < 2
        || props.max_descriptor_set_acceleration_structures < 1
        || props.max_per_stage_descriptor_acceleration_structures < 1
        || props.max_instance_count < 1
    {
        return None;
    }
    // SAFETY: Format properties are queried on the same live adapter.
    let format = unsafe {
        dev.host.instance().get_physical_device_format_properties(
            dev.host.physical_device(),
            vk::Format::R32G32B32_SFLOAT,
        )
    };
    format
        .buffer_features
        .contains(vk::FormatFeatureFlags::ACCELERATION_STRUCTURE_VERTEX_BUFFER_KHR)
        .then_some(u64::from(
            props.min_acceleration_structure_scratch_offset_alignment,
        ))
}
pub(super) fn scene(
    dev: &Arc<Device>,
    commands: &mut Commands,
    scene: &crate::BakeScene,
    indices: &[usize],
    alignment: u64,
) -> Result<(Acceleration, Acceleration)> {
    let opaque = indices.partition_point(|&i| {
        scene.materials[scene.triangles[i].material]
            .alpha_cutoff
            .is_none()
    });
    let mut data = Vec::with_capacity(indices.len() * 36);
    for &i in indices {
        for p in scene.triangles[i].positions {
            for x in p {
                data.extend(x.to_le_bytes());
            }
        }
    }
    let input =
        Usage::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR | Usage::SHADER_DEVICE_ADDRESS;
    let mut vertices = Buffer::new(
        dev,
        (data.len() as u64).max(12),
        input,
        Memory::HOST_ACCESS_SEQUENTIAL_WRITE,
    )?;
    vertices.write(&data)?;
    drop(data);
    let ranges = [(0, opaque), (opaque, indices.len() - opaque)];
    let geometry = ranges.map(|(start, n)| {
        let triangles = vk::AccelerationStructureGeometryTrianglesDataKHR::default()
            .vertex_format(vk::Format::R32G32B32_SFLOAT)
            .vertex_data(vk::DeviceOrHostAddressConstKHR {
                device_address: vertices.address() + if n == 0 { 0 } else { start as u64 * 36 },
            })
            .vertex_stride(12)
            .max_vertex((3 * n).saturating_sub(1) as u32)
            .index_type(vk::IndexType::NONE_KHR);
        vk::AccelerationStructureGeometryKHR::default()
            .geometry_type(vk::GeometryTypeKHR::TRIANGLES)
            .geometry(vk::AccelerationStructureGeometryDataKHR { triangles })
    });
    let mut geometry = geometry;
    geometry[0].flags = vk::GeometryFlagsKHR::OPAQUE;
    let blas = build(
        dev,
        commands,
        vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
        &geometry,
        &ranges.map(|(_, n)| n as u32),
        alignment,
    )?;
    // VkAccelerationStructureInstanceKHR wire layout: row-major identity, mask, flags, BLAS address.
    let mut instance = Vec::with_capacity(64);
    for x in [1f32, 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0.] {
        instance.extend(x.to_le_bytes());
    }
    instance.extend(0xff000000u32.to_le_bytes());
    instance.extend(0u32.to_le_bytes());
    // SAFETY: BLAS build completed; the address stays valid while retained by the scene.
    let address = unsafe {
        dev.acceleration.get_acceleration_structure_device_address(
            &vk::AccelerationStructureDeviceAddressInfoKHR::default()
                .acceleration_structure(blas.handle),
        )
    };
    instance.extend(address.to_le_bytes());
    let mut instances = Buffer::new(dev, 64, input, Memory::HOST_ACCESS_SEQUENTIAL_WRITE)?;
    instances.write(&instance)?;
    let geom = vk::AccelerationStructureGeometryKHR::default()
        .geometry_type(vk::GeometryTypeKHR::INSTANCES)
        .geometry(vk::AccelerationStructureGeometryDataKHR {
            instances: vk::AccelerationStructureGeometryInstancesDataKHR::default().data(
                vk::DeviceOrHostAddressConstKHR {
                    device_address: instances.address(),
                },
            ),
        });
    let tlas = build(
        dev,
        commands,
        vk::AccelerationStructureTypeKHR::TOP_LEVEL,
        &[geom],
        &[1],
        alignment,
    )?;
    Ok((tlas, blas))
}
