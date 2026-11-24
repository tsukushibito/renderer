//! Native device adapter. The audio callback never waits on a game-thread edit.
use super::Mixer;
use cpal::{
    FromSample, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::{
    error::Error,
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

pub struct Output {
    mixer: Arc<Mutex<Mixer>>,
    _stream: cpal::Stream,
}

impl Output {
    pub fn open(capacity: usize) -> Result<Self, Box<dyn Error>> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no default audio device")?;
        let format = device.default_output_config()?;
        let config = format.config();
        let mixer = Arc::new(Mutex::new(Mixer::new(capacity)));
        let stream = match format.sample_format() {
            cpal::SampleFormat::F32 => stream::<f32>(&device, &config, mixer.clone())?,
            cpal::SampleFormat::I16 => stream::<i16>(&device, &config, mixer.clone())?,
            cpal::SampleFormat::U16 => stream::<u16>(&device, &config, mixer.clone())?,
            other => return Err(format!("unsupported default sample format {other}").into()),
        };
        stream.play()?;
        Ok(Self {
            mixer,
            _stream: stream,
        })
    }

    pub fn edit<R>(&self, f: impl FnOnce(&mut Mixer) -> R) -> R {
        f(&mut self.mixer.lock().expect("audio mixer poisoned"))
    }
}

fn stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mixer: Arc<Mutex<Mixer>>,
) -> Result<cpal::Stream, Box<dyn Error>> {
    let rate = NonZeroU32::new(config.sample_rate.0).ok_or("zero audio device sample rate")?;
    let channels = usize::from(config.channels);
    if channels == 0 {
        return Err("device has no output channels".into());
    }
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let Ok(mut mixer) = mixer.try_lock() else {
                data.fill(T::from_sample(0.));
                return;
            };
            for output in data.chunks_exact_mut(channels) {
                let [l, r] = mixer.next_frame(rate);
                if channels == 1 {
                    output[0] = T::from_sample((l + r) * 0.5);
                } else {
                    output[0] = T::from_sample(l);
                    output[1] = T::from_sample(r);
                    output[2..].fill(T::from_sample(0.));
                }
            }
        },
        |err| eprintln!("audio device: {err}"),
        None,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{Bus, Clip};
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "requires an active Windows audio endpoint"]
    fn device_consumes_and_releases_a_finite_silent_clip() {
        let output = Output::open(1).expect("default audio endpoint");
        let clip = Clip::new(vec![0.; 480], NonZeroU32::new(48_000).unwrap()).unwrap();
        assert!(output.edit(|m| m.play(1, &clip, Bus::Effects, [0.; 3], false)));
        let start = Instant::now();
        while !output.edit(|m| m.play(2, &clip, Bus::Effects, [0.; 3], false)) {
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "audio endpoint did not consume the clip"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
