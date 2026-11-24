//! Fixed-step updates driven by a live clock or by recorded actions.
use serde::{Deserialize, Serialize};
use std::{num::NonZeroU32, time::Duration};

/// Window presentation is independent of update numbers. A host advances this
/// driver once per update batch and may retry presenting the resulting state.
pub struct App<G> {
    pub game: G,
    rate: NonZeroU32,
    remainder: u128,
    updates: usize,
}

impl<G> App<G> {
    pub fn new(game: G, rate: NonZeroU32) -> Self {
        Self {
            game,
            rate,
            remainder: 0,
            updates: 0,
        }
    }

    /// Carry fractional updates without rounding the duration of one tick.
    pub fn elapsed_ticks(&mut self, elapsed: Duration) -> u128 {
        self.remainder += elapsed.as_nanos() * u128::from(self.rate.get());
        let ticks = self.remainder / 1_000_000_000;
        self.remainder %= 1_000_000_000;
        ticks
    }

    pub fn advance<I>(
        &mut self,
        count: u128,
        next: impl FnMut(usize, &G) -> I,
        mut update: impl FnMut(&mut G, I),
    ) {
        let Ok(()) = self.try_advance(count, next, |game, action| {
            update(game, action);
            Ok::<(), std::convert::Infallible>(())
        });
    }

    /// Each callback consumes one action. A callback failure stops the batch;
    /// the failed update may have modified game state, so propagate the failure.
    pub fn try_advance<I, E>(
        &mut self,
        count: u128,
        mut next: impl FnMut(usize, &G) -> I,
        mut update: impl FnMut(&mut G, I) -> Result<(), E>,
    ) -> Result<(), E> {
        for _ in 0..count {
            let action = next(self.updates, &self.game);
            update(&mut self.game, action)?;
            self.updates = self
                .updates
                .checked_add(1)
                .expect("simulation update count overflow");
        }
        Ok(())
    }
}

/// Portable replay. The game supplies its serializable starting configuration.
/// Hosts may also import historical recordings containing just an input array.
#[derive(Serialize, Deserialize)]
pub struct Replay<S, I> {
    pub version: u32,
    pub tick_hz: u32,
    pub initial: S,
    pub inputs: Vec<I>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Stored<S, I> {
    Replay(Replay<S, I>),
    Inputs(Vec<I>),
}

impl<S, I> Replay<S, I> {
    pub fn new(initial: S, rate: NonZeroU32, inputs: Vec<I>) -> Self {
        Self {
            version: 1,
            tick_hz: rate.get(),
            initial,
            inputs,
        }
    }
}

impl<S: serde::de::DeserializeOwned, I: serde::de::DeserializeOwned> Replay<S, I> {
    pub fn read(reader: impl std::io::Read, initial: S, rate: NonZeroU32) -> std::io::Result<Self> {
        match serde_json::from_reader(reader)? {
            Stored::Replay(tape) if tape.version == 1 && tape.tick_hz == rate.get() => Ok(tape),
            Stored::Replay(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported replay format or update rate",
            )),
            Stored::Inputs(inputs) => Ok(Self::new(initial, rate, inputs)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_fixed_steps_and_replay_have_identical_results() {
        let rate = NonZeroU32::new(60).unwrap();
        let mut app = App::new(Vec::new(), rate);
        for _ in 0..10 {
            let ticks = app.elapsed_ticks(Duration::from_millis(100));
            app.advance(ticks, |i, _| i, |state, input| state.push(input));
        }
        assert_eq!(app.game, (0..60).collect::<Vec<_>>());
        let legacy = Replay::<(), usize>::read("[3,4]".as_bytes(), (), rate).unwrap();
        assert_eq!(legacy.inputs, [3, 4]);
        let mut invalid = legacy;
        invalid.tick_hz = 50;
        assert!(
            Replay::<(), usize>::read(serde_json::to_vec(&invalid).unwrap().as_slice(), (), rate)
                .is_err()
        );
    }
}
