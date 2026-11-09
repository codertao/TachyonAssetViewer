//! Sound output: a small software mixer feeding the default output device through `cpal`
//! (WASAPI on Windows, CoreAudio on macOS, ALSA on Linux). If there's no device,
//! `Audio::new` returns `None` and the viewer stays silent.

use crate::pwf::Sound;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::sync::{Arc, Mutex};

struct Voice {
    sound: Arc<Sound>,
    pos: f64,
    step: f64,
    /// Current and target (left, right) gains; loops glide to new targets to avoid clicks.
    gain: [f32; 2],
    target: [f32; 2],
    /// Looping voices are owned by a caller key and stopped when it's no longer set.
    key: Option<u64>,
}

struct Mixer {
    voices: Vec<Voice>,
    /// A streamed source (the music), added on top of the voices.
    music: Option<Box<dyn Stream>>,
    rate: u32,
}

/// A source that produces its own samples, such as the streamed music.
pub trait Stream: Send {
    /// Add interleaved stereo frames at `rate` Hz into `out`.
    fn fill(&mut self, out: &mut [f32], rate: u32);
}

impl Mixer {
    /// Mix into interleaved stereo `acc` (cleared first).
    fn mix(&mut self, acc: &mut [f32]) {
        acc.fill(0.0);
        let frames = acc.len() / 2;
        for v in &mut self.voices {
            let n = v.sound.samples.len();
            if n == 0 {
                continue;
            }
            for f in 0..frames {
                for c in 0..2 {
                    v.gain[c] += (v.target[c] - v.gain[c]) * 0.002;
                }
                let i = v.pos as usize;
                if i + 1 >= n {
                    if v.key.is_some() {
                        v.pos -= (n - 1) as f64;
                        continue;
                    }
                    v.pos = f64::MAX;
                    break;
                }
                let fr = (v.pos - i as f64) as f32;
                let s = v.sound.samples[i] * (1.0 - fr) + v.sound.samples[i + 1] * fr;
                acc[2 * f] += s * v.gain[0];
                acc[2 * f + 1] += s * v.gain[1];
                v.pos += v.step;
            }
        }
        self.voices.retain(|v| v.pos != f64::MAX);
        if let Some(m) = &mut self.music {
            m.fill(acc, self.rate);
        }
    }
}

pub struct Audio {
    mixer: Arc<Mutex<Mixer>>,
    /// The device's sample rate.
    rate: u32,
    _stream: cpal::Stream,
}

/// Equal-power gains for a volume and a pan (−1 left .. 1 right).
fn gains(volume: f32, pan: f32) -> [f32; 2] {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    [volume * a.cos(), volume * a.sin()]
}

/// An output stream of sample type `T`: the mixer's stereo goes to the first two channels
/// (a mono device gets the average), any others are silent.
fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mixer: Arc<Mutex<Mixer>>,
) -> Option<cpal::Stream> {
    let ch = config.channels as usize;
    let mut acc = vec![];
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _| {
                let frames = out.len() / ch;
                acc.resize(frames * 2, 0.0);
                match mixer.lock() {
                    Ok(mut m) => m.mix(&mut acc),
                    Err(_) => acc.fill(0.0),
                }
                for (f, o) in out.chunks_mut(ch).enumerate() {
                    let (l, r) = (acc[2 * f].clamp(-1.0, 1.0), acc[2 * f + 1].clamp(-1.0, 1.0));
                    for (c, x) in o.iter_mut().enumerate() {
                        *x = T::from_sample(match (ch, c) {
                            (1, _) => (l + r) * 0.5,
                            (_, 0) => l,
                            (_, 1) => r,
                            _ => 0.0,
                        });
                    }
                }
            },
            |e| eprintln!("audio: {e}"),
            None,
        )
        .ok()
}

impl Audio {
    pub fn new() -> Option<Audio> {
        let device = cpal::default_host().default_output_device()?;
        let supported = device.default_output_config().ok()?;
        let format = supported.sample_format();
        let mut config: cpal::StreamConfig = supported.into();
        // A short buffer keeps sounds in step with the picture; fall back to the default.
        config.buffer_size = cpal::BufferSize::Fixed(config.sample_rate.0 / 50);
        let mixer = Arc::new(Mutex::new(Mixer { voices: vec![], music: None, rate: config.sample_rate.0 }));
        let make = |config: &cpal::StreamConfig| {
            let m = mixer.clone();
            use cpal::SampleFormat as F;
            match format {
                F::F32 => build::<f32>(&device, config, m),
                F::I16 => build::<i16>(&device, config, m),
                F::U16 => build::<u16>(&device, config, m),
                F::I32 => build::<i32>(&device, config, m),
                F::F64 => build::<f64>(&device, config, m),
                F::U8 => build::<u8>(&device, config, m),
                _ => None,
            }
        };
        let stream = make(&config).or_else(|| {
            config.buffer_size = cpal::BufferSize::Default;
            make(&config)
        })?;
        stream.play().ok()?;
        Some(Audio { mixer, rate: config.sample_rate.0, _stream: stream })
    }

    fn step(&self, sound: &Sound, pitch: f32) -> f64 {
        sound.rate as f64 / self.rate as f64 * pitch.max(0.01) as f64
    }

    /// Play a one-shot sound.
    pub fn play(&self, sound: Arc<Sound>, volume: f32, pan: f32, pitch: f32) {
        let g = gains(volume * sound.volume, pan);
        let step = self.step(&sound, pitch);
        if let Ok(mut m) = self.mixer.lock() {
            m.voices.push(Voice { sound, pos: 0.0, step, gain: g, target: g, key: None });
        }
    }

    /// Start or update the looping sounds; loops whose key isn't listed stop.
    pub fn set_loops(&self, loops: &[(u64, Arc<Sound>, f32, f32, f32)]) {
        let steps: Vec<f64> = loops.iter().map(|l| self.step(&l.1, l.4)).collect();
        let Ok(mut m) = self.mixer.lock() else { return };
        m.voices.retain(|v| v.key.is_none_or(|k| loops.iter().any(|l| l.0 == k)));
        for ((key, sound, volume, pan, _), step) in loops.iter().zip(steps) {
            let g = gains(volume * sound.volume, *pan);
            match m.voices.iter_mut().find(|v| v.key == Some(*key)) {
                Some(v) => v.target = g,
                None => m.voices.push(Voice {
                    sound: sound.clone(),
                    pos: 0.0,
                    step,
                    gain: [0.0; 2],
                    target: g,
                    key: Some(*key),
                }),
            }
        }
    }

    /// Start, replace or (with `None`) stop the streamed music.
    pub fn set_music(&self, music: Option<Box<dyn Stream>>) {
        if let Ok(mut m) = self.mixer.lock() {
            m.music = music;
        }
    }

    /// Stop one-shots (when the scene changes or time jumps).
    pub fn stop_one_shots(&self) {
        if let Ok(mut m) = self.mixer.lock() {
            m.voices.retain(|v| v.key.is_some());
        }
    }
}
