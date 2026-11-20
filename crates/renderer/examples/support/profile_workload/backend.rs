use super::{BYTES, CompiledGraph, Error, Options, PassHandle};

pub(super) trait Backend: Sized {
    type Pending;
    type Completed;
    fn create(options: &Options) -> Result<Self, Error>;
    fn info(&self) -> serde_json::Value;
    fn submit(&mut self, plan: &CompiledGraph) -> Result<Self::Pending, Error>;
    fn poll(pending: &mut Self::Pending) -> Result<Option<Self::Completed>, Error>;
    fn readback(
        result: &mut Self::Completed,
        pass: PassHandle,
        bytes: &mut [u8; BYTES],
    ) -> Result<(), Error>;
    fn close(self) -> Result<serde_json::Value, Error>;
}

#[cfg(feature = "vulkan")]
pub(super) struct Vulkan {
    runtime: renderer::vulkan::VulkanRuntime,
}
#[cfg(feature = "vulkan")]
impl Backend for Vulkan {
    type Pending = renderer::vulkan::Submission;
    type Completed = renderer::vulkan::SubmissionResult;
    fn create(options: &Options) -> Result<Self, Error> {
        let mut runtime = renderer::vulkan::VulkanRuntime::new(renderer::vulkan::RuntimeConfig {
            execution_slots: 1,
            resource_capacity: options.resources,
            staging_bytes: 16 * 1024,
            staging_ranges: 1,
            max_shader_bindings: 1,
        })?;
        if runtime.device_info().device_type == "CPU" {
            return Err("CPU Vulkan adapter is not accepted".into());
        }
        runtime.prepare_working_set(
            renderer::vulkan::FrameCapacity {
                graph: super::capacity(options.resources)?,
                work_slots: 1,
                result_slots: 1,
                metadata_slots: 1,
                range_snapshots: 1,
                initialized_ranges_per_resource: 1,
                provenance_segments_per_resource: 0,
                index_prefixes_per_resource: 0,
            },
            &[renderer::vulkan::ResourceClass::Buffer {
                desc: renderer::graph::BufferDesc { size: BYTES as u64 },
                count: options.resources,
            }],
        )?;
        Ok(Self { runtime })
    }
    fn info(&self) -> serde_json::Value {
        let info = self.runtime.device_info();
        serde_json::json!({ "name": info.name, "device_type": info.device_type,
            "vendor_id": info.vendor_id, "device_id": info.device_id,
            "device_luid": info.device_luid, "api_version": info.api_version,
            "driver_version_raw": info.driver_version_raw, "driver_name": info.driver_name,
            "driver_info": info.driver_info })
    }
    fn submit(&mut self, plan: &CompiledGraph) -> Result<Self::Pending, Error> {
        Ok(self.runtime.submit(plan)?)
    }
    fn poll(pending: &mut Self::Pending) -> Result<Option<Self::Completed>, Error> {
        Ok(pending.wait(std::time::Duration::ZERO)?)
    }
    fn readback(
        result: &mut Self::Completed,
        pass: PassHandle,
        bytes: &mut [u8; BYTES],
    ) -> Result<(), Error> {
        if result.readback_len(pass)? != BYTES {
            return Err("unexpected readback length".into());
        }
        result.readback_into(pass, bytes)?;
        Ok(())
    }
    fn close(self) -> Result<serde_json::Value, Error> {
        let monitor = self.runtime.validation_monitor();
        drop(self);
        let report = monitor.report();
        if !report.is_clean() {
            return Err(format!("Vulkan validation: {report:#?}").into());
        }
        if cfg!(feature = "development-validation")
            && (report.core_validation != Some(true)
                || report.synchronization_validation != Some(true))
        {
            return Err(format!("Vulkan effective validation unverified: {report:#?}").into());
        }
        Ok(
            serde_json::json!({ "clean": true, "requested": format!("{:?}", report.requested),
            "core_validation": report.core_validation, "synchronization_validation": report.synchronization_validation,
            "warnings": report.warnings, "errors": report.errors, "callback_failures": report.callback_failures }),
        )
    }
}

#[cfg(feature = "d3d12")]
pub(super) struct D3d12 {
    runtime: renderer::d3d12::D3D12Runtime,
}
#[cfg(feature = "d3d12")]
impl Backend for D3d12 {
    type Pending = renderer::d3d12::D3D12Submission;
    type Completed = renderer::d3d12::D3D12SubmissionResult;
    fn create(options: &Options) -> Result<Self, Error> {
        let config = renderer::d3d12::D3D12RuntimeConfig {
            capacity: renderer::d3d12::D3D12FrameCapacity {
                executions: 1,
                results: 1,
                resources: options.resources,
                resource_capacity: options.resources,
                operations: options
                    .resources
                    .checked_add(1)
                    .ok_or("operation capacity overflow")?,
                provenance_segments_per_resource: 0,
                index_prefixes_per_resource: 0,
                initialized_ranges_per_resource: 1,
                range_snapshots: 1,
                buffers: options.resources,
                buffer_bytes: BYTES as u64,
                upload_bytes: BYTES as u64,
                readback_bytes: BYTES as u64,
                scratch_bytes: BYTES as u64,
            },
            ..Default::default()
        };
        Ok(Self {
            runtime: renderer::d3d12::D3D12Runtime::new(config)?,
        })
    }
    fn info(&self) -> serde_json::Value {
        let info = self.runtime.device_info();
        serde_json::json!({ "name": info.name, "device_luid": info.luid,
            "vendor_id": info.vendor_id, "device_id": info.device_id, "driver_version": info.driver_version })
    }
    fn submit(&mut self, plan: &CompiledGraph) -> Result<Self::Pending, Error> {
        Ok(self.runtime.submit(plan)?)
    }
    fn poll(pending: &mut Self::Pending) -> Result<Option<Self::Completed>, Error> {
        Ok(pending.poll()?)
    }
    fn readback(
        result: &mut Self::Completed,
        pass: PassHandle,
        bytes: &mut [u8; BYTES],
    ) -> Result<(), Error> {
        if result.readback_len(pass)? != BYTES {
            return Err("unexpected readback length".into());
        }
        result.readback_into(pass, bytes)?;
        Ok(())
    }
    fn close(self) -> Result<serde_json::Value, Error> {
        let report = self.runtime.shutdown()?;
        if !report.is_clean() {
            return Err(format!("D3D12 validation: {report:#?}").into());
        }
        let strict = cfg!(feature = "development-validation");
        if report.debug_layer_enabled != strict
            || report.gpu_based_validation_enabled != strict
            || (strict && !report.teardown_checked)
        {
            return Err(format!("D3D12 effective validation unverified: {report:#?}").into());
        }
        Ok(
            serde_json::json!({ "clean": true, "debug_layer_enabled": report.debug_layer_enabled,
            "gpu_based_validation_enabled": report.gpu_based_validation_enabled,
            "teardown_checked": report.teardown_checked, "warnings": report.warnings,
            "errors": report.errors, "corruption": report.corruption, "callback_failures": report.callback_failures }),
        )
    }
}
