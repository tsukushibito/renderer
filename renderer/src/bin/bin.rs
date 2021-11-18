use winit::event::{ElementState, Event, KeyboardInput, VirtualKeyCode, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::platform::macos::WindowExtMacOS;
use winit::window::Window;

extern crate renderer;
use renderer::ash_renderer::ash_renderer::AshRenderer;

use std::mem;

#[cfg(target_os = "macos")]
use cocoa::appkit::{NSView, NSWindow};
#[cfg(target_os = "macos")]
use cocoa::base::id as cocoa_id;
#[cfg(target_os = "macos")]
use metal::MetalLayer;
#[cfg(target_os = "macos")]
use objc::runtime::YES;

// Constants
const WINDOW_TITLE: &'static str = "00.Base Code";
const WINDOW_WIDTH: u32 = 800;
const WINDOW_HEIGHT: u32 = 600;

pub struct VulkanApp {}

impl VulkanApp {
    pub fn init_window(event_loop: &EventLoop<()>) -> winit::window::Window {
        let window = winit::window::WindowBuilder::new()
            .with_title(WINDOW_TITLE)
            .with_inner_size(winit::dpi::LogicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT))
            .build(event_loop)
            .expect("Failed to create window.");

        if cfg!(target_os = "macos") {
            unsafe {
                let wnd: cocoa_id = mem::transmute(window.ns_window());

                let layer = MetalLayer::new();

                layer.set_edge_antialiasing_mask(0);
                layer.set_presents_with_transaction(false);
                layer.remove_all_animations();

                let view = wnd.contentView();

                layer.set_contents_scale(view.backingScaleFactor());
                view.setLayer(mem::transmute(layer.as_ref()));
                view.setWantsLayer(YES);
            }
        }

        window
    }

    pub fn main_loop<F>(event_loop: EventLoop<()>, window: Window, draw: F)
    where
        F: 'static + FnMut(),
    {
        let mut draw_func = Box::new(draw);
        event_loop.run(move |event, _, control_flow| match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                WindowEvent::KeyboardInput { input, .. } => match input {
                    KeyboardInput {
                        virtual_keycode,
                        state,
                        ..
                    } => match (virtual_keycode, state) {
                        (Some(VirtualKeyCode::Escape), ElementState::Pressed) => {
                            dbg!();
                            *control_flow = ControlFlow::Exit
                        }
                        _ => {}
                    },
                },
                _ => {}
            },
            Event::MainEventsCleared => {
                window.request_redraw();
            }
            Event::RedrawRequested(_window_id) => {
                draw_func();
            }
            _ => (),
        })
    }
}

fn main() {
    let event_loop = EventLoop::new();
    let window = VulkanApp::init_window(&event_loop);
    let window_size = window.inner_size();
    let mut renderer = AshRenderer::new(
        "",
        ash::vk::make_api_version(0, 1, 0, 0),
        window.ns_view(),
        window_size.width,
        window_size.height,
    );

    VulkanApp::main_loop(event_loop, window, move || {
        renderer.draw_frame();
    });
}
