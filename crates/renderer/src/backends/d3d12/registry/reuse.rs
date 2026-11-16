//! Prepared, class-local candidates for physical resource selection.
//!
//! Registered entries are unlinked. Per-recording cursors inspect each remaining
//! candidate at most once, including leased resources. Generation and actual Arc
//! ownership are still checked by the registry before any resource is borrowed.
//! A candidate released after its cursor passed can be used by the next recording.

use std::collections::TryReserveError;

const NONE: usize = usize::MAX;

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum ReuseClass {
    Buffer,
    Image {
        width: u32,
        height: u32,
        format: u8,
        mip_levels: u32,
        array_layers: u32,
    },
}

#[derive(Clone, Copy)]
struct Node {
    class: usize,
    previous: usize,
    next: usize,
    linked: bool,
}
impl Default for Node {
    fn default() -> Self {
        Self {
            class: NONE,
            previous: NONE,
            next: NONE,
            linked: false,
        }
    }
}

struct Class<K> {
    key: K,
    head: usize,
    tail: usize,
    cursor: usize,
    epoch: u64,
}

pub(super) struct ReuseIndex<K> {
    nodes: Vec<Node>,
    classes: Vec<Class<K>>,
    epoch: u64,
}

impl<K: Copy + Ord> ReuseIndex<K> {
    pub(super) fn new(capacity: usize) -> Result<Self, TryReserveError> {
        let mut nodes = Vec::new();
        nodes.try_reserve_exact(capacity)?;
        nodes.resize(capacity, Node::default());
        let mut classes = Vec::new();
        classes.try_reserve_exact(capacity)?;
        Ok(Self {
            nodes,
            classes,
            epoch: 0,
        })
    }

    /// Explicit preparation only: install each registry entry once. Class
    /// insertion can move metadata here, never during ordinary recording.
    pub(super) fn install(&mut self, index: usize, key: K) {
        assert!(
            self.nodes[index].class == NONE,
            "new prepared registry entry"
        );
        let class = match self.classes.binary_search_by_key(&key, |class| class.key) {
            Ok(class) => class,
            Err(class) => {
                assert!(
                    self.classes.len() < self.nodes.len(),
                    "prepared class capacity"
                );
                self.classes.insert(
                    class,
                    Class {
                        key,
                        head: NONE,
                        tail: NONE,
                        cursor: NONE,
                        epoch: 0,
                    },
                );
                for node in &mut self.nodes {
                    if node.class != NONE && node.class >= class {
                        node.class += 1;
                    }
                }
                class
            }
        };
        self.nodes[index].class = class;
        self.restore(index);
    }

    /// Registration and terminal generations leave the candidate list without
    /// moving its other elements. Removing an already removed entry is inert.
    pub(super) fn remove(&mut self, index: usize) {
        let node = self.nodes[index];
        if !node.linked {
            return;
        }
        let class = &mut self.classes[node.class];
        if node.previous == NONE {
            class.head = node.next;
        } else {
            self.nodes[node.previous].next = node.next;
        }
        if node.next == NONE {
            class.tail = node.previous;
        } else {
            self.nodes[node.next].previous = node.previous;
        }
        if class.cursor == index {
            class.cursor = node.next;
        }
        self.nodes[index].linked = false;
        self.nodes[index].previous = NONE;
        self.nodes[index].next = NONE;
    }

    /// Deletion is an explicit resource-change boundary. Restore ascending
    /// registry order here; no sorting or element movement is needed by bind.
    pub(super) fn restore(&mut self, index: usize) {
        if self.nodes[index].linked {
            return;
        }
        let class = &mut self.classes[self.nodes[index].class];
        let mut next = class.head;
        let mut previous = NONE;
        if class.tail < index {
            previous = class.tail;
            next = NONE;
        } else {
            while next != NONE && next < index {
                previous = next;
                next = self.nodes[next].next;
            }
        }
        self.nodes[index].previous = previous;
        self.nodes[index].next = next;
        self.nodes[index].linked = true;
        if previous == NONE {
            class.head = index;
        } else {
            self.nodes[previous].next = index;
        }
        if next == NONE {
            class.tail = index;
        } else {
            self.nodes[next].previous = index;
        }
    }

    pub(super) fn begin(&mut self) {
        if let Some(next) = self.epoch.checked_add(1) {
            self.epoch = next;
        } else {
            for class in &mut self.classes {
                class.epoch = 0;
            }
            self.epoch = 1;
        }
    }

    /// Explicit preparation only. Retain both backing arrays for rebuilding.
    pub(super) fn clear(&mut self) {
        self.nodes.fill(Node::default());
        self.classes.clear();
        self.epoch = 0;
    }

    pub(super) fn select(
        &mut self,
        key: K,
        mut eligible: impl FnMut(usize) -> bool,
    ) -> Option<usize> {
        let class = self
            .classes
            .binary_search_by_key(&key, |class| class.key)
            .ok()?;
        let class = &mut self.classes[class];
        if class.epoch != self.epoch {
            class.cursor = class.head;
            class.epoch = self.epoch;
        }
        while class.cursor != NONE {
            let index = class.cursor;
            class.cursor = self.nodes[index].next;
            if eligible(index) {
                return Some(index);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_prefix_and_other_classes_are_not_inspected() {
        let mut index = ReuseIndex::new(1032).unwrap();
        for position in 0..1032 {
            index.install(position, if position % 2 == 0 { 1 } else { 2 });
            if position < 1024 {
                index.remove(position);
            }
        }
        index.begin();
        let mut inspected = Vec::new();
        for wanted in [1025, 1027, 1029, 1031] {
            assert_eq!(
                index.select(2, |position| {
                    inspected.push(position);
                    true
                }),
                Some(wanted)
            );
        }
        assert_eq!(index.select(2, |_| panic!("exhausted class")), None);
        assert_eq!(inspected, [1025, 1027, 1029, 1031]);
        assert_eq!(index.select(3, |_| panic!("absent class")), None);
    }

    #[test]
    fn leased_candidates_are_checked_once_and_return_on_the_next_recording() {
        let mut index = ReuseIndex::new(8).unwrap();
        for position in 0..8 {
            index.install(position, 0);
        }
        index.begin();
        let mut checks = 0;
        for wanted in [4, 5, 6, 7] {
            assert_eq!(
                index.select(0, |position| {
                    checks += 1;
                    position >= 4
                }),
                Some(wanted)
            );
        }
        assert_eq!(checks, 8);
        assert_eq!(index.select(0, |_| panic!("cursor exhausted")), None);
        index.begin();
        assert_eq!(index.select(0, |_| true), Some(0));
        // Failed/cancelled recording returns its physical ownership. A fresh
        // recording resets the cursor, without requiring any release callback.
        index.begin();
        assert_eq!(index.select(0, |_| true), Some(0));
    }

    #[test]
    fn registration_deletion_and_repreparation_preserve_index_order() {
        let mut index = ReuseIndex::new(8).unwrap();
        for (position, key) in [3, 1, 3, 2, 1, 3, 2, 1].into_iter().enumerate() {
            index.install(position, key);
        }
        index.remove(1);
        index.remove(7);
        index.remove(1);
        index.begin();
        assert_eq!(index.select(1, |_| true), Some(4));
        assert_eq!(index.select(1, |_| true), None);
        index.restore(7);
        index.restore(1);
        index.restore(1);
        index.begin();
        for wanted in [1, 4, 7] {
            assert_eq!(index.select(1, |_| true), Some(wanted));
        }
        index.clear();
        for (position, key) in [2, 2, 1].into_iter().enumerate() {
            index.install(position, key);
        }
        index.begin();
        assert_eq!(index.select(1, |_| true), Some(2));
        assert_eq!(index.select(3, |_| true), None);
        assert_eq!(index.select(2, |_| true), Some(0));
        assert_eq!(index.select(2, |_| true), Some(1));
        assert_eq!(index.select(2, |_| true), None);
    }

    #[test]
    fn epoch_wrap_resets_lazy_cursors_without_reviving_removed_entries() {
        let mut index = ReuseIndex::new(2).unwrap();
        index.install(0, 0);
        index.install(1, 0);
        index.remove(0);
        index.begin();
        assert_eq!(index.select(0, |_| true), Some(1));
        index.epoch = u64::MAX;
        index.begin();
        assert_eq!(index.select(0, |_| true), Some(1));
        assert_eq!(index.select(0, |_| true), None);
    }

    #[test]
    fn unlinking_the_next_candidate_keeps_recording_progress_and_owned_delete_waits() {
        let mut index = ReuseIndex::new(4).unwrap();
        for position in 0..4 {
            index.install(position, 0);
        }
        index.begin();
        assert_eq!(index.select(0, |_| true), Some(0));
        index.remove(1);
        assert_eq!(index.select(0, |_| true), Some(2));
        index.remove(2);
        index.restore(1);
        assert_eq!(index.select(0, |_| true), Some(3));
        assert_eq!(index.select(0, |_| true), None);
        index.begin();
        // Deleted native ownership can still be leased by accepted GPU work.
        // Restoring its identity alone must not bypass the eligibility check.
        assert_eq!(
            index.select(0, |position| position != 0 && position != 1),
            Some(3)
        );
        index.begin();
        assert_eq!(index.select(0, |_| true), Some(0));
        assert_eq!(index.select(0, |_| true), Some(1));
        assert_eq!(index.select(0, |_| true), Some(3));
    }
}
