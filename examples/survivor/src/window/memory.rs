//! Explicit snapshots at frame boundaries. Native budget estimates are not residency.
use sashiko::profiling::{renderer_memory, working_set};
use sashiko_renderer::{WindowRenderer, pipeline::PipelineResult};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};
use survivor::Game;

pub(super) struct Snapshots<'a> {
    path: Option<&'a Path>,
    entries: Vec<Value>,
}

impl<'a> Snapshots<'a> {
    pub fn new(path: Option<&'a Path>) -> Self {
        Self {
            path,
            entries: Vec::new(),
        }
    }

    pub fn before_preparation(&mut self, stage: &str, game: &Game) -> PipelineResult<()> {
        if self.path.is_some() {
            self.entries
                .push(json!({"stage":stage,"tick":game.tick,"process":working_set()?}));
        }
        Ok(())
    }

    pub fn observe(
        &mut self,
        stage: &str,
        renderer: &mut WindowRenderer,
        game: &Game,
    ) -> PipelineResult<()> {
        if self.path.is_none() {
            return Ok(());
        }
        let mut observation = renderer_memory(renderer.runtime_info())?;
        observation["stage"] = stage.into();
        observation["tick"] = game.tick.into();
        observation["process"] = working_set()?;
        observation["game"] = json!({"enemies":[game.enemies.len(),game.enemies.capacity()],
            "shots":[game.shots.len(),game.shots.capacity()], "drops":[game.drops.len(),game.drops.capacity()]});
        self.entries.push(observation);
        Ok(())
    }

    pub fn save(self) -> PipelineResult<()> {
        if let Some(path) = self.path {
            let mut writer = BufWriter::new(File::create(path)?);
            serde_json::to_writer_pretty(&mut writer, &self.entries)?;
            writer.flush()?;
        }
        Ok(())
    }
}
