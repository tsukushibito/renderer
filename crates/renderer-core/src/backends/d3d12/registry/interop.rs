//! Native engine texture exchange at a resource-change boundary.

use super::*;
use crate::backends::d3d12::device::DeviceOwner;
use crate::graph::ImageDesc;
use windows_core::{IUnknown, Interface};

/// Keeps the Device, queue and diagnostic lifetime alive for engine-created
/// textures. Query the returned COM interfaces with the host's D3D12 bindings.
#[derive(Clone)]
pub struct D3D12ExternalDevice(Arc<DeviceOwner>);

impl D3D12ExternalDevice {
    pub fn device(&self) -> IUnknown {
        self.0
            .raw
            .cast()
            .expect("a native Device implements IUnknown")
    }

    /// Issue a native queue operation under the shared submission lock. Do not
    /// recursively call Renderer queue methods while holding this borrow.
    pub fn with_queue<T>(&self, work: impl FnOnce(&IUnknown) -> T) -> Result<T, Error> {
        let _guard = self.0.queue_gate.lock().unwrap_or_else(|e| e.into_inner());
        self.0.ensure_active()?;
        let queue = self
            .0
            .queue
            .cast()
            .expect("a native queue implements IUnknown");
        Ok(work(&queue))
    }
}

impl D3D12Runtime {
    pub fn external_device(&self) -> D3D12ExternalDevice {
        D3D12ExternalDevice(Arc::clone(&self.device))
    }

    /// Register a host texture. Its COM reference is retained through the last
    /// Renderer command. Reserve frame image slots for its descriptor, or call
    /// prepare_working_set after registration to prepare slots for live imports.
    /// All Renderer accesses use the normal Graph state transitions.
    ///
    /// # Safety
    /// Supply a committed DEFAULT texture in COMMON on every subresource. The
    /// host must retire earlier accesses or establish a completed handoff to
    /// the Runtime's DIRECT queue. Until release_external_image succeeds, the
    /// host must keep the allocation resident and give Renderer exclusive GPU
    /// and CPU access, including through other COM objects, mappings and
    /// aliases. Set initialized only if all texels of every mip are defined.
    pub unsafe fn register_external_image(
        &mut self,
        resource: &IUnknown,
        desc: ImageDesc,
        initialized: bool,
    ) -> Result<ImportImage, Error> {
        self.device.ensure_active()?;
        if desc.byte_len().is_none() || desc.array_layers != 1 {
            return Err(Error::invalid("invalid external texture dimensions"));
        }
        let native: ID3D12Resource = resource
            .cast()
            .map_err(|_| Error::invalid("expected ID3D12Resource"))?;
        let mut device: Option<IUnknown> = None;
        // SAFETY: QueryInterface returned a live resource; the out parameter
        // receives an owned reference to its creating Device.
        unsafe { native.GetDevice(&mut device) }
            .map_err(|e| self.device.native("ID3D12DeviceChild::GetDevice", e))?;
        if device != Some(self.external_device().device()) {
            return Err(Error::wrong_owner(
                "external texture belongs to another Device",
            ));
        }
        // SAFETY: The queried resource is retained for the duration of all queries.
        let actual = unsafe { native.GetDesc() };
        let mut expected = DeviceImage::graph_resource_desc(desc);
        expected.Alignment = actual.Alignment;
        if actual != expected {
            return Err(Error::invalid(
                "external texture descriptor differs from the Graph texture contract",
            ));
        }
        let mut heap = D3D12_HEAP_PROPERTIES::default();
        // SAFETY: A live resource and valid out pointer; reserved textures
        // without heap properties are rejected by the native query.
        unsafe { native.GetHeapProperties(Some(&mut heap), None) }
            .ok()
            .map_err(|e| self.device.native("ID3D12Resource::GetHeapProperties", e))?;
        if heap.Type != D3D12_HEAP_TYPE_DEFAULT
            || heap.CreationNodeMask != 1
            || heap.VisibleNodeMask != 1
        {
            return Err(Error::unsupported(
                "external texture requires the default heap on node zero",
            ));
        }
        let mut registry = self.pool.registry.lock().unwrap_or_else(|e| e.into_inner());
        registry.register_host_texture(native, desc, initialized)
    }

    /// Return the host texture in COMMON when every recording, GPU and
    /// presentation lease has returned. None requests another poll. Some
    /// reports whether all texels remain initialized and invalidates the key.
    pub fn release_external_image(&mut self, key: ResourceKey) -> Result<Option<bool>, Error> {
        self.device.ensure_active()?;
        let mut registry = self.pool.registry.lock().unwrap_or_else(|e| e.into_inner());
        let entry = registry.entry(key)?;
        if !matches!(&entry.physical().0, Native::HostImage(_)) {
            return Err(Error::invalid("expected an external image registration"));
        }
        if Arc::strong_count(entry.physical()) != 1 {
            return Ok(None);
        }
        let initialized = registry.refresh_image(key)?.initialized();
        let entry = &mut registry.entries[key.index as usize];
        entry.registered = false;
        entry.retire();
        Ok(Some(initialized))
    }
}

impl Registry {
    fn register_host_texture(
        &mut self,
        resource: ID3D12Resource,
        desc: ImageDesc,
        initialized: bool,
    ) -> Result<ImportImage, Error> {
        if self
            .entries
            .iter()
            .filter_map(|e| e.physical.as_ref())
            .any(|p| matches!(&p.0, Native::HostImage(existing) if existing == &resource))
        {
            return Err(Error::invalid(
                "texture is already registered on this Runtime",
            ));
        }
        let index = self
            .entries
            .iter()
            .position(|entry| entry.physical.is_none() && entry.generation < u32::MAX)
            .unwrap_or(self.entries.len());
        if index == self.capacity.resource_capacity {
            return Err(Error::capacity_with(
                "external texture registration capacity exhausted",
                RuntimeCapacityError::new(
                    RuntimeCapacityKind::Resources,
                    CapacityRecovery::Reconfigure,
                    Some(index as u64),
                    Some(index as u64 + 1),
                ),
            ));
        }
        let key = ResourceKey {
            owner: self.owner,
            index: index as u32,
            generation: self.entries.get(index).map_or(1, |e| e.generation + 1),
        };
        let mut entry = Entry::new(
            Arc::new(Physical(Native::HostImage(resource))),
            ResourceDesc::Image(desc),
            self.capacity,
        );
        entry.generation = key.generation;
        entry.registered = true;
        // Host textures never enter the candidate lists for transient Graph storage.
        entry.prepared = false;
        if initialized {
            if entry.initialized.capacity() == 0 {
                return Err(Error::capacity_with(
                    "external texture needs one initialized range",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::GraphMetadata,
                        CapacityRecovery::Reconfigure,
                        Some(0),
                        Some(1),
                    ),
                ));
            }
            entry.initialized.push(0..u64::from(desc.mip_levels));
        }
        let imported = ImportImage::new(key, entry.revision, desc, &entry.initialized)
            .map_err(|e| Error::invalid(e.to_string()))?;
        if index == self.entries.len() {
            self.entries.push(entry);
        } else {
            self.entries[index] = entry;
        }
        Ok(imported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        ResourceClass, Runtime, RuntimeConfig, RuntimeLimits, WorkingSet, native_image_tests,
    };

    #[test]
    #[ignore = "requires a hardware D3D12 adapter and configured validation"]
    fn d3d12_host_texture_roundtrip() {
        let desc = ImageDesc::rgba8(2, 1);
        let set = WorkingSet {
            resources: &[ResourceClass::Image { desc, count: 1 }],
            ..WorkingSet::default()
        };
        let mut runtime = Runtime::new(
            RuntimeConfig::D3D12(Default::default()),
            RuntimeLimits::default(),
            set,
        )
        .unwrap();
        let dx = runtime.as_d3d12().unwrap();
        let monitor = dx.validation_monitor();
        let host_device = dx.external_device();
        let device: ID3D12Device = host_device.device().cast().unwrap();
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
            ..Default::default()
        };
        let mut external: Option<IUnknown> = None;
        // SAFETY: The test creates a committed single-node texture on the queried
        // Device, with a supported Graph descriptor and no in-flight users.
        unsafe {
            device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &DeviceImage::graph_resource_desc(desc),
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut external,
            )
        }
        .unwrap();
        let external = external.unwrap();
        for write in [true, false] {
            // SAFETY: The fresh/returned committed texture is exclusively available
            // in COMMON. Only the second handoff declares the now verified pixels.
            let image = unsafe {
                runtime
                    .as_d3d12_mut()
                    .unwrap()
                    .register_external_image(&external, desc, !write)
            }
            .unwrap();
            let key = image.key();
            assert!(runtime.delete(key).is_err());
            native_image_tests::transfer(&mut runtime, &image, write, |rt| {
                assert!(
                    rt.as_d3d12_mut()
                        .unwrap()
                        .release_external_image(key)
                        .unwrap()
                        .is_none()
                );
            });
            assert_eq!(
                runtime
                    .as_d3d12_mut()
                    .unwrap()
                    .release_external_image(key)
                    .unwrap(),
                Some(true)
            );
            assert!(runtime.refresh_image(key).is_err());
        }
        drop((external, device, host_device, runtime));
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:#?}");
        if cfg!(feature = "development-validation") {
            assert!(
                report.debug_layer_enabled
                    && report.gpu_based_validation_enabled
                    && report.teardown_checked
            );
        }
    }
}
