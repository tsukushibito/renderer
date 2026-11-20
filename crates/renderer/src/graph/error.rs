//! Classified, owned error context without retaining a Graph or native resource.

use std::{borrow::Cow, fmt};

use super::{AccessRange, PassHandle, ResourceHandle};

/// The prepared limit that prevented Graph construction or analysis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GraphCapacityKind {
    /// All prepared Graph slots are retained, or no slots were prepared.
    Slots,
    /// A snapshot's prepared input scratch is already connected.
    SnapshotInputs,
    Resources,
    Passes,
    Accesses,
    Draws,
    /// Packed inline constant words across all recorded Raster draws.
    RasterConstantWords,
    Bindings,
    Attachments,
    Dependencies,
    InitialRanges,
    NameBytes,
    UploadBytes,
    Edges,
    Segments,
    Readers,
    /// Initialized ranges in one [`super::ImportRangesPool`] snapshot.
    ImportRanges,
    /// Retained slots in an [`super::ImportRangesPool`].
    ImportRangeSlots,
}

/// Allocation-free details for capacity backpressure or a failed reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphCapacityError {
    pub kind: GraphCapacityKind,
    /// The configured limit; reservation failures may occur below this limit.
    pub limit: usize,
    /// The minimum required count observed at the failure, when available.
    /// `None` means analysis did not report a count or arithmetic overflowed.
    pub required: Option<usize>,
}

/// Native-independent categories for handling graph construction and analysis errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GraphErrorKind {
    /// A prepared pool or metadata buffer has no available capacity.
    CapacityExhausted,
    /// Descriptors, values, access purposes or ranges are incompatible or invalid.
    InvalidInput,
    /// A handle has the current owner but no matching declaration of its kind.
    InvalidHandle,
    /// A resource or pass handle belongs to another graph.
    WrongOwner,
    /// The requested operation or subresource granularity is not supported.
    Unsupported,
    /// Repeated imports disagree about the same canonical runtime resource.
    InconsistentImport,
    /// A read or export requires contents not established by earlier declarations.
    UninitializedRead,
    /// Data, hazard or explicit dependencies contain a directed cycle.
    DependencyCycle,
    /// Application-defined failure created with [`GraphError::custom`].
    Custom,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct GraphErrorContext {
    pass: Option<PassHandle>,
    resource: Option<ResourceHandle>,
    pass_name: Option<String>,
    resource_name: Option<String>,
    range: Option<AccessRange>,
    binding: Option<(u32, u32)>,
    cycle_passes: Vec<PassHandle>,
}

/// A classified error with optional owned graph context. Handles identify the
/// declaration involved; retaining an error does not retain its graph or GPU data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphError {
    pub message: Cow<'static, str>,
    kind: GraphErrorKind,
    context: Option<Box<GraphErrorContext>>,
    capacity: Option<GraphCapacityError>,
}
impl GraphError {
    /// Creates an application-defined error, for example from a graph extension.
    pub fn custom(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(GraphErrorKind::Custom, message)
    }

    /// Creates a validation error for an invalid graph-builder input.
    pub fn invalid_input(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(GraphErrorKind::InvalidInput, message)
    }

    pub fn kind(&self) -> GraphErrorKind {
        self.kind
    }
    /// Identifies which prepared limit to adjust, or which pool needs owners
    /// returned before retrying. These details never allocate or retain a Graph.
    pub fn capacity(&self) -> Option<GraphCapacityError> {
        self.capacity
    }
    /// An existing pass involved in the error. Failed builders have no pass yet.
    pub fn pass(&self) -> Option<PassHandle> {
        self.context.as_ref().and_then(|context| context.pass)
    }
    /// The resource involved, when known; this may itself be an invalid handle.
    pub fn resource(&self) -> Option<ResourceHandle> {
        self.context.as_ref().and_then(|context| context.resource)
    }
    /// The existing pass name, or the requested name of a rejected builder.
    pub fn pass_name(&self) -> Option<&str> {
        self.context.as_ref()?.pass_name.as_deref()
    }
    /// The known resource name. Foreign handles never resolve names in this graph.
    pub fn resource_name(&self) -> Option<&str> {
        self.context.as_ref()?.resource_name.as_deref()
    }
    /// The offending access or undefined contents, when a range is representable.
    pub fn range(&self) -> Option<&AccessRange> {
        self.context.as_ref()?.range.as_ref()
    }
    /// The Program's `(group, binding)` slot involved in a construction error.
    pub fn binding(&self) -> Option<(u32, u32)> {
        self.context.as_ref()?.binding
    }
    /// The directed cycle, including its repeated first pass at the end.
    /// Names may be duplicated; these handles identify the actual participants.
    pub fn cycle_passes(&self) -> &[PassHandle] {
        self.context
            .as_ref()
            .map_or(&[], |context| context.cycle_passes.as_slice())
    }

    pub(crate) fn new(kind: GraphErrorKind, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            message: message.into(),
            kind,
            context: None,
            capacity: None,
        }
    }
    /// Creates an allocation-free error for a prepared Graph construction
    /// limit, including a bound enforced by a caller's graph builder.
    pub fn capacity_error(kind: GraphCapacityKind, limit: usize, required: Option<usize>) -> Self {
        Self {
            capacity: Some(GraphCapacityError {
                kind,
                limit,
                required,
            }),
            ..Self::new(
                GraphErrorKind::CapacityExhausted,
                "prepared Graph capacity exhausted",
            )
        }
    }
    pub(super) fn with_cycle_passes(mut self, passes: Vec<PassHandle>) -> Self {
        self.context_mut().cycle_passes = passes;
        self
    }

    fn context_mut(&mut self) -> &mut GraphErrorContext {
        self.context.get_or_insert_with(Box::default)
    }
    pub(super) fn with_binding(mut self, group: u32, binding: u32) -> Self {
        if self.kind != GraphErrorKind::CapacityExhausted {
            self.context_mut().binding = Some((group, binding));
        }
        self
    }
    /// Adds the name of a pass being built for error reporting. Capacity
    /// errors remain unchanged so reporting them requires no allocation.
    pub fn with_pass_name(mut self, name: &str) -> Self {
        if self.kind == GraphErrorKind::CapacityExhausted {
            return self;
        }
        self.context_mut().pass_name = Some(name.into());
        self
    }
    pub(super) fn with_resource_name(mut self, name: &str) -> Self {
        if self.kind == GraphErrorKind::CapacityExhausted {
            return self;
        }
        self.context_mut().resource_name = Some(name.into());
        self
    }
    pub(super) fn with_pass(mut self, handle: PassHandle, name: Option<&str>) -> Self {
        if self.kind == GraphErrorKind::CapacityExhausted {
            return self;
        }
        let context = self.context_mut();
        context.pass = Some(handle);
        context.pass_name = name.map(str::to_owned);
        self
    }
    pub(crate) fn with_resource(mut self, handle: ResourceHandle, name: Option<&str>) -> Self {
        if self.kind == GraphErrorKind::CapacityExhausted {
            return self;
        }
        let context = self.context_mut();
        context.resource = Some(handle);
        context.resource_name = name.map(str::to_owned);
        self
    }
    pub(super) fn with_range(mut self, range: AccessRange) -> Self {
        if self.kind == GraphErrorKind::CapacityExhausted {
            return self;
        }
        self.context_mut().range = Some(range);
        self
    }
}
impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for GraphError {}
