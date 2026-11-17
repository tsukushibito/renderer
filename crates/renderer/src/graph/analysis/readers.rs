//! Original read-access intervals in a prepared resource-local AVL index.
//!
//! Reads never split the writer frontier or copy another read's history. One
//! node represents one read-only access for the entire ordering analysis. A
//! query excludes readers at or before the current writer of the queried byte
//! interval, so partial overwrites retire only the bytes they actually cover.
//! Identical ranges share one tree key and link their pass records directly.
//! Insertion is O(log G) for G distinct ranges; overlap queries prune by range
//! end and newest pass, but may visit O(R) records for overlapping histories.
//! That cost occurs when finding write dependencies, not on read-only splits.

use std::ops::Range;

use super::{Capacity, index::Index};

type Link = Option<Index>;

struct Node {
    range: Range<u64>,
    pass: usize,
    left: Link,
    right: Link,
    // Tree nodes link duplicate accesses here. Duplicate records have no tree
    // children; their maximum_pass summarizes only the rest of this chain.
    same_range: Link,
    height: u8,
    maximum_end: u64,
    maximum_pass: usize,
}

pub(super) struct Readers {
    nodes: Vec<Node>,
    roots: Vec<Link>,
    limit: usize,
}

impl Readers {
    pub fn new(resources: usize, readers: usize) -> Result<Self, Capacity> {
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(readers)
            .map_err(|_| Capacity::Readers)?;
        let mut roots = Vec::new();
        roots
            .try_reserve_exact(resources)
            .map_err(|_| Capacity::Resources)?;
        Ok(Self {
            nodes,
            roots,
            limit: readers,
        })
    }

    pub fn clear(&mut self, resources: usize) {
        self.nodes.clear();
        self.roots.clear();
        self.roots.resize(resources, None);
    }

    pub fn insert(
        &mut self,
        resource: usize,
        range: Range<u64>,
        pass: usize,
    ) -> Result<(), Capacity> {
        if self.nodes.len() == self.limit {
            return Err(Capacity::Readers);
        }
        let index = Index::new(self.nodes.len());
        self.nodes.push(Node {
            maximum_end: range.end,
            maximum_pass: pass,
            range,
            pass,
            left: None,
            right: None,
            same_range: None,
            height: 1,
        });
        self.roots[resource] = Some(self.insert_at(self.roots[resource], index));
        Ok(())
    }

    /// Visit overlapping reads newer than this interval's last writer. Both
    /// subtree summaries are conservative: they only skip impossible matches.
    pub fn visit<E>(
        &self,
        resource: usize,
        range: Range<u64>,
        writer: Option<usize>,
        mut visit: impl FnMut(usize) -> Result<(), E>,
    ) -> Result<(), E> {
        self.visit_at(self.roots[resource], &range, writer, &mut visit)
    }

    fn visit_at<E>(
        &self,
        link: Link,
        range: &Range<u64>,
        writer: Option<usize>,
        visit: &mut impl FnMut(usize) -> Result<(), E>,
    ) -> Result<(), E> {
        let Some(index) = link else {
            return Ok(());
        };
        let node = &self.nodes[index.get()];
        if node.maximum_end <= range.start
            || writer.is_some_and(|writer| node.maximum_pass <= writer)
        {
            return Ok(());
        }
        self.visit_at(node.left, range, writer, visit)?;
        if node.range.start < range.end {
            if range.start < node.range.end {
                if writer.is_none_or(|writer| node.pass > writer) {
                    visit(node.pass)?;
                }
                let mut same_range = node.same_range;
                while let Some(index) = same_range {
                    let same = &self.nodes[index.get()];
                    if writer.is_some_and(|writer| same.maximum_pass <= writer) {
                        break;
                    }
                    if writer.is_none_or(|writer| same.pass > writer) {
                        visit(same.pass)?;
                    }
                    same_range = same.same_range;
                }
            }
            self.visit_at(node.right, range, writer, visit)?;
        }
        Ok(())
    }

    fn insert_at(&mut self, link: Link, inserted: Index) -> Index {
        let Some(index) = link else {
            return inserted;
        };
        let range = &self.nodes[inserted.get()].range;
        let existing = &self.nodes[index.get()].range;
        match (range.start, range.end).cmp(&(existing.start, existing.end)) {
            std::cmp::Ordering::Less => {
                self.nodes[index.get()].left =
                    Some(self.insert_at(self.nodes[index.get()].left, inserted));
            }
            std::cmp::Ordering::Greater => {
                self.nodes[index.get()].right =
                    Some(self.insert_at(self.nodes[index.get()].right, inserted));
            }
            std::cmp::Ordering::Equal => {
                let previous = self.nodes[index.get()].same_range;
                self.nodes[inserted.get()].same_range = previous;
                if let Some(previous) = previous {
                    self.nodes[inserted.get()].maximum_pass = self.nodes[inserted.get()]
                        .maximum_pass
                        .max(self.nodes[previous.get()].maximum_pass);
                }
                self.nodes[index.get()].same_range = Some(inserted);
                self.update(index);
                return index;
            }
        }
        self.balance(index)
    }

    fn height(&self, link: Link) -> u8 {
        link.map_or(0, |index| self.nodes[index.get()].height)
    }

    fn update(&mut self, index: Index) {
        let node = &self.nodes[index.get()];
        let (left, right) = (node.left, node.right);
        let mut maximum_end = node.range.end;
        let mut maximum_pass = node.pass;
        if let Some(same) = node.same_range {
            maximum_pass = maximum_pass.max(self.nodes[same.get()].maximum_pass);
        }
        for child in [left, right].into_iter().flatten() {
            maximum_end = maximum_end.max(self.nodes[child.get()].maximum_end);
            maximum_pass = maximum_pass.max(self.nodes[child.get()].maximum_pass);
        }
        let height = self.height(left).max(self.height(right)) + 1;
        let node = &mut self.nodes[index.get()];
        node.height = height;
        node.maximum_end = maximum_end;
        node.maximum_pass = maximum_pass;
    }

    fn balance(&mut self, index: Index) -> Index {
        self.update(index);
        let (left, right) = (self.nodes[index.get()].left, self.nodes[index.get()].right);
        if self.height(left) > self.height(right) + 1 {
            let left = left.expect("left-heavy node");
            if self.height(self.nodes[left.get()].right) > self.height(self.nodes[left.get()].left)
            {
                self.nodes[index.get()].left = Some(self.rotate_left(left));
            }
            return self.rotate_right(index);
        }
        if self.height(right) > self.height(left) + 1 {
            let right = right.expect("right-heavy node");
            if self.height(self.nodes[right.get()].left)
                > self.height(self.nodes[right.get()].right)
            {
                self.nodes[index.get()].right = Some(self.rotate_right(right));
            }
            return self.rotate_left(index);
        }
        index
    }

    fn rotate_left(&mut self, index: Index) -> Index {
        let root = self.nodes[index.get()].right.expect("right child");
        self.nodes[index.get()].right = self.nodes[root.get()].left;
        self.nodes[root.get()].left = Some(index);
        self.update(index);
        self.update(root);
        root
    }

    fn rotate_right(&mut self, index: Index) -> Index {
        let root = self.nodes[index.get()].left.expect("left child");
        self.nodes[index.get()].left = self.nodes[root.get()].right;
        self.nodes[root.get()].right = Some(index);
        self.update(index);
        self.update(root);
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(
        readers: &Readers,
        resource: usize,
        range: Range<u64>,
        writer: Option<usize>,
    ) -> Vec<usize> {
        let mut result = Vec::new();
        readers
            .visit(resource, range, writer, |pass| {
                result.push(pass);
                Ok::<_, ()>(())
            })
            .unwrap();
        result.sort_unstable();
        result
    }

    #[test]
    fn overlapping_reader_queries_match_ranges_and_writer_cutoffs_in_any_insertion_order() {
        // Equal starts, nested/identical ranges and unrelated resources exercise
        // both tree rotations and same-range chains in either insertion order.
        let inputs = (0..512)
            .map(|pass| {
                let start = (pass % 64) as u64;
                (pass % 2, start..start + (pass / 64 % 4 + 1) as u64, pass)
            })
            .collect::<Vec<_>>();
        for reverse in [false, true] {
            let mut readers = Readers::new(2, inputs.len()).unwrap();
            readers.clear(2);
            for i in 0..inputs.len() {
                let (resource, range, pass) =
                    &inputs[if reverse { inputs.len() - i - 1 } else { i }];
                readers.insert(*resource, range.clone(), *pass).unwrap();
            }
            for resource in 0..2 {
                for start in (0..80).step_by(3) {
                    let range = start..start + 5;
                    for writer in [None, Some(0), Some(256), Some(511)] {
                        let expected = inputs
                            .iter()
                            .filter(|(r, read, pass)| {
                                *r == resource
                                    && read.start < range.end
                                    && range.start < read.end
                                    && writer.is_none_or(|writer| *pass > writer)
                            })
                            .map(|(_, _, pass)| *pass)
                            .collect::<Vec<_>>();
                        assert_eq!(matches(&readers, resource, range.clone(), writer), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn exhausted_reader_index_keeps_prior_queries_and_reset_reuses_prepared_capacity() {
        let mut readers = Readers::new(2, 2).unwrap();
        readers.clear(2);
        readers.insert(0, 0..8, 1).unwrap();
        readers.insert(0, 4..12, 3).unwrap();
        assert_eq!(readers.insert(1, 0..4, 4), Err(Capacity::Readers));
        assert_eq!(matches(&readers, 0, 6..10, Some(2)), [3]);
        assert!(matches(&readers, 1, 0..4, None).is_empty());
        let capacity = readers.nodes.capacity();
        readers.clear(1);
        readers.insert(0, 20..24, 0).unwrap();
        assert!(matches(&readers, 0, 0..12, None).is_empty());
        assert_eq!(matches(&readers, 0, 20..24, None), [0]);
        assert_eq!(readers.nodes.capacity(), capacity);
    }
}
