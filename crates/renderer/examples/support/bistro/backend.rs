//! Small acceptance adapter; all assets and Graph construction are API-neutral.

use super::{Error, Options, pipeline::Budget};
use renderer::graph::{
    BufferHandle, CompiledGraph, ImageHandle, ImportBuffer, ImportImage, PassHandle, ResourceKey,
};
use std::time::{Duration, Instant};

pub(super) trait Backend: Sized {
    type Pending;
    type Completed;
    fn create(options: &Options, budget: &Budget) -> Result<Self, Error>;
    fn info(&self) -> serde_json::Value;
    fn submit(&mut self, graph: &CompiledGraph) -> Result<Self::Pending, Error>;
    fn poll(pending: &mut Self::Pending) -> Result<Option<Self::Completed>, Error>;
    fn export_buffer(pending: &Self::Pending, buffer: BufferHandle) -> Result<ImportBuffer, Error>;
    fn export_image(pending: &Self::Pending, image: ImageHandle) -> Result<ImportImage, Error>;
    fn delete(&mut self, key: ResourceKey) -> Result<(), Error>;
    fn metrics(completed: &Self::Completed) -> Option<renderer::SubmissionMetrics>;
    fn readback(
        completed: &mut Self::Completed,
        pass: PassHandle,
        bytes: &mut [u8],
    ) -> Result<(), Error>;
    fn set_profiling(&mut self, enabled: bool) -> Result<(), Error>;
    fn close(self) -> Result<serde_json::Value, Error>;
    fn wait(pending: &mut Self::Pending, timeout: Duration) -> Result<Self::Completed, Error> {
        let start = Instant::now();
        loop {
            if let Some(completed) = Self::poll(pending)? {
                return Ok(completed);
            }
            if start.elapsed() > timeout {
                return Err("Bistro completion timed out".into());
            }
            std::thread::yield_now();
        }
    }
}

#[cfg(feature = "vulkan")]
pub(super) struct Vulkan {
    runtime: renderer::VulkanRuntime,
}
#[cfg(feature = "vulkan")]
impl Backend for Vulkan {
    type Pending = renderer::Submission;
    type Completed = renderer::SubmissionResult;
    fn create(options: &Options, budget: &Budget) -> Result<Self, Error> {
        let mut runtime = renderer::VulkanRuntime::new(renderer::RuntimeConfig {
            execution_slots: options.inflight,
            resource_capacity: budget.physical_resources,
            staging_bytes: (budget.upload_bytes as u64).max(budget.readback_bytes) + 8192,
            staging_ranges: 64 + options.inflight * 4,
            max_shader_bindings: budget.graph.draws as u32,
        })?;
        if runtime.device_info().device_type == "CPU" {
            return Err("CPU Vulkan adapters are not accepted".into());
        }
        let resources = budget
            .buffers
            .iter()
            .map(|&(size, count)| renderer::ResourceClass::Buffer {
                desc: renderer::graph::BufferDesc { size },
                count,
            })
            .chain(
                budget
                    .images
                    .iter()
                    .map(|&(desc, count)| renderer::ResourceClass::Image { desc, count }),
            )
            .collect::<Vec<_>>();
        runtime.prepare_working_set(
            renderer::FrameCapacity {
                graph: budget.graph,
                work_slots: options.inflight,
                result_slots: options.inflight + 1,
                metadata_slots: options.inflight + 1,
                range_snapshots: budget.ranges,
                initialized_ranges_per_resource: 32,
                provenance_segments_per_resource: 1,
            },
            &resources,
        )?;
        runtime.prepare_raster_programs(&super::program::programs())?;
        Ok(Self { runtime })
    }
    fn info(&self) -> serde_json::Value {
        let info = self.runtime.device_info();
        serde_json::json!({"api":"vulkan","name":info.name,"device_type":info.device_type,"vendor_id":info.vendor_id,
            "device_id":info.device_id,"device_luid":info.device_luid,"api_version":info.api_version,
            "driver_version_raw":info.driver_version_raw,"driver_name":info.driver_name,"driver_info":info.driver_info,
            "validation_mode":if cfg!(feature="development-validation"){"strict"}else{"disabled"}})
    }
    fn submit(&mut self, graph: &CompiledGraph) -> Result<Self::Pending, Error> {
        Ok(self.runtime.submit(graph)?)
    }
    fn poll(pending: &mut Self::Pending) -> Result<Option<Self::Completed>, Error> {
        Ok(pending.wait(Duration::ZERO)?)
    }
    fn export_buffer(pending: &Self::Pending, buffer: BufferHandle) -> Result<ImportBuffer, Error> {
        Ok(pending.export_buffer(buffer)?)
    }
    fn export_image(pending: &Self::Pending, image: ImageHandle) -> Result<ImportImage, Error> {
        Ok(pending.export_image(image)?)
    }
    fn delete(&mut self, key: ResourceKey) -> Result<(), Error> {
        Ok(self.runtime.delete(key)?)
    }
    fn metrics(completed: &Self::Completed) -> Option<renderer::SubmissionMetrics> {
        completed.metrics().copied()
    }
    fn readback(
        completed: &mut Self::Completed,
        pass: PassHandle,
        bytes: &mut [u8],
    ) -> Result<(), Error> {
        if completed.readback_len(pass)? != bytes.len() {
            return Err("capture readback length mismatch".into());
        }
        Ok(completed.readback_into(pass, bytes)?)
    }
    fn set_profiling(&mut self, enabled: bool) -> Result<(), Error> {
        Ok(self.runtime.set_profiling(enabled)?)
    }
    fn close(self) -> Result<serde_json::Value, Error> {
        let monitor = self.runtime.validation_monitor();
        drop(self);
        let report = monitor.report();
        if !report.is_clean() {
            return Err(format!("Vulkan validation: {report:#?}").into());
        }
        let strict = cfg!(feature = "development-validation");
        if strict
            && (report.core_validation != Some(true)
                || report.synchronization_validation != Some(true))
        {
            return Err(format!("Vulkan effective validation not verified: {report:#?}").into());
        }
        Ok(
            serde_json::json!({"clean":true,"mode":if strict{"strict"}else{"disabled"},
            "requested":format!("{:?}",report.requested),"core_validation":report.core_validation,
            "synchronization_validation":report.synchronization_validation,"warnings":report.warnings,
            "errors":report.errors,"callback_failures":report.callback_failures}),
        )
    }
}

#[cfg(all(windows, feature = "d3d12"))]
pub(super) struct D3d12 {
    runtime: renderer::D3D12Runtime,
}
#[cfg(all(windows, feature = "d3d12"))]
impl Backend for D3d12 {
    type Pending = renderer::D3D12Submission;
    type Completed = renderer::D3D12SubmissionResult;
    fn create(options: &Options, budget: &Budget) -> Result<Self, Error> {
        let capacity = renderer::D3D12FrameCapacity {
            executions: options.inflight,
            results: options.inflight + 1,
            resources: budget.graph.resources,
            resource_capacity: budget.physical_resources,
            operations: budget.graph.draws + 16,
            provenance_segments_per_resource: 1,
            initialized_ranges_per_resource: 32,
            range_snapshots: budget.ranges,
            buffers: budget.buffer_count,
            buffer_bytes: budget.max_buffer_bytes,
            upload_bytes: budget.upload_bytes as u64,
            readback_bytes: budget.readback_bytes,
            scratch_bytes: 16,
        };
        let buffers = budget
            .buffers
            .iter()
            .map(|&(size, count)| renderer::D3D12BufferClass { size, count })
            .collect::<Vec<_>>();
        let images = budget
            .images
            .iter()
            .map(|&(desc, count)| renderer::D3D12ImageClass { desc, count })
            .collect::<Vec<_>>();
        let runtime = renderer::D3D12Runtime::new_with_working_set(
            renderer::D3D12RuntimeConfig {
                capacity,
                ..Default::default()
            },
            &buffers,
            &images,
            &super::program::programs(),
            &[],
        )?;
        Ok(Self { runtime })
    }
    fn info(&self) -> serde_json::Value {
        let info = self.runtime.device_info();
        serde_json::json!({"api":"d3d12","name":info.name,"vendor_id":info.vendor_id,"device_id":info.device_id,
            "device_luid":info.luid,"driver_version":info.driver_version,
            "validation_mode":if cfg!(feature="development-validation"){"strict"}else{"disabled"}})
    }
    fn submit(&mut self, graph: &CompiledGraph) -> Result<Self::Pending, Error> {
        Ok(self.runtime.submit(graph)?)
    }
    fn poll(pending: &mut Self::Pending) -> Result<Option<Self::Completed>, Error> {
        Ok(pending.poll()?)
    }
    fn export_buffer(pending: &Self::Pending, buffer: BufferHandle) -> Result<ImportBuffer, Error> {
        Ok(pending.export_buffer(buffer)?)
    }
    fn export_image(pending: &Self::Pending, image: ImageHandle) -> Result<ImportImage, Error> {
        Ok(pending.export_image(image)?)
    }
    fn delete(&mut self, key: ResourceKey) -> Result<(), Error> {
        Ok(self.runtime.delete(key)?)
    }
    fn metrics(completed: &Self::Completed) -> Option<renderer::SubmissionMetrics> {
        completed.metrics().copied()
    }
    fn readback(
        completed: &mut Self::Completed,
        pass: PassHandle,
        bytes: &mut [u8],
    ) -> Result<(), Error> {
        if completed.readback_len(pass)? != bytes.len() {
            return Err("capture readback length mismatch".into());
        }
        Ok(completed.readback_into(pass, bytes)?)
    }
    fn set_profiling(&mut self, enabled: bool) -> Result<(), Error> {
        Ok(self.runtime.set_profiling(enabled)?)
    }
    fn close(self) -> Result<serde_json::Value, Error> {
        let report = self.runtime.shutdown()?;
        let strict = cfg!(feature = "development-validation");
        if !report.is_clean()
            || report.debug_layer_enabled != strict
            || report.gpu_based_validation_enabled != strict
            || (strict && !report.teardown_checked)
        {
            return Err(format!("D3D12 validation: {report:#?}").into());
        }
        Ok(
            serde_json::json!({"clean":true,"mode":if strict{"strict"}else{"disabled"},
            "debug_layer_enabled":report.debug_layer_enabled,"gpu_based_validation_enabled":report.gpu_based_validation_enabled,
            "teardown_checked":report.teardown_checked,"warnings":report.warnings,"errors":report.errors,
            "corruption":report.corruption,"callback_failures":report.callback_failures}),
        )
    }
}
