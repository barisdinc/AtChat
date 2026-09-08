#!/usr/bin/env python3
"""
The reverse-direction cross-check: demodulate the Rust modulator's outputs
with the Python `modem.py` and verify a bit-for-bit match.

First: cargo run -p modem --example emit_vectors
Then:  python3 rust/tools/check_vectors.py
"""
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, PROJECT_ROOT)

from modem import Modem  # noqa: E402

VEC_DIR = os.path.join(PROJECT_ROOT, "rust", "target", "rust_vectors")


def main():
    manifest_path = os.path.join(VEC_DIR, "manifest.json")
    if not os.path.exists(manifest_path):
        sys.exit("no manifest.json — first: cargo run -p modem --example emit_vectors")
    with open(manifest_path) as f:
        manifest = json.load(f)

    m = Modem()
    fails = 0
    for e in manifest:
        samples = np.fromfile(os.path.join(VEC_DIR, e["file"]), dtype="<i2")
        want = bytes.fromhex(e["payload_hex"])
        got = m.demodulate(samples)
        ok = got == want
        if not ok:
            fails += 1
        print(f"  {e['file']:20s} {e['mode']:4s}  {'OK' if ok else 'FAIL'}")

    if fails:
        sys.exit(f"\n{fails} vectors did not match")
    print(f"\n all {len(manifest)} vectors decoded bit-for-bit with the Python modem.py")


if __name__ == "__main__":
    main()
