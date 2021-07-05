use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

mod renderer;

fn main() {
    let event_loop = EventLoop::new();

    let window = match WindowBuilder::new()
        .with_title("renderer")
        .with_inner_size(winit::dpi::LogicalSize::new(1080.0, 720.0))
        .build(&event_loop)
    {
        Err(e) => {
            println!("Error! {}", e);
            return;
        }
        Ok(w) => w,
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                window_id,
            } if window_id == window.id() => *control_flow = ControlFlow::Exit,
            Event::MainEventsCleared => {
                window.request_redraw();
            }
            _ => (),
        }
    });
}
