//! Reusable, bounded construction-order dataflow and scheduling analysis.
//!
//! All storage is reserved by `Workspace::new` and `Output::new`. Inputs are
//! borrowed directly; neither successful analysis nor resetting these arrays
//! allocates, grows a heap buffer, or releases its backing allocation.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Range;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub resources: usize,
    pub passes: usize,
    pub accesses: usize,
    pub edges: usize,
    pub segments: usize,
    pub readers: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Access {
    pub resource: usize,
    pub range: Range<u64>,
    pub read: bool,
    pub write: bool,
    pub valid_after: bool,
}

/// Graph construction establishes immutable owner/range/access guarantees.
/// Repeated iterators must describe the same input throughout one analysis.
pub(crate) trait Input {
    fn resource_count(&self) -> usize;
    fn resource_size(&self, resource: usize) -> u64;
    fn initial_ranges(&self, resource: usize) -> &[Range<u64>];
    fn resource_external(&self, resource: usize) -> bool;
    fn pass_count(&self) -> usize;
    fn access_count(&self) -> usize;
    fn pass_side_effect(&self, pass: usize) -> bool;
    fn pass_dependencies(&self, pass: usize) -> &[usize];
    fn pass_accesses(&self, pass: usize) -> impl Iterator<Item = Access>;
    fn exports(&self) -> impl Iterator<Item = (usize, Range<u64>)>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Capacity {
    Resources,
    Passes,
    Accesses,
    Edges,
    Segments,
    Readers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ErrorKind {
    Capacity(Capacity),
    #[cfg(feature = "development-validation")]
    InvalidResource,
    #[cfg(feature = "development-validation")]
    InvalidPass,
    #[cfg(feature = "development-validation")]
    InvalidRange,
    #[cfg(feature = "development-validation")]
    InvalidAccess,
    #[cfg(feature = "development-validation")]
    ConflictingAccesses,
    UninitializedRead,
    DependencyCycle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Error {
    pub kind: ErrorKind,
    pub pass: Option<usize>,
    pub resource: Option<usize>,
    pub range: Option<Range<u64>>,
}

impl Error {
    fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            pass: None,
            resource: None,
            range: None,
        }
    }
    fn capacity(capacity: Capacity) -> Self {
        Self::new(ErrorKind::Capacity(capacity))
    }
    fn at_pass(mut self, pass: usize) -> Self {
        self.pass = Some(pass);
        self
    }
    fn at_resource(mut self, resource: usize) -> Self {
        self.resource = Some(resource);
        self
    }
    fn at_range(mut self, range: Range<u64>) -> Self {
        self.range = Some(range);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum DependencyKind {
    Raw,
    War,
    Waw,
    Explicit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Edge {
    pub from: usize,
    pub to: usize,
    pub kind: DependencyKind,
}

/// Frozen output storage. Its arrays retain their capacities across analyses.
pub struct Output {
    limits: Limits,
    pub(crate) order: Vec<usize>,
    pub(crate) live: Vec<bool>,
    pub(crate) final_ranges: Vec<Range<u64>>,
    pub(crate) final_offsets: Vec<Range<usize>>,
    /// Closed cycle witness on `DependencyCycle`; empty on success.
    pub(crate) cycle: Vec<usize>,
    #[cfg(any(test, feature = "graph-diagnostics"))]
    pub(crate) data_edges: Vec<(usize, usize)>,
    #[cfg(any(test, feature = "graph-diagnostics"))]
    pub(crate) ordering_edges: Vec<Edge>,
    #[cfg(any(test, feature = "graph-diagnostics"))]
    pub(crate) lifetimes: Vec<Option<(usize, usize)>>,
}

impl Output {
    pub(crate) fn new(limits: Limits) -> Result<Self, Error> {
        Ok(Self {
            limits,
            order: reserve(limits.passes, Capacity::Passes)?,
            live: reserve(limits.passes, Capacity::Passes)?,
            final_ranges: reserve(limits.segments, Capacity::Segments)?,
            final_offsets: reserve(limits.resources, Capacity::Resources)?,
            cycle: reserve(
                limits
                    .passes
                    .checked_add(1)
                    .ok_or_else(|| Error::capacity(Capacity::Passes))?,
                Capacity::Passes,
            )?,
            #[cfg(any(test, feature = "graph-diagnostics"))]
            data_edges: reserve(limits.edges, Capacity::Edges)?,
            #[cfg(any(test, feature = "graph-diagnostics"))]
            ordering_edges: reserve(limits.edges, Capacity::Edges)?,
            #[cfg(any(test, feature = "graph-diagnostics"))]
            lifetimes: reserve(limits.resources, Capacity::Resources)?,
        })
    }

    #[cfg_attr(
        not(any(test, feature = "vulkan")),
        allow(dead_code, reason = "used by Vulkan lowering and CPU contracts")
    )]
    pub(crate) fn final_contents(&self, resource: usize) -> &[Range<u64>] {
        &self.final_ranges[self.final_offsets[resource].clone()]
    }

    fn reset(&mut self, resources: usize, passes: usize) -> Result<(), Error> {
        if resources > self.limits.resources {
            return Err(Error::capacity(Capacity::Resources));
        }
        if passes > self.limits.passes {
            return Err(Error::capacity(Capacity::Passes));
        }
        self.order.clear();
        self.live.clear();
        self.live.resize(passes, false);
        self.final_ranges.clear();
        self.final_offsets.clear();
        self.cycle.clear();
        #[cfg(any(test, feature = "graph-diagnostics"))]
        {
            self.data_edges.clear();
            self.ordering_edges.clear();
            self.lifetimes.clear();
            self.lifetimes.resize(resources, None);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ContentSegment {
    resource: usize,
    start: u64,
    end: u64,
    initialized: bool,
    producer: Option<usize>,
}

#[derive(Clone, Copy)]
struct HazardSegment {
    resource: usize,
    start: u64,
    end: u64,
    writer: Option<usize>,
    readers: Option<usize>,
}

struct Reader {
    pass: usize,
    previous: Option<usize>,
    visited: usize,
}

#[cfg(feature = "development-validation")]
#[derive(Clone, Copy)]
struct End {
    value: u64,
    index: usize,
}

#[cfg(feature = "development-validation")]
#[derive(Clone, Copy, Default)]
struct Prefix {
    longest: Option<End>,
    second: Option<End>,
    writer: Option<End>,
}

#[cfg(feature = "development-validation")]
impl Prefix {
    fn insert(&mut self, end: End, write: bool) {
        if self.longest.is_none_or(|v| end.value > v.value) {
            self.second = self.longest;
            self.longest = Some(end);
        } else if self.second.is_none_or(|v| end.value > v.value) {
            self.second = Some(end);
        }
        if write && self.writer.is_none_or(|v| end.value > v.value) {
            self.writer = Some(end);
        }
    }
    fn overlaps(self, index: usize, start: u64, write: bool) -> bool {
        let candidate = if write {
            self.longest.filter(|v| v.index != index).or(self.second)
        } else {
            self.writer
        };
        candidate.is_some_and(|v| v.value > start)
    }
}

#[cfg(feature = "development-validation")]
#[derive(Clone, Copy)]
struct IndexedAccess {
    resource: usize,
    start: u64,
    end: u64,
    write: bool,
    index: usize,
    prefix: Prefix,
}

pub(crate) struct Workspace {
    limits: Limits,
    contents: Vec<ContentSegment>,
    hazards: Vec<HazardSegment>,
    readers: Vec<Reader>,
    visit_epoch: usize,
    data: Vec<(usize, usize)>,
    data_offsets: Vec<Range<usize>>,
    data_marks: Vec<usize>,
    roots: Vec<bool>,
    pending: Vec<usize>,
    edges: Vec<Edge>,
    edge_marks: Vec<usize>,
    edge_kinds: Vec<u8>,
    neighbors: Vec<(usize, usize)>,
    offsets: Vec<usize>,
    incoming: Vec<usize>,
    done: Vec<bool>,
    ready: BinaryHeap<Reverse<usize>>,
    path: Vec<usize>,
    positions: Vec<usize>,
    #[cfg(feature = "development-validation")]
    conflicts: Vec<IndexedAccess>,
}

impl Workspace {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        let mut ready = BinaryHeap::new();
        ready
            .try_reserve_exact(limits.passes)
            .map_err(|_| Error::capacity(Capacity::Passes))?;
        Ok(Self {
            limits,
            contents: reserve(limits.segments, Capacity::Segments)?,
            hazards: reserve(limits.segments, Capacity::Segments)?,
            readers: reserve(limits.readers, Capacity::Readers)?,
            visit_epoch: 0,
            data: reserve(limits.edges, Capacity::Edges)?,
            data_offsets: reserve(limits.passes, Capacity::Passes)?,
            data_marks: reserve(limits.passes, Capacity::Passes)?,
            roots: reserve(limits.passes, Capacity::Passes)?,
            pending: reserve(limits.passes, Capacity::Passes)?,
            edges: reserve(limits.edges, Capacity::Edges)?,
            edge_marks: reserve(limits.passes, Capacity::Passes)?,
            edge_kinds: reserve(limits.passes, Capacity::Passes)?,
            neighbors: reserve(limits.edges, Capacity::Edges)?,
            offsets: reserve(
                limits
                    .passes
                    .checked_add(1)
                    .ok_or_else(|| Error::capacity(Capacity::Passes))?,
                Capacity::Passes,
            )?,
            incoming: reserve(limits.passes, Capacity::Passes)?,
            done: reserve(limits.passes, Capacity::Passes)?,
            ready,
            path: reserve(
                limits
                    .passes
                    .checked_add(1)
                    .ok_or_else(|| Error::capacity(Capacity::Passes))?,
                Capacity::Passes,
            )?,
            positions: reserve(limits.passes, Capacity::Passes)?,
            #[cfg(feature = "development-validation")]
            conflicts: reserve(limits.accesses, Capacity::Accesses)?,
        })
    }

    fn reset(&mut self, passes: usize) {
        self.contents.clear();
        self.hazards.clear();
        self.readers.clear();
        self.visit_epoch = 0;
        self.data.clear();
        self.data_offsets.clear();
        self.data_marks.clear();
        self.data_marks.resize(passes, usize::MAX);
        self.roots.clear();
        self.roots.resize(passes, false);
        self.pending.clear();
        self.edges.clear();
        self.edge_marks.clear();
        self.edge_marks.resize(passes, usize::MAX);
        self.edge_kinds.clear();
        self.edge_kinds.resize(passes, 0);
        self.neighbors.clear();
        self.offsets.clear();
        self.offsets.resize(passes + 1, 0);
        self.incoming.clear();
        self.incoming.resize(passes, 0);
        self.done.clear();
        self.done.resize(passes, false);
        self.ready.clear();
        self.path.clear();
        self.positions.clear();
        self.positions.resize(passes, usize::MAX);
    }

    fn initialize_contents(&mut self, input: &impl Input) -> Result<(), Error> {
        self.contents.clear();
        for resource in 0..input.resource_count() {
            push(
                &mut self.contents,
                ContentSegment {
                    resource,
                    start: 0,
                    end: input.resource_size(resource),
                    initialized: false,
                    producer: None,
                },
                self.limits.segments,
                Capacity::Segments,
            )?;
            for range in input.initial_ranges(resource) {
                self.write_contents(resource, range.clone(), true, None)?;
            }
        }
        Ok(())
    }

    fn write_contents(
        &mut self,
        resource: usize,
        range: Range<u64>,
        initialized: bool,
        producer: Option<usize>,
    ) -> Result<(), Error> {
        let first = self.contents.partition_point(|s| {
            s.resource < resource || (s.resource == resource && s.end <= range.start)
        });
        let end = self.contents.partition_point(|s| {
            s.resource < resource || (s.resource == resource && s.start < range.end)
        });
        let head = self.contents[first];
        let tail = self.contents[end - 1];
        let middle = ContentSegment {
            resource,
            start: range.start,
            end: range.end,
            initialized,
            producer,
        };
        let mut replacement = [middle; 3];
        let mut count = 0;
        if head.start < range.start {
            replacement[count] = ContentSegment {
                end: range.start,
                ..head
            };
            count += 1;
        }
        replacement[count] = middle;
        count += 1;
        if range.end < tail.end {
            replacement[count] = ContentSegment {
                start: range.end,
                ..tail
            };
            count += 1;
        }
        replace(
            &mut self.contents,
            first..end,
            &replacement[..count],
            self.limits.segments,
            Capacity::Segments,
        )
    }

    fn add_data(&mut self, from: usize, to: usize) -> Result<(), Error> {
        if self.data_marks[from] != to {
            push(
                &mut self.data,
                (from, to),
                self.limits.edges,
                Capacity::Edges,
            )?;
            self.data_marks[from] = to;
        }
        Ok(())
    }

    fn read_contents(
        &mut self,
        resource: usize,
        range: Range<u64>,
        pass: Option<usize>,
    ) -> Result<(), Error> {
        let first = self.contents.partition_point(|s| {
            s.resource < resource || (s.resource == resource && s.end <= range.start)
        });
        for index in first..self.contents.len() {
            let segment = self.contents[index];
            if segment.resource != resource || segment.start >= range.end {
                break;
            }
            if !segment.initialized {
                let mut error = Error::new(ErrorKind::UninitializedRead)
                    .at_resource(resource)
                    .at_range(segment.start.max(range.start)..segment.end.min(range.end));
                error.pass = pass;
                return Err(error);
            }
            if let Some(from) = segment.producer {
                if let Some(to) = pass {
                    self.add_data(from, to)?;
                } else {
                    self.roots[from] = true;
                }
            }
        }
        Ok(())
    }

    fn add_edge(&mut self, from: usize, to: usize, kind: DependencyKind) -> Result<(), Error> {
        if self.edge_marks[from] != to {
            self.edge_marks[from] = to;
            self.edge_kinds[from] = 0;
        }
        let bit = 1 << kind as u8;
        if self.edge_kinds[from] & bit == 0 {
            push(
                &mut self.edges,
                Edge { from, to, kind },
                self.limits.edges,
                Capacity::Edges,
            )?;
            self.edge_kinds[from] |= bit;
        }
        Ok(())
    }

    fn hazard_dependencies(&mut self, to: usize, access: &Access) -> Result<(), Error> {
        if access.write {
            if self.visit_epoch == usize::MAX {
                for reader in &mut self.readers {
                    reader.visited = 0;
                }
                self.visit_epoch = 1;
            } else {
                self.visit_epoch += 1;
            }
        }
        let first = self.hazards.partition_point(|s| {
            s.resource < access.resource
                || (s.resource == access.resource && s.end <= access.range.start)
        });
        for index in first..self.hazards.len() {
            let segment = self.hazards[index];
            if segment.resource != access.resource || segment.start >= access.range.end {
                break;
            }
            if let Some(from) = segment.writer {
                if access.read {
                    self.add_edge(from, to, DependencyKind::Raw)?;
                }
                if access.write {
                    self.add_edge(from, to, DependencyKind::Waw)?;
                }
            }
            if access.write {
                let mut reader = segment.readers;
                while let Some(index) = reader {
                    if self.readers[index].visited == self.visit_epoch {
                        break;
                    }
                    self.readers[index].visited = self.visit_epoch;
                    let entry = &self.readers[index];
                    let from = entry.pass;
                    reader = entry.previous;
                    self.add_edge(from, to, DependencyKind::War)?;
                }
            }
        }
        Ok(())
    }

    fn split_hazard(&mut self, resource: usize, point: u64) -> Result<(), Error> {
        let end = self.hazards.partition_point(|s| {
            s.resource < resource || (s.resource == resource && s.start <= point)
        });
        if end == 0 {
            return Ok(());
        }
        let index = end - 1;
        let segment = self.hazards[index];
        if segment.resource != resource || segment.start == point || point >= segment.end {
            return Ok(());
        }
        if self.hazards.len() == self.limits.segments {
            return Err(Error::capacity(Capacity::Segments));
        }
        self.hazards[index].end = point;
        self.hazards.insert(
            index + 1,
            HazardSegment {
                start: point,
                ..segment
            },
        );
        Ok(())
    }

    fn record_hazard(&mut self, pass: usize, access: &Access) -> Result<(), Error> {
        if access.write {
            let first = self.hazards.partition_point(|s| {
                s.resource < access.resource
                    || (s.resource == access.resource && s.end <= access.range.start)
            });
            let end = self.hazards.partition_point(|s| {
                s.resource < access.resource
                    || (s.resource == access.resource && s.start < access.range.end)
            });
            let head = self.hazards[first];
            let tail = self.hazards[end - 1];
            let middle = HazardSegment {
                resource: access.resource,
                start: access.range.start,
                end: access.range.end,
                writer: Some(pass),
                readers: None,
            };
            let mut replacement = [middle; 3];
            let mut count = 0;
            if head.start < access.range.start {
                replacement[count] = HazardSegment {
                    end: access.range.start,
                    ..head
                };
                count += 1;
            }
            replacement[count] = middle;
            count += 1;
            if access.range.end < tail.end {
                replacement[count] = HazardSegment {
                    start: access.range.end,
                    ..tail
                };
                count += 1;
            }
            replace(
                &mut self.hazards,
                first..end,
                &replacement[..count],
                self.limits.segments,
                Capacity::Segments,
            )?;
        } else {
            self.split_hazard(access.resource, access.range.start)?;
            self.split_hazard(access.resource, access.range.end)?;
            let first = self.hazards.partition_point(|s| {
                s.resource < access.resource
                    || (s.resource == access.resource && s.end <= access.range.start)
            });
            for index in first..self.hazards.len() {
                let segment = self.hazards[index];
                if segment.resource != access.resource || segment.start >= access.range.end {
                    break;
                }
                let reader = self.readers.len();
                push(
                    &mut self.readers,
                    Reader {
                        pass,
                        previous: segment.readers,
                        visited: 0,
                    },
                    self.limits.readers,
                    Capacity::Readers,
                )?;
                self.hazards[index].readers = Some(reader);
            }
        }
        Ok(())
    }

    fn ordering(&mut self, input: &impl Input, live: &[bool]) -> Result<(), Error> {
        self.hazards.clear();
        self.readers.clear();
        self.edges.clear();
        self.visit_epoch = 0;
        self.edge_marks.fill(usize::MAX);
        self.edge_kinds.fill(0);
        for resource in 0..input.resource_count() {
            push(
                &mut self.hazards,
                HazardSegment {
                    resource,
                    start: 0,
                    end: input.resource_size(resource),
                    writer: None,
                    readers: None,
                },
                self.limits.segments,
                Capacity::Segments,
            )?;
        }
        for pass in 0..input.pass_count() {
            if !live[pass] {
                continue;
            }
            for &from in input.pass_dependencies(pass) {
                if live[from] {
                    self.add_edge(from, pass, DependencyKind::Explicit)?;
                }
            }
            // Compare all accesses before recording any use from this pass.
            for access in input.pass_accesses(pass) {
                self.hazard_dependencies(pass, &access)?;
            }
            for access in input.pass_accesses(pass) {
                self.record_hazard(pass, &access)?;
            }
        }
        self.edges.sort_unstable();
        Ok(())
    }

    fn topological(&mut self, live: &[bool], out: &mut Output) -> Result<(), Error> {
        self.incoming.fill(0);
        self.done.fill(false);
        self.offsets.fill(0);
        self.neighbors.clear();
        self.ready.clear();
        out.order.clear();
        for edge in &self.edges {
            if self.neighbors.last() == Some(&(edge.from, edge.to)) {
                continue;
            }
            push(
                &mut self.neighbors,
                (edge.from, edge.to),
                self.limits.edges,
                Capacity::Edges,
            )?;
            self.incoming[edge.to] += 1;
            self.offsets[edge.from + 1] += 1;
        }
        for index in 1..self.offsets.len() {
            self.offsets[index] += self.offsets[index - 1];
        }
        for (pass, &is_live) in live.iter().enumerate() {
            if is_live && self.incoming[pass] == 0 {
                self.ready.push(Reverse(pass));
            }
        }
        while let Some(Reverse(pass)) = self.ready.pop() {
            out.order.push(pass);
            self.done[pass] = true;
            for index in self.offsets[pass]..self.offsets[pass + 1] {
                let next = self.neighbors[index].1;
                self.incoming[next] -= 1;
                if self.incoming[next] == 0 {
                    self.ready.push(Reverse(next));
                }
            }
        }
        if out.order.len() == live.iter().filter(|&&v| v).count() {
            return Ok(());
        }
        let mut current = (0..live.len())
            .find(|&p| live[p] && self.incoming[p] != 0)
            .expect("remaining cyclic node");
        self.path.clear();
        self.positions.fill(usize::MAX);
        loop {
            let position = self.positions[current];
            self.path.push(current);
            if position != usize::MAX {
                out.cycle
                    .extend(self.path[position..].iter().rev().copied());
                return Err(Error::new(ErrorKind::DependencyCycle).at_pass(out.cycle[0]));
            }
            self.positions[current] = self.path.len() - 1;
            current = self
                .neighbors
                .iter()
                .find_map(|&(from, to)| {
                    (to == current && live[from] && !self.done[from]).then_some(from)
                })
                .expect("remaining predecessor");
        }
    }
}

pub(crate) fn analyze(
    input: &impl Input,
    work: &mut Workspace,
    out: &mut Output,
) -> Result<(), Error> {
    let resources = input.resource_count();
    let passes = input.pass_count();
    if resources > work.limits.resources {
        return Err(Error::capacity(Capacity::Resources));
    }
    if passes > work.limits.passes {
        return Err(Error::capacity(Capacity::Passes));
    }
    if input.access_count() > work.limits.accesses {
        return Err(Error::capacity(Capacity::Accesses));
    }
    out.reset(resources, passes)?;
    work.reset(passes);
    #[cfg(feature = "development-validation")]
    validate(input, work)?;
    work.initialize_contents(input)?;
    for pass in 0..passes {
        let begin = work.data.len();
        work.roots[pass] = input.pass_side_effect(pass);
        for &from in input.pass_dependencies(pass) {
            work.add_data(from, pass)?;
        }
        for access in input.pass_accesses(pass) {
            if access.read {
                work.read_contents(access.resource, access.range, Some(pass))?;
            }
        }
        for access in input.pass_accesses(pass) {
            if access.write {
                if input.resource_external(access.resource) {
                    work.roots[pass] = true;
                }
                work.write_contents(
                    access.resource,
                    access.range,
                    access.valid_after,
                    access.valid_after.then_some(pass),
                )?;
            }
        }
        work.data_offsets.push(begin..work.data.len());
    }
    for (resource, range) in input.exports() {
        work.read_contents(resource, range, None)?;
    }
    #[cfg(feature = "development-validation")]
    {
        out.live.fill(true);
        work.ordering(input, &out.live)?;
        // Keep output and live independently borrowed while topological runs.
        work.topological_live_output(out)?;
        out.live.fill(false);
    }
    for pass in 0..passes {
        if work.roots[pass] {
            out.live[pass] = true;
            work.pending.push(pass);
        }
    }
    while let Some(pass) = work.pending.pop() {
        for index in work.data_offsets[pass].clone() {
            let dependency = work.data[index].0;
            if !out.live[dependency] {
                out.live[dependency] = true;
                work.pending.push(dependency);
            }
        }
    }
    work.ordering(input, &out.live)?;
    work.topological_live_output(out)?;
    // A culled writer must not publish valid contents. Replay only live order.
    work.initialize_contents(input)?;
    for (position, &pass) in out.order.iter().enumerate() {
        #[cfg(not(any(test, feature = "graph-diagnostics")))]
        let _ = position;
        for access in input.pass_accesses(pass) {
            if access.write {
                work.write_contents(
                    access.resource,
                    access.range,
                    access.valid_after,
                    Some(pass),
                )?;
            }
        }
        #[cfg(any(test, feature = "graph-diagnostics"))]
        {
            for access in input.pass_accesses(pass) {
                match &mut out.lifetimes[access.resource] {
                    Some((_, last)) => *last = position,
                    slot @ None => *slot = Some((position, position)),
                }
            }
        }
    }
    let mut segment = 0;
    for resource in 0..resources {
        let begin = out.final_ranges.len();
        while segment < work.contents.len() && work.contents[segment].resource == resource {
            let entry = work.contents[segment];
            segment += 1;
            if !entry.initialized {
                continue;
            }
            if out.final_ranges.len() != begin
                && out
                    .final_ranges
                    .last()
                    .is_some_and(|r| r.end == entry.start)
            {
                out.final_ranges.last_mut().expect("previous range").end = entry.end;
            } else {
                push(
                    &mut out.final_ranges,
                    entry.start..entry.end,
                    out.limits.segments,
                    Capacity::Segments,
                )?;
            }
        }
        out.final_offsets.push(begin..out.final_ranges.len());
    }
    #[cfg(any(test, feature = "graph-diagnostics"))]
    {
        if work.data.len() > out.limits.edges || work.edges.len() > out.limits.edges {
            return Err(Error::capacity(Capacity::Edges));
        }
        out.data_edges.extend_from_slice(&work.data);
        out.ordering_edges.extend_from_slice(&work.edges);
    }
    Ok(())
}

impl Workspace {
    // `Output.live` is needed immutably by topological, while its other arrays
    // receive the result. Temporarily moving the reserved order/cycle buffers
    // avoids aliasing without allocating another live bitmap.
    fn topological_live_output(&mut self, out: &mut Output) -> Result<(), Error> {
        let live = std::mem::take(&mut out.live);
        let result = self.topological(&live, out);
        out.live = live;
        result
    }
}

#[cfg(feature = "development-validation")]
fn validate(input: &impl Input, work: &mut Workspace) -> Result<(), Error> {
    for resource in 0..input.resource_count() {
        if input.resource_size(resource) == 0 {
            return Err(Error::new(ErrorKind::InvalidRange).at_resource(resource));
        }
        for range in input.initial_ranges(resource) {
            validate_range(input, resource, range.clone())?;
        }
    }
    for pass in 0..input.pass_count() {
        for &dependency in input.pass_dependencies(pass) {
            if dependency >= input.pass_count() {
                return Err(Error::new(ErrorKind::InvalidPass).at_pass(pass));
            }
        }
        for access in input.pass_accesses(pass) {
            validate_range(input, access.resource, access.range.clone())
                .map_err(|e| e.at_pass(pass))?;
            if !access.read && !access.write {
                return Err(Error::new(ErrorKind::InvalidAccess)
                    .at_pass(pass)
                    .at_resource(access.resource)
                    .at_range(access.range));
            }
        }
        if !input.pass_accesses(pass).any(|a| a.write) {
            continue;
        }
        work.conflicts.clear();
        for (index, access) in input.pass_accesses(pass).enumerate() {
            push(
                &mut work.conflicts,
                IndexedAccess {
                    resource: access.resource,
                    start: access.range.start,
                    end: access.range.end,
                    write: access.write,
                    index,
                    prefix: Prefix::default(),
                },
                work.limits.accesses,
                Capacity::Accesses,
            )?;
        }
        work.conflicts
            .sort_unstable_by_key(|a| (a.resource, a.start, a.end, a.index));
        let mut resource = None;
        let mut prefix = Prefix::default();
        for access in &mut work.conflicts {
            if resource != Some(access.resource) {
                resource = Some(access.resource);
                prefix = Prefix::default();
            }
            prefix.insert(
                End {
                    value: access.end,
                    index: access.index,
                },
                access.write,
            );
            access.prefix = prefix;
        }
        // Query in declaration order to preserve first-conflict attribution.
        for (index, access) in input.pass_accesses(pass).enumerate() {
            let end = work.conflicts.partition_point(|other| {
                other.resource < access.resource
                    || (other.resource == access.resource && other.start < access.range.end)
            });
            if work.conflicts[..end]
                .last()
                .filter(|other| other.resource == access.resource)
                .is_some_and(|other| {
                    other
                        .prefix
                        .overlaps(index, access.range.start, access.write)
                })
            {
                let later = input
                    .pass_accesses(pass)
                    .skip(index + 1)
                    .find(|other| {
                        access.resource == other.resource
                            && access.range.start < other.range.end
                            && other.range.start < access.range.end
                            && (access.write || other.write)
                    })
                    .expect("first conflicting access has a later partner");
                return Err(Error::new(ErrorKind::ConflictingAccesses)
                    .at_pass(pass)
                    .at_resource(later.resource)
                    .at_range(later.range));
            }
        }
    }
    for (resource, range) in input.exports() {
        validate_range(input, resource, range)?;
    }
    Ok(())
}

#[cfg(feature = "development-validation")]
fn validate_range(input: &impl Input, resource: usize, range: Range<u64>) -> Result<(), Error> {
    if resource >= input.resource_count() {
        return Err(Error::new(ErrorKind::InvalidResource)
            .at_resource(resource)
            .at_range(range));
    }
    if range.start >= range.end || range.end > input.resource_size(resource) {
        return Err(Error::new(ErrorKind::InvalidRange)
            .at_resource(resource)
            .at_range(range));
    }
    Ok(())
}

fn reserve<T>(capacity: usize, kind: Capacity) -> Result<Vec<T>, Error> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(capacity)
        .map_err(|_| Error::capacity(kind))?;
    Ok(result)
}

fn push<T>(items: &mut Vec<T>, value: T, limit: usize, kind: Capacity) -> Result<(), Error> {
    if items.len() >= limit || items.len() == items.capacity() {
        return Err(Error::capacity(kind));
    }
    items.push(value);
    Ok(())
}

fn replace<T: Copy>(
    items: &mut Vec<T>,
    range: Range<usize>,
    replacement: &[T],
    limit: usize,
    kind: Capacity,
) -> Result<(), Error> {
    let old_len = items.len();
    let new_len = old_len - (range.end - range.start) + replacement.len();
    if new_len > limit || new_len > items.capacity() {
        return Err(Error::capacity(kind));
    }
    if new_len > old_len {
        items.resize(new_len, replacement[0]);
    }
    items.copy_within(range.end..old_len, range.start + replacement.len());
    items[range.start..range.start + replacement.len()].copy_from_slice(replacement);
    items.truncate(new_len);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Resource {
        size: u64,
        initial: Vec<Range<u64>>,
        external: bool,
    }
    struct Pass {
        accesses: Vec<Access>,
        dependencies: Vec<usize>,
        side_effect: bool,
    }
    struct Fixture {
        resources: Vec<Resource>,
        passes: Vec<Pass>,
        exports: Vec<(usize, Range<u64>)>,
    }

    impl Input for Fixture {
        fn resource_count(&self) -> usize {
            self.resources.len()
        }
        fn resource_size(&self, resource: usize) -> u64 {
            self.resources[resource].size
        }
        fn initial_ranges(&self, resource: usize) -> &[Range<u64>] {
            &self.resources[resource].initial
        }
        fn resource_external(&self, resource: usize) -> bool {
            self.resources[resource].external
        }
        fn pass_count(&self) -> usize {
            self.passes.len()
        }
        fn access_count(&self) -> usize {
            self.passes.iter().map(|p| p.accesses.len()).sum()
        }
        fn pass_side_effect(&self, pass: usize) -> bool {
            self.passes[pass].side_effect
        }
        fn pass_dependencies(&self, pass: usize) -> &[usize] {
            &self.passes[pass].dependencies
        }
        fn pass_accesses(&self, pass: usize) -> impl Iterator<Item = Access> {
            self.passes[pass].accesses.iter().cloned()
        }
        fn exports(&self) -> impl Iterator<Item = (usize, Range<u64>)> {
            self.exports.iter().cloned()
        }
    }

    fn limits() -> Limits {
        Limits {
            resources: 4,
            passes: 16,
            accesses: 64,
            edges: 128,
            segments: 128,
            readers: 128,
        }
    }
    fn fixture(initial: Vec<Range<u64>>) -> Fixture {
        Fixture {
            resources: vec![Resource {
                size: 16,
                initial,
                external: false,
            }],
            passes: vec![],
            exports: vec![],
        }
    }
    fn read(range: Range<u64>) -> Access {
        Access {
            resource: 0,
            range,
            read: true,
            write: false,
            valid_after: false,
        }
    }
    fn write(range: Range<u64>) -> Access {
        Access {
            resource: 0,
            range,
            read: false,
            write: true,
            valid_after: true,
        }
    }
    fn pass(accesses: Vec<Access>, side_effect: bool) -> Pass {
        Pass {
            accesses,
            dependencies: vec![],
            side_effect,
        }
    }

    #[test]
    fn reused_workspace_replans_values_and_culls_overwritten_writers() {
        let mut work = Workspace::new(limits()).unwrap();
        let mut out = Output::new(limits()).unwrap();
        let mut graph = fixture(vec![]);
        graph.passes = vec![
            pass(vec![write(0..16)], false),
            pass(vec![read(0..16)], false),
            pass(vec![write(0..16)], false),
            pass(vec![read(0..16)], true),
        ];
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.order, [2, 3]);
        assert_eq!(out.live, [false, false, true, true]);
        assert_eq!(out.final_contents(0), [0..16]);
        assert_eq!(out.data_edges, [(0, 1), (2, 3)]);

        graph.passes = vec![pass(vec![write(4..8)], false)];
        analyze(&graph, &mut work, &mut out).unwrap();
        assert!(out.order.is_empty());
        assert!(out.final_contents(0).is_empty());
        graph.exports.push((0, 4..8));
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.order, [0]);
        assert_eq!(out.final_contents(0), [4..8]);
    }

    #[test]
    fn holes_and_capacity_errors_do_not_poison_later_analysis() {
        let limits = Limits {
            readers: 0,
            ..limits()
        };
        let mut work = Workspace::new(limits).unwrap();
        let mut out = Output::new(limits).unwrap();
        let mut graph = fixture(vec![]);
        graph.passes = vec![
            pass(vec![write(0..4)], false),
            pass(vec![write(8..12)], false),
            pass(vec![read(0..12)], false),
        ];
        assert_eq!(
            analyze(&graph, &mut work, &mut out),
            Err(Error::new(ErrorKind::UninitializedRead)
                .at_pass(2)
                .at_resource(0)
                .at_range(4..8))
        );
        graph.passes = vec![
            pass(vec![write(0..16)], false),
            pass(vec![read(0..16)], true),
        ];
        assert_eq!(
            analyze(&graph, &mut work, &mut out).unwrap_err().kind,
            ErrorKind::Capacity(Capacity::Readers)
        );
        graph.passes.pop();
        graph.exports.push((0, 0..16));
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.order, [0]);
        assert_eq!(out.final_contents(0), [0..16]);
    }

    #[test]
    fn live_frontier_ignores_culled_readers_without_losing_war() {
        let mut graph = fixture(vec![0..16]);
        graph.passes = vec![
            pass(vec![read(0..16)], true),
            pass(vec![read(0..16)], false),
            pass(vec![write(0..4)], false),
            pass(vec![read(0..16)], true),
        ];
        let mut work = Workspace::new(limits()).unwrap();
        let mut out = Output::new(limits()).unwrap();
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.order, [0, 2, 3]);
        assert!(out.ordering_edges.contains(&Edge {
            from: 0,
            to: 2,
            kind: DependencyKind::War
        }));
        assert!(!out.ordering_edges.iter().any(|e| e.from == 1 || e.to == 1));

        graph.resources[0].initial.clear();
        graph.passes = vec![
            pass(vec![write(0..4)], false),
            pass(vec![read(0..4), write(4..8)], true),
        ];
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.order, [0, 1]);
        assert!(!out.ordering_edges.iter().any(|e| e.from == e.to));
        assert_eq!(out.final_contents(0), [0..8]);
    }

    #[test]
    fn read_write_needs_prior_contents_and_cycle_witness_excludes_blocked_tail() {
        let mut graph = fixture(vec![]);
        graph.passes.push(pass(
            vec![Access {
                read: true,
                ..write(0..4)
            }],
            true,
        ));
        let mut work = Workspace::new(limits()).unwrap();
        let mut out = Output::new(limits()).unwrap();
        assert_eq!(
            analyze(&graph, &mut work, &mut out).unwrap_err().kind,
            ErrorKind::UninitializedRead
        );
        graph.resources[0].initial.push(0..16);
        analyze(&graph, &mut work, &mut out).unwrap();
        graph.passes = vec![pass(vec![], true), pass(vec![], false), pass(vec![], true)];
        graph.passes[0].dependencies.push(1);
        graph.passes[1].dependencies.push(0);
        graph.passes[2].dependencies.push(0);
        assert_eq!(
            analyze(&graph, &mut work, &mut out).unwrap_err().kind,
            ErrorKind::DependencyCycle
        );
        assert_eq!(out.cycle, [0, 1, 0]);
        graph.passes[0].dependencies.clear();
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.order, [0, 2]);
        assert!(out.cycle.is_empty());

        #[cfg(feature = "development-validation")]
        {
            graph.passes = vec![pass(vec![read(0..8), write(4..12)], true)];
            let error = analyze(&graph, &mut work, &mut out).unwrap_err();
            assert_eq!(error.kind, ErrorKind::ConflictingAccesses);
            assert_eq!(error.range, Some(4..12));
            graph.passes[0].accesses = vec![Access {
                read: false,
                write: false,
                ..read(0..4)
            }];
            assert_eq!(
                analyze(&graph, &mut work, &mut out).unwrap_err().kind,
                ErrorKind::InvalidAccess
            );
            graph.passes[0].accesses = vec![Access {
                resource: 1,
                ..read(0..4)
            }];
            let error = analyze(&graph, &mut work, &mut out).unwrap_err();
            assert_eq!(error.kind, ErrorKind::InvalidResource);
            assert_eq!(error.range, Some(0..4));
        }
    }

    #[test]
    fn sparse_hazards_preserve_dense_scheduling_and_cycle_decisions() {
        use std::collections::BTreeSet;

        // All-pairs conflicts define the reference independently of the sparse
        // frontier and its storage. Try masks directly to exercise live-frontier
        // rebuilding even when a removed access used to bridge two live ones.
        fn reference(graph: &Fixture, live: &[bool]) -> BTreeSet<Edge> {
            let mut edges = BTreeSet::new();
            for (to, pass) in graph.passes.iter().enumerate().filter(|&(p, _)| live[p]) {
                for &from in &pass.dependencies {
                    if live[from] {
                        edges.insert(Edge {
                            from,
                            to,
                            kind: DependencyKind::Explicit,
                        });
                    }
                }
                for (from, previous) in graph.passes[..to]
                    .iter()
                    .enumerate()
                    .filter(|&(p, _)| live[p])
                {
                    for before in &previous.accesses {
                        for after in &pass.accesses {
                            if before.resource != after.resource
                                || before.range.start >= after.range.end
                                || after.range.start >= before.range.end
                            {
                                continue;
                            }
                            for (required, kind) in [
                                (before.write && after.read, DependencyKind::Raw),
                                (before.read && after.write, DependencyKind::War),
                                (before.write && after.write, DependencyKind::Waw),
                            ] {
                                if required {
                                    edges.insert(Edge { from, to, kind });
                                }
                            }
                        }
                    }
                }
            }
            edges
        }

        fn reference_order(edges: &BTreeSet<Edge>, live: &[bool]) -> Option<Vec<usize>> {
            let mut done = vec![false; live.len()];
            let mut order = Vec::new();
            while order.len() < live.iter().filter(|&&v| v).count() {
                let ready = (0..live.len()).find(|&p| {
                    live[p] && !done[p] && edges.iter().filter(|e| e.to == p).all(|e| done[e.from])
                })?;
                done[ready] = true;
                order.push(ready);
            }
            Some(order)
        }

        let mut work = Workspace::new(limits()).unwrap();
        let mut out = Output::new(limits()).unwrap();
        for seed in 0..32_u64 {
            let mut state = seed;
            let mut next = || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                state >> 32
            };
            let graph = Fixture {
                resources: (0..2)
                    .map(|_| Resource {
                        size: 16,
                        initial: vec![0..16],
                        external: false,
                    })
                    .collect(),
                passes: (0..6)
                    .map(|_| {
                        let resource = (next() % 2) as usize;
                        let start = next() % 4 * 4;
                        let end = start + (1 + next() % ((16 - start) / 4)) * 4;
                        let mode = next() % 3;
                        Pass {
                            accesses: vec![Access {
                                resource,
                                range: start..end,
                                read: mode != 1,
                                write: mode != 0,
                                valid_after: true,
                            }],
                            dependencies: if next() % 3 == 0 {
                                vec![(next() % 6) as usize]
                            } else {
                                vec![]
                            },
                            side_effect: false,
                        }
                    })
                    .collect(),
                exports: vec![],
            };
            for mask in 0..64_u64 {
                work.reset(graph.pass_count());
                out.reset(graph.resource_count(), graph.pass_count())
                    .unwrap();
                for (p, value) in out.live.iter_mut().enumerate() {
                    *value = mask & (1 << p) != 0;
                }
                let dense = reference_order(&reference(&graph, &out.live), &out.live);
                work.ordering(&graph, &out.live).unwrap();
                match (work.topological_live_output(&mut out), dense) {
                    (Ok(()), Some(expected)) => {
                        assert_eq!(out.order, expected, "seed={seed} mask={mask}")
                    }
                    (Err(error), None) => assert_eq!(error.kind, ErrorKind::DependencyCycle),
                    (actual, expected) => panic!(
                        "different scheduling/cycle decision seed={seed} mask={mask}: {actual:?} {expected:?}"
                    ),
                }
            }
        }
    }

    #[test]
    fn splitting_read_ranges_preserves_all_readers_within_prepared_history_capacity() {
        let count = 1_024_usize;
        let size = count as u64 * 4;
        let writer = count * 2;
        let limits = Limits {
            resources: 1,
            passes: writer + 2,
            accesses: writer + 2,
            edges: writer + 1,
            segments: count,
            readers: writer + 1,
        };
        let mut graph = fixture(vec![0..size]);
        graph.resources[0].size = size;
        graph
            .passes
            .extend((0..count).map(|_| pass(vec![read(0..size)], true)));
        graph
            .passes
            .extend((0..count).map(|i| pass(vec![read(i as u64 * 4..i as u64 * 4 + 4)], true)));
        graph.passes.push(pass(vec![write(0..size)], true));
        graph.passes.push(pass(vec![read(0..size)], true));
        let mut work = Workspace::new(limits).unwrap();
        let mut out = Output::new(limits).unwrap();
        // The declared capacity admits each whole or partial read once. A split
        // that cloned all prior histories would fail this ordinary analysis.
        analyze(&graph, &mut work, &mut out).unwrap();
        for from in 0..writer {
            assert!(
                out.ordering_edges
                    .binary_search(&Edge {
                        from,
                        to: writer,
                        kind: DependencyKind::War
                    })
                    .is_ok()
            );
        }
        let after_write = out
            .ordering_edges
            .iter()
            .filter(|edge| edge.to == writer + 1)
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            after_write,
            [Edge {
                from: writer,
                to: writer + 1,
                kind: DependencyKind::Raw
            }]
        );
        assert_eq!(out.final_contents(0), [0..size]);
    }

    #[test]
    fn partial_writes_retire_only_readers_of_the_overwritten_range() {
        let mut graph = fixture(vec![0..8]);
        graph.resources[0].size = 8;
        graph.passes = vec![
            pass(vec![read(0..8)], true),
            pass(vec![read(0..4)], true),
            pass(vec![write(0..4)], true),
            pass(vec![write(0..8)], true),
        ];
        let mut work = Workspace::new(limits()).unwrap();
        let mut out = Output::new(limits()).unwrap();
        analyze(&graph, &mut work, &mut out).unwrap();
        let incoming = out
            .ordering_edges
            .iter()
            .filter(|edge| edge.to == 3)
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            incoming,
            [
                Edge {
                    from: 0,
                    to: 3,
                    kind: DependencyKind::War
                },
                Edge {
                    from: 2,
                    to: 3,
                    kind: DependencyKind::Waw
                }
            ]
        );
    }

    #[test]
    fn partial_final_export_keeps_only_required_producers_and_hazards() {
        let mut graph = fixture(vec![]);
        graph.resources[0].size = 8;
        graph.passes = vec![
            pass(vec![write(0..8)], false),
            pass(vec![read(0..4)], false),
            pass(vec![write(0..8)], false),
            pass(vec![read(0..4)], true),
            pass(vec![write(0..4)], false),
        ];
        graph.exports.push((0, 0..8));
        let mut work = Workspace::new(limits()).unwrap();
        let mut out = Output::new(limits()).unwrap();
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.order, [2, 3, 4]);
        assert_eq!(
            out.data_edges
                .iter()
                .filter(|&&(_, to)| to == 3)
                .copied()
                .collect::<Vec<_>>(),
            [(2, 3)]
        );
        for (from, to, kind) in [
            (2, 3, DependencyKind::Raw),
            (3, 4, DependencyKind::War),
            (2, 4, DependencyKind::Waw),
        ] {
            assert!(out.ordering_edges.contains(&Edge { from, to, kind }));
        }
        assert_eq!(out.final_contents(0), [0..8]);
    }

    #[test]
    fn independent_ranges_resolve_exact_producers_without_read_read_ordering() {
        let mut graph = fixture(vec![]);
        graph.resources[0].size = 12;
        graph.passes = vec![
            pass(vec![write(0..4)], false),
            pass(vec![write(4..12)], false),
            pass(vec![read(0..4)], true),
            pass(vec![read(4..12)], true),
            pass(vec![read(0..12)], true),
        ];
        let mut work = Workspace::new(limits()).unwrap();
        let mut out = Output::new(limits()).unwrap();
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.data_edges, [(0, 2), (1, 3), (0, 4), (1, 4)]);
        assert!(out.ordering_edges.iter().all(|edge| edge.from < 2));
        assert_eq!(out.lifetimes[0], Some((0, 4)));
    }

    #[test]
    fn exports_and_external_writes_publish_only_live_final_contents() {
        let mut graph = fixture(vec![]);
        graph.resources[0].size = 4;
        graph.resources[0].external = true;
        graph.resources.push(Resource {
            size: 4,
            initial: vec![],
            external: false,
        });
        graph.passes = vec![
            pass(vec![write(0..4)], false),
            pass(
                vec![Access {
                    resource: 1,
                    ..write(0..4)
                }],
                false,
            ),
        ];
        let mut work = Workspace::new(limits()).unwrap();
        let mut out = Output::new(limits()).unwrap();
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.order, [0]);
        assert_eq!(out.final_contents(0), [0..4]);
        assert!(out.final_contents(1).is_empty());
        graph.exports.push((1, 0..4));
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.order, [0, 1]);
        assert_eq!(out.final_contents(1), [0..4]);
    }

    #[test]
    fn dead_cycles_are_optional_diagnostics_and_live_cycles_always_fail() {
        let mut graph = fixture(vec![]);
        graph.passes = vec![pass(vec![], false), pass(vec![], false)];
        graph.passes[0].dependencies.push(1);
        graph.passes[1].dependencies.push(0);
        let mut work = Workspace::new(limits()).unwrap();
        let mut out = Output::new(limits()).unwrap();
        if cfg!(feature = "development-validation") {
            assert_eq!(
                analyze(&graph, &mut work, &mut out).unwrap_err().kind,
                ErrorKind::DependencyCycle
            );
        } else {
            analyze(&graph, &mut work, &mut out).unwrap();
            assert!(out.order.is_empty());
        }
        graph.passes[0].side_effect = true;
        assert_eq!(
            analyze(&graph, &mut work, &mut out).unwrap_err().kind,
            ErrorKind::DependencyCycle
        );
        graph.passes = vec![
            pass(vec![write(0..4)], false),
            pass(vec![write(0..4)], false),
            pass(vec![read(0..4)], false),
        ];
        graph.passes[0].dependencies.push(2);
        if cfg!(feature = "development-validation") {
            assert_eq!(
                analyze(&graph, &mut work, &mut out).unwrap_err().kind,
                ErrorKind::DependencyCycle
            );
        } else {
            analyze(&graph, &mut work, &mut out).unwrap();
            assert!(out.order.is_empty());
        }
        graph.passes = vec![pass(vec![], true), pass(vec![], false)];
        graph.passes[0].dependencies.push(1);
        analyze(&graph, &mut work, &mut out).unwrap();
        assert_eq!(out.order, [1, 0]);
    }

    #[cfg(feature = "development-validation")]
    #[test]
    fn pass_conflicts_match_the_pairwise_contract_and_later_declaration_context() {
        let mut graph = Fixture {
            resources: (0..4)
                .map(|_| Resource {
                    size: 64,
                    initial: vec![],
                    external: false,
                })
                .collect(),
            passes: vec![],
            exports: vec![],
        };
        let mut work = Workspace::new(limits()).unwrap();
        for seed in 0..4_096_u64 {
            let mut state = seed;
            let mut next = || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                state >> 32
            };
            let count = next() as usize % 17;
            let accesses = (0..count)
                .map(|_| {
                    let resource = (next() % 4) as usize;
                    let start = next() % 16 * 4;
                    let end = start + (1 + next() % ((64 - start) / 4)) * 4;
                    let mode = next() % 3;
                    Access {
                        resource,
                        range: start..end,
                        read: mode != 1,
                        write: mode != 0,
                        valid_after: mode != 0,
                    }
                })
                .collect::<Vec<_>>();
            let expected = accesses
                .iter()
                .enumerate()
                .find_map(|(left, access)| {
                    accesses[left + 1..].iter().find(|other| {
                        access.resource == other.resource
                            && access.range.start < other.range.end
                            && other.range.start < access.range.end
                            && (access.write || other.write)
                    })
                })
                .map(|later| (later.resource, later.range.clone()));
            graph.passes = vec![pass(accesses, false)];
            match (validate(&graph, &mut work), expected) {
                (Ok(()), None) => {}
                (Err(error), Some((resource, range))) => {
                    assert_eq!(error.kind, ErrorKind::ConflictingAccesses, "seed={seed}");
                    assert_eq!(
                        (error.pass, error.resource, error.range.clone()),
                        (Some(0), Some(resource), Some(range)),
                        "seed={seed}"
                    );
                    assert_eq!(error, validate(&graph, &mut work).unwrap_err());
                }
                (actual, expected) => {
                    panic!("conflict mismatch seed={seed}: {actual:?} {expected:?}")
                }
            }
        }
    }

    #[cfg(feature = "development-validation")]
    #[test]
    fn large_pass_validation_keeps_range_first_and_overlapping_reader_contracts() {
        let count = 8_192;
        let mut graph = Fixture {
            resources: (0..count + 2)
                .map(|_| Resource {
                    size: 64,
                    initial: vec![],
                    external: false,
                })
                .collect(),
            passes: vec![pass(
                (0..count)
                    .map(|resource| Access {
                        resource,
                        ..read(0..64)
                    })
                    .collect(),
                false,
            )],
            exports: vec![],
        };
        graph.passes[0]
            .accesses
            .extend((count..count + 2).map(|resource| Access {
                resource,
                ..write(0..64)
            }));
        let limits = Limits {
            resources: count + 2,
            passes: 1,
            accesses: count + 2,
            edges: 1,
            segments: count + 2,
            readers: count + 2,
        };
        let mut work = Workspace::new(limits).unwrap();
        validate(&graph, &mut work).unwrap();
        graph.passes[0].accesses = vec![read(0..64); count];
        validate(&graph, &mut work).unwrap();
        graph.passes[0].accesses.push(write(32..64));
        graph.passes[0].accesses.push(read(64..65));
        let invalid = validate(&graph, &mut work).unwrap_err();
        assert_eq!(invalid.kind, ErrorKind::InvalidRange);
        assert_eq!(invalid.range, Some(64..65));
        graph.passes[0].accesses.pop();
        let conflict = validate(&graph, &mut work).unwrap_err();
        assert_eq!(conflict.kind, ErrorKind::ConflictingAccesses);
        assert_eq!(
            (conflict.pass, conflict.resource, conflict.range),
            (Some(0), Some(0), Some(32..64))
        );
    }
}
