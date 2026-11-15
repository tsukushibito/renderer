//! Owned attachment operations, independent of a graphics shader or native API.

use super::{
    AccessPurpose, AttachmentClearOperands, AttachmentOperand, Graph, GraphCapacityKind,
    GraphError, GraphErrorKind, ImageFormat, LoadAction, LoadOp, Operation, PassHandle,
    RasterAttachments, ResourceAccess, StoreOp, storage::capacity_error,
};

impl Graph {
    /// Apply attachment Load/Clear/Discard and Store/Discard without drawing.
    /// Owns the borrowed color array and optional depth after this call. Each
    /// View selects one complete mip/layer; all selected extents must match.
    ///
    /// CPU planning supports MRT, color-only and depth-only passes. Vulkan
    /// currently executes only the original one-color-plus-depth Clear/Store
    /// form on single-subresource images; other native forms are unsupported.
    pub fn attachment_pass(
        &mut self,
        name: &str,
        attachments: RasterAttachments<'_>,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let count = attachments
                .colors
                .len()
                .checked_add(usize::from(attachments.depth.is_some()));
            if count == Some(0) {
                return Err(GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "attachment pass requires at least one target",
                ));
            }
            let required = count.and_then(|count| graph.attachments.len().checked_add(count));
            if required.is_none_or(|required| required > graph.capacity.attachments) {
                return Err(capacity_error(
                    GraphCapacityKind::Attachments,
                    graph.capacity.attachments,
                    required,
                ));
            }
            let first = graph.attachments.len();
            let first_access = graph.accesses.len();
            let mut extent = None;
            for attachment in attachments.colors {
                let clear = match attachment.load {
                    LoadOp::Clear(value) if value.iter().all(|v| v.is_finite()) => value,
                    LoadOp::Clear(_) => {
                        return Err(graph.resource_error(
                            GraphErrorKind::InvalidInput,
                            "color attachment clear must be finite",
                            graph.image_id(attachment.view.image)?,
                        ));
                    }
                    _ => [0.0; 4],
                };
                graph.append_attachment(
                    AttachmentOperand {
                        view: attachment.view,
                        ops: attachment.ops(),
                        clear,
                    },
                    ImageFormat::Rgba8Unorm,
                    &mut extent,
                )?;
            }
            let colors = first..graph.attachments.len();
            let depth = if let Some(attachment) = attachments.depth {
                let value = match attachment.load {
                    LoadOp::Clear(value) if value.is_finite() && (0.0..=1.0).contains(&value) => {
                        value
                    }
                    LoadOp::Clear(_) => {
                        return Err(graph.resource_error(
                            GraphErrorKind::InvalidInput,
                            "depth clear must be finite and within zero and one",
                            graph.image_id(attachment.view.image)?,
                        ));
                    }
                    _ => 0.0,
                };
                let index = graph.attachments.len();
                graph.append_attachment(
                    AttachmentOperand {
                        view: attachment.view,
                        ops: attachment.ops(),
                        clear: [value, 0.0, 0.0, 0.0],
                    },
                    ImageFormat::Depth32Float,
                    &mut extent,
                )?;
                Some(index)
            } else {
                None
            };
            let accesses = first_access..graph.accesses.len();
            let conflict = {
                let data = graph.data_mut();
                super::accesses::conflicting_accesses_in(
                    &data.accesses[accesses.clone()],
                    |access| (access.resource, access.range.clone(), true),
                    &mut data.overlap,
                )
                .map_err(|_| {
                    capacity_error(
                        GraphCapacityKind::Accesses,
                        data.capacity.accesses,
                        Some(accesses.len()),
                    )
                })?
            };
            if let Some((left, _)) = conflict {
                let access = &graph.accesses[accesses.start + left];
                return Err(graph
                    .resource_error(
                        GraphErrorKind::InvalidInput,
                        "attachment Views must not overlap",
                        access.resource,
                    )
                    .with_range(graph.access_range(access.resource, access.range.clone())));
            }
            let simple_clear = if let (1, Some(depth)) = (colors.len(), depth) {
                let color = graph.attachments[colors.start];
                let depth = graph.attachments[depth];
                if color.ops.load == LoadAction::Clear
                    && color.ops.store == StoreOp::Store
                    && depth.ops.load == LoadAction::Clear
                    && depth.ops.store == StoreOp::Store
                    && graph
                        .image_desc_at(color.view.image.index)?
                        .is_single_subresource()
                    && graph
                        .image_desc_at(depth.view.image.index)?
                        .is_single_subresource()
                {
                    Some(AttachmentClearOperands {
                        color: color.view.image.index,
                        depth: depth.view.image.index,
                        clear_color: color.clear,
                        clear_depth: depth.clear[0],
                    })
                } else {
                    None
                }
            } else {
                None
            };
            Ok((
                Operation::AttachmentPass {
                    colors,
                    depth,
                    simple_clear,
                },
                accesses,
                false,
            ))
        })
    }

    fn append_attachment(
        &mut self,
        operand: AttachmentOperand,
        format: ImageFormat,
        extent: &mut Option<[u32; 2]>,
    ) -> Result<(), GraphError> {
        let resource = self.image_id(operand.view.image)?;
        let desc = self.image_desc_at(resource)?;
        if desc.format != format {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "attachment format does not match its color/depth role",
                resource,
            ));
        }
        // View creation validated these coordinates; descriptors cannot change
        // and image_id above establishes the same Graph owner and resource.
        let current = desc
            .mip_extent(operand.view.mip)
            .expect("immutable View extent");
        if extent.is_some_and(|extent| extent != current) {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "attachment mip extents must match",
                resource,
            ));
        }
        *extent = Some(current);
        let unit = u64::from(operand.view.layer) * u64::from(desc.mip_levels)
            + u64::from(operand.view.mip);
        self.append_access(ResourceAccess {
            resource,
            range: unit..unit + 1,
            read: operand.ops.load == LoadAction::Load,
            write: true,
            initializes: operand.ops.load != LoadAction::Discard,
            discard_after: operand.ops.store == StoreOp::Discard,
            purpose: if format == ImageFormat::Rgba8Unorm {
                AccessPurpose::ColorAttachment
            } else {
                AccessPurpose::DepthAttachment
            },
        })?;
        self.data_mut().attachments.push(operand);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Attachment, GraphCapacity, GraphPool, ImageDesc};

    #[test]
    fn plan_owns_canonical_color_and_depth_operands_after_inputs_end() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let a = graph.image("a", ImageDesc::rgba8(4, 4)).unwrap();
        let b = graph.image("b", ImageDesc::rgba8(4, 4)).unwrap();
        let d = graph.image("d", ImageDesc::depth32(4, 4)).unwrap();
        let a = graph.image_view(a, 0, 0).unwrap();
        let b = graph.image_view(b, 0, 0).unwrap();
        let d = graph.image_view(d, 0, 0).unwrap();
        let pass = {
            let colors = [
                Attachment {
                    view: b,
                    load: LoadOp::Clear([0.1, 0.2, 0.3, 0.4]),
                    store: StoreOp::Store,
                },
                Attachment {
                    view: a,
                    load: LoadOp::Clear([0.8, 0.7, 0.6, 0.5]),
                    store: StoreOp::Discard,
                },
            ];
            graph
                .attachment_pass(
                    "owned",
                    RasterAttachments {
                        colors: &colors,
                        depth: Some(Attachment {
                            view: d,
                            load: LoadOp::Clear(0.75),
                            store: StoreOp::Store,
                        }),
                    },
                )
                .unwrap()
        };
        graph.export_image(b.image()).unwrap();
        let plan = graph.compile().unwrap();
        let Operation::AttachmentPass {
            colors,
            depth,
            simple_clear,
        } = &plan.passes[pass.index].operation
        else {
            panic!("expected owned attachment pass");
        };
        assert!(simple_clear.is_none());
        let colors = &plan.attachments[colors.clone()];
        assert_eq!(colors.len(), 2);
        assert_eq!(colors[0].view, b);
        assert_eq!(colors[0].clear, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(colors[0].ops.load, LoadAction::Clear);
        assert_eq!(colors[0].ops.store, StoreOp::Store);
        assert_eq!(colors[1].view, a);
        assert_eq!(colors[1].clear, [0.8, 0.7, 0.6, 0.5]);
        assert_eq!(colors[1].ops.store, StoreOp::Discard);
        let depth = plan.attachments[depth.unwrap()];
        assert_eq!(depth.view, d);
        assert_eq!(depth.clear[0], 0.75);
        assert_eq!(depth.ops.load, LoadAction::Clear);
        assert_eq!(depth.ops.store, StoreOp::Store);
    }

    #[test]
    fn original_clear_shape_preserves_the_native_lowering_operands() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let color = graph.image("color", ImageDesc::rgba8(3, 2)).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(3, 2)).unwrap();
        let pass = graph
            .attachment_pass(
                "baseline",
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(color, 0, 0).unwrap(),
                        load: LoadOp::Clear([0.25, 0.5, 0.75, 1.0]),
                        store: StoreOp::Store,
                    }],
                    depth: Some(Attachment {
                        view: graph.image_view(depth, 0, 0).unwrap(),
                        load: LoadOp::Clear(0.625),
                        store: StoreOp::Store,
                    }),
                },
            )
            .unwrap();
        graph.export_image(color).unwrap();
        let plan = graph.compile().unwrap();
        let Operation::AttachmentPass {
            simple_clear: Some(clear),
            ..
        } = &plan.passes[pass.index].operation
        else {
            panic!("original clear shape must retain its native lowering contract");
        };
        assert_eq!(clear.color, color.index);
        assert_eq!(clear.depth, depth.index);
        assert_eq!(clear.clear_color, [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(clear.clear_depth, 0.625);
        assert_eq!(
            pool.begin().unwrap_err().kind(),
            GraphErrorKind::CapacityExhausted
        );
        drop(plan);
        assert_eq!(pool.begin().unwrap().compile().unwrap().pass_count(), 0);
    }
}
