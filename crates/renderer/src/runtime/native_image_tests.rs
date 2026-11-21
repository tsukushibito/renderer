//! Public Graph operations shared by both native texture exchange fixtures.

use super::*;
use crate::graph::{GraphCapacity, GraphPool};

pub(crate) fn transfer(
    runtime: &mut Runtime,
    texture: &ImportImage,
    write: bool,
    verify_busy: impl FnOnce(&mut Runtime),
) {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let image = graph.import_image("host texture", texture).unwrap();
    let pixels = [31, 194, 2, 255, 9, 81, 62, 1];
    if write {
        graph.upload_image("paint", image, &pixels).unwrap();
    }
    let read = graph.readback_image("verify host storage", image).unwrap();
    let plan = graph.compile().unwrap();
    let id = runtime.submit(&plan).unwrap().id();
    verify_busy(runtime);
    let mut result = runtime
        .wait_for_frame(id, Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let mut bytes = [0; 8];
    result.readback_into(read, &mut bytes).unwrap();
    assert_eq!(bytes, pixels);
}
