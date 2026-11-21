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
    objects: Vec<RenderObject>,
    ids: Vec<InstanceId>,
    indices: HashMap<InstanceId, usize>,
}

impl Scene {
    pub fn spawn(&mut self, object: RenderObject) -> InstanceId {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = InstanceId(
            NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("render instance identity space exhausted"),
        );
        self.indices.insert(id, self.objects.len());
        self.ids.push(id);
        self.objects.push(object);
        id
    }

    pub fn remove(&mut self, id: InstanceId) -> Option<RenderObject> {
        let index = self.indices.remove(&id)?;
        self.ids.swap_remove(index);
        if let Some(moved) = self.ids.get(index) {
            self.indices.insert(*moved, index);
        }
        Some(self.objects.swap_remove(index))
    }

    pub fn instance_mut(&mut self, id: InstanceId) -> Option<&mut RenderObject> {
        self.objects.get_mut(*self.indices.get(&id)?)
    }

    /// Apply the extracted changes as a unit. On stale input no pose changes.
    /// Duplicate entries take the final supplied pose.
    pub fn update_transforms(
        &mut self,
        changes: &[(InstanceId, Transform)],
    ) -> Result<(), InstanceId> {
        for &(id, _) in changes {
            if !self.indices.contains_key(&id) {
                return Err(id);
            }
        }
        for &(id, pose) in changes {
            self.objects[self.indices[&id]].transform = pose;
        }
        Ok(())
    }

    pub fn instances(&self) -> impl Iterator<Item = (InstanceId, &RenderObject)> {
        self.ids.iter().copied().zip(&self.objects)
    }

    /// Borrow the exact storage consumed by the standard drawing path.
    pub fn objects(&self) -> &[RenderObject] {
        &self.objects
    }
}
