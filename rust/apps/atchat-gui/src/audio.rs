//! cpal ile monitör sesini hoparlöre verir. 8 kHz -> cihaz örnekleme hızına
//! basit (en yakın komşu) yeniden örnekleme. Cihaz yoksa sessizce başarısız.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct AudioOut {
    _stream: cpal::Stream,
}

impl AudioOut {
    /// `ring`: motorun doldurduğu 8 kHz i16 örnek halkası.
    /// `volume`: 0..1 (paylaşımlı, canlı ayarlanır).
    pub fn start(ring: Arc<Mutex<VecDeque<i16>>>, volume: Arc<Mutex<f32>>) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("çıkış cihazı bulunamadı"))?;
        let cfg = device.default_output_config()?;
        let sample_rate = cfg.sample_rate().0 as f32;
        let channels = cfg.channels() as usize;
        let step = 8000.0 / sample_rate; // çıkış örneği başına kaç kaynak örneği

        let err_fn = |e| tracing::warn!("cpal akış hatası: {e}");

        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => {
                let cb = make_callback::<f32>(ring, volume, channels, step);
                device.build_output_stream(&cfg.into(), cb, err_fn, None)?
            }
            cpal::SampleFormat::I16 => {
                let cb = make_callback::<i16>(ring, volume, channels, step);
                device.build_output_stream(&cfg.into(), cb, err_fn, None)?
            }
            cpal::SampleFormat::U16 => {
                let cb = make_callback::<u16>(ring, volume, channels, step);
                device.build_output_stream(&cfg.into(), cb, err_fn, None)?
            }
            other => anyhow::bail!("desteklenmeyen örnek formatı: {other:?}"),
        };
        stream.play()?;
        Ok(Self { _stream: stream })
    }
}

fn make_callback<T>(
    ring: Arc<Mutex<VecDeque<i16>>>,
    volume: Arc<Mutex<f32>>,
    channels: usize,
    step: f32,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static
where
    T: cpal::Sample + cpal::FromSample<f32> + Send + 'static,
{
    let mut staging: VecDeque<i16> = VecDeque::new();
    let mut frac: f32 = 0.0;

    move |out: &mut [T], _| {
        let frames = out.len() / channels.max(1);
        let need = (frames as f32 * step).ceil() as usize + 2;
        {
            let mut r = ring.lock().unwrap();
            while staging.len() < need {
                match r.pop_front() {
                    Some(s) => staging.push_back(s),
                    None => break,
                }
            }
        }
        let vol = (*volume.lock().unwrap()).clamp(0.0, 1.0);

        for f in 0..frames {
            let sample = if staging.is_empty() {
                frac = 0.0;
                0.0
            } else {
                let v = *staging.front().unwrap() as f32 / 32768.0;
                frac += step;
                let consume = frac.floor() as usize;
                for _ in 0..consume {
                    if staging.pop_front().is_none() {
                        break;
                    }
                }
                frac -= consume as f32;
                v * vol
            };
            let t: T = T::from_sample(sample);
            for c in 0..channels {
                out[f * channels + c] = t;
            }
        }
    }
}
