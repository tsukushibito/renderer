use crate::Locomotion;
use glam::DVec3;
use sashiko_animation::{AnimationError, Clip, ClipTimeline, JointMask, LocalBuffer};
use std::{num::NonZeroU32, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Upper,
    Full,
}
/// A recording only needs timing. Display assets pair the same timeline with its masked pose.
pub enum ActionClip {
    Timeline(ClipTimeline),
    Posed { clip: Arc<Clip>, mask: JointMask },
}
impl ActionClip {
    fn timeline(&self) -> &ClipTimeline {
        match self {
            Self::Timeline(t) => t,
            Self::Posed { clip, .. } => clip.timeline(),
        }
    }
}
pub struct Action {
    pub id: u32,
    pub clip: ActionClip,
    pub slot: Slot,
    pub priority: u32,
    pub interruptible: bool,
    /// Fixed ticks of visual fade in and out. Gameplay ownership changes immediately.
    pub fade_ticks: u32,
}
pub struct ActionSet(Vec<Action>);
impl ActionSet {
    pub fn new(mut actions: Vec<Action>) -> Result<Arc<Self>, AnimationError> {
        actions.sort_by_key(|a| a.id);
        if actions.windows(2).any(|w| w[0].id == w[1].id)
            || actions.iter().any(|a| a.clip.timeline().is_looping())
        {
            return Err(AnimationError::Invalid(
                "actions require unique IDs and one-shot clips",
            ));
        }
        let mut skeleton = None;
        for action in &actions {
            if let ActionClip::Posed { clip, mask } = &action.clip {
                if !Arc::ptr_eq(clip.skeleton(), mask.skeleton())
                    || skeleton.is_some_and(|rig| !Arc::ptr_eq(rig, clip.skeleton()))
                {
                    return Err(AnimationError::Invalid("incompatible action skeleton"));
                }
                skeleton = Some(clip.skeleton());
            }
        }
        Ok(Arc::new(Self(actions)))
    }
}
/// Unique within a character's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    UnknownAction,
    FullBodyActive,
    Protected,
    Priority,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notification {
    Started(u32, Handle),
    Rejected(u32, Rejection),
    Interrupted(Handle),
    Finished(Handle),
    MovementMarker(u32),
    ActionMarker(Handle, u32),
}
#[derive(Debug, PartialEq)]
pub struct TickResult {
    pub translation: DVec3,
    pub notifications: usize,
}

#[derive(Clone, Copy)]
struct Playing {
    action: usize,
    handle: Handle,
    elapsed: u64,
}
impl Playing {
    fn weight(self, actions: &ActionSet) -> f32 {
        let duration = actions.0[self.action].fade_ticks;
        if duration == 0 {
            1.
        } else {
            (self.elapsed as f64 / f64::from(duration)).min(1.) as f32
        }
    }
}
#[derive(Clone, Copy, Default)]
struct Layer {
    active: Option<Playing>,
    // Hold the last visible sample while its contribution decays.
    outgoing: Option<(Playing, u32)>,
}
impl Layer {
    fn retire(&mut self, actions: &ActionSet, finished: bool) {
        if let Some(play) = self.active.take() {
            let fade = actions.0[play.action].fade_ticks;
            // A completed one-tick action still has a final display sample.
            if finished || (fade > 0 && play.elapsed > 0) {
                self.outgoing = Some((play, fade));
            }
        }
    }
    fn update(
        &mut self,
        actions: &ActionSet,
        rate: NonZeroU32,
        emit: &mut impl FnMut(Notification),
    ) {
        if let Some((_, remaining)) = &mut self.outgoing {
            if *remaining <= 1 {
                self.outgoing = None;
            } else {
                *remaining -= 1;
            }
        }
        if let Some(play) = &mut self.active {
            play.elapsed += 1;
            let t = actions.0[play.action].clip.timeline();
            for id in t.events_at(play.elapsed, rate) {
                emit(Notification::ActionMarker(play.handle, id));
            }
            if t.finished_at(play.elapsed, rate) {
                emit(Notification::Finished(play.handle));
                self.retire(actions, true);
            }
        }
    }
}
#[derive(Clone, Copy, Default)]
struct Slots {
    upper: Layer,
    full: Layer,
    serial: u64,
}
impl Slots {
    fn request(
        &mut self,
        id: u32,
        actions: &ActionSet,
        emit: &mut impl FnMut(Notification),
    ) -> Result<(), AnimationError> {
        let Ok(index) = actions.0.binary_search_by_key(&id, |a| a.id) else {
            emit(Notification::Rejected(id, Rejection::UnknownAction));
            return Ok(());
        };
        let incoming = &actions.0[index];
        if incoming.slot == Slot::Upper && self.full.active.is_some() {
            emit(Notification::Rejected(id, Rejection::FullBodyActive));
            return Ok(());
        }
        for play in [self.upper.active, self.full.active].into_iter().flatten() {
            let old = &actions.0[play.action];
            let reason = if !old.interruptible {
                Some(Rejection::Protected)
            } else if incoming.priority <= old.priority {
                Some(Rejection::Priority)
            } else {
                None
            };
            if let Some(reason) = reason {
                emit(Notification::Rejected(id, reason));
                return Ok(());
            }
        }
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or(AnimationError::Invalid("action handle overflow"))?;
        for layer in [&mut self.upper, &mut self.full] {
            if let Some(play) = layer.active {
                emit(Notification::Interrupted(play.handle));
                layer.retire(actions, false);
            }
        }
        let target = match incoming.slot {
            Slot::Upper => &mut self.upper,
            Slot::Full => &mut self.full,
        };
        let handle = Handle(self.serial);
        target.active = Some(Playing {
            action: index,
            handle,
            elapsed: 0,
        });
        emit(Notification::Started(id, handle));
        Ok(())
    }
}

pub struct Character {
    locomotion: Locomotion,
    actions: Arc<ActionSet>,
    slots: Slots,
    tick: u64,
}
impl Character {
    pub fn new(locomotion: Locomotion, actions: Arc<ActionSet>) -> Self {
        Self {
            locomotion,
            actions,
            slots: Slots::default(),
            tick: 0,
        }
    }
    pub fn tick(&self) -> u64 {
        self.tick
    }
    pub fn locomotion(&self) -> &Locomotion {
        &self.locomotion
    }

    /// Commit one successive tick. Requests are arbitrated in input order before markers.
    /// On error, both the caller's output slice and the entire character remain unchanged.
    pub fn advance(
        &mut self,
        tick: u64,
        speed: f32,
        requests: &[u32],
        out: &mut [Notification],
    ) -> Result<TickResult, AnimationError> {
        if self.tick.checked_add(1) != Some(tick) {
            return Err(AnimationError::Invalid("expected next character tick"));
        }
        let rate = self.locomotion.tick_rate();
        let mut locomotion = self.locomotion.clone();
        let movement = locomotion.advance(speed)?;
        let mut next = self.slots;
        let mut request_count = 0usize;
        for &id in requests {
            next.request(id, &self.actions, &mut |_| request_count += 1)?;
        }
        let full = next.full.active;
        let movement_count = if full.is_none() {
            movement.event_count()
        } else {
            0
        };
        let mut needed = request_count as u128 + movement_count as u128;
        for play in [next.upper.active, full].into_iter().flatten() {
            let t = self.actions.0[play.action].clip.timeline();
            let elapsed = play.elapsed + 1;
            needed += t.event_count_at(elapsed, rate) + u128::from(t.finished_at(elapsed, rate));
        }
        if needed > out.len() as u128 {
            return Err(AnimationError::EventCapacity { required: needed });
        }
        let mut count = 0;
        let mut emit = |event| {
            out[count] = event;
            count += 1;
        };
        if !requests.is_empty() {
            let mut replay = self.slots;
            for &id in requests {
                replay.request(id, &self.actions, &mut emit)?;
            }
        }
        let translation = if let Some(play) = full {
            self.actions.0[play.action]
                .clip
                .timeline()
                .translation_at_tick(play.elapsed + 1, rate)
        } else {
            for id in movement.events() {
                emit(Notification::MovementMarker(id));
            }
            movement.translation
        };
        next.upper.update(&self.actions, rate, &mut emit);
        next.full.update(&self.actions, rate, &mut emit);
        self.tick = tick;
        self.slots = next;
        self.locomotion = locomotion;
        Ok(TickResult {
            translation,
            notifications: count,
        })
    }

    /// Compose the settled tick into reusable storage. The sample can be submitted to
    /// a palette batch or evaluated into a full or partial pose workspace.
    pub fn compose(
        &self,
        walk: &Clip,
        jog: Option<&Clip>,
        out: &mut LocalBuffer,
    ) -> Result<(), AnimationError> {
        let (w, j, weight) = self.locomotion.sample();
        out.mix(&[
            (walk.sample_phase(w)?, 1. - weight),
            (jog.unwrap_or(walk).sample_phase(j)?, weight),
        ])?;
        let mut overlay = |play: Playing, strength: f32| {
            let ActionClip::Posed { clip, mask } = &self.actions.0[play.action].clip else {
                return Err(AnimationError::Invalid("action has no display clip"));
            };
            out.overlay(
                clip.interpolated_at(play.elapsed, self.locomotion.tick_rate()),
                mask,
                strength,
            )
        };
        for layer in [self.slots.upper, self.slots.full] {
            if let Some((play, remaining)) = layer.outgoing {
                let fade = self.actions.0[play.action].fade_ticks;
                overlay(
                    play,
                    if fade == 0 {
                        1.
                    } else {
                        play.weight(&self.actions) * (remaining as f32 / fade as f32)
                    },
                )?;
            }
            if let Some(play) = layer.active {
                overlay(play, play.weight(&self.actions))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gaits;
    use glam::{Quat, Vec3};
    use sashiko_animation::{LocalPose, PoseWorkspace, Skeleton};
    #[test]
    fn one_tick_action_without_a_fade_displays_its_terminal_sample_once() {
        let rig = Skeleton::new(vec![None]).unwrap();
        let clip = |x, looping| {
            Clip::new(
                rig.clone(),
                vec![LocalPose {
                    translation: Vec3::X * x,
                    scale: Vec3::ONE,
                    rotation: Quat::IDENTITY,
                }],
                NonZeroU32::MIN,
                looping,
                vec![],
                vec![],
            )
            .unwrap()
        };
        let walk = clip(0., true);
        let action = Action {
            id: 1,
            slot: Slot::Full,
            priority: 0,
            interruptible: false,
            fade_ticks: 0,
            clip: ActionClip::Posed {
                clip: Arc::new(clip(1., false)),
                mask: JointMask::new(&rig, vec![1.]).unwrap(),
            },
        };
        let gaits = Gaits::new(walk.timeline().clone(), None, 1., 2., NonZeroU32::MIN).unwrap();
        let mut actor = Character::new(
            Locomotion::new(gaits),
            ActionSet::new(vec![action]).unwrap(),
        );
        let mut local = LocalBuffer::new(&rig);
        let mut pose = PoseWorkspace::all(&rig);
        let mut out = [Notification::MovementMarker(0); 2];
        for (tick, requests, expected) in [(1, &[1][..], 1.), (2, &[][..], 0.)] {
            let result = actor.advance(tick, 0., requests, &mut out).unwrap();
            if tick == 1 {
                assert!(
                    matches!(out,[Notification::Started(1,a),Notification::Finished(b)] if a==b)
                );
                assert_eq!(result.notifications, 2);
            }
            actor.compose(&walk, None, &mut local).unwrap();
            local.sample().evaluate(&mut pose).unwrap();
            assert_eq!(pose.joint(0).unwrap().w_axis.x, expected);
        }
    }
}
