//! Private Vulkan instance bootstrap; Device and command execution follow separately.

// qhg deliberately has no public API or production caller yet. Compile the real
// implementation in normal builds, without publishing native handles just to use it.
#![cfg_attr(
    not(test),
    allow(dead_code, reason = "private bootstrap awaits runtime integration")
)]

mod diagnostics;

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::CStr;
use std::fmt;
use std::sync::Arc;

use ash::{Entry, vk};

use diagnostics::Diagnostics;

const REQUIRED_API: u32 = vk::API_VERSION_1_3;
const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";
const LOG_TARGET: &str = "renderer::vulkan";

/// One independently discovered problem; unknown query results are not called absent.
#[derive(Debug, PartialEq, Eq)]
enum PrerequisiteFailure {
    ApiVariant {
        available: u32,
    },
    ApiVersion {
        available: u32,
        required: u32,
    },
    MissingLayer(&'static CStr),
    MissingExtension(&'static CStr),
    Query {
        operation: &'static str,
        result: vk::Result,
    },
    InvalidName {
        operation: &'static str,
    },
}

impl fmt::Display for PrerequisiteFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiVariant { available } => write!(
                f,
                "unsupported Vulkan API variant {} (version {}); require variant 0",
                vk::api_version_variant(*available),
                api_version(*available)
            ),
            Self::ApiVersion {
                available,
                required,
            } => write!(
                f,
                "Vulkan API {} is below required {}",
                api_version(*available),
                api_version(*required)
            ),
            Self::MissingLayer(name) => write!(f, "missing layer {}", name.to_string_lossy()),
            Self::MissingExtension(name) => {
                write!(f, "missing instance extension {}", name.to_string_lossy())
            }
            Self::Query { operation, result } => write!(f, "{operation} failed: {result:?}"),
            Self::InvalidName { operation } => {
                write!(f, "{operation} returned a name without a NUL terminator")
            }
        }
    }
}

#[derive(Debug)]
enum InitError {
    Loader(ash::LoadingError),
    Prerequisites(Vec<PrerequisiteFailure>),
    Vulkan {
        operation: &'static str,
        result: vk::Result,
    },
    MissingEntryPoint(&'static CStr),
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loader(source) => write!(f, "loading the Vulkan loader failed: {source}"),
            Self::Prerequisites(failures) => {
                write!(f, "Vulkan prerequisites not satisfied")?;
                for failure in failures {
                    write!(f, "; {failure}")?;
                }
                Ok(())
            }
            Self::Vulkan { operation, result } => write!(f, "{operation} failed: {result:?}"),
            Self::MissingEntryPoint(name) => write!(
                f,
                "enabled VK_EXT_debug_utils has no entry point {}",
                name.to_string_lossy()
            ),
        }
    }
}

impl Error for InitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Loader(source) => Some(source),
            Self::Vulkan { result, .. } => Some(result),
            Self::Prerequisites(failures) => failures.iter().find_map(|failure| match failure {
                PrerequisiteFailure::Query { result, .. } => Some(result as &dyn Error),
                _ => None,
            }),
            Self::MissingEntryPoint(_) => None,
        }
    }
}

/// Query results remain optional so a failed query cannot invent a missing feature.
#[derive(Default)]
struct Discovery {
    api: Option<u32>,
    layers: Option<BTreeSet<String>>,
    extensions: Option<BTreeSet<String>>,
    failures: Vec<PrerequisiteFailure>,
}

impl Discovery {
    fn check(mut self) -> Result<Self, InitError> {
        if let Some(available) = self.api {
            if vk::api_version_variant(available) != 0 {
                self.failures
                    .push(PrerequisiteFailure::ApiVariant { available });
            } else if available < REQUIRED_API {
                self.failures.push(PrerequisiteFailure::ApiVersion {
                    available,
                    required: REQUIRED_API,
                });
            }
        }
        if let Some(layers) = &self.layers
            && !layers.contains("VK_LAYER_KHRONOS_validation")
        {
            self.failures
                .push(PrerequisiteFailure::MissingLayer(VALIDATION_LAYER));
        }
        if let Some(extensions) = &self.extensions
            && !extensions.contains("VK_EXT_debug_utils")
        {
            self.failures.push(PrerequisiteFailure::MissingExtension(
                ash::ext::debug_utils::NAME,
            ));
        }
        if self.failures.is_empty() {
            Ok(self)
        } else {
            Err(InitError::Prerequisites(self.failures))
        }
    }
}

fn api_version(version: u32) -> String {
    format!(
        "{}.{}.{}",
        vk::api_version_major(version),
        vk::api_version_minor(version),
        vk::api_version_patch(version)
    )
}

fn query<T>(
    result: Result<T, vk::Result>,
    operation: &'static str,
    failures: &mut Vec<PrerequisiteFailure>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(result) => {
            failures.push(PrerequisiteFailure::Query { operation, result });
            None
        }
    }
}

fn extension_names(
    properties: &[vk::ExtensionProperties],
    operation: &'static str,
    failures: &mut Vec<PrerequisiteFailure>,
) -> Option<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    let mut valid = true;
    for property in properties {
        match property.extension_name_as_c_str() {
            Ok(name) => {
                names.insert(name.to_string_lossy().into_owned());
            }
            Err(_) => {
                failures.push(PrerequisiteFailure::InvalidName { operation });
                valid = false;
            }
        }
    }
    valid.then_some(names)
}

fn discover(entry: &Entry) -> Result<Discovery, InitError> {
    let mut discovery = Discovery::default();
    // SAFETY: Entry owns the live loader. These enumeration operations have no
    // instance handles or shared host objects and are called serially here.
    discovery.api = query(
        unsafe { entry.try_enumerate_instance_version() },
        "vkEnumerateInstanceVersion",
        &mut discovery.failures,
    )
    .map(|version| version.unwrap_or(vk::API_VERSION_1_0));

    // SAFETY: Same live Entry; ash manages the enumeration output storage.
    let layers = query(
        unsafe { entry.enumerate_instance_layer_properties() },
        "vkEnumerateInstanceLayerProperties",
        &mut discovery.failures,
    );
    if let Some(properties) = layers {
        let mut names = BTreeSet::new();
        let mut valid = true;
        for property in properties {
            match property.layer_name_as_c_str() {
                Ok(name) => {
                    names.insert(name.to_string_lossy().into_owned());
                    tracing::debug!(target: LOG_TARGET, layer = %name.to_string_lossy(),
                        spec_version = %api_version(property.spec_version),
                        implementation_version = property.implementation_version,
                        "Available Vulkan layer");
                }
                Err(_) => {
                    discovery.failures.push(PrerequisiteFailure::InvalidName {
                        operation: "vkEnumerateInstanceLayerProperties",
                    });
                    valid = false;
                }
            }
        }
        discovery.layers = valid.then_some(names);
    }

    // SAFETY: Null layer name requests global extensions from the live loader.
    let global_properties = query(
        unsafe { entry.enumerate_instance_extension_properties(None) },
        "vkEnumerateInstanceExtensionProperties(global)",
        &mut discovery.failures,
    );
    discovery.extensions = global_properties.as_deref().and_then(|properties| {
        extension_names(
            properties,
            "vkEnumerateInstanceExtensionProperties(global)",
            &mut discovery.failures,
        )
    });

    // An explicitly enabled layer can provide an extension that is not global.
    // Do not query an absent layer, which would itself be an invalid request.
    if discovery
        .layers
        .as_ref()
        .is_some_and(|layers| layers.contains("VK_LAYER_KHRONOS_validation"))
    {
        // SAFETY: The exact NUL-terminated layer name was just enumerated.
        let layer_properties = query(
            unsafe { entry.enumerate_instance_extension_properties(Some(VALIDATION_LAYER)) },
            "vkEnumerateInstanceExtensionProperties(validation layer)",
            &mut discovery.failures,
        );
        let layer_names = layer_properties.as_deref().and_then(|properties| {
            extension_names(
                properties,
                "vkEnumerateInstanceExtensionProperties(validation layer)",
                &mut discovery.failures,
            )
        });
        match (&mut discovery.extensions, layer_names) {
            (Some(global), Some(layer)) => global.extend(layer),
            _ => discovery.extensions = None,
        }
    } else if discovery.layers.is_none() {
        // The layer query failed, so extensions provided by that layer are unknown.
        discovery.extensions = None;
    }
    tracing::info!(target: LOG_TARGET,
        loader_api = ?discovery.api.map(api_version), requested_api = %api_version(REQUIRED_API),
        required_layer = %VALIDATION_LAYER.to_string_lossy(),
        required_extension = %ash::ext::debug_utils::NAME.to_string_lossy(),
        "Checking Vulkan instance prerequisites");
    tracing::debug!(target: LOG_TARGET, extensions = ?discovery.extensions, "Available Vulkan instance extensions");
    discovery.check()
}

/// Owns all dependencies through destruction, including create/destroy callbacks.
/// No sharing/interop API is exposed: Drop has exclusive access to the instance.
struct VulkanInstance {
    entry: Entry,
    instance: ash::Instance,
    debug_utils: Option<ash::ext::debug_utils::Instance>,
    messenger: Option<vk::DebugUtilsMessengerEXT>,
    _diagnostics: Arc<Diagnostics>,
}

impl VulkanInstance {
    fn new(diagnostics: Arc<Diagnostics>) -> Result<Self, InitError> {
        let result = Self::load_and_initialize(diagnostics);
        if let Err(error) = &result {
            tracing::error!(target: LOG_TARGET, error = %error, "Vulkan instance initialization failed");
        }
        result
    }

    fn load_and_initialize(diagnostics: Arc<Diagnostics>) -> Result<Self, InitError> {
        // SAFETY: Use the platform Vulkan loader, which supplies Vulkan entry
        // points; the resulting Entry is retained until after all instance calls.
        let entry = unsafe { Entry::load() }.map_err(InitError::Loader)?;
        Self::initialize(entry, diagnostics, || Ok(()))
    }

    /// The narrow creation hook also tests cleanup after a real Instance exists;
    /// it is not a Backend abstraction or a configurable production failure mode.
    fn initialize(
        entry: Entry,
        diagnostics: Arc<Diagnostics>,
        before_messenger_create: impl FnOnce() -> Result<(), vk::Result>,
    ) -> Result<Self, InitError> {
        discover(&entry)?;
        let application_info = vk::ApplicationInfo::default()
            .application_name(c"renderer")
            .engine_name(c"renderer")
            .api_version(REQUIRED_API);
        let layers = [VALIDATION_LAYER.as_ptr()];
        let extensions = [ash::ext::debug_utils::NAME.as_ptr()];
        let mut callback = diagnostics.messenger_create_info();
        let create_info = vk::InstanceCreateInfo::default()
            .application_info(&application_info)
            .enabled_layer_names(&layers)
            .enabled_extension_names(&extensions)
            .push_next(&mut callback);
        // SAFETY: All names and pNext data remain live for the call. The Arc
        // allocation behind user_data remains live through vkDestroyInstance.
        // No surface, Device, queue, or other instance child exists at this stage.
        let instance = unsafe { entry.create_instance(&create_info, None) }.map_err(|result| {
            InitError::Vulkan {
                operation: "vkCreateInstance",
                result,
            }
        })?;
        // Immediately establish ownership: every later return/unwind drops this
        // guard and destroys the already-created Instance exactly once.
        let mut owner = Self {
            entry,
            instance,
            debug_utils: None,
            messenger: None,
            _diagnostics: Arc::clone(&diagnostics),
        };
        for name in [
            c"vkCreateDebugUtilsMessengerEXT",
            c"vkDestroyDebugUtilsMessengerEXT",
            c"vkSubmitDebugUtilsMessageEXT",
        ] {
            // SAFETY: The Instance belongs to this live Entry, extension enabled,
            // and each entry-point name is a static NUL-terminated string.
            if unsafe {
                owner
                    .entry
                    .get_instance_proc_addr(owner.instance.handle(), name.as_ptr())
            }
            .is_none()
            {
                return Err(InitError::MissingEntryPoint(name));
            }
        }
        let debug_utils = ash::ext::debug_utils::Instance::new(&owner.entry, &owner.instance);
        // Rebuild a standalone create_info: push_next above may have modified its
        // chain, and messenger creation needs no InstanceCreateInfo linkage.
        before_messenger_create().map_err(|result| InitError::Vulkan {
            operation: "vkCreateDebugUtilsMessengerEXT",
            result,
        })?;
        // SAFETY: The extension and its functions were checked, callback state
        // belongs to the live guard, and the resulting messenger is owned once.
        let messenger = unsafe {
            debug_utils.create_debug_utils_messenger(&diagnostics.messenger_create_info(), None)
        }
        .map_err(|result| InitError::Vulkan {
            operation: "vkCreateDebugUtilsMessengerEXT",
            result,
        })?;
        owner.debug_utils = Some(debug_utils);
        owner.messenger = Some(messenger);
        tracing::info!(target: LOG_TARGET, api = %api_version(REQUIRED_API),
            layer = %VALIDATION_LAYER.to_string_lossy(), extension = %ash::ext::debug_utils::NAME.to_string_lossy(),
            "Vulkan instance and debug messenger initialized");
        Ok(owner)
    }
}

impl Drop for VulkanInstance {
    fn drop(&mut self) {
        if let (Some(debug_utils), Some(messenger)) = (&self.debug_utils, self.messenger.take()) {
            // SAFETY: We exclusively own this live messenger and Instance; no
            // concurrent API calls or child objects exist. The callback Arc lives.
            unsafe { debug_utils.destroy_debug_utils_messenger(messenger, None) };
        }
        // SAFETY: All children are destroyed, Instance is uniquely owned, Entry
        // and the pNext callback's user_data remain live until this call returns.
        unsafe { self.instance.destroy_instance(None) };
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lifecycle_tests;
