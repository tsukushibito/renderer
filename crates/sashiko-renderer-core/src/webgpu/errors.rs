//! Each consumer subscribes to the host device without replacing other listeners.
use std::sync::Arc;
use web_sys::js_sys::Reflect;
use web_sys::wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{Event, EventTarget};

type Observer = dyn Fn(bool, String) + Send + Sync;

pub(super) struct Subscription {
    target: EventTarget,
    callback: Closure<dyn FnMut(Event)>,
    // The browser resolves device.lost once. Dropping this owner disconnects
    // the observer even if a host keeps the device alive after detaching.
    _observer: Arc<Observer>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.target.remove_event_listener_with_callback(
            "uncapturederror",
            self.callback.as_ref().unchecked_ref(),
        );
    }
}

pub(super) fn observe(
    device: &wgpu::Device,
    observer: Arc<Observer>,
) -> Result<Subscription, String> {
    let target: &EventTarget = device
        .as_webgpu()
        .ok_or("expected a browser WebGPU device")?
        .as_ref();
    let errors = Arc::downgrade(&observer);
    let callback = Closure::new(move |event: Event| {
        if let Some(observer) = errors.upgrade() {
            let message = Reflect::get(&event, &JsValue::from_str("error"))
                .and_then(|error| Reflect::get(&error, &JsValue::from_str("message")))
                .ok()
                .and_then(|message| message.as_string())
                .unwrap_or_else(|| "uncaptured WebGPU error".into());
            observer(false, message);
        }
    });
    target
        .add_event_listener_with_callback("uncapturederror", callback.as_ref().unchecked_ref())
        .map_err(|error| format!("could not observe WebGPU errors: {error:?}"))?;
    let lost = Arc::downgrade(&observer);
    device.set_device_lost_callback(move |reason, message| {
        if let Some(observer) = lost.upgrade() {
            observer(true, format!("WebGPU device lost: {reason:?}: {message}"));
        }
    });
    Ok(Subscription {
        target: target.clone(),
        callback,
        _observer: observer,
    })
}
