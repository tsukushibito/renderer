//! Resource-local interval frontiers in a prepared, reusable AVL node arena.
//!
//! Keys are interval starts. Splits and replacements touch only intersecting
//! nodes; neither insertion order nor unrelated resources move an array suffix.

use std::ops::Range;

use super::Capacity;

type Link = Option<usize>;

#[derive(Clone, Copy)]
pub(super) struct Cursor(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Entry<T> {
    pub start: u64,
    pub end: u64,
    pub value: T,
}

struct Node<T> {
    entry: Entry<T>,
    left: Link,
    right: Link,
    parent: Link,
    height: u8,
}

pub(super) struct Intervals<T> {
    nodes: Vec<Node<T>>,
    roots: Vec<Link>,
    free: Link,
    len: usize,
    limit: usize,
}

impl<T: Copy> Intervals<T> {
    pub fn new(resources: usize, segments: usize) -> Result<Self, Capacity> {
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(segments)
            .map_err(|_| Capacity::Segments)?;
        let mut roots = Vec::new();
        roots
            .try_reserve_exact(resources)
            .map_err(|_| Capacity::Resources)?;
        Ok(Self {
            nodes,
            roots,
            free: None,
            len: 0,
            limit: segments,
        })
    }

    pub fn clear(&mut self, resources: usize) {
        self.nodes.clear();
        self.roots.clear();
        self.roots.resize(resources, None);
        self.free = None;
        self.len = 0;
    }

    pub fn insert(&mut self, resource: usize, entry: Entry<T>) -> Result<(), ()> {
        if self.len == self.limit {
            return Err(());
        }
        let node = Node {
            entry,
            left: None,
            right: None,
            parent: None,
            height: 1,
        };
        let index = if let Some(index) = self.free {
            self.free = self.nodes[index].left;
            self.nodes[index] = node;
            index
        } else {
            let index = self.nodes.len();
            self.nodes.push(node);
            index
        };
        self.len += 1;
        self.roots[resource] = Some(self.insert_at(self.roots[resource], index));
        Ok(())
    }

    pub fn at(&self, resource: usize, point: u64) -> Option<Entry<T>> {
        self.index_at(resource, point)
            .map(|index| self.nodes[index].entry)
    }

    pub fn find(&self, resource: usize, point: u64) -> Option<Cursor> {
        self.index_at(resource, point).map(Cursor)
    }

    pub fn entry(&self, cursor: Cursor) -> Entry<T> {
        self.nodes[cursor.0].entry
    }

    pub fn next(&self, cursor: Cursor) -> Option<Cursor> {
        let mut index = cursor.0;
        if let Some(mut right) = self.nodes[index].right {
            while let Some(left) = self.nodes[right].left {
                right = left;
            }
            return Some(Cursor(right));
        }
        while let Some(parent) = self.nodes[index].parent {
            if self.nodes[parent].left == Some(index) {
                return Some(Cursor(parent));
            }
            index = parent;
        }
        None
    }

    pub fn set_cursor_value(&mut self, cursor: Cursor, value: T) {
        self.nodes[cursor.0].entry.value = value;
    }

    fn index_at(&self, resource: usize, point: u64) -> Link {
        let mut next = self.roots[resource];
        while let Some(index) = next {
            let node = &self.nodes[index];
            if point < node.entry.start {
                next = node.left;
            } else if point >= node.entry.end {
                next = node.right;
            } else {
                return Some(index);
            }
        }
        None
    }

    pub fn split(&mut self, resource: usize, point: u64) -> Result<(), ()> {
        let Some(index) = self.index_at(resource, point) else {
            return Ok(());
        };
        let entry = self.nodes[index].entry;
        if entry.start == point {
            return Ok(());
        }
        if self.len == self.limit {
            return Err(());
        }
        self.nodes[index].entry.end = point;
        self.insert(
            resource,
            Entry {
                start: point,
                ..entry
            },
        )
    }

    pub fn replace(&mut self, resource: usize, range: Range<u64>, value: T) -> Result<(), ()> {
        let index = self
            .index_at(resource, range.start)
            .expect("frontier covers resource");
        let head = self.nodes[index].entry;
        if range.end <= head.end {
            let added = usize::from(head.start < range.start) + usize::from(range.end < head.end);
            if self.len + added > self.limit {
                return Err(());
            }
            if head.start == range.start {
                self.nodes[index].entry.end = range.end;
                self.nodes[index].entry.value = value;
            } else {
                self.nodes[index].entry.end = range.start;
                self.insert(
                    resource,
                    Entry {
                        start: range.start,
                        end: range.end,
                        value,
                    },
                )
                .expect("preflighted capacity");
            }
            if range.end < head.end {
                self.insert(
                    resource,
                    Entry {
                        start: range.end,
                        ..head
                    },
                )
                .expect("preflighted capacity");
            }
            return Ok(());
        }
        let tail = self
            .at(resource, range.end - 1)
            .expect("frontier covers resource");
        let mut removed = 0;
        let mut cursor = Some(Cursor(index));
        while let Some(current) = cursor {
            if self.entry(current).start >= range.end {
                break;
            }
            removed += 1;
            cursor = self.next(current);
        }
        let added = 1 + usize::from(head.start < range.start) + usize::from(range.end < tail.end);
        if self.len - removed + added > self.limit {
            return Err(());
        }
        let mut point = head.start;
        while point < range.end {
            let entry = self.at(resource, point).expect("contiguous frontier");
            self.roots[resource] = self.remove_at(self.roots[resource].unwrap(), entry.start);
            point = entry.end;
        }
        // Capacity was established for the complete mutation before removing
        // anything. Recycled and previously unused arena slots supply every
        // insertion below without growing the prepared backing.
        if head.start < range.start {
            self.insert(
                resource,
                Entry {
                    end: range.start,
                    ..head
                },
            )
            .expect("preflighted capacity");
        }
        self.insert(
            resource,
            Entry {
                start: range.start,
                end: range.end,
                value,
            },
        )
        .expect("preflighted capacity");
        if range.end < tail.end {
            self.insert(
                resource,
                Entry {
                    start: range.end,
                    ..tail
                },
            )
            .expect("preflighted capacity");
        }
        Ok(())
    }

    fn height(&self, link: Link) -> u8 {
        link.map_or(0, |i| self.nodes[i].height)
    }

    fn update(&mut self, index: usize) {
        self.nodes[index].height = 1 + self
            .height(self.nodes[index].left)
            .max(self.height(self.nodes[index].right));
    }

    fn rotate_left(&mut self, index: usize) -> usize {
        let pivot = self.nodes[index].right.unwrap();
        self.nodes[pivot].parent = self.nodes[index].parent;
        self.nodes[index].right = self.nodes[pivot].left;
        if let Some(child) = self.nodes[index].right {
            self.nodes[child].parent = Some(index);
        }
        self.nodes[pivot].left = Some(index);
        self.nodes[index].parent = Some(pivot);
        self.update(index);
        self.update(pivot);
        pivot
    }

    fn rotate_right(&mut self, index: usize) -> usize {
        let pivot = self.nodes[index].left.unwrap();
        self.nodes[pivot].parent = self.nodes[index].parent;
        self.nodes[index].left = self.nodes[pivot].right;
        if let Some(child) = self.nodes[index].left {
            self.nodes[child].parent = Some(index);
        }
        self.nodes[pivot].right = Some(index);
        self.nodes[index].parent = Some(pivot);
        self.update(index);
        self.update(pivot);
        pivot
    }

    fn balance(&mut self, index: usize) -> usize {
        self.update(index);
        let left = self.nodes[index].left;
        let right = self.nodes[index].right;
        if self.height(left) > self.height(right) + 1 {
            let left = left.unwrap();
            if self.height(self.nodes[left].right) > self.height(self.nodes[left].left) {
                self.nodes[index].left = Some(self.rotate_left(left));
            }
            self.rotate_right(index)
        } else if self.height(right) > self.height(left) + 1 {
            let right = right.unwrap();
            if self.height(self.nodes[right].left) > self.height(self.nodes[right].right) {
                self.nodes[index].right = Some(self.rotate_right(right));
            }
            self.rotate_left(index)
        } else {
            index
        }
    }

    fn insert_at(&mut self, root: Link, index: usize) -> usize {
        let Some(root) = root else {
            return index;
        };
        if self.nodes[index].entry.start < self.nodes[root].entry.start {
            let child = self.insert_at(self.nodes[root].left, index);
            self.nodes[root].left = Some(child);
            self.nodes[child].parent = Some(root);
        } else {
            let child = self.insert_at(self.nodes[root].right, index);
            self.nodes[root].right = Some(child);
            self.nodes[child].parent = Some(root);
        }
        self.balance(root)
    }

    fn remove_at(&mut self, root: usize, start: u64) -> Link {
        if start < self.nodes[root].entry.start {
            self.nodes[root].left = self.remove_at(self.nodes[root].left.unwrap(), start);
        } else if start > self.nodes[root].entry.start {
            self.nodes[root].right = self.remove_at(self.nodes[root].right.unwrap(), start);
        } else {
            let left = self.nodes[root].left;
            let right = self.nodes[root].right;
            if let (Some(_), Some(mut successor)) = (left, right) {
                while let Some(left) = self.nodes[successor].left {
                    successor = left;
                }
                let entry = self.nodes[successor].entry;
                self.nodes[root].entry = entry;
                self.nodes[root].right = self.remove_at(right.unwrap(), entry.start);
            } else {
                if let Some(child) = left.or(right) {
                    self.nodes[child].parent = self.nodes[root].parent;
                }
                self.nodes[root].left = self.free;
                self.free = Some(root);
                self.len -= 1;
                return left.or(right);
            }
        }
        Some(self.balance(root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check<T: Copy>(
        map: &Intervals<T>,
        root: Link,
        low: Option<u64>,
        high: Option<u64>,
    ) -> (usize, u8) {
        let Some(index) = root else {
            return (0, 0);
        };
        let node = &map.nodes[index];
        for child in [node.left, node.right].into_iter().flatten() {
            assert_eq!(map.nodes[child].parent, Some(index));
        }
        assert!(low.is_none_or(|low| node.entry.start > low));
        assert!(high.is_none_or(|high| node.entry.start < high));
        let (a, left) = check(map, node.left, low, Some(node.entry.start));
        let (b, right) = check(map, node.right, Some(node.entry.start), high);
        assert!(left.abs_diff(right) <= 1);
        assert_eq!(node.height, 1 + left.max(right));
        (a + b + 1, node.height)
    }

    #[test]
    fn fragmented_edits_match_dense_contents_and_reuse_reserved_nodes() {
        let mut map = Intervals::new(2, 256).unwrap();
        map.clear(2);
        for r in 0..2 {
            map.insert(
                r,
                Entry {
                    start: 0,
                    end: 64,
                    value: 0,
                },
            )
            .unwrap();
        }
        let mut dense = [[0; 64]; 2];
        let backing = map.nodes.as_ptr();
        let mut seed = 1u64;
        for value in 1..4096 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let resource = (seed >> 63) as usize;
            let start = (seed % 64) as usize;
            let end = start + 1 + ((seed >> 8) as usize % (64 - start));
            map.replace(resource, start as u64..end as u64, value)
                .unwrap();
            dense[resource][start..end].fill(value);
            for (r, expected) in dense.iter().enumerate() {
                assert!(map.nodes[map.roots[r].unwrap()].parent.is_none());
                for (p, &value) in expected.iter().enumerate() {
                    assert_eq!(map.at(r, p as u64).unwrap().value, value);
                }
                check(&map, map.roots[r], None, None);
                let mut cursor = map.find(r, 0);
                let mut end = 0;
                while let Some(current) = cursor {
                    let entry = map.entry(current);
                    assert_eq!(entry.start, end);
                    assert!(
                        expected[entry.start as usize..entry.end as usize]
                            .iter()
                            .all(|&value| value == entry.value)
                    );
                    end = entry.end;
                    cursor = map.next(current);
                }
                assert_eq!(end, 64);
            }
            assert_eq!(map.nodes.as_ptr(), backing);
        }
        assert!(map.nodes.len() <= 128);
    }

    #[test]
    fn split_and_capacity_failure_preserve_contents_then_whole_write_recycles_slots() {
        let mut map = Intervals::new(1, 3).unwrap();
        map.clear(1);
        map.insert(
            0,
            Entry {
                start: 0,
                end: 16,
                value: 0,
            },
        )
        .unwrap();
        map.replace(0, 4..8, 1).unwrap();
        assert!(map.replace(0, 10..12, 2).is_err());
        assert!(map.split(0, 2).is_err());
        assert_eq!(map.at(0, 10).unwrap().value, 0);
        assert_eq!(map.at(0, 4).unwrap().value, 1);
        map.replace(0, 0..16, 3).unwrap();
        map.split(0, 8).unwrap();
        map.set_cursor_value(map.find(0, 8).unwrap(), 4);
        assert_eq!(map.at(0, 7).unwrap().value, 3);
        assert_eq!(map.at(0, 8).unwrap().value, 4);
        assert_eq!(check(&map, map.roots[0], None, None).0, map.len);
    }

    #[test]
    fn sorted_and_reverse_fragmentation_stay_balanced_across_removal() {
        for reverse in [false, true] {
            let mut map = Intervals::new(1, 2049).unwrap();
            map.clear(1);
            map.insert(
                0,
                Entry {
                    start: 0,
                    end: 4096,
                    value: 0,
                },
            )
            .unwrap();
            for i in 0..1024 {
                let point = if reverse { 1023 - i } else { i } * 4;
                map.replace(0, point..point + 2, 1).unwrap();
            }
            assert_eq!(check(&map, map.roots[0], None, None).0, map.len);
            map.replace(0, 1..4095, 2).unwrap();
            assert_eq!(check(&map, map.roots[0], None, None).0, 3);
        }
    }
}
