//! Retained rendering inputs for a game without its own render-object store.
//! Hosts with extracted objects can pass an iterator of references directly.
use renderer::{pbr::Transform, pipeline::RenderObject};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

/// Process-wide identity. Delayed selection or loading results cannot resolve
/// to another object after removal, including in another scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceId(u64);

#[derive(Default)]
pub struct Scene {
    objects: HashMap<InstanceId, RenderObject>,
}

impl Scene {
    pub fn spawn(&mut self, object: RenderObject) -> InstanceId {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = InstanceId(
            NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("render instance identity space exhausted"),
        );
        self.objects.insert(id, object);
        id
    }

    pub fn remove(&mut self, id: InstanceId) -> Option<RenderObject> {
        self.objects.remove(&id)
    }

    pub fn instance_mut(&mut self, id: InstanceId) -> Option<&mut RenderObject> {
        self.objects.get_mut(&id)
    }

    /// Apply the extracted changes as a unit. On stale input no pose changes.
    /// Duplicate entries take the final supplied pose.
    pub fn update_transforms(
        &mut self,
        changes: &[(InstanceId, Transform)],
    ) -> Result<(), InstanceId> {
        for &(id, _) in changes {
            if !self.objects.contains_key(&id) {
                return Err(id);
            }
        }
        for &(id, pose) in changes {
            self.objects
                .get_mut(&id)
                .expect("validated instance")
                .transform = pose;
        }
        Ok(())
    }

    pub fn instances(&self) -> impl Iterator<Item = (InstanceId, &RenderObject)> {
        self.objects.iter().map(|(&id, object)| (id, object))
    }
}
