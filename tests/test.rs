use std::rc::Rc;

use raw_window_handle::HasRawDisplayHandle;
use renderer::{
    vulkan::{VkDevice, VkRenderBackend},
    PhysicalDevice, RenderBackend,
};
use winit::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::EventLoop,
    window::WindowBuilder,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("renderer example.");
    let event_loop = EventLoop::new().unwrap();
    let window = WindowBuilder::new()
        .with_title("renderer example")
        .with_inner_size(LogicalSize::new(1080.0, 720.0))
        .build(&event_loop)
        .unwrap();

    let display_handle = window.raw_display_handle();
    let render_backend = VkRenderBackend::new(&display_handle)?;
    let physical_devices = render_backend.physical_devices();
    let physical_device = if physical_devices.len() == 1 {
        physical_devices.first().unwrap()
    } else {
        physical_devices.first().unwrap()
    };
    println!("vram size: {}", physical_device.vram_size());
    let device = Rc::new(VkDevice::new(&render_backend, physical_device));

    // event_loop.run(move |event, control_flow| match event {
    //     Event::WindowEvent { window_id, event } if window_id == window_id => match event {
    //         WindowEvent::CloseRequested => control_flow.exit(),
    //         WindowEvent::Resized(size) => {
    //             println!("window resized. size: {:?}", size);
    //         }
    //         WindowEvent::RedrawRequested => {}
    //         _ => (),
    //     },
    //     Event::AboutToWait => window.request_redraw(),
    //     _ => (),
    // })?;
    println!("exit.");

    Ok(())
}
