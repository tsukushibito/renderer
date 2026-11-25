use super::{CompiledGraph, Graph, GraphError, ImageHandle, PassHandle};

/// Responsibility for the final output's runtime registration. This does not
/// imply GPU completion or ownership of the native resource itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameOutputOwnership {
    /// This frame creates the registration; release it after scheduling consumers.
    Frame,
    /// The output is an imported registration retained by its original caller.
    Caller,
}

/// A finalized graph with one selected image output and optional readback.
///
/// The output, its registration ownership and the readback selection stay
/// together until [`Self::compile`] consumes the frame. Build with ordinary
/// Graph operations, then call [`Self::select_output`] to finalize the selection.
///
/// The final selection cannot be reassigned independently of its ownership:
/// ```compile_fail
/// use sashiko_renderer_core::graph::FrameGraph;
/// use sashiko_renderer_core::graph::ImageHandle;
/// fn change_output(frame: &mut FrameGraph, output: ImageHandle) {
///     frame.output = output;
/// }
/// ```
/// Replacing the graph after finalization is also disallowed:
/// ```compile_fail
/// use sashiko_renderer_core::graph::FrameGraph;
/// use sashiko_renderer_core::graph::Graph;
/// fn replace_graph(frame: &mut FrameGraph, graph: Graph) {
///     frame.graph = graph;
/// }
/// ```
pub struct FrameGraph {
    graph: Graph,
    output: ImageHandle,
    readback: Option<PassHandle>,
    output_ownership: FrameOutputOwnership,
}

impl FrameGraph {
    /// Export a graph-local image and optionally append a readback of it.
    /// Imported registrations remain caller-owned. Graph compilation validates
    /// the contents needed for the export and readback.
    pub fn select_output(
        mut graph: Graph,
        output: ImageHandle,
        readback: Option<&str>,
    ) -> Result<Self, GraphError> {
        graph.export_image(output)?;
        let output_ownership = if graph.resources[output.index].import.is_some() {
            FrameOutputOwnership::Caller
        } else {
            FrameOutputOwnership::Frame
        };
        let readback = readback
            .map(|name| graph.readback_image(name, output))
            .transpose()?;
        Ok(Self {
            graph,
            output,
            readback,
            output_ownership,
        })
    }

    /// Inspect the finalized declarations without changing their identity,
    /// selected output or readback association.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// The exported final output selected during graph construction.
    /// The handle belongs only to this frame's graph.
    pub fn output(&self) -> ImageHandle {
        self.output
    }

    /// The readback of [`Self::output`], if requested during construction.
    /// Save this handle before compilation to identify its eventual CPU result.
    ///
    /// Its association cannot be edited separately from the finalized output:
    /// ```compile_fail
    /// use sashiko_renderer_core::graph::FrameGraph;
    /// fn remove_readback(frame: &mut FrameGraph) {
    ///     frame.readback = None;
    /// }
    /// ```
    pub fn readback(&self) -> Option<PassHandle> {
        self.readback
    }

    /// Reports who releases the final output registration after GPU consumers
    /// have been scheduled. Imported outputs remain owned by their caller.
    pub fn output_ownership(&self) -> FrameOutputOwnership {
        self.output_ownership
    }

    /// Consume the finalized declarations and compile them without loading a
    /// native backend. Output and readback handles obtained above remain valid
    /// for the resulting plan. Failure returns the graph slot to its pool.
    pub fn compile(self) -> Result<CompiledGraph, GraphError> {
        self.graph.compile()
    }

    /// The native adapter transfers the complete finalized selection together.
    #[cfg(feature = "vulkan")]
    pub(crate) fn into_parts(
        self,
    ) -> (Graph, ImageHandle, Option<PassHandle>, FrameOutputOwnership) {
        (
            self.graph,
            self.output,
            self.readback,
            self.output_ownership,
        )
    }
}
