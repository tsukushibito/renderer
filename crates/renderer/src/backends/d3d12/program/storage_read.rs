//! Test-only two-dispatch StorageWrite -> StorageRead execution.

use super::fixture::{ExecutionLease, FixtureRecording};
use super::*;

/// Immutable prepare-time connection between the two reviewed shader assets.
/// Their exact 16-byte write/read footprints are not inferred from min_size.
pub(in super::super) struct StorageReadFixture<'a> {
    producer: &'a D3D12Bindings,
    reader: &'a D3D12Bindings,
    input_resource: &'a BoundResource,
    output_resource: &'a BoundResource,
    input: &'a D3D12Buffer,
    output: &'a D3D12Buffer,
    output_range: BufferRange,
}

fn buffer_slot(
    bindings: &D3D12Bindings,
    kind: BindingType,
) -> Result<(&BoundResource, &D3D12Buffer, BufferRange), D3D12RuntimeError> {
    let index = bindings
        .layout()
        .iter()
        .position(|slot| slot.kind == kind)
        .ok_or_else(|| {
            D3D12RuntimeError::invalid("storage fixture Program is missing a required buffer kind")
        })?;
    let resource = &bindings.0.resources[index];
    let BoundResource::Buffer { _buffer, _range } = resource else {
        unreachable!("binding creation established buffer kinds")
    };
    Ok((resource, _buffer, *_range))
}

impl<'a> StorageReadFixture<'a> {
    pub(in super::super) fn new(
        owner: &Arc<DeviceOwner>,
        producer: &'a D3D12Bindings,
        reader: &'a D3D12Bindings,
    ) -> Result<Self, D3D12RuntimeError> {
        producer.check_owner(owner)?;
        reader.check_owner(owner)?;
        if !producer
            .0
            ._program
            .0
            .managed
            .same_asset(&ManagedProgram::uniform_readback())
            || !reader
                .0
                ._program
                .0
                .managed
                .same_asset(&ManagedProgram::storage_readback())
        {
            return Err(D3D12RuntimeError::invalid(
                "storage chain requires the reviewed uniform producer and storage reader assets",
            ));
        }
        let (_, produced, produced_range) = buffer_slot(producer, BindingType::StorageWrite)?;
        let (input_resource, input, input_range) = buffer_slot(reader, BindingType::StorageRead)?;
        let (output_resource, output, output_range) =
            buffer_slot(reader, BindingType::StorageWrite)?;
        if !Arc::ptr_eq(&produced.0, &input.0) || produced_range.offset != input_range.offset {
            return Err(D3D12RuntimeError::invalid(
                "StorageRead input must refer to the producer's actual initialized 16-byte footprint",
            ));
        }
        // A buffer has one legacy state. Read-only SRV and writeable UAV states
        // cannot coexist, even when logical ranges do not overlap.
        if Arc::ptr_eq(&input.0, &output.0) {
            return Err(D3D12RuntimeError::invalid(
                "storage chain input and output must be distinct physical buffers",
            ));
        }
        Ok(Self {
            producer,
            reader,
            input_resource,
            output_resource,
            input,
            output,
            output_range,
        })
    }

    pub(in super::super) fn record(
        &self,
        list: &ID3D12GraphicsCommandList,
    ) -> Result<FixtureRecording, D3D12RuntimeError> {
        // The intermediate is producer output and reader input; reserve it once
        // across both dispatches, then reserve the final output independently.
        let leases = vec![
            ExecutionLease::acquire(self.input_resource)?,
            ExecutionLease::acquire(self.output_resource)?,
        ];
        let owner = &self.reader.0._program.0.owner;
        let readback = DeviceBuffer::new(owner, 16, D3D12_HEAP_TYPE_READBACK)?;
        let mut input_state = D3D12_RESOURCE_STATE_COMMON;
        let mut output_state = D3D12_RESOURCE_STATE_COMMON;
        super::super::graph::transition(
            list,
            self.input.0._native.raw(),
            &mut input_state,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        );
        // SAFETY: Prepare selected the reviewed uniform shader and its immutable
        // 16-byte UBO input. The destination is exclusively leased in UAV state.
        unsafe { self.producer.record_dispatch(list) };
        // This transition provides execution and memory ordering for GPU writes
        // consumed by the second dispatch. No CPU wait/readback lies between.
        super::super::graph::transition(
            list,
            self.input.0._native.raw(),
            &mut input_state,
            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
        );
        super::super::graph::transition(
            list,
            self.output.0._native.raw(),
            &mut output_state,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        );
        // SAFETY: Prepare connected exactly the producer's initialized 16 bytes
        // to this reviewed Load4 shader. SRV/UAV resources are distinct and live,
        // and switching dispatches records the reader's PSO/root signature/args.
        unsafe { self.reader.record_dispatch(list) };
        super::super::graph::transition(
            list,
            self.input.0._native.raw(),
            &mut input_state,
            D3D12_RESOURCE_STATE_COMMON,
        );
        super::super::graph::transition(
            list,
            self.output.0._native.raw(),
            &mut output_state,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        // SAFETY: The second dispatch initialized these exact 16 bytes and its
        // transition makes the writes visible to this retained readback copy.
        unsafe {
            list.CopyBufferRegion(
                readback.raw(),
                0,
                self.output.0._native.raw(),
                self.output_range.offset,
                16,
            )
        };
        super::super::graph::transition(
            list,
            self.output.0._native.raw(),
            &mut output_state,
            D3D12_RESOURCE_STATE_COMMON,
        );
        Ok(FixtureRecording {
            readback,
            size: 16,
            staging: vec![],
            depth_targets: vec![],
            leases,
            bindings: vec![self.producer.clone(), self.reader.clone()],
        })
    }
}
