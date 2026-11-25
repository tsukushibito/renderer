//! UI-thread ABI shared by the .NET and browser samples. Handles expire at close.
use crate::{Game, Scenario, TICK_HZ, controls};
use sashiko::{
    App,
    input::{Button, DeviceInput},
    math::Vec2,
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::CString,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

#[cfg(target_arch = "wasm32")]
mod canvas;
#[cfg(windows)]
mod desktop;

struct Session {
    app: App<Game>,
    controls: DeviceInput<controls::Action>,
    #[cfg(windows)]
    view: Option<desktop::View>,
    #[cfg(target_arch = "wasm32")]
    scene: canvas::CanvasScene,
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
thread_local! {
    static VIEWS: RefCell<BTreeMap<u32, Session>> = const { RefCell::new(BTreeMap::new()) };
    static ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn report(error: Box<dyn std::error::Error>) -> i32 {
    ERROR.with_borrow_mut(|msg| *msg = CString::new(error.to_string().replace('\0', " ")).unwrap());
    -1
}

fn access(id: u32, operation: impl FnOnce(&mut Session) -> Result<i32>) -> i32 {
    let result = VIEWS.with_borrow_mut(|sessions| {
        operation(
            sessions
                .get_mut(&id)
                .ok_or("closed view or wrong UI thread")?,
        )
    });
    result.unwrap_or_else(report)
}

#[unsafe(no_mangle)]
extern "C" fn survivor_error() -> *const std::ffi::c_char {
    ERROR.with_borrow(|e| e.as_ptr())
}

#[unsafe(no_mangle)]
extern "C" fn survivor_open(seed: u32) -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let Ok(id) = NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1)) else {
        report("session handles exhausted".into());
        return 0;
    };
    let game = Game::new(Default::default(), Scenario::Early, u64::from(seed));
    #[cfg(target_arch = "wasm32")]
    let scene = canvas::CanvasScene::new(game.arena.obstacles.len());
    let session = Session {
        app: App::new(game, TICK_HZ.try_into().unwrap()),
        controls: controls::controls(),
        #[cfg(windows)]
        view: None,
        #[cfg(target_arch = "wasm32")]
        scene,
    };
    VIEWS.with_borrow_mut(|sessions| sessions.insert(id, session));
    id
}

#[unsafe(no_mangle)]
extern "C" fn survivor_close(id: u32) -> i32 {
    let Some(session) = VIEWS.with_borrow_mut(|s| s.remove(&id)) else {
        return report("closed view or wrong UI thread".into());
    };
    #[cfg(windows)]
    if let Some(view) = session.view {
        return view.close().map(|()| 0).unwrap_or_else(report);
    }
    #[cfg(target_arch = "wasm32")]
    drop(session);
    0
}

#[unsafe(no_mangle)]
extern "C" fn survivor_focus(id: u32, focused: i32) -> i32 {
    access(id, |s| {
        s.controls.focus(focused != 0);
        Ok(0)
    })
}

#[unsafe(no_mangle)]
extern "C" fn survivor_button(id: u32, mouse: i32, code: u16, pressed: i32) -> i32 {
    access(id, |s| {
        s.controls.button(
            if mouse == 0 {
                Button::Key(code)
            } else {
                Button::Mouse(code)
            },
            pressed != 0,
        );
        Ok(0)
    })
}

fn advance(s: &mut Session, ticks: u128) {
    s.app.advance(
        ticks,
        |_, _| controls::sample(&mut s.controls),
        |game, input| {
            game.step(input);
        },
    );
}

#[unsafe(no_mangle)]
extern "C" fn survivor_advance(id: u32, micros: u32) -> i32 {
    access(id, |s| {
        let ticks = s.app.elapsed_ticks(Duration::from_micros(micros.into()));
        advance(s, ticks);
        Ok(0)
    })
}

#[unsafe(no_mangle)]
extern "C" fn survivor_step(id: u32) -> i32 {
    access(id, |s| {
        advance(s, 1);
        Ok(0)
    })
}

/// Returns 1 for a blocked edit, 0 for success.
#[unsafe(no_mangle)]
extern "C" fn survivor_place(id: u32, x: f32, z: f32) -> i32 {
    access(id, |s| {
        let p = Vec2::new(x, z);
        if !x.is_finite()
            || !z.is_finite()
            || !s.app.game.arena.floor().open(p, crate::PLAYER_RADIUS)
        {
            return Ok(1);
        }
        s.app.game.player = p;
        Ok(0)
    })
}

#[unsafe(no_mangle)]
extern "C" fn survivor_tick(id: u32) -> f64 {
    VIEWS.with_borrow(|views| views.get(&id).map_or(f64::NAN, |s| s.app.game.tick as f64))
}
#[unsafe(no_mangle)]
extern "C" fn survivor_position(id: u32, axis: u32) -> f32 {
    VIEWS.with_borrow(|views| {
        views
            .get(&id)
            .and_then(|s| s.app.game.player.to_array().get(axis as usize).copied())
            .unwrap_or(f32::NAN)
    })
}
