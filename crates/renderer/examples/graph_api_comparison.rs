//! CPU-only API design study: these types are not Renderer APIs or a GPU backend.
//! Run `cargo run --example graph_api_comparison --no-default-features`.

#[path = "support/graph_api_comparison.rs"]
mod study;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    study::compare()?;
    Ok(())
}
