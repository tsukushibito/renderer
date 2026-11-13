//! Native diagnostic state exists only when explicitly selected.

#[cfg(any(test, feature = "vulkan-diagnostics"))]
use ash::vk;

#[cfg(any(test, feature = "vulkan-diagnostics"))]
#[cfg_attr(
    not(feature = "vulkan-diagnostics"),
    allow(dead_code, reason = "test-only shape for native acceptance assertions")
)]
#[derive(Clone, Debug)]
pub(super) struct DiagnosticMessage {
    pub severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    #[cfg(feature = "vulkan-diagnostics")]
    pub message_types: vk::DebugUtilsMessageTypeFlagsEXT,
    pub message_id_number: i32,
    pub message_id_name: Option<String>,
    pub message: String,
}

/// Counts are independent of log filtering and bounded message retention.
/// Acceptance checks take this snapshot only after callback-producing operations
/// finish; separate counters do not form a coherent snapshot during callbacks.
#[cfg(any(test, feature = "vulkan-diagnostics"))]
#[cfg_attr(
    not(feature = "vulkan-diagnostics"),
    allow(dead_code, reason = "test-only shape for native acceptance assertions")
)]
#[derive(Debug, Default)]
pub(super) struct DiagnosticSnapshot {
    pub warnings: usize,
    pub errors: usize,
    pub callback_failures: usize,
    #[cfg(all(test, feature = "vulkan-diagnostics"))]
    pub messages_seen: usize,
    /// Acceptance tests observe recent INFO delivery independently of logging.
    #[cfg(test)]
    pub messages: Vec<DiagnosticMessage>,
    /// The first warning/error messages; verbose traffic cannot displace them.
    pub problems: Vec<DiagnosticMessage>,
    pub dropped_problems: usize,
    /// Distinct layer startup reports must survive later verbose traffic.
    pub layer_status_messages: Vec<String>,
    pub dropped_layer_statuses: usize,
}

#[cfg(test)]
impl DiagnosticSnapshot {
    pub fn is_clean(&self) -> bool {
        self.warnings == 0
            && self.errors == 0
            && self.callback_failures == 0
            && self.dropped_problems == 0
    }
}

#[cfg(feature = "vulkan-diagnostics")]
mod native;
#[cfg(feature = "vulkan-diagnostics")]
pub(super) use native::Diagnostics;
#[cfg(feature = "vulkan-diagnostics")]
pub(super) type DiagnosticsHandle = std::sync::Arc<Diagnostics>;
#[cfg(not(feature = "vulkan-diagnostics"))]
pub(super) type DiagnosticsHandle = Diagnostics;

/// Shipping configurations retain no callback state, strings or tracing Dispatch.
#[cfg(not(feature = "vulkan-diagnostics"))]
#[derive(Clone, Default)]
pub(super) struct Diagnostics;

#[cfg(not(feature = "vulkan-diagnostics"))]
impl Diagnostics {
    pub fn new() -> Self {
        Self
    }

    #[cfg(test)]
    pub fn snapshot(&self) -> DiagnosticSnapshot {
        DiagnosticSnapshot::default()
    }
}
