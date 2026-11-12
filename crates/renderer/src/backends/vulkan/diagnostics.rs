//! Vulkan callback state. Its owner must retain the `Arc` through Instance destruction.

use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use ash::vk;
use tracing::{Dispatch, Level};

const RECENT_MESSAGE_LIMIT: usize = 64;
const PROBLEM_MESSAGE_LIMIT: usize = 128;
const STARTUP_STATUS_LIMIT: usize = 8;
const STARTUP_STATUS_ID: &str = "WARNING-CreateInstance-status-message";

#[derive(Clone, Debug)]
pub(super) struct DiagnosticMessage {
    pub severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    pub message_types: vk::DebugUtilsMessageTypeFlagsEXT,
    pub message_id_number: i32,
    pub message_id_name: Option<String>,
    pub message: String,
}

/// Counts are independent of log filtering and bounded message retention.
/// Acceptance checks take this snapshot only after callback-producing operations
/// finish; separate counters do not form a coherent snapshot during callbacks.
#[derive(Debug)]
pub(super) struct DiagnosticSnapshot {
    pub warnings: usize,
    pub errors: usize,
    pub callback_failures: usize,
    pub messages_seen: usize,
    /// The most recent messages at every severity, rather than a full transcript.
    pub messages: Vec<DiagnosticMessage>,
    /// The first warning/error messages; verbose traffic cannot displace them.
    pub problems: Vec<DiagnosticMessage>,
    pub dropped_problems: usize,
    /// Distinct layer startup reports must survive later verbose traffic.
    pub layer_status_messages: Vec<String>,
    pub dropped_layer_statuses: usize,
}

impl DiagnosticSnapshot {
    pub fn is_clean(&self) -> bool {
        self.warnings == 0
            && self.errors == 0
            && self.callback_failures == 0
            && self.dropped_problems == 0
    }
}

#[derive(Default)]
struct RetainedMessages {
    recent: VecDeque<DiagnosticMessage>,
    problems: Vec<DiagnosticMessage>,
    dropped_problems: usize,
    layer_status_messages: Vec<String>,
    dropped_layer_statuses: usize,
}

pub(super) struct Diagnostics {
    dispatch: Dispatch,
    warnings: AtomicUsize,
    errors: AtomicUsize,
    callback_failures: AtomicUsize,
    messages_seen: AtomicUsize,
    dispatch_failed: AtomicBool,
    retained: Mutex<RetainedMessages>,
}

impl Diagnostics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            // Vulkan may call back on a different thread. Retain the initiating
            // application's subscriber without installing a global subscriber.
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            warnings: AtomicUsize::new(0),
            errors: AtomicUsize::new(0),
            callback_failures: AtomicUsize::new(0),
            messages_seen: AtomicUsize::new(0),
            dispatch_failed: AtomicBool::new(false),
            retained: Mutex::new(RetainedMessages::default()),
        })
    }

    pub fn messenger_create_info(self: &Arc<Self>) -> vk::DebugUtilsMessengerCreateInfoEXT<'_> {
        vk::DebugUtilsMessengerCreateInfoEXT::default()
            .message_severity(
                vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                    | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                    | vk::DebugUtilsMessageSeverityFlagsEXT::INFO
                    | vk::DebugUtilsMessageSeverityFlagsEXT::VERBOSE,
            )
            .message_type(
                vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                    | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                    | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
            )
            .pfn_user_callback(Some(debug_callback))
            .user_data(Arc::as_ptr(self).cast_mut().cast())
    }

    pub fn snapshot(&self) -> DiagnosticSnapshot {
        let retained = self.lock_retained();
        DiagnosticSnapshot {
            warnings: self.warnings.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            callback_failures: self.callback_failures.load(Ordering::Relaxed),
            messages_seen: self.messages_seen.load(Ordering::Relaxed),
            messages: retained.recent.iter().cloned().collect(),
            problems: retained.problems.clone(),
            dropped_problems: retained.dropped_problems,
            layer_status_messages: retained.layer_status_messages.clone(),
            dropped_layer_statuses: retained.dropped_layer_statuses,
        }
    }

    fn fail_callback(&self) {
        self.callback_failures.fetch_add(1, Ordering::Relaxed);
        // Do not repeatedly invoke a panicking application subscriber. This also
        // prevents repeated panic-payload leaks at this FFI boundary. Callbacks
        // already dispatching concurrently may each still fail once.
        self.dispatch_failed.store(true, Ordering::Relaxed);
    }

    fn lock_retained(&self) -> MutexGuard<'_, RetainedMessages> {
        match self.retained.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                self.fail_callback();
                self.retained.clear_poison();
                poisoned.into_inner()
            }
        }
    }

    fn retain(&self, message: &DiagnosticMessage) {
        let mut retained = self.lock_retained();
        if message.message_id_name.as_deref() == Some(STARTUP_STATUS_ID)
            && !retained.layer_status_messages.contains(&message.message)
        {
            if retained.layer_status_messages.len() < STARTUP_STATUS_LIMIT {
                retained.layer_status_messages.push(message.message.clone());
            } else {
                retained.dropped_layer_statuses = retained.dropped_layer_statuses.saturating_add(1);
            }
        }
        if message.severity.intersects(
            vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
        ) {
            if retained.problems.len() < PROBLEM_MESSAGE_LIMIT {
                retained.problems.push(message.clone());
            } else {
                retained.dropped_problems = retained.dropped_problems.saturating_add(1);
            }
        }
        if retained.recent.len() == RECENT_MESSAGE_LIMIT {
            retained.recent.pop_front();
        }
        retained.recent.push_back(message.clone());
    }
}

/// Copies only while Vulkan's callback pointers are valid. Null optional strings
/// are permitted; the caller guarantees every non-null pointer is NUL-terminated.
unsafe fn copy_string(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        None
    } else {
        // SAFETY: The callback caller guarantees a live, NUL-terminated string;
        // ownership is copied before returning to Vulkan. UTF-8 is not assumed.
        Some(
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

fn log_message(message: &DiagnosticMessage) {
    macro_rules! emit {
        ($level:expr) => {
            tracing::event!(
                target: "renderer::vulkan::validation",
                $level,
                message_id_number = message.message_id_number,
                message_id_name = message.message_id_name.as_deref().unwrap_or(""),
                message_types = ?message.message_types,
                "{}", message.message,
            )
        };
    }
    if message
        .severity
        .contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR)
    {
        emit!(Level::ERROR);
    } else if message
        .severity
        .contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING)
    {
        emit!(Level::WARN);
    } else if message
        .severity
        .contains(vk::DebugUtilsMessageSeverityFlagsEXT::INFO)
    {
        emit!(Level::INFO);
    } else {
        emit!(Level::TRACE);
    }
}

unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user_data: *mut c_void,
) -> vk::Bool32 {
    // SAFETY: messenger_create_info passes Arc::as_ptr; the Instance owner keeps
    // that allocation alive until all callbacks, including destruction, finish.
    // A null pointer is handled defensively and is never supplied by our owner.
    let Some(diagnostics) = (unsafe { user_data.cast::<Diagnostics>().as_ref() }) else {
        return vk::FALSE;
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        diagnostics.messages_seen.fetch_add(1, Ordering::Relaxed);
        // Count before allocation, formatting or external subscriber calls.
        if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
            diagnostics.warnings.fetch_add(1, Ordering::Relaxed);
        }
        if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
            diagnostics.errors.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: Vulkan supplies callback data valid for this invocation. Null
        // is handled defensively without dereferencing or fabricating data.
        let Some(data) = (unsafe { data.as_ref() }) else {
            diagnostics.fail_callback();
            return;
        };
        let message = DiagnosticMessage {
            severity,
            message_types,
            message_id_number: data.message_id_number,
            // SAFETY: The Vulkan callback guarantees its non-null string fields
            // remain valid for this invocation. copy_string takes owned copies.
            message_id_name: unsafe { copy_string(data.p_message_id_name) },
            // SAFETY: Same callback string lifetime as the message ID above.
            message: unsafe { copy_string(data.p_message) }.unwrap_or_default(),
        };
        diagnostics.retain(&message);
        // The collector lock is released before application code is called.
        // Subscribers must not reenter Vulkan from this callback.
        if !diagnostics.dispatch_failed.load(Ordering::Relaxed) {
            tracing::dispatcher::with_default(&diagnostics.dispatch, || log_message(&message));
        }
    }));
    if let Err(payload) = result {
        diagnostics.fail_callback();
        // An arbitrary subscriber panic payload can itself panic on Drop. Do
        // not drop it across FFI; disable further external dispatch instead.
        std::mem::forget(payload);
    }
    // Diagnostic delivery never asks Vulkan to abort the originating operation.
    vk::FALSE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::CString;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    fn disabled_diagnostics() -> Arc<Diagnostics> {
        let dispatch = Dispatch::new(tracing::subscriber::NoSubscriber::default());
        tracing::dispatcher::with_default(&dispatch, Diagnostics::new)
    }

    fn deliver(
        diagnostics: &Arc<Diagnostics>,
        severity: vk::DebugUtilsMessageSeverityFlagsEXT,
        name: &CStr,
        message: &CStr,
    ) -> vk::Bool32 {
        let info = diagnostics.messenger_create_info();
        let data = vk::DebugUtilsMessengerCallbackDataEXT::default()
            .message_id_number(-42)
            .message_id_name(name)
            .message(message);
        // SAFETY: Both C strings and callback data remain live for the call;
        // diagnostics retains the stable Arc allocation behind user_data.
        unsafe {
            info.pfn_user_callback.expect("callback is configured")(
                severity,
                vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION,
                &data,
                info.p_user_data,
            )
        }
    }

    #[derive(Debug)]
    struct LoggedEvent {
        level: Level,
        target: &'static str,
        fields: BTreeMap<String, String>,
    }

    #[derive(Default)]
    struct Fields(BTreeMap<String, String>);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().to_owned(), value.to_owned());
        }
    }

    struct RecordingSubscriber(Arc<Mutex<Vec<LoggedEvent>>>);

    impl Subscriber for RecordingSubscriber {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.0.lock().expect("test events lock").push(LoggedEvent {
                level: *event.metadata().level(),
                target: event.metadata().target(),
                fields: fields.0,
            });
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    #[test]
    fn messages_are_owned_and_lossy_utf8_is_supported() {
        let diagnostics = disabled_diagnostics();
        let name = CString::new("test-id").unwrap();
        let message = c"invalid \xff text";
        assert_eq!(
            deliver(
                &diagnostics,
                vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                &name,
                message
            ),
            vk::FALSE
        );
        drop(name);
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.warnings, 1);
        assert_eq!(snapshot.errors, 0);
        assert_eq!(snapshot.callback_failures, 0);
        assert_eq!(snapshot.messages[0].message_id_number, -42);
        assert_eq!(
            snapshot.messages[0].message_id_name.as_deref(),
            Some("test-id")
        );
        assert_eq!(snapshot.messages[0].message, "invalid \u{fffd} text");
        assert!(!snapshot.is_clean());
    }

    #[test]
    fn captured_subscriber_receives_mapped_levels_on_another_thread() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let dispatch = Dispatch::new(RecordingSubscriber(Arc::clone(&events)));
        let diagnostics = tracing::dispatcher::with_default(&dispatch, Diagnostics::new);
        let worker = Arc::clone(&diagnostics);
        std::thread::spawn(move || {
            for severity in [
                vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
                vk::DebugUtilsMessageSeverityFlagsEXT::VERBOSE,
            ] {
                deliver(&worker, severity, c"mapped-id", c"mapped message");
            }
        })
        .join()
        .unwrap();
        let events = events.lock().unwrap();
        assert_eq!(
            events.iter().map(|event| event.level).collect::<Vec<_>>(),
            [Level::ERROR, Level::WARN, Level::INFO, Level::TRACE]
        );
        for event in events.iter() {
            assert_eq!(event.target, "renderer::vulkan::validation");
            assert_eq!(event.fields["message_id_name"], "mapped-id");
            assert_eq!(event.fields["message_id_number"], "-42");
            assert_eq!(event.fields["message"], "mapped message");
        }
    }

    #[test]
    fn concurrent_callbacks_count_without_logging_and_bound_retention() {
        let diagnostics = disabled_diagnostics();
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let diagnostics = Arc::clone(&diagnostics);
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        deliver(
                            &diagnostics,
                            vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                            c"concurrent",
                            c"an error",
                        );
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.messages_seen, 200);
        assert_eq!(snapshot.errors, 200);
        assert_eq!(snapshot.callback_failures, 0);
        assert_eq!(snapshot.messages.len(), RECENT_MESSAGE_LIMIT);
        assert_eq!(snapshot.problems.len(), PROBLEM_MESSAGE_LIMIT);
        assert_eq!(snapshot.dropped_problems, 200 - PROBLEM_MESSAGE_LIMIT);
    }

    #[test]
    fn verbose_traffic_cannot_displace_retained_problems() {
        let diagnostics = disabled_diagnostics();
        deliver(
            &diagnostics,
            vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
            c"important",
            c"retain this warning",
        );
        for _ in 0..RECENT_MESSAGE_LIMIT + 1 {
            deliver(
                &diagnostics,
                vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
                c"noise",
                c"some information",
            );
        }
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.problems.len(), 1);
        assert_eq!(
            snapshot.problems[0].message_id_name.as_deref(),
            Some("important")
        );
        assert_eq!(snapshot.dropped_problems, 0);
    }

    struct PanicPayload;
    impl Drop for PanicPayload {
        fn drop(&mut self) {
            panic!("panic payload must not be dropped across FFI");
        }
    }
    struct PanickingSubscriber(Arc<AtomicUsize>);
    impl Subscriber for PanickingSubscriber {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, _: &Event<'_>) {
            self.0.fetch_add(1, Ordering::Relaxed);
            std::panic::panic_any(PanicPayload);
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    #[test]
    fn subscriber_panic_is_contained_and_further_dispatch_stops() {
        let calls = Arc::new(AtomicUsize::new(0));
        let dispatch = Dispatch::new(PanickingSubscriber(Arc::clone(&calls)));
        let diagnostics = tracing::dispatcher::with_default(&dispatch, Diagnostics::new);
        for _ in 0..2 {
            assert_eq!(
                deliver(
                    &diagnostics,
                    vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                    c"panic-test",
                    c"still counted"
                ),
                vk::FALSE
            );
        }
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.warnings, 2);
        assert_eq!(snapshot.callback_failures, 1);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn missing_data_and_poisoned_storage_fail_diagnostics_without_unwinding() {
        let diagnostics = disabled_diagnostics();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = diagnostics.retained.lock().unwrap();
            panic!("simulate interrupted collection");
        }));
        deliver(
            &diagnostics,
            vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
            c"after-poison",
            c"still recorded",
        );
        let info = diagnostics.messenger_create_info();
        // SAFETY: The Arc remains live. The private callback deliberately handles
        // null data defensively; no Vulkan function is invoked by this test.
        let result = unsafe {
            debug_callback(
                vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
                vk::DebugUtilsMessageTypeFlagsEXT::GENERAL,
                std::ptr::null(),
                info.p_user_data,
            )
        };
        assert_eq!(result, vk::FALSE);
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.errors, 1);
        assert_eq!(snapshot.callback_failures, 2);
        assert_eq!(snapshot.messages[0].message, "still recorded");
    }
}
