#!/usr/bin/env python3
"""
Generates cross-validation vectors with the Python `modem.py`.

Each vector: the output of `modem.modulate(payload, mode)` for a payload,
written as raw int16 little-endian to
`rust/crates/modem/tests/vectors/<name>.i16`. `manifest.json` holds each
vector's mode and payload (hex). The Rust-side `tests/cross_vectors.rs`
demodulates these files and verifies the payload matches bit-for-bit.

Usage:
    python3 rust/tools/dump_vectors.py
"""
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, PROJECT_ROOT)

from modem import Modem  # noqa: E402

OUT_DIR = os.path.join(PROJECT_ROOT, "rust", "crates", "modem", "tests", "vectors")


def frame_json(d: dict) -> bytes:
    return json.dumps(d, ensure_ascii=False).encode("utf-8")


VECTORS = [
    ("tiny_bpsk", b"A", "BPSK"),
    ("hello_qpsk", b"merhaba dunya", "QPSK"),
    ("join_qpsk", frame_json({"type": "JOIN_REQUEST", "src": "TA1ABC", "dst": "ALL"}), "QPSK"),
    ("beacon_bpsk", frame_json({
        "type": "BEACON", "src": "TA1ABC", "dst": "ALL",
        "backup": "TA2DEF", "roster": ["TA1ABC", "TA2DEF", "TA3GHI"],
    }), "BPSK"),
    ("chat_bpsk", frame_json({
        "type": "CHAT", "src": "TA2DEF", "dst": "ALL", "text": "grup goruntusu geliyor"
    }), "BPSK"),
    ("bin220_qpsk", bytes((i * 7 + 3) & 0xFF for i in range(220)), "QPSK"),
    ("bin1000_qpsk", bytes((i * 13 + 1) & 0xFF for i in range(1000)), "QPSK"),
    ("bin1000_bpsk", bytes((i * 13 + 1) & 0xFF for i in range(1000)), "BPSK"),
]


def main():
    os.makedirs(OUT_DIR, exist_ok=True)
    m = Modem()
    manifest = []
    for name, payload, mode in VECTORS:
        samples = m.modulate(payload, mode)
        assert samples.dtype == np.int16
        path = os.path.join(OUT_DIR, f"{name}.i16")
        samples.tofile(path)
        manifest.append({
            "file": f"{name}.i16",
            "mode": mode,
            "payload_hex": payload.hex(),
            "n_samples": int(len(samples)),
        })
        print(f"  {name:16s} {mode:4s} payload={len(payload):5d}B  samples={len(samples):6d}")

    with open(os.path.join(OUT_DIR, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2)
    print(f"\n{len(manifest)} vectors -> {OUT_DIR}")


if __name__ == "__main__":
    main()
