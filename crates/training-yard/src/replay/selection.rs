use crate::Yard;
mod outline;
use renderer::{
    core::runtime::{FrameCompletion, FrameId},
    picking::{PickImage, PickPass, PickReadback},
    pipeline::{CameraView, PipelineResult, PreparedSetup, RenderObject, SceneRenderer, ViewFrame},
};
use std::time::Duration;

type Target = (usize, u64, usize); // game slot, generation, equipment slot
const IDS: [u32; crate::yard::EQUIPMENT_DRAWS] = [
    0xf1234501, 0x81234502, 0x7f234503, 0x01234504, 5, 6, 7, 8, 9, 10,
];

fn tokens(yard: &Yard, camera: &CameraView) -> [u32; crate::yard::EQUIPMENT_DRAWS] {
    let targets = yard.pick_targets(camera);
    // Every primitive attached to one model slot must respond to a single pick/highlight.
    std::array::from_fn(|i| IDS[targets[..=i].iter().position(|v| *v == targets[i]).unwrap()])
}

struct Click {
    submission: FrameId,
    pixel: PickReadback,
    targets: [Option<Target>; crate::yard::EQUIPMENT_DRAWS],
}

pub(super) struct Selection {
    camera: CameraView,
    pick: PickPass,
    pending: Option<Click>,
}

impl Selection {
    pub fn has_pending_click(&self) -> bool {
        self.pending.is_some()
    }
    pub fn discard_pending(&mut self, drawing: &mut SceneRenderer) -> PipelineResult<()> {
        if let Some(click) = self.pending.take() {
            drawing
                .runtime()
                .wait_for_frame(click.submission, Duration::from_secs(15))?
                .ok_or("pick timed out during shutdown")?;
        }
        Ok(())
    }
    pub fn prepare(plan: &mut PreparedSetup<'_>, camera: CameraView) -> PipelineResult<Self> {
        outline::prepare(plan, camera.extent)?;
        Ok(Self {
            camera,
            pick: PickPass::prepare(plan, camera.extent, IDS.len(), 4)?,
            pending: None,
        })
    }
    pub fn sample(
        &mut self,
        frame: &mut ViewFrame<'_>,
        yard: &Yard,
        objects: &mut Vec<RenderObject>,
    ) -> PipelineResult<(PickImage, [PickReadback; 4])> {
        let ids = tokens(yard, &self.camera);
        let scene = yard.extract(&self.camera, objects);
        let image = self
            .pick
            .record(frame, &self.camera, scene, &ids[..scene.objects.len()])?;
        Ok((
            image,
            [
                image.read_pixel(frame.graph(), [12, 15])?,
                image.read_pixel(frame.graph(), [36, 7])?,
                image.read_pixel(frame.graph(), [0, 0])?,
                image.read_pixel(frame.graph(), [12, 20])?,
            ],
        ))
    }
    pub fn check(
        &self,
        samples: [PickReadback; 4],
        result: &mut FrameCompletion,
        yard: &Yard,
        step: usize,
    ) -> PipelineResult<()> {
        let [armor, body, empty, tip] = samples;
        let targets = yard.pick_targets(&self.camera);
        let lookup = |id: u32| {
            targets[IDS
                .iter()
                .position(|&key| key == id)
                .expect("unknown GPU ID")]
        };
        let part = armor.read(result)?.and_then(|id| lookup(id.get()));
        assert_eq!(
            part,
            (step != 21).then_some((0, if step >= 22 { 2 } else { 1 }, 1)),
            "armor pick on step {step}"
        );
        assert_eq!(
            body.read(result)?.and_then(|id| lookup(id.get())),
            Some((1, 1, 0)),
            "body pick on step {step}"
        );
        assert_eq!(empty.read(result)?, None);
        assert_eq!(
            tip.read(result)?.is_some(),
            step != 4 && step != 21,
            "skinned armor tip on step {step}"
        );
        Ok(())
    }
    pub fn highlight(
        &self,
        graph: &mut renderer::core::graph::Graph,
        target: renderer::core::graph::ImageHandle,
        image: PickImage,
        yard: &Yard,
    ) -> PipelineResult<outline::Readbacks> {
        let id = yard
            .pick_targets(&self.camera)
            .iter()
            .position(|t| *t == Some((0, 1, 1)))
            .map(|index| IDS[index]);
        outline::record(graph, target, image, id)
    }
    pub fn click(
        &mut self,
        drawing: &mut SceneRenderer,
        yard: &Yard,
        objects: &mut Vec<RenderObject>,
    ) -> PipelineResult<()> {
        let mut frame = drawing.begin_frame()?;
        let mut camera = self.camera;
        let mut shifted = camera.camera.clip_from_world();
        shifted[3][0] += 0.5;
        camera.camera = renderer::pbr::ViewInput::new(
            shifted,
            [-0.5, 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            renderer::pbr::Projection::Orthographic,
        )?;
        let ids = tokens(yard, &camera);
        let scene = yard.extract(&camera, objects);
        let image = self
            .pick
            .record(&mut frame, &camera, scene, &ids[..scene.objects.len()])?;
        let pixel = image.read_pixel(frame.graph(), [24, 15])?;
        let submission = frame.submit()?.id();
        self.pending = Some(Click {
            submission,
            pixel,
            targets: yard.pick_targets(&camera),
        });
        Ok(())
    }
    pub fn resolve_after_respawn(
        &mut self,
        drawing: &mut SceneRenderer,
        yard: &Yard,
    ) -> PipelineResult<()> {
        let click = self.pending.take().expect("pre-despawn mouse query");
        let mut done = drawing
            .runtime()
            .wait_for_frame(click.submission, Duration::from_secs(15))?
            .ok_or("pick timed out")?;
        let id = click
            .pixel
            .read(&mut done)?
            .ok_or("click missed the armor")?
            .get();
        let index = IDS
            .iter()
            .position(|&candidate| candidate == id)
            .expect("unknown hit token");
        assert_eq!(click.targets[index], Some((0, 1, 1)));
        let now = yard.pick_targets(&self.camera);
        assert_eq!(now[index], Some((0, 2, 1)));
        assert!(
            !now.contains(&click.targets[index]),
            "a late click selected a recycled actor"
        );
        Ok(())
    }
}
