# NET protocol simulation (real audio-signal version)

This folder lets you test the radio-NET protocol we designed **without a real
radio/SDR**, over localhost — but it is no longer an abstract simulation: the
stations **really produce OFDM-modulated audio**, the channel carries that
audio (optionally with real noise/multipath impairment), and the receiving
stations decode it by **really demodulating** it. With `monitor.py` you can
listen to that real audio.

Dependencies: Python 3.9+ and `numpy` (`pip install numpy`).

📖 **[`OVERVIEW.md`](OVERVIEW.md)** — what ATCHAT is and what it can do (plain
language). **[`PROTOCOL.md`](PROTOCOL.md)** — the full technical specification:
the OFDM waveform bit by bit, the frame catalogue, the session state machines
and worked end-to-end scenarios.

Nicely typeset **PDF** versions with proper diagrams live in
[`docs/`](docs/): [`docs/OVERVIEW.pdf`](docs/OVERVIEW.pdf),
[`docs/PROTOCOL.pdf`](docs/PROTOCOL.pdf) (regenerate from the `.html` sources
beside them with headless Chrome `--print-to-pdf`).

> **Rust port + GUI (`rust/`):** This entire Python simulation has been ported
> to Rust and combined into a **cross-platform single-window egui app** —
> Channel / Stations / Monitor tabs, a live scope + spectrum + waterfall and
> (via cpal) the channel audio. The modem is verified **bit-for-bit** against
> Python; the Rust `atchat-channeld` speaks the same wire as the Python
> `client.py`/`monitor.py`. Setup and usage: [`rust/README.md`](rust/README.md).
> Quick start:
>
> ```
> cd rust && cargo run -p atchat-gui
> ```
>
> The Python side stays in this directory **unchanged** — as both a reference
> and a cross-check.

## If you just want to listen

Without installing anything, you can play `test_files/ornek_net_sesi.wav`
directly — real protocol frames (a join request, a beacon, chat messages, a
data block) really OFDM-modulated, back to back with short gaps between them.
It is a static example of what the audio you would hear live with `monitor.py`
below actually is.

## Architecture

```
channel_server.py   "Channel physics": carries the REAL audio samples,
                     enforces the half-duplex constraint, and optionally adds
                     real AWGN noise and multipath echo. Contains NO protocol
                     logic.

client.py            The actual station software: LBT+backoff, master
                     election/failover, roster, chat, image/file transfer via
                     block+CRC+ARQ, drop/reconnect. It REALLY modulates and
                     demodulates the frames with modem.py.

modem.py             A real OFDM modulator/demodulator: 8000 Hz sample rate,
                     256-point FFT, 8 ms guard interval, 77 subcarriers,
                     Schmidl-Cox-style synchronisation, frequency-domain
                     differential BPSK/QPSK coding.

protocol.py          Shared constants and JSON framing helpers.

monitor.py           A passive listener that plays and decodes the REAL audio
                     on the channel (optional). It also tries to decode it
                     with its own demodulator, and says so plainly when it
                     cannot.
```

This split is deliberate: when you later move to real SDR/radio hardware, you
will most likely change only `channel_server.py` (its sound-card/SDR I/O) —
the protocol logic in `client.py` and the modulation in `modem.py` can stay
the same.

## An honest note about modem.py

This is a **real** OFDM modem — not made-up tones; it modulates/demodulates
with a real IFFT/FFT and is genuinely subject to real bit errors. But it has
deliberate simplifications:

- **No adaptive bit loading**: the header is always BPSK (robust), the data
  block is BPSK or QPSK (fixed, per the `mode` chosen in `client.py`).
- **No channel estimation/equaliser**: robust to *mild* multipath echoes
  within the guard interval (8 ms) (tested: ~-16 dB gain, delays up to 7 ms),
  but it breaks on purpose for strong echoes (tested: -10 dB gain, 3 ms+) or
  delays beyond the guard interval — adding channel estimation is a natural
  next step.
- **No channel coding (LDPC/RS)**: integrity is checked with CRC32 only, and
  error correction is left to the upper layer's block-based ARQ. Measured
  behaviour: 100% on a clean signal, perfect down to ~18 dB SNR, a sharp
  "cliff" around ~10-12 dB (the expected behaviour for QPSK without FEC — if
  you recall the "COFDM cliff-edge" topic from our design discussion, this is
  exactly what you are seeing).

## Setup and running

1. **Start the channel server** (1 terminal):
   ```
   python3 channel_server.py --port 6000
   ```
   Parameters (now real audio impairments):
   - `--snr 15`  → AWGN level (dB). Omit it and no noise is added (a clean
     channel). As you lower it (e.g. 10-12 dB) you will see ARQ kick in; too
     low (< ~8 dB) and nothing gets through.
   - `--multipath-delay-ms 3 --multipath-gain 0.2`  → adds a real echo. The
     guard interval is 8 ms — delays below it (especially at low gain) show
     OFDM's advantage, delays above it show the breakdown.

   **Note:** because real audio is now carried, transmission times are REAL —
   the old `--speed` speed-up parameter has been removed. Master election
   still takes a real ~24 seconds (that is a design parameter in
   `protocol.py`, independent of the audio rate).

2. **(Optional but recommended) Listen to the channel:**
   ```
   python3 monitor.py --port 6000
   ```
   It now plays the audio actually received — if there is noise/distortion on
   the channel you will hear that too. It also tries to decode it with its own
   demodulator and logs who sent what as text; when it cannot decode it says
   "could not decode" (like a real operator saying "I heard something but
   could not read it"). No extra dependency; on Linux `aplay`/`paplay`, on Mac
   `afplay`, on Windows `winsound` are used automatically — if none are
   present it silently continues with the text log.

3. **Open the stations in 4 separate terminals:**
   ```
   python3 client.py TA1ABC
   python3 client.py TA2DEF
   python3 client.py TA3GHI
   python3 client.py TA4JKL
   ```
   You can give each station a different default modulation if you like:
   `--mode BPSK` (chat always goes out as BPSK anyway; this flag only sets the
   default mode of the bulk transfers you send with `/sendimage`/`/sendfile`).

4. In any station terminal you can type these commands:

   | Command | Description |
   |---|---|
   | `/chat hello` | Message to the common chat |
   | `/msg TA2DEF hi` | Private message (addressed, not encrypted) |
   | `/sendimage test_files/grup_gorseli.bin` | Send image/data to everyone |
   | `/sendimage test_files/grup_gorseli.bin TA3GHI` | Send to a specific station |
   | `/sendfile test_files/belge.bin TA4JKL` | File transfer (private) |
   | `/status` | Role (LISTENER/MASTER/BACKUP), roster, transfer states |
   | `/drop` | **Simulate a sudden drop** (the connection is cut, the process stays) |
   | `/reconnect` | Reconnect, resume where it left off |
   | `/quit` | Quit |

   The `test_files/` folder has two ready-made data files: `grup_gorseli.bin`
   (12 KB) and `belge.bin` (45 KB) — so you can test the protocol right away
   even without a real image/file. **Note:** because real audio is now
   carried, large files like `belge.bin` really can take minutes in QPSK (see
   the "real durations" note below) — for a first try I recommend starting
   with `grup_gorseli.bin` or a smaller file.

## Suggested test scenarios

**1) Hear the audio:** keep `monitor.py` open and in another terminal type
`/chat hello` — you will hear a short, real OFDM burst.

**2) Master election:** open only the server + 1 client, wait ~24 seconds,
type `/status` — you should see `role=MASTER`.

**3) ARQ under noise:** start the server with `--snr 13` and try
`/sendimage` — some blocks will fail their CRC, and in the terminal you will
see `X/Y blocks missing, requesting them` messages and resend rounds. If
`monitor.py` is running you will also hear it say "could not decode" for
those blocks.

**4) See the multipath limit:** try a transfer with `--multipath-delay-ms 5
--multipath-gain 0.15` (inside the guard interval, mild) — it should succeed.
Then try `--multipath-delay-ms 15` (beyond the guard interval) — observe the
failure. This is exactly why we chose OFDM.

**5) Sudden drop + resume:** start a transfer, a few seconds later type
`/drop` on the sending side, a few seconds later type `/reconnect` — you will
see the receiver automatically request only the missing blocks.

**6) Master drop:** type `/drop` on the station in the master role, wait ~24
seconds, and confirm the backup master takes over automatically.

## About real durations

Because the audio is now real, the durations are real too — no speed-up.
Rough idea: ~250 B/s in QPSK, ~125 B/s in BPSK (on a clean channel). The
12 KB `grup_gorseli.bin` takes ~50 seconds in QPSK, the 45 KB `belge.bin`
~3 minutes — consistent with the calculations from our design discussion,
because we now really modulate that much data.

## Known limitations (deliberate MVP decisions)

- Failover currently only chains as far as the **assigned backup master**; if
  the backup also drops, you would need to add the "next active station in the
  roster" logic for a third station to take over.
- `modem.py` has no adaptive bit loading, no channel estimation/equaliser and
  no LDPC/RS channel coding (see the "honest note" section above) — these are
  the natural next development step.
- Synchronisation handles each transmission as an independent "burst" (not
  starting to listen to a continuous audio stream at an arbitrary point) —
  this suits the nature of our half-duplex channel, but because a real SDR
  receiver works with continuously streaming IQ samples, this part would need
  revisiting in that transition.

## Roadmap to SDR

Because `client.py` and `modem.py` now really produce/decode audio, the next
step is actually smaller than you might think: it may be enough to replace the
"carry the audio over TCP as JSON+base64" part of `channel_server.py` with a
real sound-card/SDR I/O layer (e.g. mic/speaker via `sounddevice`, or an SDR's
IQ stream) — the `modulate()`/`demodulate()` functions of `modem.py` and all
of `client.py`'s protocol logic stay largely the same.
