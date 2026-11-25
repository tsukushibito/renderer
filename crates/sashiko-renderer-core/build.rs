use std::env;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(sashiko_webgpu)");
    println!("cargo::rustc-check-cfg=cfg(sashiko_gpu_backend)");
    let webgpu = env::var_os("CARGO_FEATURE_WEBGPU").is_some()
        && env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32")
        && env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("unknown");
    if webgpu {
        println!("cargo::rustc-cfg=sashiko_webgpu");
    }
    let vulkan = env::var_os("CARGO_FEATURE_VULKAN").is_some();
    let d3d12 = env::var_os("CARGO_FEATURE_D3D12").is_some()
        && env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if webgpu || vulkan || d3d12 {
        println!("cargo::rustc-cfg=sashiko_gpu_backend");
    }
}
