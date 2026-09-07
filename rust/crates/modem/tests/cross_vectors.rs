//! Python `modem.py` ile çapraz-doğrulama.
//!
//! 1. `tests/vectors/*.i16` — `rust/tools/dump_vectors.py` ile üretilmiş
//!    Python modülasyon çıktıları; Rust demodülatörü bunları bit-birebir
//!    çözmeli.
//! 2. Kökteki `ornek_net_sesi.wav` — gerçek protokol çerçevelerinin art
//!    arda modüle edilmiş hâli; enerji-kapılı segmentasyon + demod ile
//!    çerçeve tipleri geri okunmalı.
//!
//! Ters yön (Rust modüle -> Python demodüle) `rust/tools/check_vectors.py`
//! ile elle doğrulanır (numpy gerektirir).

use std::path::PathBuf;

use modem::Modem;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_i16_le(path: &PathBuf) -> Vec<i16> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

#[test]
fn python_vectors_decode_bit_exact() {
    let dir = manifest_dir().join("tests/vectors");
    let manifest_path = dir.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest_path)
        .expect("manifest.json yok — önce `python3 rust/tools/dump_vectors.py` çalıştırın");
    let manifest: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let entries = manifest.as_array().unwrap();
    assert!(!entries.is_empty());

    let m = Modem::new();
    for e in entries {
        let file = e["file"].as_str().unwrap();
        // Mod, demod tarafından header'dan otomatik algılanır; burada yalnız
        // manifest bütünlüğü için okunur.
        assert!(matches!(e["mode"].as_str(), Some("BPSK" | "QPSK")));
        let want = hex_decode(e["payload_hex"].as_str().unwrap());
        let samples = read_i16_le(&dir.join(file));
        let got = m
            .demodulate(&samples)
            .unwrap_or_else(|| panic!("{file}: demod None döndü"));
        assert_eq!(got, want, "{file}: payload eşleşmedi");
    }
}

/// Enerjiye göre burst'lere böl (basit VOX). Gerçek monitör de ileride
/// buna benzer bir segmentasyona ihtiyaç duyacak.
fn segment_bursts(x: &[i16]) -> Vec<Vec<i16>> {
    const THRESH: i32 = 150;
    const MAX_GAP: usize = 160; // ~20 ms @ 8 kHz
    const PAD: usize = 80;

    let mut bursts = Vec::new();
    let mut i = 0;
    while i < x.len() {
        if (x[i] as i32).abs() <= THRESH {
            i += 1;
            continue;
        }
        let start = i.saturating_sub(PAD);
        let mut last_loud = i;
        let mut j = i;
        while j < x.len() && j - last_loud <= MAX_GAP {
            if (x[j] as i32).abs() > THRESH {
                last_loud = j;
            }
            j += 1;
        }
        let end = (last_loud + PAD).min(x.len());
        bursts.push(x[start..end].to_vec());
        i = j;
    }
    bursts
}

#[test]
fn sample_wav_frames_decode() {
    let wav_path = manifest_dir().join("../../../ornek_net_sesi.wav");
    let mut reader = match hound::WavReader::open(&wav_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ornek_net_sesi.wav açılamadı ({e}) — atlanıyor");
            return;
        }
    };
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 8000);
    assert_eq!(spec.channels, 1);
    let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();

    let m = Modem::new();
    let mut decoded_types = Vec::new();
    for burst in segment_bursts(&samples) {
        if let Some(payload) = m.demodulate(&burst) {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&payload) {
                if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
                    decoded_types.push(t.to_string());
                }
            }
        }
    }
    eprintln!("çözülen çerçeveler: {decoded_types:?}");
    assert!(
        decoded_types.len() >= 4,
        "en az 4 çerçeve beklenirdi, {} çözüldü",
        decoded_types.len()
    );
    assert!(decoded_types.iter().any(|t| t == "BEACON"));
    assert!(decoded_types.iter().any(|t| t == "CHAT"));
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
