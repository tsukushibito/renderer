//! Bounded PCM mixer. Positions use listener-relative metres with positive X to the right.
use std::{num::NonZeroU32, sync::Arc};

#[cfg(all(windows, feature = "audio-output"))]
mod output;
#[cfg(all(windows, feature = "audio-output"))]
pub use output::Output;

#[derive(Clone)]
pub struct Clip {
    samples: Arc<[f32]>,
    rate: NonZeroU32,
}

impl Clip {
    /// Decode a mono signed 16-bit PCM WAVE asset at its native sample rate.
    pub fn from_wav(reader: impl std::io::Read) -> Result<Self, Box<dyn std::error::Error>> {
        let mut wave = hound::WavReader::new(reader)?;
        let spec = wave.spec();
        if spec.channels != 1
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
        {
            return Err("expected mono 16-bit PCM WAV".into());
        }
        let rate = NonZeroU32::new(spec.sample_rate).ok_or("invalid WAV sample rate")?;
        let samples = wave
            .samples::<i16>()
            .map(|s| s.map(|v| f32::from(v) / 32768.))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(samples, rate)?)
    }

    /// Shared mono linear PCM. Validation takes place at the load boundary.
    pub fn new(samples: Vec<f32>, rate: NonZeroU32) -> Result<Self, &'static str> {
        if samples.is_empty() || samples.iter().any(|v| !(-1.0..=1.0).contains(v)) {
            return Err("expected a nonempty finite mono PCM clip in [-1, 1]");
        }
        Ok(Self {
            samples: samples.into(),
            rate,
        })
    }
}

#[derive(Clone, Copy)]
pub enum Bus {
    Effects,
    Music,
}

struct Voice {
    owner: u64,
    clip: Clip,
    cursor: f64,
    bus: Bus,
    gains: [f32; 2],
    looping: bool,
}

pub struct Mixer {
    voices: Vec<Voice>,
    limit: usize,
    volume: [f32; 2],
    master: f32,
}

impl Mixer {
    pub fn new(limit: usize) -> Self {
        Self {
            voices: Vec::with_capacity(limit),
            limit,
            volume: [1.; 2],
            master: 1.,
        }
    }

    pub fn set_master(&mut self, volume: f32) {
        self.master = if volume.is_finite() {
            volume.clamp(0., 1.)
        } else {
            0.
        };
    }

    pub fn set_bus(&mut self, bus: Bus, volume: f32) {
        self.volume[bus as usize] = if volume.is_finite() {
            volume.clamp(0., 1.)
        } else {
            0.
        };
    }

    /// Play a finite or looping clip. Returns false on voice budget exhaustion.
    pub fn play(
        &mut self,
        owner: u64,
        clip: &Clip,
        bus: Bus,
        position: [f32; 3],
        looping: bool,
    ) -> bool {
        if self.voices.len() == self.limit || position.iter().any(|x| !x.is_finite()) {
            return false;
        }
        let [x, y, z] = position;
        let distance = x.hypot(y.hypot(z));
        let pan = x / distance.max(1.);
        let attenuation = 1. / (1. + 0.25 * distance);
        self.voices.push(Voice {
            owner,
            clip: clip.clone(),
            cursor: 0.,
            bus,
            gains: [
                (0.5 * (1. - pan)).sqrt() * attenuation,
                (0.5 * (1. + pan)).sqrt() * attenuation,
            ],
            looping,
        });
        true
    }

    pub fn stop(&mut self, owner: u64) {
        self.voices.retain(|v| v.owner != owner);
    }

    /// Mix one stereo sample frame. The device or offline renderer supplies the clock.
    pub fn next_frame(&mut self, output_rate: NonZeroU32) -> [f32; 2] {
        let mut sum = [0.; 2];
        self.voices.retain_mut(|v| {
            let n = v.clip.samples.len();
            let i = v.cursor as usize;
            let j = if v.looping {
                (i + 1) % n
            } else {
                (i + 1).min(n - 1)
            };
            let t = v.cursor.fract() as f32;
            let sample = v.clip.samples[i] * (1. - t) + v.clip.samples[j] * t;
            for (dst, gain) in sum.iter_mut().zip(v.gains) {
                *dst += sample * gain * self.volume[v.bus as usize];
            }
            v.cursor += f64::from(v.clip.rate.get()) / f64::from(output_rate.get());
            if v.looping {
                v.cursor %= n as f64;
            }
            v.cursor < n as f64
        });
        sum.map(|v| (v * self.master).clamp(-1., 1.))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_routing_expiry_volume_and_owner_cancel() {
        let rate = NonZeroU32::new(48_000).unwrap();
        let clip = Clip::new(vec![1.; 2], rate).unwrap();
        let mut mixer = Mixer::new(1);
        assert!(mixer.play(42, &clip, Bus::Effects, [2., 0., 0.], false));
        assert!(!mixer.play(19, &clip, Bus::Music, [0.; 3], true));
        let [left, right] = mixer.next_frame(rate);
        assert_eq!(left, 0.);
        assert!(right > 0.);
        mixer.set_master(0.5);
        assert_eq!(mixer.next_frame(rate)[1], right * 0.5);
        assert_eq!(mixer.next_frame(rate), [0.; 2]);
        assert!(mixer.play(42, &clip, Bus::Music, [0.; 3], true));
        mixer.set_bus(Bus::Music, 0.);
        assert_eq!(mixer.next_frame(rate), [0.; 2]);
        mixer.stop(42);
        assert!(mixer.play(1, &clip, Bus::Effects, [0.; 3], false));
    }
}
