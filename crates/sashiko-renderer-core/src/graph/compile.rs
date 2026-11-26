//! Consuming analysis freezes the same prepared backing; no projection or clone.

use super::storage::Slot;
use super::{
    Graph, GraphError, GraphErrorKind, GraphStorage, Pass, PassHandle, RasterBuildMetrics,
    Resource, ResourceAccess, analysis,
};
use std::{ops::Range, sync::Arc};

/// Immutable frame plan. Cloning only pins its already-prepared slot. The pool
/// can reuse that slot after every plan/native consumer has released its pin.
#[derive(Clone)]
pub struct CompiledGraph {
    slot: Arc<Slot>,
}
impl std::fmt::Debug for CompiledGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let storage = self.data();
        f.debug_struct("CompiledGraph")
            .field("id", &storage.id)
            .field("passes", &storage.order.len())
            .finish()
    }
}
impl CompiledGraph {
    pub(crate) fn data(&self) -> &GraphStorage {
        self.slot.data()
    }

    /// Raster build measurements retained from the checkout that produced this plan.
    pub fn raster_build_metrics(&self) -> Option<RasterBuildMetrics> {
        self.data().raster_metrics
    }

    #[cfg_attr(
        not(feature = "vulkan"),
        allow(dead_code, reason = "used by the Vulkan engine")
    )]
    pub(crate) fn has_export(&self, resource: usize) -> bool {
        self.data().exported.get(resource).copied().unwrap_or(false)
    }
    pub fn graph_id(&self) -> u64 {
        self.data().id
    }
    pub fn pass_count(&self) -> usize {
        self.data().order.len()
    }
    pub fn ordered_pass_names(&self) -> impl Iterator<Item = &str> {
        let storage = self.data();
        storage.order.iter().map(|&index| storage.pass_name(index))
    }
    #[cfg(feature = "graph-diagnostics")]
    pub fn culled_pass_names(&self) -> impl Iterator<Item = &str> {
        let storage = self.data();
        storage
            .passes
            .iter()
            .enumerate()
            .filter(|(index, _)| !storage.output.live[*index])
            .map(|(index, _)| storage.pass_name(index))
    }
    #[cfg(feature = "graph-diagnostics")]
    pub fn dump(&self) -> &str {
        self.data().dump.get_or_init(|| self.make_dump())
    }
    #[cfg(feature = "graph-diagnostics")]
    fn make_dump(&self) -> String {
        use std::fmt::Write;
        let storage = self.data();
        let mut text = String::from("RenderGraph plan (construction-order contents)\n");
        for (index, resource) in storage.resources.iter().enumerate() {
            writeln!(
                text,
                "resource {index} {:?} size={} external={} lifetime={:?}",
                storage.resource_name(index),
                resource.desc.units(),
                resource.import.is_some(),
                storage.output.lifetimes[index]
            )
            .expect("String writing");
        }
        for index in 0..storage.passes.len() {
            writeln!(
                text,
                "pass {index} {:?} {}",
                storage.pass_name(index),
                if storage.output.live[index] {
                    "live"
                } else {
                    "culled"
                }
            )
            .expect("String writing");
            for access in storage.accesses(index) {
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
        for edge in &storage.output.ordering_edges {
            writeln!(text, "order {} -> {} {:?}", edge.from, edge.to, edge.kind)
                .expect("String writing");
        }
        writeln!(text, "execution {:?}", storage.order).expect("String writing");
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
        self.exports.iter().flat_map(|&r| {
            let resource = &self.resources[r];
            let whole = (resource.export_mips == 0).then(|| 0..resource.desc.units());
            whole
                .into_iter()
                .chain(super::resource::mip_ranges(resource.export_mips))
                .map(move |range| (r, range))
        })
    }
}
pub(super) fn compile(mut graph: Graph) -> Result<CompiledGraph, GraphError> {
    #[cfg(feature = "tracy")]
    let _span = tracy_client::Client::running()
        .map(|client| client.span(tracy_client::span_location!("renderer.graph.compile"), 0));
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
                return Err(graph.data().capacity.analysis_capacity_error(kind));
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
                        graph.data().pass_name(pass),
                        graph.data().resource_name(resource),
                        range.start,
                        range.end
                    )
                } else {
                    format!(
                        "export reads uninitialized resource {:?} at {}..{}",
                        graph.data().resource_name(resource),
                        range.start,
                        range.end
                    )
                }
            }
            analysis::ErrorKind::DependencyCycle => {
                use std::fmt::Write;
                let mut message = String::from("dependency cycle: ");
                for (position, &pass) in graph.data().output.cycle.iter().enumerate() {
                    if position != 0 {
                        message.push_str(" -> ");
                    }
                    write!(message, "{:?}", graph.data().pass_name(pass)).expect("String writing");
                }
                message
            }
            _ => String::from("invalid Graph declaration"),
        };
        let mut reported = GraphError::new(kind, message);
        if let Some(pass) = error.pass {
            reported = reported.with_pass(
                PassHandle {
                    graph: graph.graph_id(),
                    index: pass,
                },
                Some(graph.data().pass_name(pass)),
            );
        }
        if let Some(resource) = error.resource {
            reported = graph.resource_context(reported, resource);
            if let Some(range) = error.range {
                reported = reported.with_range(graph.access_range(resource, range));
            }
        }
        if !graph.data().output.cycle.is_empty() {
            reported = reported.with_cycle_passes(
                graph
                    .data()
                    .output
                    .cycle
                    .iter()
                    .map(|&index| PassHandle {
                        graph: graph.graph_id(),
                        index,
                    })
                    .collect(),
            );
        }
        return Err(reported);
    }
    Ok(CompiledGraph { slot: graph.slot })
}
