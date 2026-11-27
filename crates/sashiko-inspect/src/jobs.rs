//! Bounded diagnostic requests shared by window controls, in-process tests and NDJSON.
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, num::NonZeroUsize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Report,
    StartVideo,
    StopVideo,
    Clip,
    SetLayers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Manual,
    Gameplay,
    Warning,
    FrameTime,
    TestFailure,
}

/// The host applies these switches to its displayed view at a frame boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RenderLayers {
    pub hud: bool,
    pub collision: bool,
    pub velocity: bool,
    pub ai: bool,
}
impl Default for RenderLayers {
    fn default() -> Self {
        Self {
            hud: true,
            collision: false,
            velocity: false,
            ai: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    pub action: Action,
    #[serde(default = "main_view")]
    pub view: String,
    #[serde(default = "final_color")]
    pub stage: String,
    pub extent: Option<[u32; 2]>,
    pub fps: Option<u32>,
    pub layers: Option<RenderLayers>,
    pub trigger: Option<Trigger>,
    /// Host monotonic time for internally generated triggers.
    #[serde(skip)]
    pub raised_ns: Option<u64>,
}

fn main_view() -> String {
    "main".into()
}
fn final_color() -> String {
    "final_srgb_ui".into()
}

impl CaptureRequest {
    pub fn new(action: Action) -> Self {
        Self {
            action,
            view: main_view(),
            stage: final_color(),
            extent: None,
            fps: None,
            layers: None,
            trigger: None,
            raised_ns: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Capabilities {
    pub actions: Vec<Action>,
    pub views: Vec<String>,
    pub stages: Vec<String>,
    pub layers: Vec<String>,
    pub depth_limit: Option<&'static str>,
    pub video_unavailable: Option<&'static str>,
    pub clip_unavailable: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Queued,
    Running { phase: &'static str },
    Complete { artifact: String },
    Applied,
    Linked { job: String },
    Partial { artifact: String, reason: String },
    Failed { reason: String },
}

impl Status {
    fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Complete { .. }
                | Self::Partial { .. }
                | Self::Failed { .. }
                | Self::Applied
                | Self::Linked { .. }
        )
    }
}

struct Entry {
    action: Action,
    id: u64,
    tick: u64,
    request: Option<CaptureRequest>,
    status: Status,
}

pub struct Ticket {
    pub id: u64,
    pub requested_tick: u64,
    pub request: CaptureRequest,
}

pub struct Jobs {
    capabilities: Capabilities,
    entries: VecDeque<Entry>,
    limit: usize,
    next: u64,
}

impl Jobs {
    pub fn new(capabilities: Capabilities, limit: NonZeroUsize) -> Self {
        Self {
            capabilities,
            entries: VecDeque::with_capacity(limit.get()),
            limit: limit.get(),
            next: 1,
        }
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    pub fn submit(&mut self, tick: u64, request: CaptureRequest) -> Result<u64, &'static str> {
        if !self.capabilities.actions.contains(&request.action)
            || !self.capabilities.views.contains(&request.view)
            || !self.capabilities.stages.contains(&request.stage)
        {
            return Err("UNSUPPORTED");
        }
        if request.extent.is_some_and(|size| size.contains(&0)) || request.fps == Some(0) {
            return Err("BAD_REQUEST");
        }
        if (request.action == Action::SetLayers) != request.layers.is_some() {
            return Err("BAD_REQUEST");
        }
        if request.trigger.is_some() && request.action != Action::Clip {
            return Err("BAD_REQUEST");
        }
        if request.stage == "scene_depth_d32" && request.action != Action::Report {
            return Err("UNSUPPORTED");
        }
        if request.action != Action::StartVideo
            && (request.extent.is_some() || request.fps.is_some())
        {
            return Err("UNSUPPORTED");
        }
        let id = self.next;
        let next = id.checked_add(1).ok_or("CAPACITY")?;
        if self.entries.len() == self.limit {
            let index = self
                .entries
                .iter()
                .position(|e| e.status.terminal())
                .ok_or("CAPACITY")?;
            self.entries.remove(index);
        }
        self.entries.push_back(Entry {
            action: request.action,
            id,
            tick,
            request: Some(request),
            status: Status::Queued,
        });
        self.next = next;
        Ok(id)
    }

    /// Host claims a request exactly once. File and encoder workers publish
    /// progress back to the host without borrowing this queue during I/O.
    pub fn take(&mut self) -> Option<Ticket> {
        let entry = self.entries.iter_mut().find(|e| e.request.is_some())?;
        entry.status = Status::Running { phase: "accepted" };
        Some(Ticket {
            id: entry.id,
            requested_tick: entry.tick,
            request: entry.request.take()?,
        })
    }

    pub fn status(&self, id: u64) -> Result<&Status, &'static str> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| &e.status)
            .ok_or("UNKNOWN_JOB")
    }
    pub fn action(&self, id: u64) -> Option<Action> {
        self.entries.iter().find(|e| e.id == id).map(|e| e.action)
    }

    pub fn update(&mut self, id: u64, status: Status) -> Result<(), &'static str> {
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or("UNKNOWN_JOB")?;
        if !matches!(entry.status, Status::Running { .. }) || matches!(status, Status::Queued) {
            return Err("BAD_JOB_STATE");
        }
        entry.status = status;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_active_save_keeps_its_slot_until_terminal() {
        let mut jobs = Jobs::new(
            Capabilities {
                actions: vec![Action::Report],
                views: vec![main_view()],
                stages: vec![final_color()],
                layers: vec![],
                depth_limit: None,
                video_unavailable: None,
                clip_unavailable: None,
            },
            NonZeroUsize::MIN,
        );
        let request = CaptureRequest::new(Action::Report);
        let id = jobs.submit(12, request.clone()).unwrap();
        assert_eq!(jobs.take().unwrap().requested_tick, 12);
        jobs.update(id, Status::Running { phase: "writing" })
            .unwrap();
        assert_eq!(jobs.submit(13, request.clone()), Err("CAPACITY"));
        assert_eq!(
            jobs.submit(13, CaptureRequest::new(Action::StartVideo)),
            Err("UNSUPPORTED")
        );
        jobs.update(
            id,
            Status::Failed {
                reason: "disk full".into(),
            },
        )
        .unwrap();
        assert!(matches!(jobs.status(id), Ok(Status::Failed { .. })));
        let next = jobs.submit(14, request).unwrap();
        assert_ne!(id, next);
        assert_eq!(jobs.status(id).unwrap_err(), "UNKNOWN_JOB");
    }
}
