//! Small adapter around the public two-API presentation contract.

use super::super::{Error, Options, backend::Backend, pipeline::Budget};
use renderer::{
    PresentationMetrics,
    graph::ImportImage,
    presentation::{PresentOutcome, PresentationConfig},
};
use std::{sync::Arc, time::Duration};
use winit::window::Window;

pub(in crate::bistro) trait Surface {
    fn prepare(&mut self) -> Result<Option<PresentOutcome>, Error>;
    fn extent(&self) -> Option<(u32, u32)>;
    fn resize(&mut self, extent: (u32, u32));
    fn idle(&mut self) -> Result<bool, Error>;
    fn close(&mut self, timeout: Duration) -> Result<bool, Error>;
    fn profile(&mut self, enabled: bool) -> Result<(), Error>;
    fn pop_profile(&mut self) -> Option<PresentationMetrics>;
}

macro_rules! surface {
    ($ty:ty) => {
        impl Surface for $ty {
            fn prepare(&mut self) -> Result<Option<PresentOutcome>, Error> {
                Ok(self.prepare()?)
            }
            fn extent(&self) -> Option<(u32, u32)> {
                self.extent()
            }
            fn resize(&mut self, (width, height): (u32, u32)) {
                self.resize(width, height);
            }
            fn idle(&mut self) -> Result<bool, Error> {
                Ok(self.wait_idle(Duration::ZERO)?)
            }
            fn close(&mut self, timeout: Duration) -> Result<bool, Error> {
                Ok(self.close(timeout)?)
            }
            fn profile(&mut self, enabled: bool) -> Result<(), Error> {
                Ok(self.set_profiling(enabled)?)
            }
            fn pop_profile(&mut self) -> Option<PresentationMetrics> {
                self.take_profile()
            }
        }
    };
}

#[cfg(feature = "vulkan")]
surface!(renderer::Presenter);
#[cfg(feature = "d3d12")]
surface!(renderer::D3D12Presenter);

pub(in crate::bistro) trait WindowBackend: Backend {
    type Surface: Surface;

    fn open(
        options: &Options,
        budget: &Budget,
        window: Arc<Window>,
        config: PresentationConfig,
    ) -> Result<(Self, Self::Surface), Error>;
    fn present(
        &mut self,
        surface: &mut Self::Surface,
        image: &ImportImage,
    ) -> Result<PresentOutcome, Error>;
    fn reprepare(&mut self, options: &Options, budget: &Budget) -> Result<(), Error>;
}

#[cfg(feature = "vulkan")]
impl WindowBackend for super::super::backend::Vulkan {
    type Surface = renderer::Presenter;

    fn open(
        options: &Options,
        budget: &Budget,
        window: Arc<Window>,
        config: PresentationConfig,
    ) -> Result<(Self, Self::Surface), Error> {
        let (runtime, surface) =
            renderer::VulkanRuntime::for_window(Self::config(options, budget), window, config)?;
        Ok((Self::prepare(runtime, options, budget)?, surface))
    }

    fn present(
        &mut self,
        surface: &mut Self::Surface,
        image: &ImportImage,
    ) -> Result<PresentOutcome, Error> {
        Ok(self.runtime.present(surface, image)?)
    }

    fn reprepare(&mut self, options: &Options, budget: &Budget) -> Result<(), Error> {
        let classes = Budget::frame_images(options)
            .into_iter()
            .map(|(desc, count)| renderer::ResourceClass::Image { desc, count })
            .collect::<Vec<_>>();
        self.runtime
            .prepare_working_set(Self::frame_capacity(options, budget), &classes)?;
        Ok(())
    }
}

#[cfg(feature = "d3d12")]
impl WindowBackend for super::super::backend::D3d12 {
    type Surface = renderer::D3D12Presenter;

    fn open(
        options: &Options,
        budget: &Budget,
        window: Arc<Window>,
        config: PresentationConfig,
    ) -> Result<(Self, Self::Surface), Error> {
        let backend = Self::create(options, budget)?;
        let surface = backend.runtime.create_presenter(window, config)?;
        Ok((backend, surface))
    }

    fn present(
        &mut self,
        surface: &mut Self::Surface,
        image: &ImportImage,
    ) -> Result<PresentOutcome, Error> {
        Ok(self.runtime.present(surface, image)?)
    }

    fn reprepare(&mut self, options: &Options, _: &Budget) -> Result<(), Error> {
        let images = Budget::frame_images(options)
            .into_iter()
            .map(|(desc, count)| renderer::D3D12ImageClass { desc, count })
            .collect::<Vec<_>>();
        self.runtime.prepare_graph_working_set(
            Some(&[]),
            &images,
            &super::super::program::raster_requests(options),
            options.bloom.programs(),
        )?;
        Ok(())
    }
}
