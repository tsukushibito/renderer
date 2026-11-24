use crate::{Yard, camera};
use renderer::{
    canvas::{self, Layer, Paint},
    core::{
        ImageFormat,
        capacity::GraphRequirements,
        graph::{Graph, ImageDesc, ImageHandle, LoadOp, PassHandle, RasterRect, SampleCount},
        runtime::ManagedImage,
    },
    pipeline::{CameraView, PipelineResult, PreparedSetup, RenderObject, ViewFrame},
};

const WIDTH: usize = 12;
const HEIGHT: usize = 6;
const LEFT: usize = 24;
const PLACEHOLDER: [u8; 4] = [255, 0, 255, 255];

pub(super) struct SecurityMonitor {
    pub camera: CameraView,
    pub latest: Option<ManagedImage>,
    pub reference: [u8; WIDTH * HEIGHT * 4],
}

impl SecurityMonitor {
    pub fn new() -> PipelineResult<Self> {
        Ok(Self {
            camera: camera(3, [WIDTH as u32, HEIGHT as u32])?,
            latest: None,
            reference: [0; WIDTH * HEIGHT * 4],
        })
    }

    pub fn prepare(&self, plan: &mut PreparedSetup<'_>) -> PipelineResult<()> {
        let [w, h] = self.camera.extent;
        let desc = ImageDesc::rgba8(w, h);
        plan.reserve_readback("fresh monitor", desc)?;
        plan.reserve_readback("composite viewport", ImageDesc::rgba8(48, 24))?;
        let prep = plan.preparation_mut();
        prep.reserve_sampler(canvas::IMAGE_SAMPLER);
        prep.reserve_image(desc, 1)?; // Previous export survives until its replacement is accepted.
        prep.add_graph_requirements(GraphRequirements::imports("previous monitor", 1, 1)?)?;
        prep.reserve_raster_pass("canvas", SampleCount::X1, |pass| {
            pass.attachment()?;
            let [sample, fill] = canvas::programs(ImageFormat::Rgba8Unorm)?;
            pass.draws(&sample, 1)?;
            pass.draws(&fill, 1)
        })?;
        Ok(())
    }

    pub fn record(
        &self,
        frame: &mut ViewFrame<'_>,
        step: usize,
        yard: &Yard,
        scratch: &mut Vec<RenderObject>,
    ) -> PipelineResult<Option<ImageHandle>> {
        if !(step + 1).is_multiple_of(6) {
            return Ok(None);
        }
        let actor = usize::from(step >= 7);
        let input = yard.inspect_actor(actor, &self.camera, scratch);
        let hdr = frame.draw_scene(&self.camera, input)?.color;
        let image = frame
            .write_output(self.camera.id, hdr, 1., None)?
            .raw_image();
        frame.graph().export_image(image)?;
        Ok(Some(image))
    }

    pub fn composite(
        &self,
        graph: &mut Graph,
        target: ImageHandle,
        fresh: Option<ImageHandle>,
    ) -> PipelineResult<Option<PassHandle>> {
        let image = match (fresh, &self.latest) {
            (Some(image), _) => Some(image),
            (None, Some(previous)) => {
                Some(graph.import_image("previous monitor", previous.snapshot())?)
            }
            (None, None) => None,
        };
        let paint = match image {
            Some(image) => Paint::Image(graph.image_view(image, 0, 0)?),
            None => Paint::Color([1., 0., 1., 1.]),
        };
        canvas::record(
            graph,
            target,
            LoadOp::Load,
            &[Layer {
                paint,
                rect: RasterRect::new(LEFT as u32, 0, WIDTH as u32, HEIGHT as u32),
                clip: None,
            }],
        )?;
        Ok(fresh
            .map(|image| graph.readback_image("fresh monitor", image))
            .transpose()?)
    }

    pub fn verify_subject(&self, step: usize) {
        let mut visible = [false; 2];
        for row in self.reference.as_chunks::<{ WIDTH * 4 }>().0 {
            for (x, pixel) in row.as_chunks::<4>().0.iter().enumerate() {
                if pixel[1] > 100 && pixel[2] > 100 {
                    visible[usize::from(x >= WIDTH / 2)] = true;
                }
            }
        }
        assert_eq!(
            visible,
            [step == 5, step > 5],
            "monitor subject on frame {}",
            step + 1
        );
    }

    pub fn verify_composite(&self, step: usize, screen: &[u8], main: &[u8]) {
        for (i, pixel) in screen.as_chunks::<4>().0.iter().enumerate() {
            let (x, y) = (i % 48, i / 48);
            let expected = if (LEFT..LEFT + WIDTH).contains(&x) && y < HEIGHT {
                if step < 5 {
                    &PLACEHOLDER
                } else {
                    &self.reference.as_chunks::<4>().0[y * WIDTH + x - LEFT]
                }
            } else {
                &main.as_chunks::<4>().0[i]
            };
            assert_eq!(
                pixel,
                expected,
                "monitor composite frame {}, ({x},{y})",
                step + 1
            );
        }
    }
}
