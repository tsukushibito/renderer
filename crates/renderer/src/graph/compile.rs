//! Consuming analysis freezes the same prepared backing; no projection or clone.

use super::storage::Slot;
use super::{
    Graph, GraphError, GraphErrorKind, GraphStorage, Pass, PassHandle, Resource, ResourceAccess,
    analysis,
};
use std::{
    ops::{Deref, Range},
    sync::Arc,
};

/// Immutable frame plan. Cloning only pins its already-prepared slot. The pool
/// can reuse that slot after every plan/native consumer has released its pin.
#[derive(Clone)]
pub struct CompiledGraph {
    slot: Arc<Slot>,
}
impl Deref for CompiledGraph {
    type Target = GraphStorage;
    fn deref(&self) -> &Self::Target {
        self.slot.data()
    }
}
impl std::fmt::Debug for CompiledGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledGraph")
            .field("id", &self.id)
            .field("passes", &self.order.len())
            .finish()
    }
}
impl CompiledGraph {
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
    pub fn pass_count(&self) -> usize {
        self.order.len()
    }
    pub fn ordered_pass_names(&self) -> impl Iterator<Item = &str> {
        self.order.iter().map(|&index| self.pass_name(index))
    }
    #[cfg(feature = "graph-diagnostics")]
    pub fn culled_pass_names(&self) -> impl Iterator<Item = &str> {
        self.passes
            .iter()
            .enumerate()
            .filter(|(index, _)| !self.output.live[*index])
            .map(|(index, _)| self.pass_name(index))
    }
    #[cfg(feature = "graph-diagnostics")]
    pub fn dump(&self) -> &str {
        self.dump.get_or_init(|| self.make_dump())
    }
    #[cfg(feature = "graph-diagnostics")]
    fn make_dump(&self) -> String {
        use std::fmt::Write;
        let mut text = String::from("RenderGraph plan (construction-order contents)\n");
        for (index, resource) in self.resources.iter().enumerate() {
            writeln!(
                text,
                "resource {index} {:?} size={} external={} lifetime={:?}",
                self.resource_name(index),
                resource.desc.units(),
                resource.import.is_some(),
                self.output.lifetimes[index]
            )
            .expect("String writing");
        }
        for index in 0..self.passes.len() {
            writeln!(
                text,
                "pass {index} {:?} {}",
                self.pass_name(index),
                if self.output.live[index] {
                    "live"
                } else {
                    "culled"
                }
            )
            .expect("String writing");
            for access in self.accesses(index) {
                writeln!(
                    text,
                    "  resource={} range={:?} purpose={:?} read={} write={} valid_after={} stages={:?}",
                    access.resource,
                    access.range,
                    access.purpose,
                    access.read,
                    access.write,
                    access.initializes && !access.discard_after,
                    access.stages
                )
                .expect("String writing");
            }
        }
        for edge in &self.output.ordering_edges {
            writeln!(text, "order {} -> {} {:?}", edge.from, edge.to, edge.kind)
                .expect("String writing");
        }
        writeln!(text, "execution {:?}", self.order).expect("String writing");
        text
    }
}

struct Input<'a> {
    resources: &'a [Resource],
    passes: &'a [Pass],
    accesses: &'a [ResourceAccess],
    dependencies: &'a [super::Dependency],
    initial: &'a [Range<u64>],
    exports: &'a [usize],
}
impl analysis::Input for Input<'_> {
    fn resource_count(&self) -> usize {
        self.resources.len()
    }
    fn resource_size(&self, r: usize) -> u64 {
        self.resources[r].desc.units()
    }
    fn initial_ranges(&self, r: usize) -> &[Range<u64>] {
        &self.initial[self.resources[r].initial.clone()]
    }
    fn resource_external(&self, r: usize) -> bool {
        self.resources[r].import.is_some()
    }
    fn pass_count(&self) -> usize {
        self.passes.len()
    }
    fn access_count(&self) -> usize {
        self.accesses.len()
    }
    fn pass_side_effect(&self, p: usize) -> bool {
        self.passes[p].side_effect
    }
    fn pass_dependencies(&self, p: usize) -> impl Iterator<Item = usize> {
        self.passes[p].dependencies.iter(self.dependencies)
    }
    fn pass_accesses(&self, p: usize) -> impl Iterator<Item = analysis::Access> {
        self.accesses[self.passes[p].accesses.clone()]
            .iter()
            .map(|a| analysis::Access {
                resource: a.resource,
                range: a.range.clone(),
                read: a.read,
                write: a.write,
                valid_after: a.initializes && !a.discard_after,
            })
    }
    fn exports(&self) -> impl Iterator<Item = (usize, Range<u64>)> {
        self.exports
            .iter()
            .map(|&r| (r, 0..self.resources[r].desc.units()))
    }
}
pub(super) fn compile(mut graph: Graph) -> Result<CompiledGraph, GraphError> {
    let result = {
        let data = graph.data_mut();
        // The input borrows only immutable declaration fields; workspace/output
        // are disjoint mutable fields. No shared borrow of the whole slot lives
        // across this call, and no lifetime is extended through a raw pointer.
        let input = Input {
            resources: &data.resources,
            passes: &data.passes,
            accesses: &data.accesses,
            dependencies: &data.dependencies,
            initial: &data.initial_ranges,
            exports: &data.exports,
        };
        analysis::analyze(&input, &mut data.workspace, &mut data.output)
    };
    if let Err(error) = result {
        let kind = match error.kind {
            analysis::ErrorKind::Capacity(kind) => {
                return Err(graph.capacity.analysis_capacity_error(kind));
            }
            analysis::ErrorKind::UninitializedRead => GraphErrorKind::UninitializedRead,
            analysis::ErrorKind::DependencyCycle => GraphErrorKind::DependencyCycle,
            #[cfg(feature = "development-validation")]
            _ => GraphErrorKind::InvalidInput,
        };
        let message = match error.kind {
            analysis::ErrorKind::UninitializedRead => {
                let resource = error.resource.expect("hole identifies its resource");
                let range = error.range.as_ref().expect("hole identifies its range");
                if let Some(pass) = error.pass {
                    format!(
                        "pass {:?} reads uninitialized resource {:?} at {}..{}",
                        graph.pass_name(pass),
                        graph.resource_name(resource),
                        range.start,
                        range.end
                    )
                } else {
                    format!(
                        "export reads uninitialized resource {:?} at {}..{}",
                        graph.resource_name(resource),
                        range.start,
                        range.end
                    )
                }
            }
            analysis::ErrorKind::DependencyCycle => {
                use std::fmt::Write;
                let mut message = String::from("dependency cycle: ");
                for (position, &pass) in graph.output.cycle.iter().enumerate() {
                    if position != 0 {
                        message.push_str(" -> ");
                    }
                    write!(message, "{:?}", graph.pass_name(pass)).expect("String writing");
                }
                message
            }
            _ => String::from("invalid Graph declaration"),
        };
        let mut reported = GraphError::new(kind, message);
        if let Some(pass) = error.pass {
            reported = reported.with_pass(
                PassHandle {
                    graph: graph.id,
                    index: pass,
                },
                Some(graph.pass_name(pass)),
            );
        }
        if let Some(resource) = error.resource {
            reported = graph.resource_context(reported, resource);
            if let Some(range) = error.range {
                reported = reported.with_range(graph.access_range(resource, range));
            }
        }
        if !graph.output.cycle.is_empty() {
            reported = reported.with_cycle_passes(
                graph
                    .output
                    .cycle
                    .iter()
                    .map(|&index| PassHandle {
                        graph: graph.id,
                        index,
                    })
                    .collect(),
            );
        }
        return Err(reported);
    }
    Ok(CompiledGraph { slot: graph.slot })
}
