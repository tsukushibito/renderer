//! Prepared managed compute pipelines. One WebGPU pass per Graph dispatch
//! preserves the Graph's dispatch order and WebGPU resource usage scopes.
use std::ops::Range;

use super::{
    bindings::{BindGroups, ProgramLayout},
    engine::Resources,
};
use crate::{
    capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind},
    graph::CompiledGraph,
    program::{ManagedProgram, ProgramId},
    runtime::{ExecutionError, ExecutionErrorKind},
};

struct Pipeline {
    program: ManagedProgram,
    layout: ProgramLayout,
    native: wgpu::ComputePipeline,
}

pub(super) struct ComputePrograms {
    programs: Vec<Pipeline>,
    groups: BindGroups,
    max_workgroups: u32,
}

impl ComputePrograms {
    pub(super) fn new(
        device: &wgpu::Device,
        programs: &[ManagedProgram],
        cache_capacity: usize,
    ) -> Result<Self, ExecutionError> {
        let limits = device.limits();
        let mut prepared = Vec::with_capacity(programs.len());
        for program in programs {
            let [x, y, z] = program.workgroup_size();
            if x > limits.max_compute_workgroup_size_x
                || y > limits.max_compute_workgroup_size_y
                || z > limits.max_compute_workgroup_size_z
                || x.checked_mul(y)
                    .and_then(|xy| xy.checked_mul(z))
                    .is_none_or(|invocations| {
                        invocations == 0
                            || invocations > limits.max_compute_invocations_per_workgroup
                    })
            {
                return Err(ExecutionError::webgpu(
                    ExecutionErrorKind::Unsupported,
                    "WebGPU compute local size exceeds the device limit",
                ));
            }
            let source = program.wgsl().ok_or_else(|| {
                ExecutionError::webgpu(
                    ExecutionErrorKind::Unsupported,
                    "compute program has no WebGPU shader",
                )
            })?;
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: None,
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            let layout = ProgramLayout::new(device, program.bindings(), &[], &[], None)?;
            let native = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: Some(layout.pipeline()),
                module: &shader,
                entry_point: Some(program.entry_point()),
                compilation_options: Default::default(),
                cache: None,
            });
            prepared.push(Pipeline {
                program: program.clone(),
                layout,
                native,
            });
        }
        Ok(Self {
            programs: prepared,
            groups: BindGroups::new(cache_capacity)?,
            max_workgroups: limits.max_compute_workgroups_per_dimension,
        })
    }

    pub(super) fn clear_bind_groups(&mut self) {
        self.groups.clear();
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the Graph stores immutable dispatch operands and the recorder borrows active device resources"
    )]
    pub(super) fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        graph: &CompiledGraph,
        program: ProgramId,
        bindings: Range<usize>,
        workgroups: [u32; 3],
        resources: &Resources<'_>,
    ) -> Result<(), ExecutionError> {
        let pipeline = self
            .programs
            .iter()
            .find(|p| p.program.identity() == program)
            .ok_or_else(|| {
                ExecutionError::with_capacity(
                    "WebGPU compute program was not prepared",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Programs,
                        CapacityRecovery::Prepare,
                        None,
                        None,
                    ),
                )
            })?;
        if workgroups
            .into_iter()
            .any(|axis| axis > self.max_workgroups)
        {
            return Err(ExecutionError::invalid(
                "WebGPU dispatch exceeds the workgroup count limit",
            ));
        }
        let groups = self.groups.resolve(
            device,
            &pipeline.layout,
            graph.raster_bindings(&bindings),
            resources,
            None,
        )?;
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline.native);
        for (index, group) in groups.iter().enumerate() {
            if let Some(group) = group {
                pass.set_bind_group(index as u32, group, &[]);
            }
        }
        let [x, y, z] = workgroups;
        pass.dispatch_workgroups(x, y, z);
        Ok(())
    }
}
