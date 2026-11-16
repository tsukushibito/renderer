//! Allocation-free runtime capacity details shared by native backends.
//!
//! Capacity failures do not imply that waiting for the GPU will help. Inspect
//! [`RuntimeCapacityError::recovery`] before retrying the same request.

/// The prepared runtime resource that prevented an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuntimeCapacityKind {
    WorkingSet,
    Programs,
    ExecutionSlots,
    FrameStorage,
    ResultSlots,
    ExportMetadata,
    ImportRanges,
    Resources,
    StagingBytes,
    StagingRanges,
    TransferBytes,
    UploadBytes,
    ReadbackBytes,
    GraphMetadata,
    Descriptors,
    Samplers,
    TimingSamples,
    PresentationSlots,
    RetiredSwapchains,
    HostMemory,
    Identity,
    DiagnosticStorage,
}

/// The action needed before retrying a runtime capacity failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CapacityRecovery {
    /// Prepare the missing working set, Program, or descriptor layout.
    Prepare,
    /// Change a configured bound at an explicit preparation boundary.
    /// Repeating the same request or polling alone cannot increase the bound.
    /// For `HostMemory`, revisit the reservation request and available memory;
    /// increasing a configured bound does not guarantee allocation success.
    Reconfigure,
    /// Recreate the exhausted resource, Presenter, or runtime at an explicit
    /// change boundary. Monotonic generations/revisions must not wrap or reset
    /// while existing owners remain valid.
    Recreate,
    /// Process-wide callback identity space is exhausted. Recreating only the
    /// current runtime cannot restore it; restart the process.
    RestartProcess,
    /// Poll accepted GPU/presentation work and return completed work owners.
    /// This is backpressure, not a promise that the next poll will succeed.
    Poll,
    /// Consume or drop retained CPU results or timing samples; GPU completion
    /// alone does not release their capacity.
    ReleaseResults,
    /// Return retained resource, export, snapshot, or recording owners.
    /// Polling may retire GPU owners but cannot release caller-held owners.
    ReleaseOwners,
}

/// Copyable, native-independent details. Counts use the units of `resource`:
/// bytes for byte capacities and element counts for slots/metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCapacityError {
    pub resource: RuntimeCapacityKind,
    pub recovery: CapacityRecovery,
    /// The configured limit, when the failing operation has one available.
    pub limit: Option<u64>,
    /// The total minimum required by this request, when representable.
    pub required: Option<u64>,
}

impl RuntimeCapacityError {
    /// Construct scalar details, for example for an application capacity report.
    pub const fn new(
        resource: RuntimeCapacityKind,
        recovery: CapacityRecovery,
        limit: Option<u64>,
        required: Option<u64>,
    ) -> Self {
        Self {
            resource,
            recovery,
            limit,
            required,
        }
    }
    #[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
    pub(crate) fn simultaneous_slots(mut self, required: u64) -> Self {
        if self.recovery == CapacityRecovery::ReleaseOwners
            && self.limit.is_some_and(|limit| required > limit)
        {
            self.recovery = CapacityRecovery::Reconfigure;
            self.required = Some(required);
        }
        self
    }
}

impl std::fmt::Display for RuntimeCapacityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} capacity unavailable", self.resource)?;
        if let Some(required) = self.required {
            write!(f, "; required {required}")?;
        }
        if let Some(limit) = self.limit {
            write!(f, "; limit {limit}")?;
        }
        write!(f, "; recovery {:?}", self.recovery)
    }
}
impl std::error::Error for RuntimeCapacityError {}

#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
pub(crate) fn import_ranges_capacity(error: &crate::graph::GraphError) -> RuntimeCapacityError {
    use crate::graph::GraphCapacityKind;
    let capacity = error.capacity();
    RuntimeCapacityError::new(
        RuntimeCapacityKind::ImportRanges,
        if capacity.is_some_and(|capacity| {
            capacity.kind == GraphCapacityKind::ImportRangeSlots && capacity.limit != 0
        }) {
            CapacityRecovery::ReleaseOwners
        } else {
            CapacityRecovery::Reconfigure
        },
        capacity.map(|capacity| capacity.limit as u64),
        capacity.and_then(|capacity| capacity.required.map(|required| required as u64)),
    )
}

#[cfg(all(test, any(feature = "vulkan", all(feature = "d3d12", windows))))]
mod tests {
    use super::*;
    use crate::graph::ImportRangesPool;

    #[test]
    fn import_range_limits_and_retained_slots_keep_distinct_recovery() {
        let mut pool = ImportRangesPool::new(1, 1).unwrap();
        let token = pool.capture(&[]).unwrap();
        let error = pool.capture(&[]).unwrap_err();
        assert_eq!(
            import_ranges_capacity(&error),
            RuntimeCapacityError::new(
                RuntimeCapacityKind::ImportRanges,
                CapacityRecovery::ReleaseOwners,
                Some(1),
                Some(2)
            )
        );
        assert_eq!(
            import_ranges_capacity(&error)
                .simultaneous_slots(2)
                .recovery,
            CapacityRecovery::Reconfigure
        );
        drop(token);
        let error = pool.capture(&[0..4, 8..12]).unwrap_err();
        assert_eq!(
            import_ranges_capacity(&error),
            RuntimeCapacityError::new(
                RuntimeCapacityKind::ImportRanges,
                CapacityRecovery::Reconfigure,
                Some(1),
                Some(2)
            )
        );
    }
}
