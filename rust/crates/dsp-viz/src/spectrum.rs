//! A frequency-domain analyser: Hann window + a Welch-style EWMA average +
//! peak-hold. Returns magnitude in dB.

use std::collections::VecDeque;
use std::sync::Arc;

use num_complex::Complex64;
use rustfft::{Fft, FftPlanner};

/// The dB floor — in silence the bottom of the spectrum sits here.
pub const DB_FLOOR: f32 = -120.0;

pub struct SpectrumAnalyzer {
    fft: Arc<dyn Fft<f64>>,
    size: usize,
    window: Vec<f64>,
    win_power: f64, // Σ w² for window normalisation
    ring: VecDeque<f32>,
    /// EWMA linear power (length size/2).
    avg: Vec<f32>,
    /// dB peak-hold (length size/2).
    peak: Vec<f32>,
    alpha: f32,
    primed: bool,
}

impl SpectrumAnalyzer {
    /// `size`: the FFT size (512 / 1024 / 2048). Need not be a power of two,
    /// but it is recommended.
    pub fn new(size: usize) -> Self {
        let size = size.max(16);
        let mut planner = FftPlanner::<f64>::new();
        let window: Vec<f64> = (0..size)
            .map(|n| {
                let x = std::f64::consts::PI * n as f64 / (size as f64 - 1.0);
                let s = x.sin();
                s * s // Hann = sin²
            })
            .collect();
        let win_power: f64 = window.iter().map(|w| w * w).sum();
        Self {
            fft: planner.plan_fft_forward(size),
            size,
            window,
            win_power,
            ring: VecDeque::with_capacity(size),
            avg: vec![0.0; size / 2],
            peak: vec![DB_FLOOR; size / 2],
            alpha: 0.5,
            primed: false,
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }
    pub fn n_bins(&self) -> usize {
        self.size / 2
    }

    /// The EWMA smoothing factor: 0 = instantaneous (no smoothing); the
    /// closer to 1, the heavier the averaging.
    pub fn set_averaging(&mut self, alpha: f32) {
        self.alpha = alpha.clamp(0.0, 0.99);
    }

    pub fn reset_peak(&mut self) {
        self.peak.iter_mut().for_each(|p| *p = DB_FLOOR);
    }

    pub fn bin_hz(&self, bin: usize, sample_rate: u32) -> f32 {
        bin as f32 * sample_rate as f32 / self.size as f32
    }

    /// Feed new samples. Once `size` samples have accumulated in the ring the
    /// spectrum is recomputed (a sliding window).
    pub fn feed_i16(&mut self, samples: &[i16]) {
        for &s in samples {
            if self.ring.len() == self.size {
                self.ring.pop_front();
            }
            self.ring.push_back(s as f32 / 32768.0);
        }
        if self.ring.len() == self.size {
            self.recompute();
        }
    }

    fn recompute(&mut self) {
        let mut buf: Vec<Complex64> = self
            .ring
            .iter()
            .zip(&self.window)
            .map(|(&s, &w)| Complex64::new(s as f64 * w, 0.0))
            .collect();
        self.fft.process(&mut buf);

        let norm = 1.0 / (self.win_power + 1e-12);
        let a = self.alpha;
        let primed = self.primed;
        let nb = self.n_bins();
        for ((slot, pk), b) in self
            .avg
            .iter_mut()
            .zip(self.peak.iter_mut())
            .zip(buf.iter().take(nb))
        {
            // One-sided power (no ×2 for DC/Nyquist — irrelevant for a relative display).
            let p = (b.norm_sqr() * norm) as f32;
            *slot = if primed { a * *slot + (1.0 - a) * p } else { p };
            let db = 10.0 * (*slot).max(1e-12).log10();
            if db > *pk {
                *pk = db;
            }
        }
        self.primed = true;
    }

    /// The current spectrum, dB (length = size/2). Full of the floor when there is no data yet.
    pub fn magnitudes_db(&self) -> Vec<f32> {
        self.avg
            .iter()
            .map(|&p| 10.0 * p.max(1e-12).log10())
            .collect()
    }

    /// The peak-hold trace, dB.
    pub fn peak_db(&self) -> &[f32] {
        &self.peak
    }
}

/// Map a dB value to 0..1 over `[floor, ceil]` (for the waterfall / drawing).
pub fn db_to_unit(db: f32, floor_db: f32, ceil_db: f32) -> f32 {
    if ceil_db <= floor_db {
        return 0.0;
    }
    ((db - floor_db) / (ceil_db - floor_db)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, sr: u32, n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| {
                let t = i as f32 / sr as f32;
                (20000.0 * (2.0 * std::f32::consts::PI * freq * t).sin()) as i16
            })
            .collect()
    }

    #[test]
    fn peak_bin_matches_tone() {
        let sr = 8000;
        let size = 1024;
        let mut sa = SpectrumAnalyzer::new(size);
        sa.set_averaging(0.0);
        // 1500 Hz tone
        for chunk in sine(1500.0, sr, size * 4).chunks(160) {
            sa.feed_i16(chunk);
        }
        let db = sa.magnitudes_db();
        let (peak_bin, _) = db
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let peak_hz = sa.bin_hz(peak_bin, sr);
        assert!((peak_hz - 1500.0).abs() < 40.0, "peak {peak_hz} Hz");
    }

    #[test]
    fn silence_is_near_floor() {
        let mut sa = SpectrumAnalyzer::new(512);
        sa.set_averaging(0.0);
        for _ in 0..8 {
            sa.feed_i16(&[0i16; 160]);
        }
        assert!(sa.magnitudes_db().iter().all(|&d| d < -100.0));
    }

    #[test]
    fn peak_hold_is_sticky() {
        let sr = 8000;
        let mut sa = SpectrumAnalyzer::new(512);
        sa.set_averaging(0.0);
        for chunk in sine(1000.0, sr, 512 * 4).chunks(160) {
            sa.feed_i16(chunk);
        }
        let peak_after_tone: Vec<f32> = sa.peak_db().to_vec();
        assert!(peak_after_tone.iter().any(|&d| d > -40.0));
        for _ in 0..40 {
            sa.feed_i16(&[0i16; 160]);
        }
        // Peak-hold NEVER drops in silence (it only rises).
        for (after, before) in sa.peak_db().iter().zip(&peak_after_tone) {
            assert!(*after >= *before - 1e-3, "peak dropped: {after} < {before}");
        }
        // The peak stays high even long after the tone ends.
        assert!(sa.peak_db().iter().any(|&d| d > -40.0));
        // The current spectrum, on the other hand, should have fallen to the floor.
        assert!(sa.magnitudes_db().iter().all(|&d| d < -100.0));
    }
}
