#![cfg(feature = "scene-viewer")]

use sashiko_asset::scene::PerspectiveView;

#[test]
fn view_zero_projects_pbrt_first_hits_to_pixel_centres() -> Result<(), Box<dyn std::error::Error>> {
    let settings: serde_json::Value = serde_json::from_str(include_str!(
        "../../../assets/landscape/overview-settings.json"
    ))?;
    let view: PerspectiveView = serde_json::from_value(settings["views"]["view-0"].clone())?;
    let camera = view.camera([3840, 2160], [0.02, 1500.])?;
    let matrix = camera.camera.clip_from_world();
    let origin_cm = [-2138.9744, 28.89301, -1707.7404];
    // First non-null surface reported by pbrt --pixelmaterial. Coordinates
    // are source centimetres. Pixel centres are half-integers on the full film.
    for (hit, pixel) in [
        ([-2983.9524, 122.23373, -2804.4756], [962.5, 1372.5]),
        ([-3058.038, 165.10266, -2456.9858], [724.5, 1222.5]),
        ([397.8318, 663.4034, 4144.4053], [1942.5, 1036.5]),
    ] {
        let local: [f32; 3] = std::array::from_fn(|i| (hit[i] - origin_cm[i]) * 0.01);
        let clip: [f32; 4] = std::array::from_fn(|i| {
            matrix[3][i] + (0..3).map(|j| matrix[j][i] * local[j]).sum::<f32>()
        });
        let x = 1920. * (1. + clip[0] / clip[3]);
        let y = 1080. * (1. + clip[1] / clip[3]);
        assert!(
            (x - pixel[0]).hypot(y - pixel[1]) < 0.25,
            "{x}, {y} vs {pixel:?}"
        );
    }
    Ok(())
}
