//! Rust modülatör çıktılarını diske yazar; `rust/tools/check_vectors.py`
//! bunları Python `modem.py` ile demodüle edip bit-birebir eşleşmeyi
//! doğrular (ters yön çapraz-doğrulama).
//!
//!   cargo run -p modem --example emit_vectors
//!   python3 rust/tools/check_vectors.py

use std::io::Write;
use std::path::PathBuf;

use modem::{Mode, Modem};

fn main() {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/rust_vectors");
    std::fs::create_dir_all(&out).unwrap();

    let cases: Vec<(&str, Vec<u8>, Mode)> = vec![
        ("r_tiny_qpsk", b"Z".to_vec(), Mode::Qpsk),
        ("r_hello_bpsk", b"selam telsiz".to_vec(), Mode::Bpsk),
        (
            "r_join_qpsk",
            br#"{"type":"JOIN_REQUEST","src":"TA9ZZZ","dst":"ALL"}"#.to_vec(),
            Mode::Qpsk,
        ),
        (
            "r_bin512_qpsk",
            (0..512u32)
                .map(|i| (i.wrapping_mul(37).wrapping_add(5)) as u8)
                .collect(),
            Mode::Qpsk,
        ),
        (
            "r_bin512_bpsk",
            (0..512u32)
                .map(|i| (i.wrapping_mul(37).wrapping_add(5)) as u8)
                .collect(),
            Mode::Bpsk,
        ),
    ];

    let m = Modem::new();
    let mut manifest = String::from("[\n");
    for (i, (name, payload, mode)) in cases.iter().enumerate() {
        let samples = m.modulate_with_leadin(payload, *mode, 91);
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in &samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::File::create(out.join(format!("{name}.i16")))
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        manifest.push_str(&format!(
            "  {{\"file\": \"{name}.i16\", \"mode\": \"{}\", \"payload_hex\": \"{}\"}}{}\n",
            mode.as_str(),
            hex(payload),
            if i + 1 == cases.len() { "" } else { "," }
        ));
        println!("{name:16} {} {}B", mode.as_str(), payload.len());
    }
    manifest.push_str("]\n");
    std::fs::write(out.join("manifest.json"), manifest).unwrap();
    println!("-> {}", out.display());
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
