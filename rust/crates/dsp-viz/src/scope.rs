//! A time-domain "scope" — a rolling sample ring + a per-pixel-column min/max
//! envelope. Both min and max are kept per column so short transients (like
//! the edge of an OFDM burst) stay visible even when they fall on a single pixel.

use std::collections::VecDeque;

pub struct ScopeBuf {
    ring: VecDeque<f32>,
    capacity: usize,
}

impl ScopeBuf {
    /// `capacity_samples`: the most samples to keep (e.g. 1 s @ 8 kHz = 8000).
    pub fn new(capacity_samples: usize) -> Self {
        Self {
            ring: VecDeque::with_capacity(capacity_samples),
            capacity: capacity_samples.max(1),
        }
    }

    pub fn push_i16(&mut self, samples: &[i16]) {
        for &s in samples {
            if self.ring.len() == self.capacity {
                self.ring.pop_front();
            }
            self.ring.push_back(s as f32 / 32768.0);
        }
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    pub fn clear(&mut self) {
        self.ring.clear();
    }

    /// Reduce the newest `window` samples into `width` pixel columns. Per
    /// column `(min, max)` (normalised, -1..1). A vector full of `(0,0)` when
    /// there is no data.
    pub fn envelope(&self, window: usize, width: usize) -> Vec<(f32, f32)> {
        let width = width.max(1);
        if self.ring.is_empty() {
            return vec![(0.0, 0.0); width];
        }
        let window = window.max(1).min(self.ring.len());
        let start = self.ring.len() - window;

        // Indexed access to slice the ring: VecDeque `get` is O(1).
        let mut out = Vec::with_capacity(width);
        for col in 0..width {
            let a = start + (col * window) / width;
            let b = (start + ((col + 1) * window) / width)
                .max(a + 1)
                .min(self.ring.len());
            let mut mn = f32::INFINITY;
            let mut mx = f32::NEG_INFINITY;
            for i in a..b {
                let v = *self.ring.get(i).unwrap();
                mn = mn.min(v);
                mx = mx.max(v);
            }
            if !mn.is_finite() {
                mn = 0.0;
                mx = 0.0;
            }
            out.push((mn, mx));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_is_bounded() {
        let mut s = ScopeBuf::new(100);
        let block: Vec<i16> = (0..500).map(|i| i as i16).collect();
        s.push_i16(&block);
        assert_eq!(s.len(), 100);
    }

    #[test]
    fn envelope_tracks_extremes() {
        let mut s = ScopeBuf::new(2000);
        let block: Vec<i16> = (0..1000)
            .map(|i| (20000.0 * (i as f32 * 0.2).sin()) as i16)
            .collect();
        s.push_i16(&block);
        let env = s.envelope(1000, 50);
        assert_eq!(env.len(), 50);
        // Sine ~±0.61; every column should have max > 0 and min < 0.
        assert!(env.iter().all(|(mn, mx)| mx >= mn));
        assert!(env.iter().any(|(_, mx)| *mx > 0.3));
        assert!(env.iter().any(|(mn, _)| *mn < -0.3));
    }

    #[test]
    fn empty_returns_zeros() {
        let s = ScopeBuf::new(100);
        assert_eq!(s.envelope(50, 10), vec![(0.0, 0.0); 10]);
    }
}
