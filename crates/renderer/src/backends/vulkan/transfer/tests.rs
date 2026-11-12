use super::super::buffer::BufferError;
use super::super::diagnostics::Diagnostics;
use super::super::instance::VulkanInstance;
use super::super::memory::VulkanAllocator;
use super::*;

#[test]
fn transfer_rejects_mixed_owners_roles_sizes_and_uninitialized_source() {
    let valid = [
        BufferSpec {
            role: BufferRole::Upload,
            size: 257,
            initialized: true,
        },
        BufferSpec {
            role: BufferRole::DeviceTransfer,
            size: 257,
            initialized: false,
        },
        BufferSpec {
            role: BufferRole::Readback,
            size: 257,
            initialized: false,
        },
    ];
    validate_request(valid, true).unwrap();
    assert!(validate_request(valid, false).is_err());
    let mut wrong_role = valid;
    wrong_role[1].role = BufferRole::Readback;
    assert!(validate_request(wrong_role, true).is_err());
    let mut wrong_size = valid;
    wrong_size[2].size = 256;
    assert!(validate_request(wrong_size, true).is_err());
    let mut uninitialized = valid;
    uninitialized[0].initialized = false;
    assert!(validate_request(uninitialized, true).is_err());
    assert_eq!(timeout_ns(Duration::ZERO), 0);
    assert_eq!(timeout_ns(Duration::MAX), u64::MAX - 1);
}

fn buffers(allocator: &VulkanAllocator, size: u64) -> TransferBuffers {
    TransferBuffers {
        upload: allocator.create_buffer(BufferRole::Upload, size).unwrap(),
        transfer: allocator
            .create_buffer(BufferRole::DeviceTransfer, size)
            .unwrap(),
        readback: allocator.create_buffer(BufferRole::Readback, size).unwrap(),
    }
}

#[test]
#[ignore = "explicit real-GPU buffer transfer test; requires validation, SyncVal and debug_utils"]
fn vulkan_transfer_smoke() {
    let dispatch = super::super::real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        for iteration in 0..4 {
            let diagnostics = Diagnostics::new();
            let instance = VulkanInstance::new(Arc::clone(&diagnostics)).unwrap();
            super::super::assert_real_validation(&diagnostics);
            let device = VulkanDevice::new(instance).unwrap();
            assert_ne!(
                device.device_type(),
                vk::PhysicalDeviceType::CPU,
                "CPU fallback does not count as real-GPU acceptance"
            );
            let allocator = VulkanAllocator::new(device).unwrap();
            let lifetime = Arc::downgrade(&allocator.owner);

            // An unsubmitted recording can be canceled without a GPU wait.
            let mut canceled = buffers(&allocator, 257);
            canceled.upload.write(0, &[0x7b; 257]).unwrap();
            drop(RecordedTransfer::new(canceled).unwrap());

            let mut reused = buffers(&allocator, 257);
            let mut output = vec![0; 257];
            assert!(matches!(
                reused.readback.read(0, &mut output),
                Err(BufferError::Uninitialized { .. })
            ));
            for pass in 0..3 {
                let pattern: Vec<_> = (0..257)
                    .map(|index| (index * 37 + pass * 19 + iteration) as u8)
                    .collect();
                reused.upload.write(0, &pattern).unwrap();
                let mut pending = RecordedTransfer::new(reused).unwrap().submit().unwrap();
                // Zero-time polling may already observe completion on fast
                // devices. Both outcomes are valid; only an actual successful
                // Fence wait gives the caller its buffers back.
                reused = match pending.wait(Duration::ZERO).unwrap() {
                    Some(done) => done,
                    None => pending
                        .wait(Duration::from_secs(10))
                        .unwrap()
                        .expect("tiny transfer must complete within test budget"),
                };
                reused.readback.read(0, &mut output).unwrap();
                assert_eq!(output, pattern, "iteration {iteration}, pass {pass}");
                assert!(matches!(
                    pending.wait(Duration::ZERO),
                    Err(TransferError::AlreadyFinished)
                ));
                tracing::info!(target: LOG_TARGET, iteration, pass, bytes = output.len(),
                    "GPU transfer readback matched every byte");
            }

            drop(reused);
            let mut abandoned = buffers(&allocator, 257);
            abandoned.upload.write(0, &[0xa5; 257]).unwrap();
            let pending = RecordedTransfer::new(abandoned).unwrap().submit().unwrap();
            drop(allocator);
            assert!(lifetime.upgrade().is_some());
            drop(pending); // bounded completion wait owns the entire chain
            assert!(lifetime.upgrade().is_none());
            let report = diagnostics.snapshot();
            assert!(
                report.is_clean(),
                "iteration {iteration}, including shutdown: {report:#?}"
            );
            tracing::info!(target: LOG_TARGET, iteration, warnings = report.warnings,
                errors = report.errors, callback_failures = report.callback_failures,
                "Vulkan transfer smoke including cancellation, reuse and Drop wait passed");
        }
    });
}
