# ATCHAT — What it is and what it can do

**ATCHAT is a digital mode for amateur radio.** It aims to be as easy to run as
SSTV, but much more robust, and to do things SSTV cannot: move **pictures and
files with error correction**, and host a **multi-station text net** — all
inside one ordinary **2.7 kHz SSB** channel.

It takes its cues from KG-STV, EasyPal and HSModem, and builds them into a
single coordinated on-air protocol.

> **Where it stands today.** ATCHAT is a full software design with a *real*
> working modem: it genuinely turns data into audio and decodes audio back into
> data, with real bit errors under real simulated noise. The "channel" is
> currently a local network link that carries that audio between stations; it is
> not yet connected to a sound card or an SDR. Everything below is implemented
> and tested in software.

---

## The one-paragraph pitch

Start the app. Your station finds the net on its own — no "who's the server?"
configuration. You type in a shared channel that everyone sees, or address one
station privately. You drop a photo in; it goes out block by block with a
checksum on every block, and if the far end misses a few blocks in a fade it
asks for **just those** and the picture still arrives pixel-perfect. While that
picture is transferring, **the chat keeps working** — ATCHAT deliberately leaves
gaps in the file stream so messages and the net heartbeat always get through. If
the link gets noisy, the sender **automatically shifts to a slower, tougher
modulation**. If a station — even the one coordinating the net — disappears,
another one **takes over automatically**, and a station that drops out
mid-transfer **resumes where it left off** when it comes back.

---

## Capabilities

### Multi-station net with zero configuration
- Stations discover each other automatically. The first one on frequency starts
  coordinating; there is no manual "master" setup.
- A live **roster** shows who is active, who has gone quiet ("lost"), and drops
  stations that stay gone.

### Self-healing coordination (multi-level failover)
- One station sends a periodic **beacon** that carries the roster.
- If it goes off the air, the **next station in line takes over automatically**
  — and if *that* one is also gone, the one after it does. The handover order is
  the same for everyone (derived from the roster), so it happens without
  negotiation, and it does **not** stop at a single designated backup.
- If two stations grab coordination at the same instant, a simple deterministic
  tie-break settles it (smaller callsign wins) and the other steps back.

### Public and private messaging, together
- **Broadcast chat:** everyone in the net sees it.
- **Directed (private) chat:** addressed to one callsign; only that station
  displays it.
- Text goes out in the most robust modulation, so short messages punch through
  conditions that would break a file transfer.

### Image and file transfer with ARQ
- Any file (image, document, data) is split into fixed **220-byte blocks**, each
  with its own checksum.
- After the sender finishes, the receiver replies with the **list of blocks it
  is still missing**. The sender resends **only those** — a fade never restarts
  the whole transfer.
- Delivery is **bit-exact**: the reassembled file is identical to the original,
  verified.
- Can be sent to **ALL** (a group image) or to **one callsign** (a private
  file).

### Chat keeps flowing during a file transfer
- A back-to-back block stream would hog the channel and choke everything else.
  ATCHAT builds in a **control window**: it pauses the file stream regularly so
  chat messages *and* the net heartbeat always have room.
- The window **widens automatically** when it detects contention — someone is
  trying to talk, or you have a message queued — then narrows again once things
  are quiet.

### Adaptive robustness
- Transfers start on the faster **QPSK** modulation.
- If an error-recovery round shows the link is losing too much (more than ~15 %
  of what was sent), the sender **drops to BPSK** — roughly 2/3 the speed but
  dramatically more noise-tolerant — for the rest of that transfer.
- The receiver needs no warning: every transmission announces its own modulation
  in the header, so a transfer can change gears mid-flight and still decode.

### Survive a dropout and resume
- A station can lose its link (a real dropout, or a deliberate `/drop`) and keep
  all its state.
- On reconnect it re-announces itself; the other side notices and **re-sends the
  blocks it still owed**. The transfer finishes the **missing tail only** —
  never a restart.
- A reconnecting station never tries to seize coordination while a beacon is
  still alive.

### Passive monitoring
- A **monitor** can listen to the channel without ever transmitting and without
  appearing in the roster.
- It shows the live **waterfall, spectrum and scope** of the on-air audio and a
  running **decode strip** ("TA1ABC → ALL | CHAT | decoded", or "undecoded" when
  it genuinely can't copy it).

### Two front ends
- A **cross-platform GUI** (Rust / egui) with tabs for the Channel, individual
  Stations, the whole NET at once, and the Monitor — plus real audio output so
  you can *hear* the channel.
- A **command-line client** with `/chat`, `/msg`, `/sendimage`, `/sendfile`,
  `/status`, `/drop`, `/reconnect`.

---

## A session, start to finish

1. **Three operators** open ATCHAT and connect to the channel.
2. Nobody hears a beacon, so after ~24 s the first station starts beaconing and
   becomes the net coordinator; the next station is marked as its backup. All
   three now share a roster.
3. **TA1ABC**: `net is open` → everyone sees it. **TA3GHI** replies privately to
   TA1ABC: `meet on 40 later?` — only TA1ABC sees that line.
4. **TA1ABC sends a group photo.** It streams out in blocks; every few blocks
   the channel opens briefly.
5. Mid-transfer, **TA2DEF** types `great pic` — it lands in one of those
   windows. ATCHAT notices the contention and opens the windows more often for a
   while.
6. **TA3GHI** had a bad few seconds and missed blocks 12, 19, 31. After the
   sender's "end", TA3GHI asks for exactly those three; they come again and the
   photo is complete and pixel-identical for all three stations.
7. **TA1ABC's laptop sleeps.** The beacon stops. ~24 s later **TA2DEF** (the
   backup) takes over coordination seamlessly.
8. **TA1ABC wakes up**, reconnects, re-announces. It does **not** try to grab
   coordination back — there's a live beacon. It rejoins as a normal station.

---

## Performance envelope (measured, not modelled)

| Situation | Behaviour |
|---|---|
| Clean channel | 100 % success, any size tested (1 B – 5 kB frames) |
| Moderate noise (≥ 18 dB SNR) | 100 % |
| Marginal (14–16 dB) | small hit; ARQ mops it up |
| Weak (10–12 dB) | QPSK falls off a cliff (normal for OFDM without FEC) — this is exactly where the automatic **BPSK downshift** rescues the transfer |
| At 10 dB, head-to-head | QPSK 0/20 vs **BPSK 19/20** |
| Mild multipath (echo within the 8 ms guard) | rides through |
| Strong / long multipath | breaks on purpose — there is no equaliser yet |
| Raw throughput | ≈ **475 B/s** at QPSK, ≈ **237 B/s** at BPSK, minus the control-window overhead that keeps chat alive |

---

## The pieces

| Piece | What it does |
|---|---|
| **Modem** | Real OFDM: 8 kHz audio, 256-point FFT, 77 subcarriers in ~312–2688 Hz, 8 ms guard. Preamble + header + data symbols, CRC-32 per frame, no FEC. |
| **Channel** | Carries the audio, enforces one-transmitter-at-a-time, can add real AWGN and multipath. Holds **no** protocol logic. |
| **Client (station)** | All the intelligence: listen-before-talk, election & failover, roster, chat, file transfer + ARQ, drop/resume. |
| **Monitor** | Listens only. Scope + spectrum + waterfall + decode strip. |

A reference implementation in **Python** and a full **Rust** port track each
other bit-for-bit.

---

## What's next

In rough order:

- **Forward error correction** (LDPC/RS) so weak-signal frames self-heal instead
  of leaning entirely on ARQ.
- **Channel estimation / equaliser** so real-world multipath beyond the guard
  interval stops being a hard wall — this also unlocks true per-subcarrier
  adaptive bit loading.
- **Sound-card audio** (`sounddevice` / `cpal`) so two radios can pass ATCHAT
  over a real SSB link on the bench.
- **SDR / RF integration.** By design this should touch only the channel layer;
  the modem and the protocol stay as they are.

---

## Getting it

Prebuilt GUI and channel binaries for **Windows (MSI + zip)**, **macOS**
(Intel + Apple Silicon) and **Linux** are published on the
[GitHub Releases page](https://github.com/barisdinc/AtChat/releases).

To run from source you only need `numpy` for the Python side, or a Rust
toolchain for the `rust/` workspace. See `README.md` for the exact commands and
`PROTOCOL.md` for the full technical specification.
