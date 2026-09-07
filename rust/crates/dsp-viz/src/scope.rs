//! Zaman domeni "scope" — kayan örnek halkası + piksel-sütunu min/max zarfı.
//! Kısa geçişler (OFDM patlamasının kenarı gibi) tek piksele düşse bile
//! görünsün diye sütun başına hem min hem max tutulur.

use std::collections::VecDeque;

pub struct ScopeBuf {
    ring: VecDeque<f32>,
    capacity: usize,
}

impl ScopeBuf {
    /// `capacity_samples`: tutulacak en fazla örnek (ör. 1 sn @ 8 kHz = 8000).
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

    /// En yeni `window` örneği `width` piksel sütununa indir. Her sütun için
    /// `(min, max)` (normalize, -1..1). Veri yoksa `(0,0)` dolu bir vektör.
    pub fn envelope(&self, window: usize, width: usize) -> Vec<(f32, f32)> {
        let width = width.max(1);
        if self.ring.is_empty() {
            return vec![(0.0, 0.0); width];
        }
        let window = window.max(1).min(self.ring.len());
        let start = self.ring.len() - window;

        // Halkayı dilimlemek için indeksli erişim: VecDeque `get` O(1).
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
        // Sinüs ~±0.61; her sütunda max > 0 ve min < 0 olmalı.
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
