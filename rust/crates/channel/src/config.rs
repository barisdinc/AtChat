//! `channel_server.py::_apply_channel` portu — kanal bozulmaları.

use rand::Rng;
use rand_distr::{Distribution, Normal};

pub const SAMPLE_RATE: u32 = netproto::SAMPLE_RATE;

/// Kanal bozulma ayarları (`channel_server.py` argümanlarıyla birebir).
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// AWGN gürültü seviyesi (dB). `None` -> gürültü eklenmez (temiz kanal).
    pub snr_db: Option<f64>,
    /// Çoklu-yol yankısının gecikmesi (ms). Koruma aralığı 8 ms.
    pub multipath_delay_ms: f64,
    /// Yankının doğrudan sinyale göre kazancı (0–1).
    pub multipath_gain: f64,
    /// TEST hook'u: 1-indeksli teslim edilen burst numaraları — listedekiler
    /// sıfırlanır (demod kesin başarısız olur). ARQ'yu deterministik tetiklemek
    /// için. Boş -> devre dışı.
    pub corrupt_burst_nums: Vec<u64>,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            snr_db: None,
            multipath_delay_ms: 0.0,
            multipath_gain: 0.0,
            corrupt_burst_nums: Vec::new(),
        }
    }
}

impl ChannelConfig {
    pub fn multipath_delay_samples(&self) -> usize {
        // Python: int(multipath_delay_ms / 1000 * SAMPLE_RATE)
        (self.multipath_delay_ms / 1000.0 * SAMPLE_RATE as f64) as usize
    }
}

/// GERÇEK kanal bozulmalarını uygular: önce çoklu-yol yankısı, sonra AWGN,
/// sonra i16'ya kırpma. Hiçbiri ayarlanmadıysa sinyal olduğu gibi geçer.
///
/// `channel_server.py::_apply_channel` ile birebir sıralama: gürültü gücü,
/// yankı EKLENDİKTEN sonraki sinyale göre hesaplanır.
pub fn apply_channel<R: Rng + ?Sized>(
    samples: &[i16],
    cfg: &ChannelConfig,
    rng: &mut R,
) -> Vec<i16> {
    let mut x: Vec<f64> = samples.iter().map(|&s| s as f64).collect();

    let d = cfg.multipath_delay_samples();
    if cfg.multipath_gain > 0.0 && d > 0 && d < x.len() {
        // echo = x'in d örnek geciktirilmiş kopyası (baş taraf sıfır).
        let echo: Vec<f64> = std::iter::repeat(0.0)
            .take(d)
            .chain(x[..x.len() - d].iter().copied())
            .collect();
        for (xi, ei) in x.iter_mut().zip(echo) {
            *xi += cfg.multipath_gain * ei;
        }
    }

    if let Some(snr_db) = cfg.snr_db {
        let mean_sq = x.iter().map(|v| v * v).sum::<f64>() / x.len().max(1) as f64;
        // Python: `np.mean(x**2) or 1.0` -> yalnız TAM 0.0 ise 1.0'a düşer.
        let sig_power = if mean_sq == 0.0 { 1.0 } else { mean_sq };
        let noise_power = sig_power / 10f64.powf(snr_db / 10.0);
        let normal = Normal::new(0.0, noise_power.sqrt()).expect("std >= 0");
        for xi in x.iter_mut() {
            *xi += normal.sample(rng);
        }
    }

    x.iter()
        .map(|v| v.clamp(-32768.0, 32767.0) as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    #[test]
    fn clean_config_is_identity() {
        let mut rng = StdRng::seed_from_u64(1);
        let x: Vec<i16> = (0..500).map(|i| ((i * 137) % 9001 - 4000) as i16).collect();
        let y = apply_channel(&x, &ChannelConfig::default(), &mut rng);
        assert_eq!(x, y);
    }

    #[test]
    fn awgn_injects_measurable_noise() {
        let mut rng = StdRng::seed_from_u64(2);
        let x: Vec<i16> = (0..4000)
            .map(|i| (8000.0 * (i as f64 * 0.05).sin()) as i16)
            .collect();
        let cfg = ChannelConfig {
            snr_db: Some(15.0),
            ..Default::default()
        };
        let y = apply_channel(&x, &cfg, &mut rng);
        let err_power: f64 = x
            .iter()
            .zip(&y)
            .map(|(a, b)| {
                let e = *a as f64 - *b as f64;
                e * e
            })
            .sum::<f64>()
            / x.len() as f64;
        let sig_power: f64 = x.iter().map(|a| (*a as f64).powi(2)).sum::<f64>() / x.len() as f64;
        let measured_snr = 10.0 * (sig_power / err_power).log10();
        assert!(
            (measured_snr - 15.0).abs() < 2.0,
            "ölçülen SNR {measured_snr:.1} dB, ~15 bekleniyordu"
        );
    }

    #[test]
    fn multipath_changes_signal_within_default_guard() {
        let mut rng = StdRng::seed_from_u64(3);
        let x: Vec<i16> = (0..2000)
            .map(|i| (10000.0 * (i as f64 * 0.1).sin()) as i16)
            .collect();
        let cfg = ChannelConfig {
            multipath_delay_ms: 3.0,
            multipath_gain: 0.3,
            ..Default::default()
        };
        let y = apply_channel(&x, &cfg, &mut rng);
        assert_ne!(x, y);
        assert_eq!(x.len(), y.len());
    }
}
