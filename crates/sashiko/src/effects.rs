//! Finite, owner-scoped effects. Time is supplied by the caller in its chosen ticks.
use std::num::NonZeroU64;

struct Entry<O, V> {
    owner: O,
    expires: u64,
    value: V,
}

pub struct Effects<O, V> {
    entries: Vec<Entry<O, V>>,
    limit: usize,
}

impl<O: Eq, V> Effects<O, V> {
    pub fn new(limit: usize) -> Self {
        Self {
            entries: Vec::with_capacity(limit),
            limit,
        }
    }

    /// At capacity the caller gets the uninstalled effect back.
    pub fn start(&mut self, owner: O, now: u64, duration: NonZeroU64, value: V) -> Result<(), V> {
        if self.entries.len() >= self.limit {
            return Err(value);
        }
        self.entries.push(Entry {
            owner,
            expires: now.saturating_add(duration.get()),
            value,
        });
        Ok(())
    }

    pub fn active(&self) -> impl Iterator<Item = (&O, &V)> {
        self.entries.iter().map(|e| (&e.owner, &e.value))
    }

    pub fn update(&mut self, now: u64, mut keep: impl FnMut(&O, &mut V) -> bool) {
        self.entries
            .retain_mut(|e| e.expires > now && keep(&e.owner, &mut e.value))
    }

    pub fn cancel(&mut self, owner: &O) {
        self.entries.retain(|e| &e.owner != owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_cancellation_and_tick_expiry() {
        let mut effects = Effects::new(1);
        let duration = NonZeroU64::new(3).unwrap();
        effects.start(5, 7, duration, "light").unwrap();
        assert_eq!(effects.start(2, 7, duration, "sound"), Err("sound"));
        effects.update(9, |_, _| true);
        assert_eq!(effects.active().count(), 1);
        effects.update(10, |_, _| true);
        effects.start(2, 10, duration, "sound").unwrap();
        effects.cancel(&2);
        assert_eq!(effects.active().count(), 0);
    }
}
