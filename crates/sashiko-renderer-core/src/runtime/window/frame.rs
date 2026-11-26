use super::WindowPresenter;
use crate::{
    ImageFormat, PresentOutcome,
    capacity::CapacityRecovery,
    graph::{CompiledGraph, ImageDesc, ImageHandle, ResourceDesc, SampleCount},
    runtime::{ExecutionError, FrameSubmission, Runtime},
};

fn check_display(desc: ImageDesc) -> Result<(), ExecutionError> {
    if desc.format != ImageFormat::Rgba8Unorm
        || desc.sample_count != SampleCount::X1
        || !desc.is_single_subresource()
    {
        return Err(ExecutionError::invalid(
            "presentation requires a single-sample, single-subresource display-linear RGBA8 image",
        ));
    }
    Ok(())
}

impl WindowPresenter {
    /// Whether a source is still waiting for an accepted presentation.
    pub fn has_pending_frame(&self) -> bool {
        self.pending_image.is_some()
    }

    fn require_empty(&self) -> Result<(), ExecutionError> {
        if self.closed {
            Err(ExecutionError::invalid("presenter is closed"))
        } else if self.has_pending_frame() {
            Err(ExecutionError::invalid(
                "present or discard the pending frame first",
            ))
        } else {
            Ok(())
        }
    }

    /// Submit a Graph and retain its selected display-linear image for
    /// retryable presentation. Other exports remain available in the receipt.
    pub fn submit_frame(
        &mut self,
        runtime: &mut Runtime,
        graph: &CompiledGraph,
        image: ImageHandle,
    ) -> Result<FrameSubmission, ExecutionError> {
        self.require_empty()?;
        if self.runtime_id != runtime.image_releases.runtime_id {
            return Err(ExecutionError::wrong_owner(
                "presenter belongs to another runtime",
            ));
        }
        if image.graph != graph.graph_id() {
            return Err(ExecutionError::invalid(
                "display image belongs to another Graph",
            ));
        }
        let Some(ResourceDesc::Image(desc)) =
            graph.data().resources.get(image.index).map(|r| r.desc)
        else {
            return Err(ExecutionError::invalid(
                "display output must identify an image",
            ));
        };
        check_display(desc)?;
        let (receipt, source) = runtime.submit_with_image(graph, image)?;
        self.pending_image = Some(source);
        Ok(receipt)
    }

    /// Attach the display-linear image selected with Graph::export_frame_image
    /// by a higher-level frame submission. Failures leave the receipt intact.
    pub fn queue_frame(&mut self, receipt: &mut FrameSubmission) -> Result<(), ExecutionError> {
        self.require_empty()?;
        let source = receipt
            .frame_image
            .as_ref()
            .ok_or_else(|| ExecutionError::invalid("the submission has no managed frame image"))?;
        if source.runtime_id != self.runtime_id {
            return Err(ExecutionError::wrong_owner(
                "display image belongs to another runtime",
            ));
        }
        check_display(source.snapshot().desc())?;
        self.pending_image = receipt.take_frame_image();
        Ok(())
    }

    /// Cancel a pending request. A transient registration is released during
    /// Runtime maintenance; an imported target retains its caller ownership.
    pub fn discard_frame(&mut self) {
        self.pending_image = None;
    }

    /// Present a queued image. NotReady (including temporary GPU backpressure)
    /// keeps the source for retry. A successful present, suspension or resize
    /// consumes it. On an error the caller may discard it or retry after the
    /// reported recovery. Native work retains its own completion leases.
    pub fn present_frame(
        &mut self,
        runtime: &mut Runtime,
    ) -> Result<PresentOutcome, ExecutionError> {
        let image = self
            .pending_image
            .as_ref()
            .ok_or_else(|| ExecutionError::invalid("no queued display image"))?
            .snapshot()
            .clone();
        let outcome = match runtime.present(self, &image) {
            Err(error)
                if error
                    .capacity()
                    .is_some_and(|e| e.recovery == CapacityRecovery::Poll) =>
            {
                PresentOutcome::NotReady
            }
            result => result?,
        };
        if outcome != PresentOutcome::NotReady {
            self.discard_frame();
        }
        Ok(outcome)
    }
}
