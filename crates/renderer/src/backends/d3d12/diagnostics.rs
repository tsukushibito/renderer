#[cfg(not(feature = "d3d12-diagnostics"))]
use super::bindings::ID3D12Device;
use super::{D3D12RuntimeError, D3D12ValidationMode};

#[derive(Clone, Debug)]
pub struct D3D12ValidationMessage {
    pub source: &'static str,
    pub severity: i32,
    pub id: i32,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct D3D12ValidationReport {
    pub debug_layer_enabled: bool,
    pub gpu_based_validation_enabled: bool,
    pub teardown_checked: bool,
    pub warnings: u64,
    pub errors: u64,
    pub corruption: u64,
    pub callback_failures: u64,
    pub dropped_messages: u64,
    pub messages: Vec<D3D12ValidationMessage>,
}
impl D3D12ValidationReport {
    pub fn is_clean(&self) -> bool {
        self.warnings == 0
            && self.errors == 0
            && self.corruption == 0
            && self.callback_failures == 0
            && self.dropped_messages == 0
    }
}

#[cfg(feature = "d3d12-diagnostics")]
mod native;
#[cfg(feature = "d3d12-diagnostics")]
pub use native::D3D12ValidationMonitor;
#[cfg(feature = "d3d12-diagnostics")]
pub(super) use native::Session;

#[cfg(not(feature = "d3d12-diagnostics"))]
#[derive(Clone, Debug, Default)]
pub struct D3D12ValidationMonitor;
#[cfg(not(feature = "d3d12-diagnostics"))]
impl D3D12ValidationMonitor {
    pub(super) fn new() -> Self {
        Self
    }
    pub fn snapshot(&self) -> D3D12ValidationReport {
        D3D12ValidationReport::default()
    }
}
#[cfg(not(feature = "d3d12-diagnostics"))]
pub(super) struct Session;
#[cfg(not(feature = "d3d12-diagnostics"))]
impl Session {
    pub(super) fn begin(
        mode: D3D12ValidationMode,
        _monitor: D3D12ValidationMonitor,
        _first: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        if mode == D3D12ValidationMode::Required {
            return Err(D3D12RuntimeError::unsupported(
                "D3D12 native diagnostics were not compiled",
            ));
        }
        Ok(Self)
    }
    pub(super) fn attach(&mut self, _device: &ID3D12Device) -> Result<(), D3D12RuntimeError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clean_report_does_not_claim_effective_validation() {
        let report = D3D12ValidationReport::default();
        assert!(report.is_clean());
        assert!(!report.debug_layer_enabled || !report.gpu_based_validation_enabled);
        assert!(!report.teardown_checked);
    }
    #[cfg(not(feature = "d3d12-diagnostics"))]
    #[test]
    fn disabled_monitor_has_no_retained_diagnostic_state() {
        assert_eq!(size_of::<D3D12ValidationMonitor>(), 0);
        assert_eq!(size_of::<Session>(), 0);
    }
}
