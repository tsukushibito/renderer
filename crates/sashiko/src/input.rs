//! Map device buttons to gameplay actions, retaining edges until a simulation tick.
#[derive(Clone, Copy)]
struct Binding<K, A> {
    key: K,
    action: A,
    held: bool,
}

/// Several keys can drive one action and one key can drive several actions.
/// Setup reserves all storage needed to process button changes and ticks.
pub struct ActionMap<K, A> {
    bindings: Vec<Binding<K, A>>,
    pressed: Vec<A>,
    released: Vec<A>,
}

impl<K: Eq + Copy, A: Eq + Copy> ActionMap<K, A> {
    pub fn new(bindings: &[(K, A)]) -> Self {
        let mut map = Self {
            bindings: Vec::with_capacity(bindings.len()),
            pressed: Vec::with_capacity(bindings.len()),
            released: Vec::with_capacity(bindings.len()),
        };
        for &(key, action) in bindings {
            if !map
                .bindings
                .iter()
                .any(|b| b.key == key && b.action == action)
            {
                map.bindings.push(Binding {
                    key,
                    action,
                    held: false,
                });
            }
        }
        map
    }

    pub fn button(&mut self, key: K, down: bool) {
        for i in 0..self.bindings.len() {
            let binding = self.bindings[i];
            if binding.key != key || binding.held == down {
                continue;
            }
            self.bindings[i].held = down;
            if self
                .bindings
                .iter()
                .enumerate()
                .any(|(j, b)| j != i && b.action == binding.action && b.held)
            {
                continue;
            }
            let edges = if down {
                &mut self.pressed
            } else {
                &mut self.released
            };
            // Repeated presses of an action in one tick update its priority.
            edges.retain(|&a| a != binding.action);
            edges.push(binding.action);
        }
    }

    pub fn held(&self, action: A) -> bool {
        self.bindings.iter().any(|b| b.action == action && b.held)
    }

    pub fn axis(&self, negative: A, positive: A) -> f32 {
        f32::from(u8::from(self.held(positive))) - f32::from(u8::from(self.held(negative)))
    }

    pub fn pressed(&self, action: A) -> bool {
        self.pressed.contains(&action)
    }
    pub fn released(&self, action: A) -> bool {
        self.released.contains(&action)
    }

    /// Unique actions pressed since the last tick, oldest first.
    pub fn presses(&self) -> &[A] {
        &self.pressed
    }

    /// Extract the value of the most recent press matching an action family.
    pub fn latest_press<T>(&self, value: impl FnMut(A) -> Option<T>) -> Option<T> {
        self.pressed.iter().rev().copied().find_map(value)
    }

    /// Sample held values and edges once for the current update. Pending edges
    /// survive display retries or any other frames that have no update.
    pub fn sample<T>(&mut self, read: impl FnOnce(&Self) -> T) -> T {
        let sample = read(self);
        self.pressed.clear();
        self.released.clear();
        sample
    }

    /// Discard all pending input when control leaves the application.
    pub fn clear(&mut self) {
        self.pressed.clear();
        self.released.clear();
        for b in &mut self.bindings {
            b.held = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregate_edges_across_bindings_and_tick_boundaries() {
        let mut map = ActionMap::new(&[
            ('A', "left"),
            ('L', "left"),
            ('A', "ability"),
            ('D', "right"),
        ]);
        map.button('A', true);
        map.button('L', true);
        map.button('A', false);
        map.button('A', true);
        map.button('A', false);
        map.sample(|m| {
            assert_eq!(m.presses(), &["left", "ability"]);
            assert!(m.released("ability"));
            assert!(!m.released("left"));
            assert_eq!(m.axis("left", "right"), -1.);
        });
        map.sample(|m| {
            assert!(!m.pressed("left") && !m.released("ability"));
            assert!(m.held("left"));
        });
        map.button('D', true);
        assert_eq!(map.axis("left", "right"), 0.);
        map.button('L', false);
        assert!(map.released("left"));
        map.clear();
        assert!(!map.held("right") && !map.released("left"));
    }
}
