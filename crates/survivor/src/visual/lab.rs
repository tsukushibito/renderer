use super::{Palette, geometry::Geometry};
use renderer::{
    mesh::Meshes,
    pipeline::{PipelineResult, RenderObject},
};
use survivor::Arena;

pub(super) fn build(
    arena: &Arena,
    meshes: &mut Meshes,
    p: &Palette,
) -> PipelineResult<Vec<RenderObject>> {
    let [w, d] = arena.half_extent;
    let mut floor = Geometry::default();
    let mut steel = Geometry::default();
    let mut ivory = Geometry::default();
    let mut rubber = Geometry::default();
    let mut safety = Geometry::default();
    let mut lamps = Geometry::default();
    let mut lcd = Geometry::default();
    floor.slab([0., -0.14, 0.], [2. * w, 0.28, 2. * d])?;
    // The camera follows the player to every boundary. Recessed service corridors
    // extend beyond the walkable floor, leaving the playable edge visually distinct.
    rubber.slab([0., -0.66, 0.], [2. * w + 60., 0.5, 2. * d + 42.])?;
    for side in [-1., 1.] {
        let z = side * (d + 1.5);
        steel.slab([0., -0.27, z], [2. * w + 3., 0.24, 0.56])?;
        for lane in 0..3 {
            let stripe = z + side * (1.7 + lane as f32 * 0.27);
            steel.slab([0., -0.27, stripe], [2. * w + 4., 0.16, 0.18])?;
        }
        for i in 0..(w as u32 / 2) {
            let x = -w + i as f32 * 4. + 1.8;
            rubber.slab([x, 0.06, z + side * 0.12], [0.32, 0.72, 0.70])?;
            lamps.slab([x, 0.43, z + side * 0.12], [0.18, 0.03, 0.16])?;
            steel.slab([x + 0.8, -0.30, z + side * 2.], [0.32, 0.21, 1.40])?;
        }
    }
    // A cutaway on the near side keeps moving subjects visible. The raised back
    // wall carries the service trench, vertical stiffeners and recessed strips.
    ivory.slab([0., 1.15, -d - 0.15], [2. * w + 0.3, 2.30, 0.30])?;
    steel.slab([0., 0.22, -d + 0.02], [2. * w, 0.4, 0.14])?;
    steel.slab([0., 2.30, -d + 0.05], [2. * w, 0.10, 0.29])?;
    let sections = (2. * w / 3.5).ceil() as u32;
    for i in 0..sections {
        let width = 2. * w / sections as f32;
        let x = -w + (i as f32 + 0.5) * width;
        steel.slab(
            [x - width / 2. + 0.045, 1.14, -d + 0.06],
            [0.09, 2.24, 0.18],
        )?;
        rubber.slab([x, 1.85, -d + 0.05], [width * 0.80, 0.34, 0.12])?;
        lamps.slab([x, 1.91, -d + 0.125], [width * 0.76, 0.07, 0.03])?;
        for n in 0..4 {
            steel.slab(
                [x, 0.83 + n as f32 * 0.10, -d + 0.09],
                [width * 0.56, 0.04, 0.10],
            )?;
        }
    }
    for sign in [-1., 1.] {
        steel.slab([sign * (w - 0.08), 0.25, 0.], [0.16, 0.50, 2. * d])?;
        for i in 0..(d as u32 / 2) {
            let z = -d + 2. + i as f32 * 4.;
            let x = sign * (w + 0.97);
            ivory.slab([x, 0.48, z], [1.15, 1.60, 1.85])?;
            steel.slab([x, 1.30, z], [1.18, 0.08, 1.88])?;
            lcd.slab([x, 1.36, z], [0.55, 0.04, 0.53])?;
            for fin in 0..7 {
                rubber.slab(
                    [x, 1.36, z - 0.82 + fin as f32 * 0.07],
                    [0.66, 0.025, 0.035],
                )?;
            }
        }
        lamps.slab([sign * (w - 0.09), 0.505, 0.], [0.04, 0.01, 2. * d])?;
        safety.slab([sign * (w - 1.2), 0.006, 0.], [0.07, 0.012, 2. * (d - 1.2)])?;
        safety.slab([0., 0.006, sign * (d - 1.2)], [2. * (w - 1.2), 0.012, 0.07])?;
    }
    // Expansion joints in the slab and a drainage channel along the rear aisle.
    for x in (-(w as i32)..w as i32).filter(|x| x.rem_euclid(4) == 0) {
        rubber.slab([x as f32, 0.001, 0.], [0.018, 0.002, 2. * d])?;
    }
    for z in (-(d as i32)..d as i32).filter(|z| z.rem_euclid(4) == 0) {
        rubber.slab([0., 0.001, z as f32], [2. * w, 0.002, 0.018])?;
    }
    rubber.slab([0., 0.003, -d + 0.75], [2. * w, 0.006, 0.19])?;
    for i in 0..(w * 4.) as usize {
        steel.slab([-w + i as f32 * 0.5, 0.009, -d + 0.75], [0.04, 0.01, 0.22])?;
    }
    // The simulation supplies the occupied footprints. Build a plinth, drawers,
    // worktop and instrument racks wholly inside each collision rectangle.
    for (i, obstacle) in arena.obstacles.iter().enumerate() {
        let [x, z] = obstacle.center;
        let [hx, hz] = obstacle.half_extent;
        for side in [-1., 1.] {
            for end in [-1., 1.] {
                // Painted L-marks leave the exposed corners of the benches readable
                // at play distance, including in a crowded shot.
                let ex = x + end * (hx + 0.30);
                let ez = z + side * (hz + 0.30);
                safety.slab([ex - end * 0.19, 0.010, ez], [0.47, 0.010, 0.09])?;
                safety.slab([ex, 0.010, ez - side * 0.19], [0.09, 0.010, 0.47])?;
            }
        }
        rubber.slab([x, 0.085, z], [1.85 * hx, 0.17, 1.85 * hz])?;
        ivory.slab([x, 0.52, z], [1.93 * hx, 0.72, 1.85 * hz])?;
        steel.slab([x, 0.94, z], [2. * hx, 0.15, 2. * hz])?;
        for side in [-1., 1.] {
            let front = z + side * hz * 0.94;
            for drawer in 0..3 {
                let h = 0.30 + drawer as f32 * 0.23;
                rubber.slab([x, h, front], [1.86 * hx, 0.018, 0.019])?;
                steel.slab(
                    [x, h + 0.075, front + side * 0.025],
                    [hx * 0.45, 0.035, 0.05],
                )?;
            }
        }
        let sx = hx.min(0.85);
        let sz = hz.min(0.65);
        let instrument = x - hx * 0.38;
        rubber.slab([instrument, 1.15, z], [sx, 0.27, sz])?;
        ivory.slab([instrument, 1.47, z], [sx * 0.94, 0.38, sz * 0.82])?;
        lcd.slab([instrument, 1.49, z + sz * 0.416], [sx * 0.72, 0.22, 0.015])?;
        for n in 0..3 {
            lamps.slab(
                [
                    instrument - sx * 0.24 + n as f32 * sx * 0.15,
                    1.28,
                    z + sz * 0.51,
                ],
                [0.035, 0.023, 0.012],
            )?;
        }
        if i % 2 == 0 {
            for n in 0..4 {
                steel.slab(
                    [x + hx * 0.40, 1.14, z + (n as f32 - 1.5) * sz * 0.30],
                    [sx * 0.38, 0.25, sz * 0.16],
                )?;
            }
        }
    }
    // Threshold bars catch the eye at the far service door.
    for n in 0..4 {
        lcd.slab(
            [w * 0.5 + (n as f32 - 1.5) * 0.25, 0.03, -d + 0.35],
            [0.13, 0.04, 0.46],
        )?;
    }
    [
        (floor, p.floor),
        (steel, p.steel),
        (rubber, p.dark),
        (ivory, p.ivory),
        (lamps, p.lamp),
        (safety, p.safety),
        (lcd, p.lcd),
    ]
    .into_iter()
    .map(|(batch, material)| Ok(RenderObject::new(meshes.create(batch.data()?)?, material)))
    .collect()
}
