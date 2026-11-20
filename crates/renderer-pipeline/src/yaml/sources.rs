use renderer::graph::{ImageHandle, LoadOp, StoreOp};
use renderer::program::{BindingType, ManagedProgram};

use super::{
    BindingSource, Pass, PassKind, PipelineError, PreparedAttachment, PreparedBinding, Resource,
};

/// How a selectable image is supplied for the current Graph.
#[derive(Clone, Copy, Debug)]
pub enum ImageSource {
    /// Use an existing image from this Graph and omit its declared producer.
    /// Graph compilation still checks that its contents are initialized.
    Reuse(ImageHandle),
    /// Run the declared producer. With a target, clear and render into an
    /// existing image in this Graph. Otherwise use a prepared transient image.
    Render { target: Option<ImageHandle> },
}

/// One source selection for an image declared with `source.render` and
/// `source.reuse`. The name is the YAML resource ID. Bindings to the same
/// Graph image share its writes. Use distinct handles for independent images.
#[derive(Clone, Copy, Debug)]
pub struct FrameImageSource<'a> {
    pub name: &'a str,
    pub source: ImageSource,
}

#[derive(Clone, Copy)]
struct Producer {
    resource: usize,
    pass: usize,
}

pub(super) struct ImageSources {
    producers: Vec<Producer>,
    choices: Vec<Option<ImageSource>>,
}

impl ImageSources {
    pub(super) fn prepare(
        resources: &[Resource],
        render_passes: Vec<(usize, String)>,
        passes: &[Pass],
        compute: &[ManagedProgram],
    ) -> Result<Self, PipelineError> {
        let mut producers = Vec::with_capacity(render_passes.len());
        for (resource, name) in render_passes {
            let id = &resources[resource].id;
            let pass = passes
                .iter()
                .position(|pass| pass.id == name)
                .ok_or_else(|| {
                    PipelineError::Invalid(format!(
                        "selectable image {id:?} references unknown producer {name:?}"
                    ))
                })?;
            if passes[pass].when.is_some() || !initializes_only(&passes[pass], resource) {
                return Err(PipelineError::Invalid(format!(
                    "producer {name:?} must clear and store only selectable image {id:?} as the sole attachment of an unconditional Raster pass"
                )));
            }
            for (index, other) in passes.iter().enumerate() {
                if index != pass && writes_image(other, resource, compute) {
                    return Err(PipelineError::Invalid(format!(
                        "pass {:?} also writes selectable image {id:?}",
                        other.id
                    )));
                }
                if index <= pass && reads_image(other, resource) {
                    return Err(PipelineError::Invalid(format!(
                        "pass {:?} reads selectable image {id:?} before its producer completes",
                        other.id
                    )));
                }
            }
            producers.push(Producer { resource, pass });
        }
        let choices = if producers.is_empty() {
            Vec::new()
        } else {
            vec![None; resources.len()]
        };
        Ok(Self { producers, choices })
    }

    pub(super) fn producers(&self) -> impl Iterator<Item = usize> {
        self.producers.iter().map(|producer| producer.resource)
    }

    pub(super) fn select(
        &mut self,
        resources: &[Resource],
        inputs: &[FrameImageSource<'_>],
        active_passes: &mut [bool],
    ) -> Result<(), PipelineError> {
        self.choices.fill(None);
        for input in inputs {
            let name = input.name;
            let producer = self
                .producers
                .iter()
                .find(|producer| resources[producer.resource].id == name)
                .ok_or_else(|| {
                    PipelineError::Invalid(format!("unknown selectable image {name:?}"))
                })?;
            let entry = &mut self.choices[producer.resource];
            if entry.replace(input.source).is_some() {
                return Err(PipelineError::Invalid(format!(
                    "duplicate image source for {name:?}"
                )));
            }
        }
        for producer in &self.producers {
            match self.choices[producer.resource] {
                Some(ImageSource::Reuse(_)) => active_passes[producer.pass] = false,
                Some(ImageSource::Render { .. }) => {}
                None => {
                    return Err(PipelineError::Invalid(format!(
                        "missing image source for {:?}",
                        resources[producer.resource].id
                    )));
                }
            }
        }
        Ok(())
    }

    pub(super) fn choice(&self, resource: usize) -> Option<ImageSource> {
        self.choices.get(resource).copied().flatten()
    }
}

fn initializes_only(pass: &Pass, resource: usize) -> bool {
    // Raster Program descriptors allow only read-only storage bindings. Its
    // attachments describe every write that would disappear on a reuse frame.
    let PassKind::Raster { attachments, .. } = &pass.kind else {
        return false;
    };
    match (attachments.colors.as_slice(), &attachments.depth) {
        ([attachment], None) => clears_image(attachment, resource),
        ([], Some(attachment)) => clears_image(attachment, resource),
        _ => false,
    }
}

fn clears_image<T>(attachment: &PreparedAttachment<T>, resource: usize) -> bool {
    attachment.resource == resource
        && attachment.resolve.is_none()
        && matches!(attachment.load, LoadOp::Clear(_))
        && attachment.store == StoreOp::Store
}

fn reads_image(pass: &Pass, resource: usize) -> bool {
    let reads = |binding: &PreparedBinding| matches!(binding.source, BindingSource::Image(index) if index == resource);
    pass.bindings.iter().any(reads)
        || matches!(&pass.kind, PassKind::Raster { batches, .. }
            if batches.iter().any(|batch| batch.bindings.iter().any(reads)))
}

fn writes_image(pass: &Pass, resource: usize, compute: &[ManagedProgram]) -> bool {
    if let Some(attachments) = pass.kind.attachments() {
        return attachments
            .colors
            .iter()
            .any(|target| target.resource == resource || target.resolve == Some(resource))
            || attachments.depth.as_ref().is_some_and(|target| {
                target.resource == resource || target.resolve == Some(resource)
            });
    }
    let PassKind::Compute { program, .. } = pass.kind else {
        return false;
    };
    pass.bindings.iter().any(|binding| {
        matches!(binding.source, BindingSource::Image(index) if index == resource)
            && compute[program].bindings().iter().any(|slot| {
                (slot.group, slot.binding) == (binding.group, binding.binding)
                    && matches!(slot.kind, BindingType::StorageImageWrite(_))
            })
    })
}
