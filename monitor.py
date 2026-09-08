"""
monitor.py - A passive listener on the REAL audio of the NET channel.

The previous version played a symbolic "sonification" (made-up tones). This
version now plays the audio samples channel_server.py actually carries, which
the stations ACTUALLY modulate with modem.py, as-is - so if there is
noise/distortion on the channel you hear that too. It also (where possible)
decodes the audio with its own demodulator and logs who sent what as text;
if it cannot decode (low SNR, a collision, etc.) it says so plainly - just
like a real operator saying "I heard something but could not read it".

Because it is passive (it never sends a JOIN_REQUEST/BEACON) it does NOT
appear in the other stations' rosters.

Dependency: numpy (client.py/channel_server.py already require it).
For audio playback it uses `aplay`/`paplay` on Linux, `afplay` on Mac and
`winsound` on Windows - if none are present it silently gives just the text log.

Usage:
    python3 monitor.py --port 6000
    python3 monitor.py --port 6000 --quiet     (quiet, text log only)
"""
import asyncio
import argparse
import base64
import os
import platform
import queue
import shutil
import subprocess
import tempfile
import threading
import time
import wave

import numpy as np

from netproto import send_json, read_json
from modem import Modem, SAMPLE_RATE


# ------------------------------------------------------------------ #
# Audio playback (per operating system, no extra dependency)
# ------------------------------------------------------------------ #
_player_checked = False
_player_cmd = None


def _find_player():
    global _player_checked, _player_cmd
    if _player_checked:
        return _player_cmd
    _player_checked = True
    system = platform.system()
    if system == "Windows":
        _player_cmd = "winsound"
    elif system == "Darwin":
        _player_cmd = "afplay" if shutil.which("afplay") else None
    else:
        for p in ("paplay", "aplay"):
            if shutil.which(p):
                _player_cmd = p
                break
    return _player_cmd


def save_wav(samples: np.ndarray, path, sr=SAMPLE_RATE):
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(samples.astype(np.int16).tobytes())


def play_wav_blocking(path):
    player = _find_player()
    if player is None:
        return False
    try:
        if player == "winsound":
            import winsound
            winsound.PlaySound(path, winsound.SND_FILENAME)
        else:
            subprocess.run([player, path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return True
    except Exception:
        return False


class AudioQueue:
    """A worker thread that plays the incoming REAL audio in order (because
    the channel is already half-duplex, sequential playback reflects the real
    timing correctly)."""
    def __init__(self):
        self.q = queue.Queue()
        self.warned = False
        self._tmpdir = tempfile.mkdtemp(prefix="net_sim_audio_")
        threading.Thread(target=self._worker, daemon=True).start()

    def push(self, samples: np.ndarray):
        self.q.put(samples)

    def _worker(self):
        i = 0
        while True:
            samples = self.q.get()
            path = os.path.join(self._tmpdir, f"clip_{i % 8}.wav")
            i += 1
            save_wav(samples, path)
            ok = play_wav_blocking(path)
            if not ok and not self.warned:
                self.warned = True
                print("[MONITOR] No audio playback command found (aplay/paplay/afplay/winsound). "
                      "Continuing with the text log only.")


# ------------------------------------------------------------------ #
# Main loop
# ------------------------------------------------------------------ #
async def monitor_loop(host, port, name, quiet):
    reader, writer = await asyncio.open_connection(host, port)
    await send_json(writer, {"cmd": "HELLO", "callsign": name})
    print(f"[MONITOR {name}] connected to the channel ({host}:{port}) - listening only, "
          f"sends nothing (will not appear in the roster)")

    modem = Modem()
    audio = None if quiet else AudioQueue()

    while True:
        msg = await read_json(reader)
        if msg is None:
            print("[MONITOR] connection dropped")
            return
        if msg.get("type") != "RX_AUDIO":
            continue

        raw = base64.b64decode(msg["audio_b64"])
        samples = np.frombuffer(raw, dtype=np.int16)
        duration = len(samples) / SAMPLE_RATE
        ts = time.strftime("%H:%M:%S")

        payload = modem.demodulate(samples)
        if payload is not None:
            try:
                import json
                frame = json.loads(payload.decode("utf-8"))
                print(f"[MON {ts}] {frame.get('src','?'):8s} -> {frame.get('dst','ALL'):8s} | "
                      f"{frame.get('type','?'):12s} | {duration:.2f}s | decoded")
            except Exception:
                print(f"[MON {ts}] {duration:.2f}s | audio decoded but not JSON (corrupt?)")
        else:
            print(f"[MON {ts}] {duration:.2f}s | could not decode (low SNR / collision / noise)")

        if audio:
            audio.push(samples)


def main():
    ap = argparse.ArgumentParser(description="A listener on the real audio of the NET channel")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=6000)
    ap.add_argument("--name", default="MONITOR")
    ap.add_argument("--quiet", action="store_true", help="no audio playback, text log only")
    args = ap.parse_args()
    try:
        asyncio.run(monitor_loop(args.host, args.port, args.name, args.quiet))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
