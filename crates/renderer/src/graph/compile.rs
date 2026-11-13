//! Deterministic dataflow analysis. Native barriers remain a backend decision.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
#[cfg(feature = "graph-diagnostics")]
use std::sync::OnceLock;

#[cfg(any(test, feature = "development-validation"))]
use super::accesses::conflicting_accesses;
use super::{Graph, GraphError, GraphErrorKind, Operation, Pass, PassHandle, Resource};

/// Validated, culled logical work. This contains no native synchronization plan.
#[derive(Clone, Debug)]
#[cfg_attr(
    not(feature = "vulkan"),
    allow(
        dead_code,
        reason = "physical materialization is in the optional Vulkan runtime"
    )
)]
pub struct CompiledGraph {
    pub(crate) id: u64,
    pub(crate) resources: Vec<Resource>,
    pub(crate) passes: Vec<Pass>,
    pub(crate) exports: Vec<usize>,
    exported: Vec<bool>,
    pub(crate) order: Vec<usize>,
    pub(crate) final_contents: Vec<Vec<Range<u64>>>,
    #[cfg(any(test, feature = "graph-diagnostics"))]
    analysis: Analysis,
    #[cfg(feature = "graph-diagnostics")]
    dump: OnceLock<String>,
}

impl CompiledGraph {
    /// Membership is independent of declaration order and resource count.
    #[cfg_attr(
        not(feature = "vulkan"),
        allow(dead_code, reason = "used by the Vulkan engine")
    )]
    pub(crate) fn has_export(&self, resource: usize) -> bool {
        self.exported.get(resource).copied().unwrap_or(false)
    }

    pub fn graph_id(&self) -> u64 {
        self.id
    }

    /// Number of retained operations; declarations culled by analysis are absent.
    pub fn pass_count(&self) -> usize {
        self.order.len()
    }

    pub fn ordered_pass_names(&self) -> Vec<&str> {
        self.order
            .iter()
            .map(|&index| self.passes[index].name.as_str())
            .collect()
    }

    /// Names of declarations excluded from execution. Retaining these names is
    /// optional diagnostic work, independent of development validation.
    #[cfg(feature = "graph-diagnostics")]
    pub fn culled_pass_names(&self) -> Vec<&str> {
        self.passes
            .iter()
            .filter(|pass| matches!(pass.operation, Operation::Culled))
            .map(|pass| pass.name.as_str())
            .collect()
    }

    /// Stable diagnostic text, with data dependencies separate from ordering.
    /// Available with `graph-diagnostics`, independently of development validation.
    #[cfg(feature = "graph-diagnostics")]
    pub fn dump(&self) -> &str {
        self.dump
            .get_or_init(|| dump(&self.resources, &self.passes, &self.exports, &self.analysis))
    }
}

pub(super) fn compile(graph: &Graph) -> Result<CompiledGraph, GraphError> {
    let resources = graph
        .resources
        .iter()
        .map(|resource| CompileResource {
            name: resource.name.clone(),
            size: resource.desc.units(),
            initial: resource.initial.clone(),
            external: resource.import.is_some(),
        })
        .collect::<Vec<_>>();
    let passes = graph
        .passes
        .iter()
        .map(|pass| CompilePass {
            name: pass.name.clone(),
            accesses: pass
                .accesses
                .iter()
                .map(|access| CompileAccess {
                    resource: access.resource,
                    range: access.range.clone(),
                    read: access.read,
                    write: access.write,
                    valid_after: access.initializes && !access.discard_after,
                })
                .collect(),
            dependencies: pass.dependencies.clone(),
            side_effect: pass.side_effect,
        })
        .collect::<Vec<_>>();
    let exports = graph
        .exports
        .iter()
        .map(|&resource| (resource, 0..graph.resources[resource].desc.units()))
        .collect::<Vec<_>>();
    let analysis =
        analyze(&resources, &passes, &exports).map_err(|error| error.into_graph_error(graph))?;
    // Culled writers must not publish valid runtime contents. Recompute the
    // final state using only the operations that will actually execute.
    let mut contents = resources
        .iter()
        .map(|resource| ContentMap::new(resource.size, &resource.initial))
        .collect::<Vec<_>>();
    #[cfg(not(feature = "graph-diagnostics"))]
    let mut used_resources = {
        // Reuse export membership, then mark live accesses in the existing pass
        // walk. This temporary index is not retained in the completed plan.
        let mut used = graph.exported.clone();
        used.resize(resources.len(), false);
        used
    };
    for &index in &analysis.order {
        for access in &passes[index].accesses {
            #[cfg(not(feature = "graph-diagnostics"))]
            {
                used_resources[access.resource] = true;
            }
            if access.write {
                contents[access.resource].write(
                    access.range.clone(),
                    Contents {
                        initialized: access.valid_after,
                        producer: Some(index),
                    },
                );
            }
        }
    }
    #[cfg(feature = "graph-diagnostics")]
    let compiled_resources = graph.resources.clone();
    #[cfg(not(feature = "graph-diagnostics"))]
    let compiled_resources = graph
        .resources
        .iter()
        .enumerate()
        .map(|(index, resource)| Resource {
            // Imports can fail the submit-time canonical revision check even
            // without a live access; retain their operational error context.
            name: if used_resources[index] || resource.import.is_some() {
                resource.name.clone()
            } else {
                String::new()
            },
            desc: resource.desc,
            import: resource.import,
            import_revision: resource.import_revision,
            initial: resource.initial.clone(),
        })
        .collect();
    Ok(CompiledGraph {
        id: graph.id,
        resources: compiled_resources,
        passes: graph
            .passes
            .iter()
            .enumerate()
            .map(|(index, pass)| Pass {
                name: if analysis.live[index] || cfg!(feature = "graph-diagnostics") {
                    pass.name.clone()
                } else {
                    String::new()
                },
                operation: if analysis.live[index] {
                    pass.operation.clone()
                } else {
                    Operation::Culled
                },
                // Culled access metadata only serves the optional dump. Live
                // accesses are still needed for native synchronization planning.
                accesses: if analysis.live[index] || cfg!(feature = "graph-diagnostics") {
                    pass.accesses.clone()
                } else {
                    Vec::new()
                },
                side_effect: pass.side_effect,
                // The order is already fixed; explicit edges are not runtime
                // operands. The optional Analysis carries their diagnostic view.
                dependencies: Vec::new(),
            })
            .collect(),
        exports: graph.exports.clone(),
        exported: graph.exported.clone(),
        order: analysis.order.clone(),
        final_contents: contents.iter().map(ContentMap::initialized).collect(),
        #[cfg(any(test, feature = "graph-diagnostics"))]
        analysis,
        #[cfg(feature = "graph-diagnostics")]
        dump: OnceLock::new(),
    })
}

#[derive(Clone, Debug)]
pub(super) struct CompileResource {
    pub name: String,
    pub size: u64,
    pub initial: Vec<Range<u64>>,
    pub external: bool,
}

#[derive(Clone, Debug)]
pub(super) struct CompileAccess {
    pub resource: usize,
    pub range: Range<u64>,
    pub read: bool,
    pub write: bool,
    pub valid_after: bool,
}

#[derive(Clone, Debug)]
pub(super) struct CompilePass {
    pub name: String,
    pub accesses: Vec<CompileAccess>,
    /// Explicit prerequisites are semantic dependencies and keep producers live.
    pub dependencies: Vec<usize>,
    pub side_effect: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum DependencyKind {
    Raw,
    War,
    Waw,
    Explicit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Dependency {
    pub from: usize,
    pub to: usize,
    pub kind: DependencyKind,
}

#[derive(Clone, Debug)]
pub(super) struct Analysis {
    pub order: Vec<usize>,
    pub live: Vec<bool>,
    #[cfg(any(test, feature = "graph-diagnostics"))]
    pub data_dependencies: Vec<Vec<usize>>,
    #[cfg(any(test, feature = "graph-diagnostics"))]
    pub ordering_dependencies: Vec<Dependency>,
    /// Inclusive positions in `order`, absent for resources unused by live work.
    #[cfg(any(test, feature = "graph-diagnostics"))]
    pub lifetimes: Vec<Option<(usize, usize)>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CompileError {
    kind: GraphErrorKind,
    message: String,
    pass: Option<usize>,
    resource: Option<usize>,
    range: Option<Range<u64>>,
    cycle: Vec<usize>,
}
impl CompileError {
    fn new(kind: GraphErrorKind, message: String) -> Self {
        Self {
            kind,
            message,
            pass: None,
            resource: None,
            range: None,
            cycle: Vec::new(),
        }
    }
    fn at_pass(mut self, index: usize) -> Self {
        self.pass = Some(index);
        self
    }
    fn at_resource(mut self, index: usize) -> Self {
        self.resource = Some(index);
        self
    }
    fn at_range(mut self, range: Range<u64>) -> Self {
        self.range = Some(range);
        self
    }
    fn into_graph_error(self, graph: &Graph) -> GraphError {
        let mut error = GraphError::new(self.kind, self.message);
        if let Some(index) = self.pass {
            error = error.with_pass(
                PassHandle {
                    graph: graph.id,
                    index,
                },
                graph.passes.get(index).map(|pass| pass.name.as_str()),
            );
        }
        if let Some(index) = self.resource {
            error = graph.resource_context(error, index);
            if let Some(range) = self.range.filter(|_| graph.resources.get(index).is_some()) {
                error = error.with_range(graph.access_range(index, range));
            }
        }
        if !self.cycle.is_empty() {
            error.context_mut().cycle_passes = self
                .cycle
                .into_iter()
                .map(|index| PassHandle {
                    graph: graph.id,
                    index,
                })
                .collect();
        }
        error
    }
}

/// Validate every declaration before culling. Construction order determines
/// which contents a read consumes; explicit edges constrain, but never rewrite,
/// that rule. Logical ranges are bytes for buffers and one unit for whole images.
pub(super) fn analyze(
    resources: &[CompileResource],
    passes: &[CompilePass],
    exports: &[(usize, Range<u64>)],
) -> Result<Analysis, CompileError> {
    // Graph's private builder establishes resource sizes, owner/range/purpose
    // validity and per-pass conflicts before producing this internal input.
    // Rechecking those immutable declarations is an independent development
    // diagnostic, not a prerequisite for execution planning below.
    #[cfg(feature = "development-validation")]
    validate(resources, passes, exports)?;
    let mut contents = resources
        .iter()
        .map(|resource| ContentMap::new(resource.size, &resource.initial))
        .collect::<Vec<_>>();
    let mut data = vec![BTreeSet::new(); passes.len()];
    let mut roots = BTreeSet::new();
    for (index, pass) in passes.iter().enumerate() {
        if pass.side_effect {
            roots.insert(index);
        }
        data[index].extend(pass.dependencies.iter().copied());
        // All reads precede this pass's writes: read_write cannot initialize
        // itself and accidental access ordering cannot hide an undefined read.
        for access in &pass.accesses {
            if access.read {
                let producers = contents[access.resource]
                    .read(access.range.clone())
                    .map_err(|hole| {
                        CompileError::new(
                            GraphErrorKind::UninitializedRead,
                            format!(
                                "pass {:?} reads uninitialized resource {:?} at {}..{}",
                                pass.name, resources[access.resource].name, hole.start, hole.end
                            ),
                        )
                        .at_pass(index)
                        .at_resource(access.resource)
                        .at_range(hole)
                    })?;
                data[index].extend(producers);
            }
        }
        for access in &pass.accesses {
            if access.write {
                if resources[access.resource].external {
                    roots.insert(index);
                }
                contents[access.resource].write(
                    access.range.clone(),
                    Contents {
                        initialized: access.valid_after,
                        producer: access.valid_after.then_some(index),
                    },
                );
            }
        }
    }
    for (resource, range) in exports {
        let producers = contents[*resource].read(range.clone()).map_err(|hole| {
            CompileError::new(
                GraphErrorKind::UninitializedRead,
                format!(
                    "export reads uninitialized resource {:?} at {}..{}",
                    resources[*resource].name, hole.start, hole.end
                ),
            )
            .at_resource(*resource)
            .at_range(hole)
        })?;
        roots.extend(producers);
    }

    // Scheduling edges do not define liveness. In particular WAW cannot keep
    // an overwritten writer alive, and WAR cannot keep an unrelated reader.
    #[cfg(feature = "development-validation")]
    {
        let all = vec![true; passes.len()];
        let dependencies = ordering(resources, passes, &all);
        // Detect cycles even when all involved work will be culled. Live cycles
        // are rejected by the mandatory scheduling step in every configuration.
        topological(passes, &dependencies, &all)?;
    }

    let mut live = vec![false; passes.len()];
    let mut pending = roots.into_iter().collect::<Vec<_>>();
    while let Some(index) = pending.pop() {
        if !live[index] {
            live[index] = true;
            pending.extend(data[index].iter().copied());
        }
    }
    // A culled access must not hide ordering between the remaining accesses.
    // Rebuild the frontier rather than filtering edges through removed passes.
    let dependencies = ordering(resources, passes, &live);
    let order = topological(passes, &dependencies, &live)?;
    #[cfg(any(test, feature = "graph-diagnostics"))]
    let ordering_dependencies = dependencies.into_iter().collect::<Vec<_>>();
    #[cfg(any(test, feature = "graph-diagnostics"))]
    let mut lifetimes = vec![None; resources.len()];
    #[cfg(any(test, feature = "graph-diagnostics"))]
    for (position, &index) in order.iter().enumerate() {
        for access in &passes[index].accesses {
            let lifetime = &mut lifetimes[access.resource];
            match lifetime {
                Some((_, last)) => *last = position,
                None => *lifetime = Some((position, position)),
            }
        }
    }
    #[cfg(any(test, feature = "graph-diagnostics"))]
    let data_dependencies = data
        .into_iter()
        .map(|dependencies| dependencies.into_iter().collect())
        .collect::<Vec<_>>();
    Ok(Analysis {
        order,
        live,
        #[cfg(any(test, feature = "graph-diagnostics"))]
        data_dependencies,
        #[cfg(any(test, feature = "graph-diagnostics"))]
        ordering_dependencies,
        #[cfg(any(test, feature = "graph-diagnostics"))]
        lifetimes,
    })
}

/// A resource-range frontier is sufficient to preserve every conflicting
/// access's order without storing transitive edges to all earlier passes.
fn ordering(
    resources: &[CompileResource],
    passes: &[CompilePass],
    live: &[bool],
) -> BTreeSet<Dependency> {
    let mut frontiers = resources
        .iter()
        .map(|resource| HazardMap::new(resource.size))
        .collect::<Vec<_>>();
    let mut dependencies = BTreeSet::new();
    for (index, pass) in passes.iter().enumerate().filter(|&(index, _)| live[index]) {
        for &from in &pass.dependencies {
            if live[from] {
                dependencies.insert(Dependency {
                    from,
                    to: index,
                    kind: DependencyKind::Explicit,
                });
            }
        }
        // All uses are compared with earlier passes before this pass is added;
        // overlapping reads and disjoint self-copies cannot create self edges.
        for access in &pass.accesses {
            frontiers[access.resource].dependencies(index, access, &mut dependencies);
        }
        for access in &pass.accesses {
            frontiers[access.resource].record(index, access);
        }
    }
    dependencies
}

#[derive(Clone)]
struct HazardSegment {
    end: u64,
    writer: Option<usize>,
    readers: Option<usize>,
}

/// An immutable reader history. Segment splits share its index rather than
/// cloning every reader. Arena indices also avoid recursive destruction of long
/// histories when a frontier or a whole compilation is dropped.
struct Reader {
    pass: usize,
    previous: Option<usize>,
}

struct HazardMap {
    segments: BTreeMap<u64, HazardSegment>,
    readers: Vec<Reader>,
}

impl HazardMap {
    fn new(size: u64) -> Self {
        Self {
            segments: BTreeMap::from([(
                0,
                HazardSegment {
                    end: size,
                    writer: None,
                    readers: None,
                },
            )]),
            readers: Vec::new(),
        }
    }

    fn dependencies(
        &self,
        to: usize,
        access: &CompileAccess,
        dependencies: &mut BTreeSet<Dependency>,
    ) {
        // Split segments can share an arbitrarily long tail. Visit it only
        // once per write, even when that write spans many such segments.
        let mut visited = BTreeSet::new();
        let (&first, _) = self
            .segments
            .range(..=access.range.start)
            .next_back()
            .expect("validated nonempty resource range");
        for segment in self
            .segments
            .range(first..access.range.end)
            .map(|(_, value)| value)
        {
            if let Some(from) = segment.writer {
                if access.read {
                    dependencies.insert(Dependency {
                        from,
                        to,
                        kind: DependencyKind::Raw,
                    });
                }
                if access.write {
                    dependencies.insert(Dependency {
                        from,
                        to,
                        kind: DependencyKind::Waw,
                    });
                }
            }
            if access.write {
                let mut reader = segment.readers;
                while let Some(index) = reader {
                    if !visited.insert(index) {
                        break;
                    }
                    let entry = &self.readers[index];
                    dependencies.insert(Dependency {
                        from: entry.pass,
                        to,
                        kind: DependencyKind::War,
                    });
                    reader = entry.previous;
                }
            }
        }
    }

    fn split(&mut self, point: u64) {
        let Some((&start, segment)) = self.segments.range(..=point).next_back() else {
            return;
        };
        if start == point || point >= segment.end {
            return;
        }
        let tail = segment.clone();
        self.segments.get_mut(&start).expect("existing segment").end = point;
        self.segments.insert(point, tail);
    }

    fn record(&mut self, pass: usize, access: &CompileAccess) {
        self.split(access.range.start);
        self.split(access.range.end);
        if access.write {
            let keys = self
                .segments
                .range(access.range.clone())
                .map(|(&start, _)| start)
                .collect::<Vec<_>>();
            for key in keys {
                self.segments.remove(&key);
            }
            self.segments.insert(
                access.range.start,
                HazardSegment {
                    end: access.range.end,
                    writer: Some(pass),
                    readers: None,
                },
            );
        } else {
            for segment in self
                .segments
                .range_mut(access.range.clone())
                .map(|(_, value)| value)
            {
                let index = self.readers.len();
                self.readers.push(Reader {
                    pass,
                    previous: segment.readers,
                });
                segment.readers = Some(index);
            }
        }
    }
}

#[cfg(test)]
fn overlaps(left: &Range<u64>, right: &Range<u64>) -> bool {
    left.start < right.end && right.start < left.end
}

#[cfg(any(test, feature = "development-validation"))]
fn validate(
    resources: &[CompileResource],
    passes: &[CompilePass],
    exports: &[(usize, Range<u64>)],
) -> Result<(), CompileError> {
    for (resource_index, resource) in resources.iter().enumerate() {
        if resource.size == 0 {
            return Err(CompileError::new(
                GraphErrorKind::InvalidInput,
                format!("resource {:?} has zero logical size", resource.name),
            )
            .at_resource(resource_index));
        }
        for range in &resource.initial {
            if range.start >= range.end || range.end > resource.size {
                return Err(CompileError::new(
                    GraphErrorKind::InvalidInput,
                    format!(
                        "resource {:?} has invalid initial range {:?}",
                        resource.name, range
                    ),
                )
                .at_resource(resource_index)
                .at_range(range.clone()));
            }
        }
    }
    for (pass_index, pass) in passes.iter().enumerate() {
        for &dependency in &pass.dependencies {
            if dependency >= passes.len() {
                return Err(CompileError::new(
                    GraphErrorKind::InvalidHandle,
                    format!(
                        "pass {:?} depends on undefined pass {}",
                        pass.name, dependency
                    ),
                )
                .at_pass(pass_index));
            }
        }
        // Validate in declaration order before interpreting any ranges below.
        for access in &pass.accesses {
            validate_range(resources, access.resource, &access.range).map_err(|mut error| {
                error.message = format!("pass {:?}: {}", pass.name, error.message);
                error.at_pass(pass_index)
            })?;
            if !access.read && !access.write {
                return Err(CompileError::new(
                    GraphErrorKind::InvalidInput,
                    format!(
                        "pass {:?} contains an access with no read or write",
                        pass.name
                    ),
                )
                .at_pass(pass_index)
                .at_resource(access.resource)
                .at_range(access.range.clone()));
            }
        }
        if let Some((left, right)) = conflicting_accesses(&pass.accesses, |access| {
            (access.resource, access.range.clone(), access.write)
        }) {
            // Attribute the conflict to the later original declaration. Sorting
            // is deterministic and does not change content/producer semantics.
            let access = &pass.accesses[left.max(right)];
            return Err(CompileError::new(GraphErrorKind::InvalidInput, format!(
                "pass {:?} has overlapping accesses to {:?}; use one read_write access or separate passes",
                pass.name, resources[access.resource].name
            )).at_pass(pass_index).at_resource(access.resource).at_range(access.range.clone()));
        }
    }
    for (resource, range) in exports {
        validate_range(resources, *resource, range).map_err(|mut error| {
            error.message = format!("export: {}", error.message);
            error
        })?;
    }
    Ok(())
}

#[cfg(any(test, feature = "development-validation"))]
fn validate_range(
    resources: &[CompileResource],
    resource: usize,
    range: &Range<u64>,
) -> Result<(), CompileError> {
    let resource_index = resource;
    let Some(resource) = resources.get(resource_index) else {
        return Err(CompileError::new(
            GraphErrorKind::InvalidHandle,
            format!("undefined resource {resource_index}"),
        )
        .at_resource(resource_index)
        .at_range(range.clone()));
    };
    if range.start >= range.end || range.end > resource.size {
        return Err(CompileError::new(
            GraphErrorKind::InvalidInput,
            format!(
                "resource {:?} range {:?} exceeds logical size {} or is empty",
                resource.name, range, resource.size
            ),
        )
        .at_resource(resource_index)
        .at_range(range.clone()));
    }
    Ok(())
}

fn topological(
    passes: &[CompilePass],
    dependencies: &BTreeSet<Dependency>,
    live: &[bool],
) -> Result<Vec<usize>, CompileError> {
    let mut incoming = vec![BTreeSet::new(); passes.len()];
    let mut outgoing = vec![BTreeSet::new(); passes.len()];
    for dependency in dependencies {
        if live[dependency.from] && live[dependency.to] {
            incoming[dependency.to].insert(dependency.from);
            outgoing[dependency.from].insert(dependency.to);
        }
    }
    let mut ready = (0..passes.len())
        .filter(|&index| live[index] && incoming[index].is_empty())
        .collect::<BTreeSet<_>>();
    let mut order = Vec::new();
    while let Some(index) = ready.pop_first() {
        order.push(index);
        for &next in &outgoing[index] {
            incoming[next].remove(&index);
            if incoming[next].is_empty() {
                ready.insert(next);
            }
        }
    }
    if order.len() != live.iter().filter(|&&value| value).count() {
        let start = (0..passes.len())
            .find(|&index| live[index] && !incoming[index].is_empty())
            .expect("unprocessed cyclic node");
        let mut path = Vec::new();
        let mut positions = BTreeMap::new();
        let mut current = start;
        loop {
            if let Some(&position) = positions.get(&current) {
                path.push(current);
                let cycle = path[position..].iter().rev().copied().collect::<Vec<_>>();
                let names = cycle
                    .iter()
                    .map(|&index| format!("{:?}", passes[index].name))
                    .collect::<Vec<_>>()
                    .join(" -> ");
                let mut error = CompileError::new(
                    GraphErrorKind::DependencyCycle,
                    format!("dependency cycle: {names}"),
                )
                .at_pass(cycle[0]);
                error.cycle = cycle;
                return Err(error);
            }
            positions.insert(current, path.len());
            path.push(current);
            // Following remaining predecessors necessarily reaches a cycle.
            current = *incoming[current].first().expect("remaining predecessor");
        }
    }
    Ok(order)
}

#[cfg(feature = "graph-diagnostics")]
fn dump(resources: &[Resource], passes: &[Pass], exports: &[usize], analysis: &Analysis) -> String {
    use std::fmt::Write;

    let mut output = String::from("RenderGraph plan (construction-order contents)\n");
    for (index, resource) in resources.iter().enumerate() {
        writeln!(
            output,
            "resource {index} {:?} size={} external={} lifetime={:?}",
            resource.name,
            resource.desc.units(),
            resource.import.is_some(),
            analysis.lifetimes[index]
        )
        .expect("writing String");
    }
    for (index, pass) in passes.iter().enumerate() {
        writeln!(
            output,
            "pass {index} {:?} {} data={:?}",
            pass.name,
            if analysis.live[index] {
                "live"
            } else {
                "culled"
            },
            analysis.data_dependencies[index]
        )
        .expect("writing String");
        for access in &pass.accesses {
            writeln!(
                output,
                "  resource={} range={:?} purpose={:?} read={} write={} valid_after={}",
                access.resource,
                access.range,
                access.purpose,
                access.read,
                access.write,
                access.initializes && !access.discard_after
            )
            .expect("writing String");
        }
    }
    for dependency in &analysis.ordering_dependencies {
        writeln!(
            output,
            "order {} -> {} {:?}",
            dependency.from, dependency.to, dependency.kind
        )
        .expect("writing String");
    }
    for &resource in exports {
        let range = 0..resources[resource].desc.units();
        writeln!(output, "export resource={resource} range={range:?}").expect("writing String");
    }
    writeln!(output, "execution {:?}", analysis.order).expect("writing String");
    output
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Contents {
    initialized: bool,
    producer: Option<usize>,
}

#[derive(Clone, Debug)]
struct Segment {
    end: u64,
    contents: Contents,
}

/// Full, non-overlapping partition of a resource's supported logical range.
/// Initial imported contents have no producer inside this graph.
struct ContentMap {
    segments: BTreeMap<u64, Segment>,
}

impl ContentMap {
    fn initialized(&self) -> Vec<Range<u64>> {
        let mut result: Vec<Range<u64>> = Vec::new();
        for (&start, segment) in &self.segments {
            if !segment.contents.initialized {
                continue;
            }
            if let Some(last) = result.last_mut()
                && last.end == start
            {
                last.end = segment.end;
            } else {
                result.push(start..segment.end);
            }
        }
        result
    }

    fn new(size: u64, initialized: &[Range<u64>]) -> Self {
        let mut result = Self {
            segments: BTreeMap::from([(
                0,
                Segment {
                    end: size,
                    contents: Contents {
                        initialized: false,
                        producer: None,
                    },
                },
            )]),
        };
        for range in initialized {
            result.write(
                range.clone(),
                Contents {
                    initialized: true,
                    producer: None,
                },
            );
        }
        result
    }

    fn split(&mut self, point: u64) {
        let Some((&start, segment)) = self.segments.range(..=point).next_back() else {
            return;
        };
        if start == point || point >= segment.end {
            return;
        }
        let tail = segment.clone();
        self.segments.get_mut(&start).expect("existing segment").end = point;
        self.segments.insert(point, tail);
    }

    fn write(&mut self, range: Range<u64>, contents: Contents) {
        self.split(range.start);
        self.split(range.end);
        let keys = self
            .segments
            .range(range.clone())
            .map(|(&start, _)| start)
            .collect::<Vec<_>>();
        for key in keys {
            self.segments.remove(&key);
        }
        self.segments.insert(
            range.start,
            Segment {
                end: range.end,
                contents,
            },
        );
    }

    fn read(&self, range: Range<u64>) -> Result<BTreeSet<usize>, Range<u64>> {
        let mut producers = BTreeSet::new();
        let (&first, _) = self
            .segments
            .range(..=range.start)
            .next_back()
            .expect("validated nonempty resource range");
        for (&start, segment) in self.segments.range(first..range.end) {
            if !segment.contents.initialized {
                return Err(start.max(range.start)..segment.end.min(range.end));
            }
            if let Some(producer) = segment.contents.producer {
                producers.insert(producer);
            }
        }
        Ok(producers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{
        Access, AccessKind, AccessPurpose, AccessRange, AttachmentOps, BufferDesc, BufferHandle,
        ImageDesc, ImportBuffer, LoadOp, PassKind, ResourceKey, StoreOp,
    };
    use crate::graph::{Attachment, LoadAction, RasterAttachments, RasterDraw, RasterParameters};

    fn buffer(graph: &mut Graph, size: u64) -> BufferHandle {
        graph.buffer("buffer", BufferDesc { size }).unwrap()
    }

    #[test]
    fn diagnostic_resource_names_are_retained_only_when_execution_can_use_them() {
        let mut graph = Graph::new();
        let dead = graph
            .buffer("dead transient", BufferDesc { size: 4 })
            .unwrap();
        graph
            .upload_buffer("dead upload", dead, 0, vec![0; 4])
            .unwrap();
        let live = graph
            .buffer("live transient", BufferDesc { size: 4 })
            .unwrap();
        graph
            .upload_buffer("live upload", live, 0, vec![0; 4])
            .unwrap();
        graph.readback_buffer("live readback", live, 0, 4).unwrap();
        let exported = graph.buffer("exported", BufferDesc { size: 4 }).unwrap();
        graph
            .upload_buffer("export upload", exported, 0, vec![0; 4])
            .unwrap();
        graph.export_buffer(exported).unwrap();
        let unused_import = graph
            .import_buffer(
                "unused import",
                &ImportBuffer {
                    key: ResourceKey {
                        owner: 1,
                        index: 0,
                        generation: 1,
                    },
                    revision: 7,
                    desc: BufferDesc { size: 4 },
                    initialized: vec![],
                },
            )
            .unwrap();

        let plan = graph.compile().unwrap();
        assert_eq!(plan.pass_count(), 3);
        assert_eq!(plan.resources[live.index].name, "live transient");
        assert_eq!(plan.resources[exported.index].name, "exported");
        assert_eq!(plan.resources[unused_import.index].name, "unused import");
        assert_eq!(
            plan.resources[dead.index].name,
            if cfg!(feature = "graph-diagnostics") {
                "dead transient"
            } else {
                ""
            },
        );
        // The string policy does not change resource indices, descriptions,
        // imported revisions or initialization used by execution planning.
        assert_eq!(plan.resources.len(), graph.resources.len());
        for (compiled, original) in plan.resources.iter().zip(&graph.resources) {
            assert_eq!(compiled.desc, original.desc);
            assert_eq!(compiled.import, original.import);
            assert_eq!(compiled.import_revision, original.import_revision);
            assert_eq!(compiled.initial, original.initial);
        }
    }

    #[test]
    fn pass_conflicts_match_pairwise_contract_across_resources_ranges_and_modes() {
        let resources = (0..4)
            .map(|index| CompileResource {
                name: format!("resource {index}"),
                size: 64,
                initial: vec![],
                external: false,
            })
            .collect::<Vec<_>>();
        for seed in 0..4_096u64 {
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
                    CompileAccess {
                        resource,
                        range: start..end,
                        read: mode != 1,
                        write: mode != 0,
                        valid_after: mode != 0,
                    }
                })
                .collect::<Vec<_>>();
            // The reference is the public contract, independent of sorting:
            // overlapping readers are allowed; any overlapping writer is not.
            let conflict = accesses.iter().enumerate().any(|(index, access)| {
                accesses[..index].iter().any(|previous| {
                    access.resource == previous.resource
                        && access.range.start < previous.range.end
                        && previous.range.start < access.range.end
                        && (access.write || previous.write)
                })
            });
            let passes = [CompilePass {
                name: "generated pass".into(),
                accesses,
                dependencies: vec![],
                side_effect: false,
            }];
            let result = validate(&resources, &passes, &[]);
            assert_eq!(result.is_err(), conflict, "seed={seed}");
            if let Err(error) = result {
                assert_eq!(error.kind, GraphErrorKind::InvalidInput);
                assert_eq!(error.pass, Some(0));
                assert!(passes[0].accesses.iter().any(|access| {
                    Some(access.resource) == error.resource
                        && Some(&access.range) == error.range.as_ref()
                }));
                assert_eq!(error, validate(&resources, &passes, &[]).unwrap_err());
            }
        }
    }

    #[test]
    fn large_passes_allow_many_mesh_inputs_and_overlapping_readers() {
        let mesh_count = 8_192;
        let resources = (0..mesh_count + 2)
            .map(|index| CompileResource {
                name: format!("resource {index}"),
                size: 64,
                initial: vec![],
                external: false,
            })
            .collect::<Vec<_>>();
        let read = |resource| CompileAccess {
            resource,
            range: 0..64,
            read: true,
            write: false,
            valid_after: false,
        };
        let mut pass = CompilePass {
            name: "many mesh inputs and two attachments".into(),
            accesses: (0..mesh_count).map(read).collect(),
            dependencies: vec![],
            side_effect: false,
        };
        for resource in mesh_count..mesh_count + 2 {
            pass.accesses.push(CompileAccess {
                write: true,
                read: false,
                valid_after: true,
                ..read(resource)
            });
        }
        validate(&resources, std::slice::from_ref(&pass), &[]).unwrap();
        pass.accesses = vec![read(0); mesh_count];
        validate(&resources, std::slice::from_ref(&pass), &[]).unwrap();
        pass.accesses.push(CompileAccess {
            resource: 0,
            range: 32..64,
            read: false,
            write: true,
            valid_after: true,
        });
        let error = validate(&resources, &[pass], &[]).unwrap_err();
        assert_eq!(error.resource, Some(0));
        assert_eq!(error.range, Some(32..64));
    }

    #[test]
    fn pass_validation_checks_input_ranges_before_conflicts_and_preserves_context() {
        let resources = [CompileResource {
            name: "buffer".into(),
            size: 8,
            initial: vec![],
            external: false,
        }];
        let read = |range| CompileAccess {
            resource: 0,
            range,
            read: true,
            write: false,
            valid_after: false,
        };
        let mut pass = CompilePass {
            name: "mixed ranges".into(),
            accesses: vec![
                read(0..4),
                CompileAccess {
                    write: true,
                    ..read(4..8)
                },
            ],
            dependencies: vec![],
            side_effect: false,
        };
        // Adjacent half-open ranges may read and write the same resource.
        validate(&resources, std::slice::from_ref(&pass), &[]).unwrap();
        pass.accesses[1].range = 2..8;
        pass.accesses.push(read(8..9));
        let invalid_range = validate(&resources, std::slice::from_ref(&pass), &[]).unwrap_err();
        assert_eq!(invalid_range.range, Some(8..9));
        pass.accesses.pop();
        let overlap = validate(&resources, &[pass], &[]).unwrap_err();
        assert_eq!(overlap.kind, GraphErrorKind::InvalidInput);
        assert_eq!(overlap.pass, Some(0));
        assert_eq!(overlap.resource, Some(0));
        assert_eq!(overlap.range, Some(2..8));
        assert!(overlap.message.contains("mixed ranges"));
        assert!(overlap.message.contains("buffer"));
    }

    fn read_pass(
        graph: &mut Graph,
        name: &str,
        resource: BufferHandle,
        range: Range<u64>,
        side_effect: bool,
    ) -> crate::graph::PassHandle {
        graph
            .declare_pass(
                name,
                PassKind::Copy,
                vec![Access {
                    resource: resource.into(),
                    range: AccessRange::Buffer(range),
                    kind: AccessKind::Read,
                    purpose: AccessPurpose::CopySource,
                }],
                side_effect,
            )
            .unwrap()
    }

    #[test]
    fn overwritten_writers_and_unrelated_readers_are_culled_without_losing_live_hazards() {
        let mut graph = Graph::new();
        let resource = buffer(&mut graph, 8);
        graph
            .upload_buffer("obsolete writer", resource, 0, vec![1; 8])
            .unwrap();
        read_pass(&mut graph, "unrelated reader", resource, 0..4, false);
        graph
            .upload_buffer("live producer", resource, 0, vec![2; 8])
            .unwrap();
        read_pass(&mut graph, "observable reader", resource, 0..4, true);
        graph
            .upload_buffer("final partial writer", resource, 0, vec![3; 4])
            .unwrap();
        graph.export_buffer(resource).unwrap();

        let compiled = graph.compile().unwrap();
        assert_eq!(
            compiled.ordered_pass_names(),
            ["live producer", "observable reader", "final partial writer"]
        );
        #[cfg(feature = "graph-diagnostics")]
        assert_eq!(
            compiled.culled_pass_names(),
            ["obsolete writer", "unrelated reader"]
        );
        assert_eq!(compiled.analysis.data_dependencies[3], [2]);
        for (from, to, kind) in [
            (2, 3, DependencyKind::Raw),
            (3, 4, DependencyKind::War),
            (2, 4, DependencyKind::Waw),
        ] {
            assert!(
                compiled
                    .analysis
                    .ordering_dependencies
                    .contains(&Dependency { from, to, kind })
            );
        }
        assert_eq!(
            compiled.final_contents,
            [Vec::from_iter(std::iter::once(0..8))]
        );
    }

    #[test]
    fn independent_ranges_resolve_exact_producers_and_reads_do_not_order_each_other() {
        let mut graph = Graph::new();
        let resource = buffer(&mut graph, 12);
        graph
            .upload_buffer("head", resource, 0, vec![1; 4])
            .unwrap();
        graph
            .upload_buffer("tail", resource, 4, vec![2; 8])
            .unwrap();
        read_pass(&mut graph, "read head", resource, 0..4, true);
        read_pass(&mut graph, "read tail", resource, 4..12, true);
        graph.readback_buffer("read all", resource, 0, 12).unwrap();
        let compiled = graph.compile().unwrap();
        assert_eq!(compiled.analysis.data_dependencies[2], [0]);
        assert_eq!(compiled.analysis.data_dependencies[3], [1]);
        assert_eq!(compiled.analysis.data_dependencies[4], [0, 1]);
        assert!(
            compiled
                .analysis
                .ordering_dependencies
                .iter()
                .all(|edge| edge.from < 2)
        );
        assert_eq!(compiled.analysis.lifetimes[0], Some((0, 4)));
    }

    #[test]
    fn culled_middle_accesses_do_not_remove_live_war_or_waw_ordering() {
        for reader in [false, true] {
            let mut graph = Graph::new();
            let resource = buffer(&mut graph, 4);
            let first = if reader {
                graph
                    .upload_buffer("initial", resource, 0, vec![0; 4])
                    .unwrap();
                read_pass(&mut graph, "live first", resource, 0..4, true)
            } else {
                graph
                    .upload_buffer("live first", resource, 0, vec![0; 4])
                    .unwrap()
            };
            graph
                .upload_buffer("dead overwrite", resource, 0, vec![1; 4])
                .unwrap();
            let last = graph
                .upload_buffer("live last", resource, 0, vec![2; 4])
                .unwrap();
            let gate = graph
                .declare_pass("gate", PassKind::Copy, vec![], false)
                .unwrap();
            graph.depends_on(first, gate).unwrap();
            if !reader {
                // An independent observable operation keeps the first writer;
                // the overwritten contents alone must not keep it live.
                let observed = graph
                    .declare_pass("observe first", PassKind::Copy, vec![], true)
                    .unwrap();
                graph.depends_on(observed, first).unwrap();
            }
            graph.export_buffer(resource).unwrap();
            let compiled = graph.compile().unwrap();
            let first_position = compiled
                .order
                .iter()
                .position(|&index| index == first.index)
                .unwrap();
            let last_position = compiled
                .order
                .iter()
                .position(|&index| index == last.index)
                .unwrap();
            assert!(first_position < last_position);
            #[cfg(feature = "graph-diagnostics")]
            assert_eq!(compiled.culled_pass_names(), ["dead overwrite"]);
            assert!(
                compiled
                    .analysis
                    .ordering_dependencies
                    .contains(&Dependency {
                        from: first.index,
                        to: last.index,
                        kind: if reader {
                            DependencyKind::War
                        } else {
                            DependencyKind::Waw
                        },
                    })
            );
        }
    }

    #[test]
    fn sparse_hazards_preserve_dense_scheduling_and_cycle_decisions() {
        // The simple all-pairs reference states the ordering contract. Small
        // generated sequences exercise split/bridged ranges, culling masks and
        // forward explicit edges without a wall-clock performance assertion.
        fn reference(passes: &[CompilePass], live: &[bool]) -> BTreeSet<Dependency> {
            let mut result = BTreeSet::new();
            for (to, pass) in passes.iter().enumerate().filter(|&(index, _)| live[index]) {
                for &from in &pass.dependencies {
                    if live[from] {
                        result.insert(Dependency {
                            from,
                            to,
                            kind: DependencyKind::Explicit,
                        });
                    }
                }
                for (from, previous) in passes[..to]
                    .iter()
                    .enumerate()
                    .filter(|&(index, _)| live[index])
                {
                    for before in &previous.accesses {
                        for after in &pass.accesses {
                            if before.resource == after.resource
                                && overlaps(&before.range, &after.range)
                            {
                                for (required, kind) in [
                                    (before.write && after.read, DependencyKind::Raw),
                                    (before.read && after.write, DependencyKind::War),
                                    (before.write && after.write, DependencyKind::Waw),
                                ] {
                                    if required {
                                        result.insert(Dependency { from, to, kind });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            result
        }
        let resources = (0..2)
            .map(|index| CompileResource {
                name: format!("resource {index}"),
                size: 16,
                initial: Vec::from_iter(std::iter::once(0..16)),
                external: false,
            })
            .collect::<Vec<_>>();
        for seed in 0u64..32 {
            let mut state = seed;
            let mut next = || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                state >> 32
            };
            let passes = (0..6)
                .map(|index| {
                    let resource = (next() % 2) as usize;
                    let start = next() % 4 * 4;
                    let end = start + (1 + next() % ((16 - start) / 4)) * 4;
                    let mode = next() % 3;
                    CompilePass {
                        name: format!("pass {index}"),
                        accesses: vec![CompileAccess {
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
                .collect::<Vec<_>>();
            for mask in 0u64..64 {
                let live = (0..6)
                    .map(|index| mask & (1 << index) != 0)
                    .collect::<Vec<_>>();
                let sparse = topological(&passes, &ordering(&resources, &passes, &live), &live);
                let dense = topological(&passes, &reference(&passes, &live), &live);
                match (sparse, dense) {
                    (Ok(sparse), Ok(dense)) => assert_eq!(sparse, dense, "seed={seed} mask={mask}"),
                    (Err(_), Err(_)) => {}
                    pair => panic!("different cycle decision seed={seed} mask={mask}: {pair:?}"),
                }
            }
        }
    }

    #[test]
    fn splitting_read_ranges_shares_history_and_preserves_every_reader_before_a_write() {
        // Repeated whole-buffer readers followed by tiny readbacks used to
        // duplicate every earlier reader into every newly split interval.
        let count = 1_024;
        let size = count * 4;
        let mut frontier = HazardMap::new(size);
        let read = |range| CompileAccess {
            resource: 0,
            range,
            read: true,
            write: false,
            valid_after: false,
        };
        for pass in 0..count {
            frontier.record(pass as usize, &read(0..size));
        }
        for offset in 0..count {
            frontier.record((count + offset) as usize, &read(offset * 4..offset * 4 + 4));
        }
        // This bound describes the metadata contract, not a timing threshold.
        // A reader is retained once per touched interval; splitting an existing
        // interval does not copy its previous readers.
        assert_eq!(frontier.readers.len(), 2 * count as usize);
        let mut dependencies = BTreeSet::new();
        let write = CompileAccess {
            resource: 0,
            range: 0..size,
            read: false,
            write: true,
            valid_after: true,
        };
        let writer = 2 * count as usize;
        frontier.dependencies(writer, &write, &mut dependencies);
        assert_eq!(dependencies.len(), writer);
        for from in 0..writer {
            assert!(dependencies.contains(&Dependency {
                from,
                to: writer,
                kind: DependencyKind::War,
            }));
        }
        frontier.record(writer, &write);
        dependencies.clear();
        frontier.dependencies(writer + 1, &read(0..size), &mut dependencies);
        assert_eq!(
            dependencies,
            BTreeSet::from([Dependency {
                from: writer,
                to: writer + 1,
                kind: DependencyKind::Raw,
            }])
        );
    }

    #[test]
    fn partial_writes_retire_only_readers_of_the_overwritten_range() {
        let mut frontier = HazardMap::new(8);
        let access = |range, read, write| CompileAccess {
            resource: 0,
            range,
            read,
            write,
            valid_after: write,
        };
        frontier.record(0, &access(0..8, true, false));
        frontier.record(1, &access(0..4, true, false));
        frontier.record(2, &access(0..4, false, true));
        let mut dependencies = BTreeSet::new();
        frontier.dependencies(3, &access(0..8, false, true), &mut dependencies);
        assert_eq!(
            dependencies,
            BTreeSet::from([
                Dependency {
                    from: 0,
                    to: 3,
                    kind: DependencyKind::War,
                },
                Dependency {
                    from: 2,
                    to: 3,
                    kind: DependencyKind::Waw,
                },
            ])
        );
    }

    #[test]
    fn repeated_attachments_keep_sparse_dependencies_and_generate_dump_on_demand() {
        let mut graph = Graph::new();
        let vertices = buffer(&mut graph, 84);
        let indices = buffer(&mut graph, 12);
        graph
            .upload_buffer("vertices", vertices, 0, vec![0; 84])
            .unwrap();
        graph
            .upload_buffer("indices", indices, 0, vec![0; 12])
            .unwrap();
        let color = graph.image("color", ImageDesc::rgba8(16, 16)).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(16, 16)).unwrap();
        let draws = 256;
        for index in 0..draws {
            let ops = if index == 0 {
                AttachmentOps::CLEAR_STORE
            } else {
                AttachmentOps::LOAD_STORE
            };
            graph
                .raster(
                    "mesh",
                    RasterDraw {
                        vertices,
                        indices,
                        index_count: 3,
                        parameters: RasterParameters::default(),
                    },
                    RasterAttachments {
                        color: Attachment {
                            image: color,
                            load: match ops.load {
                                LoadAction::Clear => LoadOp::Clear([0.0; 4]),
                                LoadAction::Load => LoadOp::Load,
                                LoadAction::Discard => LoadOp::Discard,
                            },
                            store: ops.store,
                        },
                        depth: Attachment {
                            image: depth,
                            load: match ops.load {
                                LoadAction::Clear => LoadOp::Clear(1.0),
                                LoadAction::Load => LoadOp::Load,
                                LoadAction::Discard => LoadOp::Discard,
                            },
                            store: ops.store,
                        },
                    },
                )
                .unwrap();
        }
        graph.export_image(color).unwrap();
        let compiled = graph.compile().unwrap();
        assert_eq!(compiled.pass_count(), draws + 2);
        assert!(compiled.analysis.ordering_dependencies.len() <= draws * 4);
        #[cfg(feature = "graph-diagnostics")]
        {
            assert!(compiled.dump.get().is_none());
            let first = compiled.dump();
            assert!(first.contains("execution"));
            assert_eq!(first, graph.compile().unwrap().dump());
            assert_eq!(first.as_ptr(), compiled.dump().as_ptr());
        }
    }

    #[test]
    fn disjoint_self_copy_reads_the_earlier_writer_without_self_dependencies() {
        let mut graph = Graph::new();
        let resource = buffer(&mut graph, 12);
        graph
            .upload_buffer("source", resource, 0, vec![0; 12])
            .unwrap();
        graph
            .copy_buffer("self copy", resource, 0, resource, 8, 4)
            .unwrap();
        graph.export_buffer(resource).unwrap();
        let compiled = graph.compile().unwrap();
        assert_eq!(compiled.ordered_pass_names(), ["source", "self copy"]);
        assert_eq!(compiled.analysis.data_dependencies[1], [0]);
        assert!(
            compiled
                .analysis
                .ordering_dependencies
                .iter()
                .all(|edge| edge.from != edge.to)
        );
    }

    #[test]
    fn dead_undefined_reads_and_holes_are_rejected_with_resource_and_pass_names() {
        let mut graph = Graph::new();
        let resource = buffer(&mut graph, 12);
        graph
            .upload_buffer("head", resource, 0, vec![1; 4])
            .unwrap();
        graph
            .upload_buffer("tail", resource, 8, vec![2; 4])
            .unwrap();
        read_pass(&mut graph, "dead hole reader", resource, 0..12, false);
        let error = graph.compile().unwrap_err();
        assert!(error.message.contains("dead hole reader"));
        assert!(error.message.contains("buffer"));
        assert!(error.message.contains("4..8"));
    }

    #[test]
    fn read_write_needs_prior_initialized_contents() {
        fn read_write(graph: &mut Graph, resource: BufferHandle) {
            graph
                .declare_pass(
                    "read modify write",
                    PassKind::Compute,
                    vec![Access {
                        resource: resource.into(),
                        range: AccessRange::Buffer(0..4),
                        kind: AccessKind::ReadWrite,
                        purpose: AccessPurpose::StorageReadWrite,
                    }],
                    true,
                )
                .unwrap();
        }
        let mut undefined = Graph::new();
        let resource = buffer(&mut undefined, 4);
        read_write(&mut undefined, resource);
        assert!(
            undefined
                .compile()
                .unwrap_err()
                .message
                .contains("uninitialized")
        );

        let mut initialized = Graph::new();
        let resource = buffer(&mut initialized, 4);
        initialized
            .upload_buffer("initialize", resource, 0, vec![0; 4])
            .unwrap();
        read_write(&mut initialized, resource);
        let compiled = initialized.compile().unwrap();
        assert_eq!(
            compiled.ordered_pass_names(),
            ["initialize", "read modify write"]
        );
        assert_eq!(compiled.analysis.data_dependencies[1], [0]);
    }

    #[test]
    fn dead_cycles_are_development_diagnostics_and_live_cycles_always_fail() {
        let mut graph = Graph::new();
        let first = graph
            .declare_pass("first", PassKind::Copy, vec![], false)
            .unwrap();
        let second = graph
            .declare_pass("second", PassKind::Copy, vec![], false)
            .unwrap();
        graph.depends_on(first, second).unwrap();
        graph.depends_on(second, first).unwrap();
        #[cfg(feature = "development-validation")]
        {
            let error = graph.compile().unwrap_err();
            assert!(error.message.contains("cycle"));
            assert!(error.message.contains("first"));
            assert!(error.message.contains("second"));
        }
        #[cfg(not(feature = "development-validation"))]
        assert_eq!(graph.compile().unwrap().pass_count(), 0);

        // Making either node observable makes the same cycle part of the live
        // schedule, so disabled development diagnostics cannot hide it.
        graph.passes[first.index].side_effect = true;
        let error = graph.compile().unwrap_err();
        assert!(error.message.contains("cycle"));
        assert!(error.message.contains("first"));
        assert!(error.message.contains("second"));

        // A forward explicit edge also cannot contradict the resource ordering
        // of an otherwise wholly dead chain of overwrites and a final read.
        let mut dead = Graph::new();
        let resource = buffer(&mut dead, 4);
        let first = dead
            .upload_buffer("first writer", resource, 0, vec![0; 4])
            .unwrap();
        dead.upload_buffer("middle writer", resource, 0, vec![1; 4])
            .unwrap();
        let last = read_pass(&mut dead, "last reader", resource, 0..4, false);
        dead.depends_on(first, last).unwrap();
        #[cfg(feature = "development-validation")]
        {
            let error = dead.compile().unwrap_err();
            assert!(error.message.contains("cycle"));
            assert!(error.message.contains("first writer"));
            assert!(error.message.contains("last reader"));
        }
        #[cfg(not(feature = "development-validation"))]
        assert_eq!(dead.compile().unwrap().pass_count(), 0);

        let mut graph = Graph::new();
        let consumer = graph
            .declare_pass("consumer", PassKind::Copy, vec![], true)
            .unwrap();
        let producer = graph
            .declare_pass("producer", PassKind::Copy, vec![], false)
            .unwrap();
        graph.depends_on(consumer, producer).unwrap();
        let compiled = graph.compile().unwrap();
        assert_eq!(compiled.ordered_pass_names(), ["producer", "consumer"]);
        #[cfg(feature = "graph-diagnostics")]
        {
            assert_eq!(compiled.dump(), graph.compile().unwrap().dump());
            assert!(compiled.dump().contains("Explicit"));
        }
    }

    #[test]
    fn exports_keep_final_producers_and_external_writes_are_observable() {
        let mut graph = Graph::new();
        let imported = graph
            .import_buffer(
                "persistent",
                &ImportBuffer {
                    key: ResourceKey {
                        owner: 1,
                        index: 0,
                        generation: 1,
                    },
                    revision: 0,
                    desc: BufferDesc { size: 4 },
                    initialized: vec![],
                },
            )
            .unwrap();
        graph
            .upload_buffer("external write", imported, 0, vec![0; 4])
            .unwrap();
        let dead = buffer(&mut graph, 4);
        graph
            .upload_buffer("dead private write", dead, 0, vec![0; 4])
            .unwrap();
        let compiled = graph.compile().unwrap();
        assert_eq!(compiled.ordered_pass_names(), ["external write"]);
        assert_eq!(
            compiled.final_contents[dead.index],
            Vec::<Range<u64>>::new()
        );
        #[cfg(feature = "graph-diagnostics")]
        assert_eq!(compiled.culled_pass_names(), ["dead private write"]);
        #[cfg(not(feature = "graph-diagnostics"))]
        {
            let culled = compiled
                .passes
                .iter()
                .filter(|pass| matches!(pass.operation, Operation::Culled))
                .collect::<Vec<_>>();
            assert_eq!(culled.len(), 1);
            assert!(culled[0].name.is_empty());
            assert!(culled[0].accesses.is_empty());
        }
        #[cfg(feature = "graph-diagnostics")]
        assert!(compiled.dump().contains("dead private write\" culled"));

        graph.export_buffer(dead).unwrap();
        let exported = graph.compile().unwrap();
        assert_eq!(exported.pass_count(), 2);
        assert_eq!(
            exported.final_contents[dead.index],
            Vec::from_iter(std::iter::once(0..4))
        );
    }

    #[test]
    fn attachment_load_and_store_discard_change_content_validity() {
        fn draw_graph(first: AttachmentOps, second: Option<AttachmentOps>) -> Graph {
            let mut graph = Graph::new();
            let vertices = graph.buffer("vertices", BufferDesc { size: 84 }).unwrap();
            let indices = graph.buffer("indices", BufferDesc { size: 12 }).unwrap();
            graph
                .upload_buffer("vertices upload", vertices, 0, vec![0; 84])
                .unwrap();
            graph
                .upload_buffer("indices upload", indices, 0, vec![0; 12])
                .unwrap();
            let color = graph.image("color", ImageDesc::rgba8(4, 4)).unwrap();
            let depth = graph.image("depth", ImageDesc::depth32(4, 4)).unwrap();
            graph
                .raster(
                    "first draw",
                    RasterDraw {
                        vertices,
                        indices,
                        index_count: 3,
                        parameters: RasterParameters::default(),
                    },
                    RasterAttachments {
                        color: Attachment {
                            image: color,
                            load: match first.load {
                                LoadAction::Clear => LoadOp::Clear([0.0; 4]),
                                LoadAction::Load => LoadOp::Load,
                                LoadAction::Discard => LoadOp::Discard,
                            },
                            store: first.store,
                        },
                        depth: Attachment {
                            image: depth,
                            load: LoadOp::Clear(1.0),
                            store: StoreOp::Store,
                        },
                    },
                )
                .unwrap();
            if let Some(second) = second {
                graph
                    .raster(
                        "second draw",
                        RasterDraw {
                            vertices,
                            indices,
                            index_count: 3,
                            parameters: RasterParameters::default(),
                        },
                        RasterAttachments {
                            color: Attachment {
                                image: color,
                                load: match second.load {
                                    LoadAction::Clear => LoadOp::Clear([0.0; 4]),
                                    LoadAction::Load => LoadOp::Load,
                                    LoadAction::Discard => LoadOp::Discard,
                                },
                                store: second.store,
                            },
                            depth: Attachment {
                                image: depth,
                                load: LoadOp::Load,
                                store: StoreOp::Store,
                            },
                        },
                    )
                    .unwrap();
            }
            graph.readback_image("pixels", color).unwrap();
            graph
        }
        assert!(
            draw_graph(AttachmentOps::LOAD_STORE, None)
                .compile()
                .unwrap_err()
                .message
                .contains("first draw")
        );
        let compiled = draw_graph(AttachmentOps::CLEAR_STORE, Some(AttachmentOps::LOAD_STORE))
            .compile()
            .unwrap();
        assert_eq!(compiled.pass_count(), 5);
        assert_eq!(compiled.analysis.data_dependencies[3], [0, 1, 2]);

        let discard = AttachmentOps {
            load: LoadAction::Clear,
            store: StoreOp::Discard,
        };
        assert!(
            draw_graph(discard, Some(AttachmentOps::LOAD_STORE))
                .compile()
                .unwrap_err()
                .message
                .contains("second draw")
        );
        assert!(
            draw_graph(discard, None)
                .compile()
                .unwrap_err()
                .message
                .contains("pixels")
        );
        let uninitialized = AttachmentOps {
            load: LoadAction::Discard,
            store: StoreOp::Store,
        };
        assert!(
            draw_graph(uninitialized, None)
                .compile()
                .unwrap_err()
                .message
                .contains("pixels")
        );
    }
}
