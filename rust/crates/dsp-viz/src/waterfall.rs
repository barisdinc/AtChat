//! Kayan spektrogram — dB satırlarını yoğunluk (0..255) satırlarına çevirir,
//! sabit yükseklikte bir halka tutar, RGB piksel buffer üretir.

use std::collections::VecDeque;

use crate::colormap::Colormap;
use crate::spectrum::db_to_unit;

pub struct Waterfall {
    width: usize,
    height: usize,
    rows: VecDeque<Vec<u8>>,
}

impl Waterfall {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width: width.max(1),
            height: height.max(1),
            rows: VecDeque::with_capacity(height.max(1)),
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
    pub fn rows_filled(&self) -> usize {
        self.rows.len()
    }

    pub fn clear(&mut self) {
        self.rows.clear();
    }

    /// Bir dB spektrum satırı ekle. `db` uzunluğu `width`'ten farklıysa lineer
    /// yeniden örneklenir. En eski satır düşer.
    pub fn push_row_db(&mut self, db: &[f32], floor_db: f32, ceil_db: f32) {
        let mut row = vec![0u8; self.width];
        if !db.is_empty() {
            for (x, cell) in row.iter_mut().enumerate() {
                let src = if self.width == 1 {
                    0.0
                } else {
                    x as f32 * (db.len() - 1) as f32 / (self.width - 1) as f32
                };
                let i = src.floor() as usize;
                let f = src - i as f32;
                let v = if i + 1 < db.len() {
                    db[i] + (db[i + 1] - db[i]) * f
                } else {
                    db[i.min(db.len() - 1)]
                };
                *cell = (db_to_unit(v, floor_db, ceil_db) * 255.0).round() as u8;
            }
        }
        if self.rows.len() == self.height {
            self.rows.pop_front();
        }
        self.rows.push_back(row);
    }

    /// RGB8 piksel buffer (row-major, `width*height*3`). `newest_on_top` true
    /// ise en yeni satır y=0'da; boş satırlar taban rengiyle (colormap[0]).
    pub fn to_rgb(&self, cmap: Colormap, newest_on_top: bool) -> Vec<u8> {
        let lut = cmap.lut();
        let base = lut[0];
        let mut out = vec![0u8; self.width * self.height * 3];
        let filled = self.rows.len();
        let pad = self.height - filled;

        for y in 0..self.height {
            // Görüntü satırı y -> hangi veri satırı?
            // Dolu satırlar en yeni en sonda; üstte `pad` boş satır.
            let row_ref: Option<&Vec<u8>> = if newest_on_top {
                // y=0 en yeni
                if y < filled {
                    self.rows.get(filled - 1 - y)
                } else {
                    None
                }
            } else {
                // y=height-1 en yeni; üstte boşluk
                if y >= pad {
                    self.rows.get(y - pad)
                } else {
                    None
                }
            };
            let dst = &mut out[y * self.width * 3..(y + 1) * self.width * 3];
            match row_ref {
                Some(r) => {
                    for (x, px) in dst.chunks_exact_mut(3).enumerate() {
                        px.copy_from_slice(&lut[r[x] as usize]);
                    }
                }
                None => {
                    for px in dst.chunks_exact_mut(3) {
                        px.copy_from_slice(&base);
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_buffer_has_expected_size() {
        let mut wf = Waterfall::new(64, 32);
        for _ in 0..10 {
            wf.push_row_db(&[-20.0; 40], -100.0, 0.0);
        }
        let rgb = wf.to_rgb(Colormap::Viridis, true);
        assert_eq!(rgb.len(), 64 * 32 * 3);
    }

    #[test]
    fn ring_is_bounded_and_newest_on_top() {
        let mut wf = Waterfall::new(8, 4);
        // 6 satır: ilk ikisi düşmeli.
        for k in 0..6 {
            let level = -100.0 + k as f32 * 10.0;
            wf.push_row_db(&[level; 8], -100.0, 0.0);
        }
        assert_eq!(wf.rows_filled(), 4);
        let rgb = wf.to_rgb(Colormap::Gray, true);
        // En yeni satır (y=0) en parlak (level = -50 -> ~0.5 -> ~128).
        let top = rgb[0];
        let second = rgb[8 * 3];
        assert!(top > second, "en yeni satır en üstte ve en parlak olmalı");
    }

    #[test]
    fn empty_rows_are_base_color() {
        let wf = Waterfall::new(4, 4);
        let rgb = wf.to_rgb(Colormap::Inferno, true);
        let base = Colormap::Inferno.lut()[0];
        assert_eq!(&rgb[0..3], &base);
    }
}
