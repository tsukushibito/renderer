//! Offline capture reuses a prepared scene and waits for every requested image.
use super::{Evidence, Result};
use sashiko::{
    capture::CaptureImage,
    inspect::report::{FrameStamp, Manifest, Outcome},
};
use sashiko_renderer::{Renderer, core::runtime::RuntimeConfig};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use survivor::{Game, visual::Visual};

pub(crate) struct Sequence {
    root: PathBuf,
    manifest: Manifest,
    renderer: Renderer,
    visual: Visual,
    extent: [u32; 2],
}
impl Sequence {
    pub fn new(
        root: &Path,
        extent: [u32; 2],
        backend: (RuntimeConfig, &str),
        game: &Game,
        mut visual: Visual,
        evidence: &mut Evidence,
        character: &str,
    ) -> Result<Self> {
        evidence.set_visual(visual.content(character))?;
        let camera = visual.camera(game, extent)?;
        let mut renderer = Renderer::headless(backend.0, camera, visual.assets())?;
        visual.prepare_headless(&mut renderer)?;
        let mut manifest = evidence.manifest("offline-sequence");
        manifest.settings["capture_mode"] = "each_completed_tick".into();
        manifest.settings["backend"] = backend.1.into();
        std::fs::create_dir(root)?;
        manifest.publish(root)?;
        Ok(Self {
            root: root.into(),
            manifest,
            renderer,
            visual,
            extent,
        })
    }
    pub fn frame(&mut self, game: &Game, evidence: &Evidence) -> Result<()> {
        let result = (|| -> Result<()> {
            let camera = self.visual.camera(game, self.extent)?;
            self.renderer.set_camera(camera.camera);
            self.visual.sync(game, &camera)?;
            let frame = self.manifest.frames.len() as u64;
            let elapsed_ns = evidence.elapsed_ns()?;
            let image = CaptureImage::render(&mut self.renderer, Duration::from_secs(60), |f| {
                self.visual.draw(f, game)
            })?;
            let role = format!("frame_{frame:06}");
            let path = format!("{role}.png");
            super::png(&self.root.join(&path), image.extent(), image.rgba())?;
            self.manifest.files.insert(role, path.clone());
            self.manifest.files.insert("image".into(), path);
            self.manifest.frames.push(FrameStamp {
                frame,
                tick: game.tick,
                elapsed_ns,
                interpolation: None,
                view: "main".into(),
                extent: image.extent(),
                stage: "final_srgb_ui".into(),
                layers: Some(Default::default()),
            });
            Ok(())
        })();
        if let Err(error) = &result {
            self.manifest.outcome = Outcome::Failed {
                reason: error.to_string(),
            };
            self.manifest.publish(&self.root)?;
        }
        result
    }
    pub fn finish(mut self, evidence: Evidence) -> Result<()> {
        let mut runtime = self.renderer.into_runtime()?;
        self.visual.release(&mut runtime)?;
        crate::window::check_validation(runtime)?;
        self.manifest.requested_tick = evidence.last_tick;
        evidence.finish_report(&self.root, self.manifest, |_| Ok(()))
    }
}
