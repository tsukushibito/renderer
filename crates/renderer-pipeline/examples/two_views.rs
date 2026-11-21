//! Run the two_views example with a GPU backend feature and pass vulkan or d3d12.
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
#[path = "support/views.rs"]
mod views;

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    views::run(
        views::Example::TwoViews,
        |pipeline, graph, cameras, scene, _| {
            let mut colors = [None; 2];
            for (camera, slot) in cameras.iter_mut().zip(&mut colors) {
                let target =
                    graph.image("camera output", renderer::graph::ImageDesc::rgba8(64, 64))?;
                let view = pipeline.draw_view(graph, camera, scene, 1., Some(target))?;
                assert_eq!(view.sdr.raw_image(), target);
                *slot = Some(view.sdr);
            }
            Ok(colors)
        },
    )
}

#[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
fn main() {
    eprintln!("Enable the vulkan or d3d12 feature to run this example.");
    std::process::exit(2);
}
