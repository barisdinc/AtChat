//! 256 girişli colormap LUT'ları. Anchor noktalarından parça-parça lineer
//! interpolasyonla bir kez üretilir, `OnceLock` ile önbelleklenir.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colormap {
    Viridis,
    Inferno,
    Turbo,
    Gray,
}

impl Colormap {
    pub const ALL: [Colormap; 4] = [
        Colormap::Viridis,
        Colormap::Inferno,
        Colormap::Turbo,
        Colormap::Gray,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Colormap::Viridis => "Viridis",
            Colormap::Inferno => "Inferno",
            Colormap::Turbo => "Turbo",
            Colormap::Gray => "Gri",
        }
    }

    pub fn lut(self) -> &'static [[u8; 3]; 256] {
        match self {
            Colormap::Viridis => viridis(),
            Colormap::Inferno => inferno(),
            Colormap::Turbo => turbo(),
            Colormap::Gray => gray(),
        }
    }

    /// `t` 0..1 aralığına kırpılır.
    pub fn sample(self, t: f32) -> [u8; 3] {
        let idx = (t.clamp(0.0, 1.0) * 255.0).round() as usize;
        self.lut()[idx.min(255)]
    }
}

fn build(anchors: &[[f32; 3]]) -> [[u8; 3]; 256] {
    let mut lut = [[0u8; 3]; 256];
    let segs = anchors.len() - 1;
    for (i, out) in lut.iter_mut().enumerate() {
        let t = i as f32 / 255.0;
        let pos = t * segs as f32;
        let s = (pos.floor() as usize).min(segs - 1);
        let f = pos - s as f32;
        for c in 0..3 {
            let v = anchors[s][c] + (anchors[s + 1][c] - anchors[s][c]) * f;
            out[c] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    lut
}

fn viridis() -> &'static [[u8; 3]; 256] {
    static L: OnceLock<[[u8; 3]; 256]> = OnceLock::new();
    L.get_or_init(|| {
        build(&[
            [68.0, 1.0, 84.0],
            [72.0, 40.0, 120.0],
            [62.0, 74.0, 137.0],
            [49.0, 104.0, 142.0],
            [38.0, 130.0, 142.0],
            [31.0, 158.0, 137.0],
            [53.0, 183.0, 121.0],
            [110.0, 206.0, 88.0],
            [181.0, 222.0, 43.0],
            [253.0, 231.0, 37.0],
        ])
    })
}

fn inferno() -> &'static [[u8; 3]; 256] {
    static L: OnceLock<[[u8; 3]; 256]> = OnceLock::new();
    L.get_or_init(|| {
        build(&[
            [0.0, 0.0, 4.0],
            [31.0, 12.0, 72.0],
            [85.0, 15.0, 109.0],
            [136.0, 34.0, 106.0],
            [186.0, 54.0, 85.0],
            [227.0, 89.0, 51.0],
            [249.0, 140.0, 10.0],
            [249.0, 201.0, 50.0],
            [252.0, 255.0, 164.0],
        ])
    })
}

fn turbo() -> &'static [[u8; 3]; 256] {
    static L: OnceLock<[[u8; 3]; 256]> = OnceLock::new();
    L.get_or_init(|| {
        build(&[
            [48.0, 18.0, 59.0],
            [64.0, 91.0, 191.0],
            [61.0, 152.0, 246.0],
            [42.0, 204.0, 204.0],
            [76.0, 231.0, 137.0],
            [161.0, 248.0, 66.0],
            [227.0, 227.0, 39.0],
            [253.0, 165.0, 43.0],
            [232.0, 86.0, 24.0],
            [169.0, 24.0, 8.0],
            [122.0, 4.0, 3.0],
        ])
    })
}

fn gray() -> &'static [[u8; 3]; 256] {
    static L: OnceLock<[[u8; 3]; 256]> = OnceLock::new();
    L.get_or_init(|| {
        let mut lut = [[0u8; 3]; 256];
        for (i, p) in lut.iter_mut().enumerate() {
            *p = [i as u8, i as u8, i as u8];
        }
        lut
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_and_monotonic_gray() {
        for cm in Colormap::ALL {
            let lo = cm.sample(0.0);
            let hi = cm.sample(1.0);
            assert_ne!(lo, hi, "{}", cm.name());
        }
        // Gri: parlaklık monoton artmalı.
        let g = Colormap::Gray.lut();
        for w in g.windows(2) {
            assert!(w[1][0] >= w[0][0]);
        }
    }

    #[test]
    fn clamps_out_of_range() {
        assert_eq!(Colormap::Turbo.sample(-5.0), Colormap::Turbo.sample(0.0));
        assert_eq!(Colormap::Turbo.sample(9.0), Colormap::Turbo.sample(1.0));
    }
}
