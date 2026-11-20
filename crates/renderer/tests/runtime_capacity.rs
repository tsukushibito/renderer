use renderer::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};

#[test]
fn capacity_reports_are_native_independent_and_preserve_recovery_and_byte_counts() {
    let capacity = RuntimeCapacityError::new(
        RuntimeCapacityKind::ReadbackBytes,
        CapacityRecovery::Reconfigure,
        Some(16_777_216),
        Some(19_814_400),
    );
    let error: Box<dyn std::error::Error> = Box::new(capacity);
    assert_eq!(
        error.downcast_ref::<RuntimeCapacityError>(),
        Some(&capacity)
    );
    assert!(error.to_string().contains("19814400"));
    assert!(error.to_string().contains("16777216"));
    #[cfg(feature = "vulkan")]
    {
        let _: fn(&renderer::vulkan::RuntimeError) -> Option<RuntimeCapacityError> =
            renderer::vulkan::RuntimeError::capacity;
    }
    #[cfg(all(feature = "d3d12", windows))]
    {
        let _: fn(&renderer::d3d12::D3D12RuntimeError) -> Option<RuntimeCapacityError> =
            renderer::d3d12::D3D12RuntimeError::capacity;
    }
}
