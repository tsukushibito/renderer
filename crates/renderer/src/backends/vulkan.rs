//! Private Vulkan bootstrap; command execution follows separately.

// Compile the implementation in normal Vulkan builds while its runtime caller
// is still pending; do not expose native APIs merely to suppress dead code.
#![cfg_attr(
    not(test),
    allow(dead_code, reason = "private bootstrap awaits runtime integration")
)]

mod device;
mod diagnostics;
mod instance;

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
