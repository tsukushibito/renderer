//! Private Vulkan bootstrap, memory ownership, and single-queue transfers.

// Compile the implementation in normal Vulkan builds while its runtime caller
// is still pending; do not expose native APIs merely to suppress dead code.
#![cfg_attr(
    not(test),
    allow(dead_code, reason = "private bootstrap awaits runtime integration")
)]

mod buffer;
mod device;
mod diagnostics;
mod image;
mod instance;
mod memory;
mod ranges;
mod transfer;

#[cfg(test)]
fn real_vulkan_dispatch() -> tracing::Dispatch {
    let filter = match std::env::var("RUST_LOG") {
        Ok(filter) => filter,
        Err(std::env::VarError::NotPresent) => {
            "renderer=info,renderer::vulkan::validation=warn".to_owned()
        }
        Err(error) => panic!("invalid RUST_LOG: {error}"),
    };
    tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_new(filter).expect("valid RUST_LOG filter"),
            )
            .with_test_writer()
            .with_ansi(false)
            .without_time()
            .finish(),
    )
}

#[cfg(test)]
fn assert_real_validation(diagnostics: &diagnostics::Diagnostics) {
    // External configuration can override API requests. Check the documented
    // SDK's effective settings before accepting results, including standalone
    // image tests where disabled core checks would invalidate a clean report.
    let status = diagnostics.snapshot().messages.into_iter().find(|message|
        message.message_id_name.as_deref() == Some("WARNING-CreateInstance-status-message"))
        .expect("Validation Layer startup status must be available; unknown versions/configuration require review");
    let (_, enables) = status
        .message
        .split_once("Current Enables:")
        .expect("recognized Validation Layer startup enables report");
    let (enables, disables) = enables
        .split_once("Current Disables:")
        .expect("recognized Validation Layer startup disables report");
    assert!(
        enables.contains("VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT"),
        "Synchronization Validation must actually be enabled: {}",
        status.message
    );
    assert_eq!(
        disables
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .trim_end_matches('.'),
        "None",
        "core validation checks must remain enabled: {}",
        status.message
    );
    tracing::info!(target: instance::LOG_TARGET, validation_status = %status.message,
        "Validation Layer confirmed SyncVal enabled and no disabled checks");
}
