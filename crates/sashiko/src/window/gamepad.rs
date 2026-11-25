//! A host chooses which XInput slot drives its game view.
use crate::{input::PadState, math::Vec2};
use windows_sys::Win32::UI::Input::XboxController::*;

/// Returns None for a disconnected or unavailable controller.
pub fn poll_gamepad(slot: u32) -> Option<PadState> {
    let mut state = XINPUT_STATE::default();
    // SAFETY: XInput writes one initialized state to this live local variable.
    if unsafe { XInputGetState(slot, &mut state) } != 0 {
        return None;
    }
    let pad = state.Gamepad;
    let mut buttons = 0;
    for (index, mask) in [
        XINPUT_GAMEPAD_A,
        XINPUT_GAMEPAD_B,
        XINPUT_GAMEPAD_X,
        XINPUT_GAMEPAD_Y,
        XINPUT_GAMEPAD_LEFT_SHOULDER,
        XINPUT_GAMEPAD_RIGHT_SHOULDER,
        0,
        0,
        XINPUT_GAMEPAD_BACK,
        XINPUT_GAMEPAD_START,
        XINPUT_GAMEPAD_LEFT_THUMB,
        XINPUT_GAMEPAD_RIGHT_THUMB,
        XINPUT_GAMEPAD_DPAD_UP,
        XINPUT_GAMEPAD_DPAD_DOWN,
        XINPUT_GAMEPAD_DPAD_LEFT,
        XINPUT_GAMEPAD_DPAD_RIGHT,
    ]
    .into_iter()
    .enumerate()
    {
        if pad.wButtons & mask != 0 {
            buttons |= 1 << index;
        }
    }
    let triggers = [pad.bLeftTrigger, pad.bRightTrigger].map(|v| f32::from(v) / 255.);
    for (index, value) in triggers.into_iter().enumerate() {
        if value > 0.5 {
            buttons |= 1 << (6 + index);
        }
    }
    let axis = |x: i16| (f32::from(x) / 32767.).max(-1.);
    Some(PadState {
        buttons,
        triggers,
        left: Vec2::new(axis(pad.sThumbLX), -axis(pad.sThumbLY)),
        right: Vec2::new(axis(pad.sThumbRX), -axis(pad.sThumbRY)),
    })
}
