# AtCHAT — Rust port + egui GUI

A cross-platform, compilable Rust port of the Python simulation at the repo
root (`modem.py`, `client.py`, `channel_server.py`, `monitor.py`). Goal: an
egui/eframe app that gathers the `client`, `channel_server` and `monitor`
functions into **a single window**; the `monitor` tab shows the on-air
waveform as a **scope** (time domain), a **spectrum** (FFT) and a
**waterfall**, and plays the channel audio to the speakers.

The Python code stays at the root **untouched** — as both a reference and a
cross-check (Rust ↔ Python speak the same JSON wire).

## Status (phase by phase)

| Phase | Scope | Status |
|-----|--------|-------|
| 0 | Workspace scaffold | ✅ |
| 1 | `netproto` + `modem` (OFDM port) + tests | ✅ verified bit-for-bit against Python in both directions |
| 2 | `channel` + `atchat-channeld` (wire-compatible) | ✅ Python `client.py`/`monitor.py` interop verified |
| 3 | `protocol` (`Station`) + integration tests | ✅ 6 scenarios pass (election, chat, bulk bit-exact, ARQ, drop/reconnect, backup takeover) |
| 4 | `dsp-viz` (scope / spectrum / waterfall) | ✅ 11 unit tests (Hann+Welch+peak-hold, min/max envelope, colormap LUT, RGB waterfall) |
| 5 | `atchat-gui` (eframe, 3 tabs, cpal audio) | ✅ builds + runs; engine↔GUI glue test passes |
| 6 | Polish: presets, CI, packaging, docs | ✅ channel presets · matrix CI · cargo-dist release workflow · root docs |

## Prerequisite: the Rust toolchain

Installed stable via `rustup` (on this machine `~/.cargo/bin` and
`/opt/homebrew/opt/rustup/bin` must be on PATH). For a fresh install:

```
brew install rustup && rustup default stable      # or:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
rustc --version   # >= 1.75
```

## Build / test

```
cd rust

# Phase 1 gate — is the modem bit-for-bit with Python?
cargo test -p netproto
cargo test -p modem

# Cross-vectors (generate on the Python side first — once):
python3 tools/dump_vectors.py          # -> crates/modem/tests/vectors/*.i16
cargo test -p modem --test cross_vectors

# The reverse direction (Rust modulate -> Python demodulate):
cargo run -p modem --example emit_vectors
python3 tools/check_vectors.py

# AWGN performance curve (slow, reproduces the cliff from CLAUDE.md):
cargo test -p modem --test awgn_sweep -- --ignored --nocapture
```

# Phase 2 gate — channel physics + wire layer
cargo test -p channel

# Python interop (by hand): Rust channel + Python stations + Python monitor
cargo build -p atchat-channeld
./target/debug/atchat-channeld --port 6000 &
python3 ../monitor.py --port 6000 --quiet &
python3 ../client.py TA1ABC --port 6000        # separate terminal
python3 ../client.py TA2DEF --port 6000        # separate terminal
#   /chat hello   and   /sendfile <path> TA2DEF   -> the monitor logs all of it as "decoded"
```

# Phase 3 gate — station protocol logic (a port of client.py)
cargo test -p protocol                                  # 6 scenarios (~30 s)
cargo test -p protocol -- --ignored full_size_image     # full size (~75 s)

# Phase 4/5
cargo test -p dsp-viz -p atchat-gui                     # DSP + engine↔GUI glue

# GUI — all in one (quick experiments)
cargo run --bin atchat-gui

# GUI — separate processes (for multiple clients)
cargo run --bin atchat-channel -- --port 6000               # channel (one)
cargo run --bin atchat-client  -- --connect 127.0.0.1:6000  # client (AS MANY windows as you like)
cargo run --bin atchat-client  -- --connect 127.0.0.1:6000
cargo run --bin atchat-monitor -- --connect 127.0.0.1:6000  # scope/spectrum/waterfall

cargo build --release                                   # all binaries (target platform)
```

## Interfaces

| Binary | What | Tabs | How many instances |
|---|---|---|---|
| `atchat-gui` | All in one (in-proc channel) | Channel · Stations · NET · Monitor | 1 |
| `atchat-channel` | Channel physics + TCP server (`channel_server.py` wire-compatible) | Channel | 1 |
| `atchat-client` | Station(s) connected to the channel over TCP | Stations · NET | **many** |
| `atchat-monitor` | A passive visualiser listening to the channel | Monitor | many |

`atchat-channel` is the shared channel other processes connect to (including
the Python `client.py` / `monitor.py`). Open as many `atchat-client` windows
as you like and manage separate stations from each.

## Release packages

When a git tag of the form `v*` is pushed, two workflows run and upload their
outputs to the same GitHub Release (`atchat-gui` + `atchat-channeld`):

| Source | Output |
|---|---|
| `release.yml` (cargo-dist, `dist-workspace.toml`) | macOS arm64/x64 · Linux arm64/x64 · Windows x64 archives (`.tar.xz` / `.zip`) + **Windows `.msi`** + sha256 + `source.tar.gz` |
| `deb.yml` (cargo-deb) | Ubuntu/Debian **`.deb`** (x86_64); `atchat-gui` with a desktop entry |

The `atchat-gui` archive/MSI/deb contains all four interfaces (`atchat-gui`,
`atchat-channel`, `atchat-client`, `atchat-monitor`); `atchat-channeld` is a
separate package.

```
# bump the version in rust/Cargo.toml, then:
git tag v0.1.0 && git push origin v0.1.0      # triggers the release
dist plan                                     # shows what would be produced locally
```

## Using the GUI

- **Stations**: on the left, enter a callsign + mode (QPSK/BPSK) and press
  "＋ Add" (remove with ✕). For the selected station: a role badge, the
  roster, transfers with progress bars, chat (a target selector + Enter),
  "Drop"/"Reconnect", a filtered log. "Send file/image…" opens a local file
  picker.
- **NET** (bulk control): talk / send files from any station without changing
  tabs; **"All talk"** and **"All send…"** make every connected station do it
  at once; **Auto-chat** (a random station → ALL, at an adjustable interval)
  to watch the waterfall live. A merged NET chat stream (incoming + outgoing)
  + every station's active transfers in one list. **Images:** if a file
  received over the air is an image (PNG/JPG/GIF/BMP/WebP) it is displayed —
  with who, which file, when; ◀ ▶ to switch between images.
- **Channel**: AWGN dB + multipath delay/gain sliders (applied instantly),
  presets (Clean / 13 dB–ARQ / Multipath limit), a live busy state, an event
  log.
- **Monitor**: FFT size, colormap, floor/ceil dB, the scope window, reset
  peak, **Audio** (cpal — silently disables itself when there is no device).
  Below it: the scope (time), the spectrum (0–4 kHz, data band shaded,
  peak-hold), the waterfall and the passive decode strip.

## Workspace layout

```
crates/netproto   constants, frame types (serde), CRC32, line-JSON framing
crates/modem      OFDM modulator/demodulator — a bit-faithful port of modem.py
crates/channel    channel physics (half-duplex, AWGN, multipath) + Link (InProc/Tcp)
crates/protocol   Station: master election, roster, chat, ARQ (a port of client.py)
crates/dsp-viz    scope envelope, spectrum analyser, waterfall + colormap LUTs
apps/atchat-channeld   headless TCP channel server (channel_server.py wire-compatible)
apps/atchat-gui        the eframe app: Channel | Stations | Monitor
```

## Interop with Python

The Rust `atchat-channeld` and the Python `channel_server.py` speak the same
line-delimited JSON wire (`HELLO` / `TRANSMIT_AUDIO` / `TX_GRANTED` /
`CHANNEL_BUSY` / `RX_AUDIO`). So the Python `client.py` can connect to the
Rust channel and a Rust station to the Python channel — a gradual migration
and an extra cross-check layer.
