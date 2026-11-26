#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    renderer_scenes::run_landscape()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
