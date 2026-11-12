//! Deterministic draw-count workloads; input generation is outside frame timing.

use renderer::engine::{Draw, IDENTITY, MaterialHandle, MeshHandle};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Layout {
    Overlap,
    Grid,
}

impl Layout {
    pub(super) fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "overlap" => Ok(Self::Overlap),
            "grid" => Ok(Self::Grid),
            _ => Err("layout must be overlap or grid"),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Overlap => "overlap",
            Self::Grid => "grid",
        }
    }
}

pub(super) fn grid_dimensions(count: usize) -> (usize, usize) {
    let columns = count.max(1).isqrt();
    let columns = columns + usize::from(columns * columns < count);
    (columns, count.max(1).div_ceil(columns))
}

pub(super) fn draws(
    mesh: MeshHandle,
    material: MaterialHandle,
    count: usize,
    layout: Layout,
    translation_x: f32,
) -> Vec<Draw> {
    let (columns, rows) = grid_dimensions(count);
    (0..count)
        .map(|index| {
            if layout == Layout::Overlap {
                return super::scene::draw(mesh, material, translation_x);
            }
            let mut world_from_model = IDENTITY;
            world_from_model[0][0] = 1.0 / columns as f32;
            world_from_model[1][1] = 1.0 / rows as f32;
            world_from_model[3][0] = -1.0 + (2 * (index % columns) + 1) as f32 / columns as f32;
            world_from_model[3][1] = -1.0 + (2 * (index / columns) + 1) as f32 / rows as f32;
            Draw {
                mesh,
                material,
                world_from_model,
            }
        })
        .collect()
}

/// Check one interior pixel per occupied/empty grid cell, plus clear background.
/// Cell extents of at least eight pixels keep samples away from triangle edges.
pub(super) fn verify(
    rgba: &[u8],
    width: u32,
    height: u32,
    count: usize,
    layout: Layout,
    foreground: [u8; 4],
) -> Result<(), String> {
    let width = width as usize;
    let height = height as usize;
    if rgba.len() != width * height * 4 {
        return Err("unexpected output byte length".into());
    }
    let background = [0, 0, 0, 255];
    let check = |x, y, expected: [u8; 4]| {
        let start = (y * width + x) * 4;
        if rgba[start..start + 4] == expected {
            Ok(())
        } else {
            Err(format!(
                "unexpected output at ({x},{y}): {:?}, expected={expected:?}",
                &rgba[start..start + 4]
            ))
        }
    };
    check(0, 0, background)?;
    if layout == Layout::Grid {
        let (columns, rows) = grid_dimensions(count);
        for cell in 0..columns * rows {
            let x = (2 * (cell % columns) + 1) * width / (2 * columns);
            let y = (2 * (cell / columns) + 1) * height / (2 * rows);
            check(x, y, if cell < count { foreground } else { background })?;
        }
    } else {
        check(
            width / 2,
            height / 2,
            if count == 0 { background } else { foreground },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::engine::{Engine, Material};

    #[test]
    fn grid_draws_share_assets_and_samples_cover_every_draw_and_empty_cell() {
        let mut assets = Engine::new(1, 1).unwrap();
        let mesh = assets
            .create_mesh(super::super::scene::triangle([1.0, 0.0, 0.0, 1.0]).unwrap())
            .unwrap();
        let material = assets.create_material(Material::default()).unwrap();
        let draws = draws(mesh, material, 5, Layout::Grid, 0.0);
        assert_eq!(grid_dimensions(5), (3, 2));
        assert_eq!(draws.len(), 5);
        assert!(
            draws
                .iter()
                .all(|draw| draw.mesh == mesh && draw.material == material)
        );
        assert_eq!(draws[0].world_from_model[0][0], 1.0 / 3.0);
        assert_eq!(draws[4].world_from_model[3][1], 0.5);
        let (width, height) = (24, 16);
        let mut rgba = [0, 0, 0, 255].repeat(width * height);
        for cell in 0..5 {
            let offset = ((cell / 3 * 8 + 4) * width + cell % 3 * 8 + 4) * 4;
            rgba[offset..offset + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
        assert!(verify(&rgba, 24, 16, 5, Layout::Grid, [255, 0, 0, 255]).is_ok());
        rgba[(12 * width + 12) * 4] = 0;
        assert!(verify(&rgba, 24, 16, 5, Layout::Grid, [255, 0, 0, 255]).is_err());
        rgba[(12 * width + 12) * 4] = 255;
        rgba[(12 * width + 20) * 4] = 255;
        assert!(verify(&rgba, 24, 16, 5, Layout::Grid, [255, 0, 0, 255]).is_err());
    }

    #[test]
    fn non_square_and_large_grid_samples_stay_inside_triangle_interiors() {
        for count in [8, 512, 1_024] {
            let (columns, rows) = grid_dimensions(count);
            for index in 0..count {
                let column = index % columns;
                let row = index / columns;
                let x = (2 * column + 1) * 512 / (2 * columns);
                let y = (2 * row + 1) * 512 / (2 * rows);
                let local_x =
                    ((x as f64 + 0.5) / 512.0 * columns as f64 - column as f64) * 2.0 - 1.0;
                let local_y = ((y as f64 + 0.5) / 512.0 * rows as f64 - row as f64) * 2.0 - 1.0;
                // The actual triangle is (-.75,-.75),(.75,-.75),(0,.75).
                assert!(local_y > -0.75 && local_y < 0.75);
                assert!(local_x.abs() < (0.75 - local_y) / 2.0);
            }
        }
    }

    #[test]
    fn zero_draws_require_clear_output_and_reject_wrong_lengths() {
        let rgba = [0, 0, 0, 255].repeat(64);
        for layout in [Layout::Overlap, Layout::Grid] {
            assert!(verify(&rgba, 8, 8, 0, layout, [255, 0, 0, 255]).is_ok());
            assert!(verify(&rgba[..4], 8, 8, 0, layout, [255, 0, 0, 255]).is_err());
        }
    }
}
