use std::{env, error::Error};
#[cfg(feature = "live-inspector")]
mod inspection;
#[cfg(feature = "memory-profiling")]
mod measure;

#[derive(Debug, Default)]
struct Game {
    tick: u64,
    door: u64,
    open: bool,
    held_basement_key: bool,
}

impl Game {
    fn new() -> Self {
        Self {
            door: 2,
            ..Self::default()
        }
    }

    fn update(
        &mut self,
        #[cfg(feature = "live-inspector")] mut journal: Option<
            &mut sashiko::inspect::Journal<inspection::Facts>,
        >,
    ) {
        self.tick += 1;
        match self.tick % 4 {
            1 | 2 => {
                self.open = self.held_basement_key;
                #[cfg(feature = "live-inspector")]
                if let Some(journal) = journal.as_mut() {
                    self.record_decision(journal)
                }
                self.held_basement_key = true;
            }
            3 => {
                #[cfg(feature = "live-inspector")]
                if let Some(journal) = journal.as_mut() {
                    self.record_lifetime(journal, "despawn");
                }
                self.door = 0;
            }
            _ => {
                self.door = self.tick + 2;
                self.open = false;
                self.held_basement_key = false;
                #[cfg(feature = "live-inspector")]
                if let Some(journal) = journal.as_mut() {
                    self.record_lifetime(journal, "spawn");
                }
            }
        }
    }

    fn advance(&mut self) {
        self.update(
            #[cfg(feature = "live-inspector")]
            None,
        )
    }

    fn summary(&self) -> String {
        format!(
            "{{\"tick\":\"{}\",\"door\":\"{}\",\"open\":{},\"held_basement_key\":{}}}",
            self.tick, self.door, self.open, self.held_basement_key
        )
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    let mut game = Game::new();
    let ticks = match args.as_slice() {
        [] => 4,
        [flag, n] if flag == "--ticks" => n.parse()?,
        #[cfg(feature = "live-inspector")]
        [flag] if flag == "--inspect-stdio" => {
            return Ok(sashiko::inspect::serve_stdio(
                game,
                std::time::Duration::from_millis(16),
            )?);
        }
        #[cfg(feature = "memory-profiling")]
        [mode, n] if mode.starts_with("--measure") => return measure::run(mode, n.parse()?),
        _ => return Err("unsupported arguments".into()),
    };
    for _ in 0..ticks {
        game.advance()
    }
    println!("{}", game.summary());
    Ok(())
}
