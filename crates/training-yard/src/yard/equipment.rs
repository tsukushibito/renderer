//! Game-side equipment requests. The replay controls when each worker's result
//! is delivered, so superseded requests and deleted actors are reproducible.
use renderer::{core::graph::GraphError, mesh::MeshData, pbr::PbrVertex, pipeline::PipelineResult};
use std::thread::JoinHandle;

#[derive(Clone, Copy)]
pub enum EquipmentAsset {
    Narrow,
    Wide,
    Broken,
    Spare,
}

pub(super) struct LoadedArmor {
    pub mesh: MeshData,
    pub color: [f32; 3],
}

impl EquipmentAsset {
    fn load(self) -> Result<LoadedArmor, GraphError> {
        let (width, color) = match self {
            Self::Wide => (0.35, [1., 1., 0.]),
            Self::Narrow | Self::Broken | Self::Spare => (0.17, [1., 0., 1.]),
        };
        let vertices = [
            (-width, 0.),
            (width, 0.),
            (-width * 0.5, 0.75),
            (width * 0.5, 0.75),
        ]
        .map(|(x, y)| {
            PbrVertex::new([x, y, 0.5], [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap()
        });
        let last = if matches!(self, Self::Broken) { 4 } else { 2 };
        let mesh = MeshData::pbr(&vertices, &[0, 1, 2, 1, 3, last])?;
        Ok(LoadedArmor { mesh, color })
    }
}

struct Request {
    actor: u64,
    sequence: u64,
    worker: JoinHandle<Result<LoadedArmor, GraphError>>,
}

#[derive(Default)]
pub(super) struct EquipmentLoads {
    latest: u64,
    jobs: [Option<Request>; 4],
}

impl EquipmentLoads {
    pub fn request(&mut self, actor: u64, asset: EquipmentAsset) -> PipelineResult<()> {
        let slot = &mut self.jobs[asset as usize];
        if slot.is_some() {
            return Err("an equipment request for this asset is already pending".into());
        }
        let sequence = self
            .latest
            .checked_add(1)
            .ok_or("equipment sequence exhausted")?;
        let worker = std::thread::Builder::new().spawn(move || asset.load())?;
        self.latest = sequence;
        *slot = Some(Request {
            actor,
            sequence,
            worker,
        });
        Ok(())
    }

    pub fn deliver(
        &mut self,
        occupant: Option<u64>,
        asset: EquipmentAsset,
    ) -> PipelineResult<Option<Result<LoadedArmor, GraphError>>> {
        let request = self.jobs[asset as usize]
            .take()
            .ok_or("missing equipment request")?;
        let result = request
            .worker
            .join()
            .map_err(|_| "equipment worker panicked")?;
        if occupant != Some(request.actor) || request.sequence != self.latest {
            return Ok(None);
        }
        Ok(Some(result))
    }
}
