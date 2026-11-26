//! Owned attachment operations, independent of a graphics shader or native API.

use super::{
    AccessPurpose, AttachmentOperand, Graph, GraphCapacityKind, GraphError, GraphErrorKind,
    LoadAction, LoadOp, Operation, PassHandle, RasterAttachments, ResourceAccess, SampleCount,
    StoreOp, storage::capacity_error,
};

pub(super) struct AttachmentPlan {
    pub colors: std::ops::Range<usize>,
    pub depth: Option<usize>,
    pub accesses: std::ops::Range<usize>,
}

impl Graph {
    /// Apply attachment Load/Clear/Discard and Store/Discard without drawing.
    /// Owns the borrowed color array and optional depth after this call. Each
    /// View selects one complete mip/layer; all selected extents must match.
    ///
    /// Color targets may be RGBA8 or RGBA16F; depth targets use D32. CPU planning
    /// supports MRT and selected mip/layer Views. Native support and prepared
    /// capacity are established separately by the selected backend.
    pub fn attachment_pass(
        &mut self,
        name: &str,
        attachments: RasterAttachments<'_>,
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let plan = graph.append_attachments(attachments)?;
            graph.check_attachment_overlap(plan.accesses.clone())?;
            Ok((
                Operation::AttachmentPass {
                    colors: plan.colors,
                    depth: plan.depth,
                },
                plan.accesses,
                false,
            ))
        })
    }

    /// Reset a rectangular part of an initialized D32 attachment to far depth.
    /// The surrounding texels are preserved and participate in Graph dependencies.
    pub fn clear_depth_rect(
        &mut self,
        name: &str,
        view: super::ImageView,
        rect: super::RasterRect,
    ) -> Result<PassHandle, GraphError> {
        let extent = self
            .image_desc(view.image())?
            .mip_extent(view.mip_level())
            .expect("validated image view");
        let fits = |offset: u32, size: u32, limit: u32| {
            size != 0
                && offset
                    .checked_add(size)
                    .is_some_and(|end| end <= limit.min(i32::MAX as u32))
        };
        if !fits(rect.x, rect.width, extent[0]) || !fits(rect.y, rect.height, extent[1]) {
            return Err(GraphError::invalid_input(
                "depth clear rectangle exceeds its attachment",
            ));
        }
        let handle = self.attachment_pass(
            name,
            RasterAttachments {
                colors: &[],
                color_resolves: &[],
                depth: Some(super::Attachment {
                    view,
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                }),
            },
        )?;
        self.data_mut()
            .attachments
            .last_mut()
            .expect("depth attachment")
            .depth_clear_rect = Some(rect);
        Ok(handle)
    }

    pub(super) fn append_attachments(
        &mut self,
        attachments: RasterAttachments<'_>,
    ) -> Result<AttachmentPlan, GraphError> {
        if attachments.colors.len() > crate::program::MAX_RASTER_COLOR_ATTACHMENTS {
            return Err(GraphError::new(
                GraphErrorKind::Unsupported,
                "color attachment count exceeds the common Raster limit",
            ));
        }
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
        let required = count.and_then(|count| self.data().attachments.len().checked_add(count));
        if required.is_none_or(|required| required > self.data().capacity.attachments) {
            return Err(capacity_error(
                GraphCapacityKind::Attachments,
                self.data().capacity.attachments,
                required,
            ));
        }
        let first = self.data().attachments.len();
        let first_access = self.data().accesses.len();
        if !attachments.color_resolves.is_empty()
            && attachments.color_resolves.len() != attachments.colors.len()
        {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                "resolve list must be empty or match the color attachment count",
            ));
        }
        let mut shape = None;
        for (index, attachment) in attachments.colors.iter().enumerate() {
            let clear = match attachment.load {
                LoadOp::Clear(value) if value.iter().all(|v| v.is_finite()) => value,
                LoadOp::Clear(_) => {
                    return Err(self.resource_error(
                        GraphErrorKind::InvalidInput,
                        "color attachment clear must be finite",
                        self.image_id(attachment.view.image)?,
                    ));
                }
                _ => [0.0; 4],
            };
            self.append_attachment(
                AttachmentOperand {
                    view: attachment.view,
                    resolve: attachments.color_resolves.get(index).copied().flatten(),
                    ops: attachment.ops(),
                    depth_clear_rect: None,
                    clear,
                },
                super::ImageAspect::Color,
                &mut shape,
            )?;
        }
        let colors = first..self.data().attachments.len();
        let depth = if let Some(attachment) = attachments.depth {
            let value = match attachment.load {
                LoadOp::Clear(value)
                    if value.is_finite()
                        && (0.0..=1.0).contains(&value)
                        && !value.is_subnormal() =>
                {
                    value
                }
                LoadOp::Clear(_) => {
                    return Err(self.resource_error(
                        GraphErrorKind::InvalidInput,
                        "depth clear must be zero or a normal finite value within zero and one",
                        self.image_id(attachment.view.image)?,
                    ));
                }
                _ => 0.0,
            };
            let index = self.data().attachments.len();
            self.append_attachment(
                AttachmentOperand {
                    view: attachment.view,
                    resolve: None,
                    ops: attachment.ops(),
                    depth_clear_rect: None,
                    clear: [value, 0.0, 0.0, 0.0],
                },
                super::ImageAspect::Depth,
                &mut shape,
            )?;
            Some(index)
        } else {
            None
        };
        let accesses = first_access..self.data().accesses.len();
        Ok(AttachmentPlan {
            colors,
            depth,
            accesses,
        })
    }

    fn check_attachment_overlap(
        &mut self,
        accesses: std::ops::Range<usize>,
    ) -> Result<(), GraphError> {
        let conflict = {
            let data = self.data_mut();
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
            let access = &self.data().accesses[accesses.start + left];
            return Err(self
                .resource_error(
                    GraphErrorKind::InvalidInput,
                    "attachment Views must not overlap",
                    access.resource,
                )
                .with_range(self.access_range(access.resource, access.range.clone())));
        }

        Ok(())
    }

    fn append_attachment(
        &mut self,
        operand: AttachmentOperand,
        aspect: super::ImageAspect,
        shape: &mut Option<([u32; 2], SampleCount)>,
    ) -> Result<(), GraphError> {
        let resource = self.image_id(operand.view.image)?;
        let desc = self.image_desc_at(resource)?;
        if operand.view.mip_count != 1 || !desc.format.supports_attachment() {
            return Err(self.resource_error(
                GraphErrorKind::Unsupported,
                "attachments require one mip and a supported attachment format",
                resource,
            ));
        }
        if desc.format.aspect() != aspect {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "attachment format does not match its color/depth role",
                resource,
            ));
        }
        // View creation validated these coordinates; descriptors cannot change
        // and image_id above establishes the same Graph owner and resource.
        let extent = desc
            .mip_extent(operand.view.mip)
            .expect("immutable View extent");
        let current = (extent, desc.sample_count);
        if shape.is_some_and(|shape| shape != current) {
            return Err(self.resource_error(
                GraphErrorKind::InvalidInput,
                "attachment mip extents and sample counts must match",
                resource,
            ));
        }
        *shape = Some(current);
        let unit = u64::from(operand.view.layer) * u64::from(desc.mip_levels)
            + u64::from(operand.view.mip);
        self.append_access(ResourceAccess {
            stages: crate::program::ShaderStages::EMPTY,
            resource,
            range: unit..unit + 1,
            read: operand.ops.load == LoadAction::Load,
            write: true,
            initializes: operand.ops.load != LoadAction::Discard,
            discard_after: operand.ops.store == StoreOp::Discard,
            purpose: if aspect == super::ImageAspect::Color {
                AccessPurpose::ColorAttachment
            } else {
                AccessPurpose::DepthAttachment
            },
        })?;
        if let Some(view) = operand.resolve {
            let destination = self.image_id(view.image)?;
            let resolved = self.image_desc_at(destination)?;
            if desc.sample_count != SampleCount::X4
                || operand.ops.load == LoadAction::Discard
                || resolved != desc.with_sample_count(SampleCount::X1)
                || view.mip_count != 1
            {
                return Err(self.resource_error(
                    GraphErrorKind::InvalidInput,
                    "color resolve requires initialized 4x color and a matching 1x destination",
                    destination,
                ));
            }
            self.append_access(ResourceAccess {
                stages: crate::program::ShaderStages::EMPTY,
                resource: destination,
                range: 0..1,
                read: false,
                write: true,
                initializes: true,
                discard_after: false,
                purpose: AccessPurpose::ColorResolve,
            })?;
        }
        self.data_mut().attachments.push(operand);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Attachment, GraphCapacity, GraphPool, ImageDesc};

    #[test]
    fn tile_clear_requires_in_bounds_and_previously_defined_depth() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let depth = graph.image("atlas", ImageDesc::depth32(8, 4)).unwrap();
        let view = graph.image_view(depth, 0, 0).unwrap();
        for rect in [
            super::super::RasterRect::new(5, 0, 4, 4),
            super::super::RasterRect::new(0, 0, 0, 4),
        ] {
            assert_eq!(
                graph
                    .clear_depth_rect("invalid", view, rect)
                    .unwrap_err()
                    .kind(),
                GraphErrorKind::InvalidInput
            );
        }
        graph
            .clear_depth_rect("tile", view, super::super::RasterRect::new(4, 0, 4, 4))
            .unwrap();
        graph.export_image(depth).unwrap();
        assert_eq!(
            graph.compile().unwrap_err().kind(),
            GraphErrorKind::UninitializedRead
        );
    }

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
                        color_resolves: &[],
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
        let plan = plan.data();
        let Operation::AttachmentPass { colors, depth } = &plan.passes[pass.index].operation else {
            panic!("expected owned attachment pass");
        };
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
    fn clear_color_and_depth_contents_remain_owned_until_the_plan_is_returned() {
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let color = graph.image("color", ImageDesc::rgba8(3, 2)).unwrap();
        let depth = graph.image("depth", ImageDesc::depth32(3, 2)).unwrap();
        let pass = graph
            .attachment_pass(
                "baseline",
                RasterAttachments {
                    color_resolves: &[],
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
        let storage = plan.data();
        let Operation::AttachmentPass {
            colors,
            depth: depth_index,
        } = &storage.passes[pass.index].operation
        else {
            panic!("expected owned attachment pass");
        };
        assert_eq!(colors.len(), 1);
        let color_operand = &storage.attachments[colors.start];
        assert_eq!(color_operand.view.image(), color);
        assert_eq!(color_operand.clear, [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(color_operand.ops.load, LoadAction::Clear);
        assert_eq!(color_operand.ops.store, StoreOp::Store);
        let depth_operand = &storage.attachments[depth_index.unwrap()];
        assert_eq!(depth_operand.view.image(), depth);
        assert_eq!(depth_operand.clear[0], 0.625);
        assert_eq!(depth_operand.ops.load, LoadAction::Clear);
        assert_eq!(depth_operand.ops.store, StoreOp::Store);
        assert_eq!(
            pool.begin().unwrap_err().kind(),
            GraphErrorKind::CapacityExhausted
        );
        drop(plan);
        assert_eq!(pool.begin().unwrap().compile().unwrap().pass_count(), 0);
    }
}
