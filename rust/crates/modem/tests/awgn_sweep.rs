//! AWGN altında modem performans eğrisi — CLAUDE.md'de ölçülen davranışı
//! yeniden üretir:
//!   - QPSK: ~18 dB SNR'ye kadar %100, ~10–12 dB'de keskin "uçurum".
//!   - BPSK: 10 dB'de hâlâ büyük ölçüde çalışır (QPSK ~0 iken).
//!
//! Yavaş olduğu için varsayılan olarak `#[ignore]`. Çalıştırmak için:
//!   cargo test -p modem --test awgn_sweep -- --ignored --nocapture

use modem::{Mode, Modem};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Normal};

/// `channel_server.py::_apply_channel` AWGN kısmının birebir karşılığı.
fn add_awgn(x: &[i16], snr_db: f64, rng: &mut StdRng) -> Vec<i16> {
    let sig_power: f64 =
        (x.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / x.len() as f64).max(1.0);
    let noise_power = sig_power / 10f64.powf(snr_db / 10.0);
    let n = Normal::new(0.0, noise_power.sqrt()).unwrap();
    x.iter()
        .map(|&s| {
            let v = s as f64 + n.sample(rng);
            v.clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

fn success_rate(mode: Mode, snr_db: f64, trials: usize, rng: &mut StdRng) -> usize {
    let m = Modem::new();
    let mut ok = 0;
    for _ in 0..trials {
        let payload: Vec<u8> = (0..200).map(|_| rng.gen()).collect();
        let lead = rng.gen_range(20..300);
        let clean = m.modulate_with_leadin(&payload, mode, lead);
        let noisy = add_awgn(&clean, snr_db, rng);
        if m.demodulate(&noisy).as_deref() == Some(payload.as_slice()) {
            ok += 1;
        }
    }
    ok
}

#[test]
#[ignore = "yavaş; -- --ignored --nocapture ile çalıştırın"]
fn awgn_performance_curve() {
    let trials = 20;
    let mut rng = StdRng::seed_from_u64(0x00A7_C4A7);

    println!("\n  SNR(dB) |  QPSK  |  BPSK   (başarı / {trials})");
    println!("  --------+--------+-------");
    let mut results = Vec::new();
    for &snr in &[24.0, 18.0, 16.0, 14.0, 12.0, 10.0] {
        let q = success_rate(Mode::Qpsk, snr, trials, &mut rng);
        let b = success_rate(Mode::Bpsk, snr, trials, &mut rng);
        println!("  {snr:6.0}  |  {q:2}/{trials}  |  {b:2}/{trials}");
        results.push((snr, q, b));
    }

    let get = |snr: f64| results.iter().find(|(s, _, _)| *s == snr).unwrap();
    // Temiz uçta QPSK neredeyse kusursuz.
    assert!(get(24.0).1 >= 18, "QPSK@24dB çok düşük: {:?}", get(24.0));
    assert!(get(18.0).1 >= 17, "QPSK@18dB çok düşük: {:?}", get(18.0));
    // Uçurum: 10 dB'de QPSK çöker, BPSK ayakta kalır.
    assert!(
        get(10.0).1 <= 6,
        "QPSK@10dB uçurum beklenirdi: {:?}",
        get(10.0)
    );
    assert!(get(10.0).2 >= 12, "BPSK@10dB dayanmalıydı: {:?}", get(10.0));
}
