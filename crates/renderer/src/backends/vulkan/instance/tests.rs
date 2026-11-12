use super::super::real_vulkan_dispatch;
use super::*;

fn available(api: u32, layers: &[&str], extensions: &[&str]) -> Discovery {
    Discovery {
        api: Some(api),
        layers: Some(layers.iter().map(|name| (*name).to_owned()).collect()),
        extensions: Some(extensions.iter().map(|name| (*name).to_owned()).collect()),
        failures: Vec::new(),
    }
}

#[test]
fn reports_all_discoverable_missing_prerequisites_together() {
    let Err(InitError::Prerequisites(failures)) =
        available(vk::API_VERSION_1_2, &[], &[]).check(ValidationMode::Required)
    else {
        panic!("expected all missing prerequisites");
    };
    assert_eq!(
        failures,
        [
            PrerequisiteFailure::ApiVersion {
                available: vk::API_VERSION_1_2,
                required: REQUIRED_API
            },
            PrerequisiteFailure::MissingLayer(VALIDATION_LAYER),
            PrerequisiteFailure::MissingExtension(ash::ext::debug_utils::NAME),
            PrerequisiteFailure::MissingExtension(ash::ext::layer_settings::NAME),
        ]
    );
    let text = InitError::Prerequisites(failures).to_string();
    for expected in [
        "1.2.0",
        "1.3.0",
        "VK_LAYER_KHRONOS_validation",
        "VK_EXT_debug_utils",
        "VK_EXT_layer_settings",
    ] {
        assert!(
            text.contains(expected),
            "missing diagnostic context: {text}"
        );
    }
    for api in [vk::API_VERSION_1_3, vk::make_api_version(0, 1, 4, 0)] {
        assert!(
            available(
                api,
                &["VK_LAYER_KHRONOS_validation"],
                &["VK_EXT_debug_utils", "VK_EXT_layer_settings"]
            )
            .check(ValidationMode::Required)
            .is_ok()
        );
    }
    let variant_api = vk::make_api_version(1, 1, 3, 0);
    let Err(InitError::Prerequisites(failures)) = available(
        variant_api,
        &["VK_LAYER_KHRONOS_validation"],
        &["VK_EXT_debug_utils", "VK_EXT_layer_settings"],
    )
    .check(ValidationMode::Required) else {
        panic!("nonstandard API variant must be rejected");
    };
    assert_eq!(
        failures,
        [PrerequisiteFailure::ApiVariant {
            available: variant_api
        }]
    );
    assert!(failures[0].to_string().contains("variant 1"));
}

#[test]
fn failed_query_preserves_cause_and_other_known_missing_requirements() {
    let discovery = Discovery {
        api: Some(vk::API_VERSION_1_0),
        layers: None,
        extensions: Some(BTreeSet::new()),
        failures: vec![PrerequisiteFailure::Query {
            operation: "vkEnumerateInstanceLayerProperties",
            result: vk::Result::ERROR_OUT_OF_HOST_MEMORY,
        }],
    };
    let error = match discovery.check(ValidationMode::Required) {
        Err(error) => error,
        Ok(_) => panic!("query failure must reject initialization"),
    };
    assert!(error.source().unwrap().is::<vk::Result>());
    let InitError::Prerequisites(failures) = error else {
        panic!("wrong error type");
    };
    assert_eq!(failures.len(), 4);
    assert!(
        failures
            .iter()
            .any(|failure| matches!(failure, PrerequisiteFailure::ApiVersion { .. }))
    );
    assert!(failures.contains(&PrerequisiteFailure::MissingExtension(
        ash::ext::debug_utils::NAME
    )));
    assert!(
        !failures
            .iter()
            .any(|failure| matches!(failure, PrerequisiteFailure::MissingLayer(_)))
    );
}

#[test]
fn malformed_driver_property_is_a_query_error_not_a_missing_extension() {
    let properties = [vk::ExtensionProperties {
        extension_name: [b'x' as std::ffi::c_char; vk::MAX_EXTENSION_NAME_SIZE],
        spec_version: 1,
    }];
    let mut failures = Vec::new();
    assert!(extension_names(&properties, "extension enumeration", &mut failures).is_none());
    assert_eq!(
        failures,
        [PrerequisiteFailure::InvalidName {
            operation: "extension enumeration"
        }]
    );
}

#[test]
fn missing_loader_keeps_underlying_loading_error() {
    // A full nonexistent path tests loading failure without loading the platform
    // Vulkan library or altering process/system loader configuration.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("qhg-test-no-vulkan-loader")
        .join("missing-vulkan-loader");
    assert!(!path.exists());
    // SAFETY: This explicit nonexistent path cannot load library code. The
    // returned error is wrapped by the same variant used in normal initialization.
    let source = match unsafe { Entry::load_from(&path) } {
        Err(error) => error,
        Ok(_) => panic!("nonexistent loader unexpectedly loaded"),
    };
    let error = InitError::Loader(source);
    assert!(error.to_string().contains("loading the Vulkan loader"));
    assert!(error.source().unwrap().is::<ash::LoadingError>());
}

#[test]
#[ignore = "explicit Vulkan 1.3 runtime test; requires validation layer and debug_utils"]
fn vulkan_instance_smoke() {
    let dispatch = real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        for iteration in 0..4 {
            let diagnostics = Diagnostics::new();
            let instance = VulkanInstance::new(Arc::clone(&diagnostics)).unwrap_or_else(|error| {
                panic!(
                    "iteration {iteration}: {error}; {:?}",
                    diagnostics.snapshot()
                )
            });
            super::super::assert_real_validation(&diagnostics);
            let message = vk::DebugUtilsMessengerCallbackDataEXT::default()
                .message_id_number(0x716867)
                .message_id_name(c"renderer.qhg.callback-smoke")
                .message(c"qhg INFO callback delivery check");
            // SAFETY: The enabled extension and messenger belong to this live
            // instance. All callback strings remain live; INFO is a diagnostic
            // submission, not a deliberate invalid Vulkan operation.
            unsafe {
                instance
                    .debug_utils
                    .as_ref()
                    .unwrap()
                    .submit_debug_utils_message(
                        vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
                        vk::DebugUtilsMessageTypeFlagsEXT::GENERAL,
                        &message,
                    );
            }
            assert!(
                diagnostics.snapshot().messages.iter().any(|message| {
                    message.message_id_name.as_deref() == Some("renderer.qhg.callback-smoke")
                        && message.message_id_number == 0x716867
                }),
                "real debug messenger did not deliver the submitted INFO message"
            );
            drop(instance);
            let report = diagnostics.snapshot();
            assert!(
                report.is_clean(),
                "iteration {iteration}, including shutdown: {report:#?}"
            );
            tracing::info!(target: LOG_TARGET, iteration, warnings = report.warnings,
                errors = report.errors, callback_failures = report.callback_failures,
                "Vulkan instance smoke including shutdown passed");
        }
    });
}

#[test]
#[ignore = "explicit Vulkan 1.3 runtime test; requires validation layer and debug_utils"]
fn vulkan_instance_partial_failure() {
    let dispatch = real_vulkan_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        for iteration in 0..4 {
            let diagnostics = Diagnostics::new();
            // SAFETY: Use the platform loader and retain it in initialize's guard
            // until all Instance calls and failure cleanup have finished.
            let entry = unsafe { Entry::load() }.expect("load Vulkan for explicit runtime test");
            let result = VulkanInstance::initialize(entry, Arc::clone(&diagnostics), || {
                // Deliberate host-side failure at the creation boundary: no
                // invalid Vulkan call and no claim of reproducing driver OOM.
                Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY)
            });
            assert!(
                matches!(
                    result,
                    Err(InitError::Vulkan {
                        operation: "vkCreateDebugUtilsMessengerEXT",
                        result: vk::Result::ERROR_OUT_OF_HOST_MEMORY,
                    })
                ),
                "unexpected partial initialization result at iteration {iteration}"
            );
            assert_eq!(
                Arc::strong_count(&diagnostics),
                1,
                "failed owner retained diagnostic state"
            );
            super::super::assert_real_validation(&diagnostics);
            let report = diagnostics.snapshot();
            assert!(
                report.is_clean(),
                "partial failure/shutdown {iteration}: {report:#?}"
            );
            tracing::info!(target: LOG_TARGET, iteration, warnings = report.warnings,
                errors = report.errors, callback_failures = report.callback_failures,
                "Injected partial initialization failure cleanup passed");
        }
    });
}
