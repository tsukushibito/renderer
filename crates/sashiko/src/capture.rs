//! Offscreen game captures for tools and acceptance tests.
use renderer::{Frame, Renderer, pipeline::PipelineResult};
use std::{io::Write, time::Duration};

/// Top-row-first, tightly packed RGBA8 pixels from a completed frame.
pub struct CaptureImage {
    extent: [u32; 2],
    rgba: Vec<u8>,
}
impl CaptureImage {
    pub fn render(
        renderer: &mut Renderer,
        timeout: Duration,
        draw: impl FnOnce(&mut Frame<'_>) -> PipelineResult<()>,
    ) -> PipelineResult<Self> {
        let mut frame = renderer.begin_frame()?;
        draw(&mut frame)?;
        let ticket = frame.capture()?;
        let extent = ticket.extent();
        let size = (extent[0] as usize)
            .checked_mul(extent[1] as usize)
            .and_then(|p| p.checked_mul(4))
            .ok_or("capture dimensions overflow")?;
        let mut rgba = vec![0; size];
        if !renderer.read_capture(&ticket, &mut rgba, timeout)? {
            return Err("offscreen frame timed out".into());
        }
        Ok(Self { extent, rgba })
    }

    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
    pub fn write_ppm(&self, mut out: impl Write) -> std::io::Result<()> {
        writeln!(out, "P6\n{} {}\n255", self.extent[0], self.extent[1])?;
        for pixel in self.rgba.as_chunks::<4>().0 {
            out.write_all(&pixel[..3])?;
        }
        out.flush()
    }
}
