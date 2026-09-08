# CLAUDE.md — Radio NET Protocol Simulation

This file exists so that context does not have to be re-explained when
continuing this project in another session. After reading it, Claude should
know the whole project, the decisions made, the bugs found and the current
state.

## Project goal

A digital mode that would run over amateur radio — as simple as SSTV but more
robust, inspired by KGSTV/EasyPal/HSModem, operating in a 2.7 kHz SSB
bandwidth; capable of image/file transfer + error correction (ARQ) +
multi-station chat (NET) — was **designed and simulated in software**. It is
not yet connected to real SDR/radio hardware (that is described in the "Next
steps" section at the end).

The work progressed in this order:
1. Modulation choice and rationale (why COFDM was chosen)
2. NET protocol design: multiple stations, dynamic master election,
   common+directed chat, time-sliced channel access
3. A concrete scenario simulation (a 3-4 station example, timing calculations)
4. **A real test environment in Python** was built: a TCP-based channel
   server + station clients (first a JSON/abstract simulation)
5. **A real OFDM modem** was written (`modem.py`) — it now really produces
   audio and really demodulates it; it is not an abstract simulation
6. Listening to the real channel audio was added with `monitor.py`
7. Two serious bugs found in real use were fixed (detailed below)

## Architecture

```
netproto.py          Shared constants + JSON framing helpers (send_json/
                      read_json, CRC32, super-frame timing constants). NOTE:
                      the file name is deliberately NOT "protocol.py" — it was
                      renamed "netproto.py" because it clashed with another
                      PyPI package of the same name on the user's system.
                      This name must be kept in a later session too.

modem.py              A REAL OFDM modulator/demodulator. Not made-up tones —
                      real IFFT/FFT, really subject to real bit errors.
                      Details below.

channel_server.py     "Channel physics": carries the audio samples the
                      stations really produce, enforces half-duplex access
                      (one station at a time), optionally adds REAL AWGN noise
                      (--snr) and multipath echo (--multipath-delay-ms/
                      --multipath-gain). Contains NO protocol logic (master
                      election, ARQ, chat, etc.) — a deliberate layer split.

client.py             The actual station software. ALL the protocol logic is
                      here: LBT+backoff channel access, dynamic master
                      election/failover, roster, common+directed chat,
                      image/file transfer via block+CRC+ARQ, sudden drop/
                      reconnect. It really modulates/demodulates the frames
                      with modem.py.

monitor.py            A passive listener on the REAL channel audio (optional).
                      Does not appear in the roster (never transmits). Tries
                      to decode it with its own demodulator, and says "could
                      not decode" when it cannot.

README.md            The end-user setup/usage instructions (different from
                      this file — CLAUDE.md is for development context,
                      README.md is for the end user).

test_files/          grup_gorseli.bin (12KB), belge.bin (45KB) — ready-made
                      test data. ornek_net_sesi.wav — real modulated protocol
                      frames (JOIN, BEACON, 2x CHAT, BULK_META, BULK_BLOCK)
                      laid back to back, a directly playable example.
```

Deliberate layer split: `channel_server.py` = the channel PHYSICS,
`client.py` = the protocol LOGIC, `modem.py` = the MODULATION. Moving to a
real SDR most likely changes only `channel_server.py`.

## modem.py — PHY design details

Consistent with the PHY table from the design discussion:

- `SAMPLE_RATE = 8000` Hz, `N = 256` (FFT size), `CP_LEN = 64` (8 ms guard
  interval), `SYMBOL_LEN = 320`
- `DATA_CARRIERS = range(10, 87)` → 77 subcarriers (~312-2688 Hz, within the
  2.7 kHz target)
- Synchronisation: Schmidl-Cox style — the preamble carries energy only on
  the even-indexed subcarriers (two identical halves in the time domain), and
  the receiver searches for that by autocorrelation
- **Frequency-domain differential coding**: the bits are carried not as
  absolute phase but as the phase DIFFERENCE between adjacent subcarriers
  (the first carrier = a fixed reference, carries no information). This gives
  robustness against synchronisation errors without channel estimation/
  equalisation (see bug #2 below)
- Header symbol: always BPSK, `HEADER_BITS=17` (16-bit length + 1-bit mode
  flag: 0=QPSK,1=BPSK), decoded with `HEADER_REPEAT=4` repeats + majority
  voting
- Data symbols: BPSK or QPSK per the `mode` parameter
  (`Modem.modulate(payload, mode)`)
- Integrity: CRC32 (appended to the payload and sent), NO FEC/LDPC/RS — error
  correction is left to the upper layer's ARQ (a deliberate design)

**Measured real performance** (tested, not made up):
- Noiseless: 100% success (tested at various sizes from 1B to 5000B)
- AWGN: 100% from a clean signal down to 18 dB SNR, a slight drop at
  ~14-16 dB, a sharp "cliff" at ~10-12 dB (expected for QPSK without FEC —
  concrete evidence of the "COFDM cliff-edge" topic from the design
  discussion)
- BPSK is markedly more robust than QPSK but ~67% slower (real measurement:
  at 10 dB QPSK 0/20 success, BPSK 19/20 success)
- Multipath: solid for MILD echoes within the guard interval (8 ms) (-16 dB
  gain, delays up to 7 ms); breaks on purpose for STRONG echoes (-10 dB gain,
  3 ms+) or delays beyond the guard interval (because there is no channel
  estimation — see the limitations)

## client.py — Protocol design

**Frame types:** `JOIN_REQUEST`, `BEACON`, `MASTER_CLAIM` (implicit, inside
BEACON), `CHAT` (broadcast/unicast, via the DST field), `BULK_META`,
`BULK_BLOCK`, `BULK_END`, `BULK_STATUS`.

**Master election/failover:** The first station to connect declares itself
master if it hears no beacon for `BEACON_TIMEOUT` (24 s, `netproto.py`). The
master broadcasts a beacon every `BEACON_INTERVAL` (8 s) (with the roster +
the assigned backup master). If two stations become master at the same time,
the alphabetically smaller callsign wins (a simple tie-break).

**Multi-level failover (next-step #1, DONE):** `master_watchdog` no longer
special-cases a single backup. On beacon timeout every non-master station
derives the SAME ordered "succession line" from its local roster — the sorted
set `{self} ∪ {active peers}` minus the presumed-dead master — and takes over
once the beacon has been silent for `BEACON_TIMEOUT + pos * BEACON_INTERVAL`
(`pos` = its index in that line). Higher-priority survivors key up first;
dead peers ahead age out to "lost", drop off the line and everyone below
moves up, so the chain continues arbitrarily deep. A genuine simultaneous
claim is still resolved by the alphabetical tie-break in `on_beacon`.
Bootstrap (no master ever seen) still takes over immediately. Helpers:
`_succession_line` / `_succession_pos` / `_should_take_over` in `client.py`;
`succession_pos` in the Rust `station.rs`. Tested: Rust
`failover_chains_past_a_single_backup`, and a real end-to-end run against
`channel_server.py` + 3 `client.py` (master→backup→third station).

**Roster:** Each station keeps `{callsign: {last_seen, status}}` locally.
Marked "lost" after `LOST_TIMEOUT` (30 s), removed entirely after
`REMOVE_TIMEOUT` (120 s).

**ARQ (block-based):** A file/image is split into `BLOCK_SIZE=220`-byte
blocks, each protected by CRC32. After `BULK_END` the receiver sends the list
of missing blocks (`BULK_STATUS`), and the sender resends only those blocks —
the whole transfer does not start over.

**Adaptive modulation (next-step #4, DONE):** there is still no pilot-based
per-carrier bit loading (that needs the channel estimator, #5), but the ARQ
loop is used as a real link-quality signal: if a QPSK round loses more than
`ADAPT_DOWNSHIFT_FRAC` (0.15) of the blocks it sent, `send_bulk` drops that
transfer to BPSK for the rest (`_adapt_mode` in `client.py`; inline in
`send_bulk` in the Rust port). Downshift only — it never oscillates back up.
The receiver needs no change: every frame's header carries the BPSK/QPSK flag,
so a mixed-mode transfer decodes fine. Tested: Rust
`qpsk_round_with_heavy_loss_downshifts_to_bpsk`.

**Sudden drop/reconnect:** `/drop` closes the TCP connection but the
process/state stays alive in RAM. `/reconnect` reconnects, and if an active
beacon is heard it NEVER declares itself master (it only sends a
JOIN_REQUEST). When the receiving side sees the sending station come back with
a JOIN_REQUEST (whether or not it is "lost" in the roster — in
`handle_frame`'s JOIN_REQUEST branch) it automatically requests the missing
blocks for the half-finished transfers. On the sending side too
(`on_bulk_status`), if a delayed BULK_STATUS arrives it responds with the
blocks it holds even when there is no active ARQ loop (as a background task,
`_resend_missing`).

**Control windows (VERY IMPORTANT, the fix for a real bug):** In
`_send_blocks` there is a deliberate pause of `CONTROL_WINDOW_PAUSE=1.2`
seconds every `CONTROL_WINDOW_EVERY=3` blocks. WITHOUT it the bulk transfer
occupies the channel continuously — chat messages AND even BEACONS starve,
which leads to wrong master-election conflicts (we saw this for real, see
Bug #3 below). I am not saying do not lower/raise these parameters, but do not
change them without knowing why they are at these values — the maths is
explained in a comment (the block above `_send_blocks` in `client.py`).

**Adaptive control window (next-step #9, DONE):** the values above are now the
QUIET baseline (enough to keep beacons alive during an otherwise idle
transfer). When the channel is actually contended — local chat queued
(`_chat_waiting`), or another station sent a CHAT/JOIN/BULK_STATUS within
`CONTROL_CONTENDED_FOR` (6 s, tracked as `_foreign_ctrl_ts`) — `_send_blocks`
switches to the BUSY values (`*_BUSY`: a window after every single block, held
1.5 s) and relaxes back on its own. `_control_window()` picks the pair each
block. Same logic in the Rust port (`control_window()` + `pending_chat` /
`foreign_ctrl`, config fields `control_window_*_busy` /
`control_contended_for`).

## Real bugs found and fixed (important, do not fall into them again)

These are NOT guesses — they were really tested, observed and fixed:

1. **`receive_loop` deadlock** (in the first JSON-based version): responses
   triggered by an incoming frame (`on_bulk_end`, `on_bulk_status`) `await`ed
   `send_frame` DIRECTLY; but the one that would handle `send_frame`'s own
   reply (TX_GRANTED) was `receive_loop` itself — it was waiting on itself.
   **Fix:** ALL sends triggered by an incoming frame must be started in the
   background with `asyncio.create_task(...)`, never `await`ed on
   `receive_loop`'s call chain.

2. **OFDM synchronisation bugs** (while developing modem.py, 3 separate bugs):
   - Because the CP (cyclic prefix) is a copy of the periodic preamble, the
     correlation score formed a "plateau" that started CP_LEN BEFORE the true
     start → fixed with a moving average.
   - Absolute-phase-based demodulation caused a large phase shift on the
     high-index subcarriers on even a 1-sample sync error → switched to
     frequency-domain differential coding (explained above).
   - A small sync slip pushed the last symbol past the buffer → a `CP_LEN`
     sample pad (padding) was added to the end of the waveform.

3. **Bulk transfer choked the channel** (found by the user in real use): in
   the first fix the control window was too short/sparse (0.5 s every 6
   blocks) — a competing station's retry timing syncs to "the end of the
   current block" from the server's `retry_after`, but it does not know
   exactly when the window opens, so it most likely missed it. The real
   result: not just chat but even BEACONS were missed, both stations could not
   "hear" each other and both declared themselves master (seen in a real log:
   `master conflict`, `marked as lost`). **Fix:** the window was made more
   frequent and wider (1.2 s every 3 blocks), and the retry count was raised
   from 20 to 40. Re-tested with a real 45-block transfer (README.md, 9815B):
   ZERO master conflicts (except the one-time normal election conflict at the
   start), both chat messages delivered successfully (within 1.3 s and 3.7 s),
   the file completed bit-exact.

4. **`protocol.py` name clash**: another package of the same name was
   installed on the user's system
   (`~/Library/Python/3.9/site-packages/protocol/`) and took precedence over
   the local `protocol.py`. **Fix:** the file was renamed `netproto.py` and
   all imports were updated. The fix was verified by simulating a fake
   clashing package (deliberately placed at the front of `sys.path`).

## Test status (verified scenarios)

All really run and verified (not made up):

- ✅ 2 stations: basic connection, JOIN_REQUEST, master election
- ✅ Common (broadcast) and directed (unicast) chat, over real audio
- ✅ Small (600B) and medium (1200-9815B) file transfer, with real OFDM
  modulation/demodulation, a bit-exact result
- ✅ Live ARQ recovery under 14 dB AWGN noise (1 block corrupted, fixed in
  2 ARQ rounds, the file was bit-exact)
- ✅ Sudden drop (`/drop`) + reconnect (`/reconnect`) + automatic missing-
  block request + completing only the missing part (without starting over) —
  tested in both the master and normal-station roles
- ✅ Master drop + the backup master taking over automatically
- ✅ Multi-level failover: master → backup → THIRD station (the chain no longer
  stops at one backup) — Rust `failover_chains_past_a_single_backup` + a real
  `channel_server.py` + 3×`client.py` end-to-end run
- ✅ Adaptive modulation: a QPSK round with heavy block loss makes the sender
  fall back to BPSK mid-transfer, still bit-exact — Rust
  `qpsk_round_with_heavy_loss_downshifts_to_bpsk`
- ✅ A 3-station scenario (master + 2 stations, roster synchronisation)
- ✅ Chat AND beacons being delivered reliably while a real 45-block (9815B)
  file transfer is in progress (after the control-window fix)
- ✅ The `netproto.py` name-clash scenario (deliberately simulated)
- ✅ Multipath end to end (integration, not just `modem.py`): a real transfer
  through a MILD echo (2 ms, ~-18 dB, inside the 8 ms guard) lands bit-exact —
  Rust `transfer_survives_multipath_within_guard`. NOTE: a stronger echo pushes
  sustained block loss into the known BULK_END-loss ARQ stall, so it is not
  asserted; BPSK + a channel estimator (#5) are the real fix. Still not tested
  with the Python `--multipath-*` server flags specifically.
- ✅ The full 4-station NET scenario (group ALL image + private file + a station
  dropping/reconnecting mid-run) — Rust `four_station_net_group_and_private`.
  The two transfers run one after the other, not literally at once: two big
  transfers on a 2.7 kHz half-duplex channel starve each other (a channel
  property). Not yet run with 4 real Python `client.py` processes.
- ❌ No real sound-card/microphone loopback test was done (still carried over
  TCP as base64)

## Known limitations / next steps (not in priority order)

1. ~~**The failover chain is limited to a single backup.**~~ DONE — succession
   line, see "Multi-level failover" above.
2. ~~**Multipath not verified end to end.**~~ DONE for a mild echo (Rust
   `transfer_survives_multipath_within_guard`); a strong echo still stalls on
   the BULK_END-loss issue, and the Python `--multipath-*` path is untested.
3. ~~**The full 4-station scenario was not tested.**~~ DONE in Rust
   (`four_station_net_group_and_private`); not yet with 4 real Python processes.
4. ~~**No adaptive bit loading.**~~ PARTLY DONE — ARQ-loss-driven QPSK→BPSK
   downshift (see "Adaptive modulation" above). Real per-carrier bit loading by
   SNR still needs the channel estimator (#5).
5. **No channel estimation/equaliser** — the root cause of the multipath
   limitation; adding pilot-based channel estimation is a natural next step.
6. **No LDPC/RS FEC** — integrity is CRC32 only, error correction is left to
   ARQ (a deliberate MVP decision, but a real next step).
7. **No real sound-card/microphone I/O** — samples are still carried over
   TCP+JSON+base64, not connected to audio hardware via real `sounddevice`.
8. **No real SDR/RF integration** — see the "Roadmap to SDR" section in the
   README; `channel_server.py` is expected to change, `client.py`/`modem.py`
   to stay largely the same.
9. ~~**The control window (CONTROL_WINDOW_EVERY/PAUSE) is fixed.**~~ DONE —
   QUIET/BUSY pair, see "Adaptive control window" above.

## How to run (summary, details in README.md)

```
pip install numpy   # the only external dependency

# terminal 1
python3 channel_server.py --port 6000
# optional real impairment: --snr 15 --multipath-delay-ms 3 --multipath-gain 0.2

# terminal 2 (optional but recommended - to listen to the real audio)
python3 monitor.py --port 6000

# terminals 3, 4, 5, 6 - stations
python3 client.py TA1ABC
python3 client.py TA2DEF
# commands: /chat, /msg, /sendimage, /sendfile, /status, /drop, /reconnect, /quit
```

## Rust port + egui GUI (the `rust/` directory)

The user asked for "a UI for client and channel_server, ideally something
cross-platform and compilable (Rust), and a UI for monitor too — showing the
on-air waveform as scope + spectrum + waterfall". Decisions (user-approved):
**a full Rust port** (modem + protocol + channel + GUI), **egui/eframe**,
**all in one app**, the code in the `rust/` subdirectory, **audio in the
monitor via cpal** too, and the Rust side speaks **the same JSON TCP wire as
Python**.

The Python files (`modem.py`, `client.py`, `channel_server.py`, `monitor.py`,
`netproto.py`) stay at the root **untouched** — reference + cross-check.

### Workspace (`rust/`, a Cargo workspace)

```
crates/netproto   constants, Frame/ClientMsg/ServerMsg (serde), CRC32, line-JSON framing
crates/modem      OFDM mod/demod — a BIT-EXACT port of modem.py (rustfft). The fixed preamble is embedded.
crates/channel    ChannelCore (half-duplex, AWGN, multipath) + Link (LinkTx/LinkRx separate) +
                  InProc/Tcp Connector + tcp_server (channel_server.py wire-compatible) +
                  passive monitor demod -> ChannelEvent::Decoded. TEST hook: cfg.corrupt_burst_nums.
crates/protocol   Station = a tokio-async port of client.py. Timings can be shortened for tests via
                  StationConfig (the GUI uses the real 24 s election).
crates/dsp-viz    ScopeBuf (min/max envelope) + SpectrumAnalyzer (Hann+Welch EWMA+peak-hold) +
                  Waterfall (dB->RGB) + Colormap (256 LUT). Independent of any GUI framework.
apps/atchat-channeld  headless TCP channel — the channel_server.py arguments exactly
apps/atchat-gui       eframe: Channel | Stations | Monitor tabs + cpal audio.
                      The engine (engine.rs) runs a tokio runtime on its own thread; GUI↔engine mpsc + Arc<Mutex<Snapshot>>.
```

### Verification status (all passing)

- `modem`: noiseless roundtrip 100%; **bidirectional Python cross-vector
  bit-exact** (`tools/dump_vectors.py` + `tests/cross_vectors.rs` +
  `tools/check_vectors.py`); the AWGN curve reproduces the cliff from
  CLAUDE.md (`tests/awgn_sweep.rs`, `#[ignore]`).
- `channel`: 8 tests + **the Python `client.py`/`monitor.py` connecting to the
  Rust `atchat-channeld` with bit-exact chat/ARQ/file** (verified by hand).
- `protocol`: 10 scenarios — election, chat, bulk bit-exact, ARQ (lost block),
  drop/reconnect resume, backup takeover, **multi-level failover chain
  (`failover_chains_past_a_single_backup`)**, **adaptive QPSK→BPSK downshift
  (`qpsk_round_with_heavy_loss_downshifts_to_bpsk`)**, **mild multipath end to
  end (`transfer_survives_multipath_within_guard`)**, **full 4-station NET
  (`four_station_net_group_and_private`)**. Plus a `#[ignore]`-d full-size test.
- `dsp-viz`: 11 unit tests. `atchat-gui`: 4 engine↔GUI glue tests.
- `cargo clippy --workspace` clean, `cargo fmt` applied.

### Bugs found/preserved during the port

- **CLAUDE.md #1 (deadlock)**: ALL sends triggered by an incoming frame use
  `tokio::spawn` — never `await`ed on the `receive_loop` chain.
- **CLAUDE.md #3 (control window)**: 1.2 s every 3 blocks in `_send_blocks`.
- **NEW (Rust-specific)**: `receive_loop` held the `rx_slot` (tokio Mutex)
  guard across `notified().await` → `reconnect` could not acquire the lock →
  deadlock. Fix: the guard is held only for the duration of `take()`.
- **Known limitation (inherited from client.py)**: if the BULK_END after an
  ARQ round is lost, the transfer stays suspended until the sender
  reconnects. That is why the `arq_recovers_from_lost_blocks` test uses
  deterministic block corruption via `cfg.corrupt_burst_nums` instead of pure
  AWGN.

### Remaining (Phase 6)

- Package production with `cargo-dist` (matrix CI `.github/workflows/ci.yml`
  is READY).
- The Rust section of the root CLAUDE.md/README.md (THIS section).
- Waterfall frequency zoom (0–2.76 kHz) — ADDED.

### GUI fixes

- **SEND button dead in the chat rows** (Stations tab + NET tab): the
  single-line text field had `desired_width = INFINITY`, so in the horizontal
  row it consumed all the width and pushed the "Send"/"All talk" buttons past
  the clip rect — visible but unclickable. Fix (`app.rs`): the buttons are
  placed first in a `right_to_left` sub-layout so their space is reserved, and
  the field fills what's left; Enter in the field still sends.

## Language

The original conversation with the user was conducted in Turkish, with
technical terms usually left in English (e.g. "airtime", "backoff",
"multipath"). The codebase, comments and documentation have since been
translated to English throughout. The user really runs and tests the code,
shares results/logs and reports real bugs — so this project is not "on
paper", it is actively tested by hand. In a later session the user will
likely either report a new real bug, move on to one of the "next steps" in
the list above, or move on to real sound-card/SDR integration.
