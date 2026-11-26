use super::{Result, access};
use crate::{Game, visual::Visual};
use sashiko::window::HostChild;
use sashiko_renderer::{
    WindowRenderer,
    core::{PresentOutcome, runtime::RuntimeConfig},
};
use std::num::NonZeroIsize;

pub(super) struct View {
    renderer: WindowRenderer,
    visual: Visual,
    size: [u32; 2],
    recreate: bool,
}
impl View {
    fn new(
        child: HostChild,
        size: [u32; 2],
        game: &Game,
        catalog: &std::path::Path,
        parallelism: std::num::NonZeroUsize,
    ) -> Result<Self> {
        if size.contains(&0) {
            return Err("attach requires a non-empty client area".into());
        }
        let jobs = sashiko::jobs::JobSystem::new(parallelism)?.into();
        let mut visual = Visual::new(game, catalog, "primary", Default::default(), jobs)?;
        let camera = visual.camera(game, size)?;
        #[cfg(feature = "d3d12")]
        let backend = RuntimeConfig::D3D12(Default::default());
        #[cfg(not(feature = "d3d12"))]
        let backend = RuntimeConfig::Vulkan(Default::default());
        let mut renderer = WindowRenderer::new(child, backend, camera, visual.assets())?;
        visual.prepare_window(&mut renderer)?;
        Ok(Self {
            renderer,
            visual,
            size,
            recreate: false,
        })
    }

    fn draw(&mut self, game: &Game, size: [u32; 2]) -> Result<i32> {
        if self.recreate || self.size != size {
            self.renderer.resize(size, self.visual.assets())?;
            self.size = size;
            self.recreate = false;
        }
        if size.contains(&0) {
            return Ok(1);
        }
        let camera = self.visual.camera(game, size)?;
        self.renderer.set_camera(camera.camera);
        self.visual.sync(game, &camera)?;
        let outcome = self.renderer.draw(|f| self.visual.draw(f, game))?;
        self.recreate = matches!(outcome, PresentOutcome::RecreateRequired);
        Ok(i32::from(!matches!(
            outcome,
            PresentOutcome::Presented { .. }
        )))
    }
    pub(super) fn close(self) -> Result<()> {
        let mut runtime = self.renderer.into_runtime()?;
        self.visual.release(&mut runtime)
    }
}

/// # Safety
/// A live Win32 child owned by the calling UI thread must outlive survivor_close.
/// catalog points to a NUL-terminated UTF-8 path valid for this call.
/// All operations including close must run on that thread before DestroyWindow.
#[unsafe(no_mangle)]
unsafe extern "C" fn survivor_attach(
    id: u32,
    hwnd: isize,
    width: u32,
    height: u32,
    catalog: *const std::ffi::c_char,
) -> i32 {
    access(id, |s| {
        if s.view.is_some() {
            return Err("view is already attached".into());
        }
        if catalog.is_null() {
            return Err("null catalog path".into());
        }
        // SAFETY: the host supplies a live UTF-8 C string for this attach call.
        let path = unsafe { std::ffi::CStr::from_ptr(catalog) }.to_str()?;
        let hwnd = NonZeroIsize::new(hwnd).ok_or("null HWND")?;
        // SAFETY: the caller retains its child and closes this session before destroying it.
        let child = unsafe { HostChild::from_hwnd(hwnd) };
        let catalog_path = std::path::Path::new(path);
        let catalog = sashiko::animation::CharacterCatalog::open(catalog_path)?;
        let locomotion = crate::Locomotion::from_catalog(&catalog, "primary")?;
        s.view = Some(View::new(
            child,
            [width, height],
            &s.app.game,
            catalog_path,
            s.parallelism,
        )?);
        s.app.game.locomotion.replace_tracks(locomotion);
        Ok(0)
    })
}

#[unsafe(no_mangle)]
extern "C" fn survivor_draw(id: u32, width: u32, height: u32) -> i32 {
    access(id, |s| {
        s.view
            .as_mut()
            .ok_or("view is detached")?
            .draw(&s.app.game, [width, height])
    })
}
#[unsafe(no_mangle)]
extern "C" fn survivor_poll_pad(id: u32, slot: u32) -> i32 {
    access(id, |s| {
        if s.controls.focused() {
            s.controls.gamepad(sashiko::window::poll_gamepad(slot));
        }
        Ok(0)
    })
}
