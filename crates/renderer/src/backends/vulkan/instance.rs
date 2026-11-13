//! Vulkan Instance ownership, validation, and loader discovery.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::CStr;
use std::fmt;

use ash::{Entry, vk};

#[cfg(test)]
use super::diagnostics::Diagnostics;
use super::diagnostics::DiagnosticsHandle;

pub(super) const REQUIRED_API: u32 = vk::API_VERSION_1_3;
const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";
const REQUIRED_EXTENSIONS: [&CStr; 2] =
    [ash::ext::debug_utils::NAME, ash::ext::layer_settings::NAME];
pub(super) const LOG_TARGET: &str = "renderer::vulkan";

/// Native validation defaults to Required with `development-validation`, and to
/// Disabled otherwise. Renderer safety and execution contracts are always enabled.
/// Loader environment/settings may still inject layers in either mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationMode {
    Required,
    Disabled,
}

impl Default for ValidationMode {
    fn default() -> Self {
        if cfg!(feature = "development-validation") {
            Self::Required
        } else {
            Self::Disabled
        }
    }
}

impl ValidationMode {
    fn extensions(self) -> &'static [&'static CStr] {
        match self {
            Self::Required => &REQUIRED_EXTENSIONS,
            Self::Disabled => {
                #[cfg(feature = "vulkan-diagnostics")]
                {
                    &[ash::ext::debug_utils::NAME]
                }
                #[cfg(not(feature = "vulkan-diagnostics"))]
                {
                    &[]
                }
            }
        }
    }
}

// Compare extension names, never the addresses of independently stored names.
// The references retain their input lifetime until the native call finishes.
fn requested_extension_names<'a>(
    validation: ValidationMode,
    additional: &[&'a CStr],
) -> BTreeSet<&'a CStr> {
    validation
        .extensions()
        .iter()
        .copied()
        .chain(additional.iter().copied())
        .collect()
}

/// One independently discovered problem; unknown query results are not called absent.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PrerequisiteFailure {
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
pub(super) enum InitError {
    Loader(ash::LoadingError),
    Prerequisites(Vec<PrerequisiteFailure>),
    Vulkan {
        operation: &'static str,
        result: vk::Result,
    },
    #[cfg(feature = "vulkan-diagnostics")]
    MissingEntryPoint(&'static CStr),
    #[cfg(not(feature = "vulkan-diagnostics"))]
    DiagnosticsNotCompiled,
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
            #[cfg(not(feature = "vulkan-diagnostics"))]
            Self::DiagnosticsNotCompiled => write!(
                f,
                "native validation requires the vulkan-diagnostics or development-validation feature"
            ),
            #[cfg(feature = "vulkan-diagnostics")]
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
            #[cfg(feature = "vulkan-diagnostics")]
            Self::MissingEntryPoint(_) => None,
            #[cfg(not(feature = "vulkan-diagnostics"))]
            Self::DiagnosticsNotCompiled => None,
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
    fn check(mut self, validation: ValidationMode) -> Result<Self, InitError> {
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
        if validation == ValidationMode::Required
            && let Some(layers) = &self.layers
            && !layers.contains("VK_LAYER_KHRONOS_validation")
        {
            self.failures
                .push(PrerequisiteFailure::MissingLayer(VALIDATION_LAYER));
        }
        if let Some(extensions) = &self.extensions {
            for &name in validation.extensions() {
                if !extensions.contains(name.to_str().expect("static extension name is UTF-8")) {
                    self.failures
                        .push(PrerequisiteFailure::MissingExtension(name));
                }
            }
        }
        if self.failures.is_empty() {
            Ok(self)
        } else {
            Err(InitError::Prerequisites(self.failures))
        }
    }
}

pub(super) fn api_version(version: u32) -> String {
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

fn discover(entry: &Entry, validation: ValidationMode) -> Result<Discovery, InitError> {
    let mut discovery = Discovery::default();
    // SAFETY: Entry owns the live loader. These enumeration operations have no
    // instance handles or shared host objects and are called serially here.
    discovery.api = query(
        unsafe { entry.try_enumerate_instance_version() },
        "vkEnumerateInstanceVersion",
        &mut discovery.failures,
    )
    .map(|version| version.unwrap_or(vk::API_VERSION_1_0));

    // Disabled mode does not depend on validation-layer installation or queries.
    let layers = if validation == ValidationMode::Required {
        // SAFETY: Same live Entry; ash manages the enumeration output storage.
        query(
            unsafe { entry.enumerate_instance_layer_properties() },
            "vkEnumerateInstanceLayerProperties",
            &mut discovery.failures,
        )
    } else {
        None
    };
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
    } else if validation == ValidationMode::Required && discovery.layers.is_none() {
        // The layer query failed, so extensions provided by that layer are unknown.
        discovery.extensions = None;
    }
    tracing::info!(target: LOG_TARGET,
        loader_api = ?discovery.api.map(api_version), requested_api = %api_version(REQUIRED_API),
        validation = ?validation,
        required_extensions = ?validation.extensions(),
        "Checking Vulkan instance prerequisites");
    tracing::debug!(target: LOG_TARGET, extensions = ?discovery.extensions, "Available Vulkan instance extensions");
    discovery.check(validation)
}

/// Owns all dependencies through destruction, including create/destroy callbacks.
/// A Device owner retains this value until all Device calls have finished.
pub(super) struct VulkanInstance {
    _entry: Entry,
    instance: ash::Instance,
    #[cfg(feature = "vulkan-diagnostics")]
    debug_utils: Option<ash::ext::debug_utils::Instance>,
    #[cfg(feature = "vulkan-diagnostics")]
    messenger: Option<vk::DebugUtilsMessengerEXT>,
    #[cfg(feature = "vulkan-diagnostics")]
    _diagnostics: DiagnosticsHandle,
    #[cfg(feature = "vulkan-diagnostics")]
    validation: ValidationMode,
}

impl VulkanInstance {
    #[cfg(feature = "vulkan-diagnostics")]
    pub(super) fn validation_mode(&self) -> ValidationMode {
        self.validation
    }

    #[cfg(test)]
    pub(super) fn new(diagnostics: DiagnosticsHandle) -> Result<Self, InitError> {
        // Native acceptance always requests validation, even in a shipping build.
        Self::with_validation(diagnostics, ValidationMode::Required)
    }

    pub(super) fn with_validation(
        diagnostics: DiagnosticsHandle,
        validation: ValidationMode,
    ) -> Result<Self, InitError> {
        let result = Self::load_and_initialize(diagnostics, validation);
        if let Err(error) = &result {
            tracing::error!(target: LOG_TARGET, error = %error, "Vulkan instance initialization failed");
        }
        result
    }

    pub(super) fn raw(&self) -> &ash::Instance {
        &self.instance
    }

    #[cfg(all(feature = "presentation", windows))]
    pub(super) fn entry(&self) -> &Entry {
        &self._entry
    }

    #[cfg(all(feature = "presentation", windows))]
    pub(super) fn with_extensions(
        diagnostics: DiagnosticsHandle,
        validation: ValidationMode,
        extensions: &[&'static CStr],
    ) -> Result<Self, InitError> {
        // SAFETY: The platform loader owns these entry points through Instance
        // destruction, just as on the headless creation path.
        let entry = unsafe { Entry::load() }.map_err(InitError::Loader)?;
        Self::initialize_with_extensions(entry, diagnostics, validation, extensions, || Ok(()))
    }

    fn load_and_initialize(
        diagnostics: DiagnosticsHandle,
        validation: ValidationMode,
    ) -> Result<Self, InitError> {
        // SAFETY: Use the platform Vulkan loader, which supplies Vulkan entry
        // points; the resulting Entry is retained until after all instance calls.
        let entry = unsafe { Entry::load() }.map_err(InitError::Loader)?;
        Self::initialize_with_validation(entry, diagnostics, validation, || Ok(()))
    }

    /// The narrow creation hook also tests cleanup after a real Instance exists;
    /// it is not a Backend abstraction or a configurable production failure mode.
    fn initialize(
        entry: Entry,
        diagnostics: DiagnosticsHandle,
        before_messenger_create: impl FnOnce() -> Result<(), vk::Result>,
    ) -> Result<Self, InitError> {
        Self::initialize_with_validation(
            entry,
            diagnostics,
            ValidationMode::Required,
            before_messenger_create,
        )
    }

    fn initialize_with_validation(
        entry: Entry,
        diagnostics: DiagnosticsHandle,
        validation: ValidationMode,
        before_messenger_create: impl FnOnce() -> Result<(), vk::Result>,
    ) -> Result<Self, InitError> {
        Self::initialize_with_extensions(
            entry,
            diagnostics,
            validation,
            &[],
            before_messenger_create,
        )
    }

    fn initialize_with_extensions(
        entry: Entry,
        diagnostics: DiagnosticsHandle,
        validation: ValidationMode,
        additional_extensions: &[&'static CStr],
        before_messenger_create: impl FnOnce() -> Result<(), vk::Result>,
    ) -> Result<Self, InitError> {
        #[cfg(not(feature = "vulkan-diagnostics"))]
        if validation == ValidationMode::Required {
            return Err(InitError::DiagnosticsNotCompiled);
        }
        let discovery = discover(&entry, validation)?;
        if let Some(available) = discovery.extensions {
            let missing = additional_extensions
                .iter()
                .filter(|name| !available.contains(name.to_string_lossy().as_ref()))
                .map(|&name| PrerequisiteFailure::MissingExtension(name))
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(InitError::Prerequisites(missing));
            }
        }
        let application_info = vk::ApplicationInfo::default()
            .application_name(c"renderer")
            .engine_name(c"renderer")
            .api_version(REQUIRED_API);
        let layers = if validation == ValidationMode::Required {
            vec![VALIDATION_LAYER.as_ptr()]
        } else {
            Vec::new()
        };
        let extensions: Vec<_> = requested_extension_names(validation, additional_extensions)
            .into_iter()
            .map(CStr::as_ptr)
            .collect();
        #[cfg(feature = "vulkan-diagnostics")]
        let enable_sync: vk::Bool32 = vk::TRUE;
        // ash's byte-slice setter counts bytes rather than BOOL32 values. Keep
        // the correctly typed value and its count explicit for this setting.
        #[cfg(feature = "vulkan-diagnostics")]
        let settings = [vk::LayerSettingEXT {
            value_count: 1,
            p_values: std::ptr::from_ref(&enable_sync).cast(),
            ..vk::LayerSettingEXT::default()
                .layer_name(VALIDATION_LAYER)
                .setting_name(c"validate_sync")
                .ty(vk::LayerSettingTypeEXT::BOOL32)
        }];
        #[cfg(feature = "vulkan-diagnostics")]
        let mut layer_settings = vk::LayerSettingsCreateInfoEXT::default().settings(&settings);
        #[cfg(feature = "vulkan-diagnostics")]
        let mut callback = diagnostics.messenger_create_info();
        #[cfg_attr(not(feature = "vulkan-diagnostics"), allow(unused_mut))]
        let mut create_info = vk::InstanceCreateInfo::default()
            .application_info(&application_info)
            .enabled_layer_names(&layers)
            .enabled_extension_names(&extensions);
        #[cfg(feature = "vulkan-diagnostics")]
        {
            create_info = create_info.push_next(&mut callback);
        }
        #[cfg(feature = "vulkan-diagnostics")]
        if validation == ValidationMode::Required {
            create_info = create_info.push_next(&mut layer_settings);
        }
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
        #[cfg_attr(not(feature = "vulkan-diagnostics"), allow(unused_mut))]
        let mut owner = Self {
            _entry: entry,
            instance,
            #[cfg(feature = "vulkan-diagnostics")]
            debug_utils: None,
            #[cfg(feature = "vulkan-diagnostics")]
            messenger: None,
            #[cfg(feature = "vulkan-diagnostics")]
            _diagnostics: diagnostics.clone(),
            #[cfg(feature = "vulkan-diagnostics")]
            validation,
        };
        #[cfg(feature = "vulkan-diagnostics")]
        {
            for name in [
                c"vkCreateDebugUtilsMessengerEXT",
                c"vkDestroyDebugUtilsMessengerEXT",
                c"vkSubmitDebugUtilsMessageEXT",
            ] {
                // SAFETY: The Instance belongs to this live Entry, extension enabled,
                // and each entry-point name is a static NUL-terminated string.
                if unsafe {
                    owner
                        ._entry
                        .get_instance_proc_addr(owner.instance.handle(), name.as_ptr())
                }
                .is_none()
                {
                    return Err(InitError::MissingEntryPoint(name));
                }
            }
            let debug_utils = ash::ext::debug_utils::Instance::new(&owner._entry, &owner.instance);
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
        }
        #[cfg(not(feature = "vulkan-diagnostics"))]
        let _ = (diagnostics, before_messenger_create);
        tracing::info!(target: LOG_TARGET, api = %api_version(REQUIRED_API),
            validation = ?validation, extensions = ?validation.extensions(),
            synchronization_validation_requested = validation == ValidationMode::Required,
            "Vulkan instance initialized");
        Ok(owner)
    }
}

impl Drop for VulkanInstance {
    fn drop(&mut self) {
        #[cfg(feature = "vulkan-diagnostics")]
        if let (Some(debug_utils), Some(messenger)) = (&self.debug_utils, self.messenger.take()) {
            // SAFETY: We exclusively own this live messenger and Instance; no
            // concurrent API calls exist. A containing Device owner has already
            // destroyed its Device before dropping this value. The callback Arc lives.
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
pub(super) mod lifecycle_tests;
