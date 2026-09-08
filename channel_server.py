"""
channel_server.py - The "channel physics" simulator (REAL AUDIO SIGNAL version).

The previous version used an abstract "airtime formula + probabilistic packet
loss". This version now carries the raw audio samples the clients ACTUALLY
modulate (see modem.py) and can apply REAL channel impairments on top: AWGN
noise (--snr) and multipath echo (--multipath-*). The transmission duration is
no longer from a formula but straight from the sample count
(n_samples / SAMPLE_RATE) - because there is now a real signal.

NONE of the protocol logic (master election, ARQ, chat, etc.) is here - it is
all in client.py. channel_server.py only: (1) enforces half-duplex access,
(2) broadcasts the audio as-is (or distorted) to every station. This is
largely the part that would need to change when moving to a real SDR/radio.
"""
import asyncio
import argparse
import base64
import time

import numpy as np

from netproto import send_json, read_json
from modem import SAMPLE_RATE


class ChannelServer:
    def __init__(self, snr_db=None, multipath_delay_ms=0.0, multipath_gain=0.0):
        self.clients = {}          # callsign -> StreamWriter
        self.busy_until = 0.0
        self.lock = asyncio.Lock()
        self.snr_db = snr_db
        self.multipath_delay = int(multipath_delay_ms / 1000 * SAMPLE_RATE)
        self.multipath_gain = multipath_gain

    def log(self, msg):
        print(f"[CHAN {time.strftime('%H:%M:%S')}] {msg}")

    async def handle_client(self, reader, writer):
        callsign = None
        try:
            hello = await read_json(reader)
            if not hello or hello.get("cmd") != "HELLO":
                writer.close()
                return
            callsign = hello["callsign"]
            self.clients[callsign] = writer
            self.log(f"{callsign} joined ({len(self.clients)} active)")

            while True:
                msg = await read_json(reader)
                if msg is None:
                    break
                if msg.get("cmd") == "TRANSMIT_AUDIO":
                    await self.handle_transmit(callsign, msg)
        except (ConnectionResetError, asyncio.IncompleteReadError, OSError):
            pass
        finally:
            if callsign and self.clients.get(callsign) is writer:
                del self.clients[callsign]
                self.log(f"{callsign} left ({len(self.clients)} active)")
            try:
                writer.close()
            except Exception:
                pass

    async def handle_transmit(self, src, msg):
        raw = base64.b64decode(msg["audio_b64"])
        samples = np.frombuffer(raw, dtype=np.int16)
        n = len(samples)
        loop = asyncio.get_event_loop()
        now = loop.time()

        async with self.lock:
            if now < self.busy_until:
                writer = self.clients.get(src)
                if writer:
                    await send_json(writer, {
                        "type": "CHANNEL_BUSY",
                        "retry_after": round(self.busy_until - now, 3),
                    })
                return
            duration = n / SAMPLE_RATE
            self.busy_until = now + duration

        self.log(f"{src:8s} -> ALL      | audio | {n:6d} samples | {duration:.2f}s")

        writer = self.clients.get(src)
        if writer:
            await send_json(writer, {"type": "TX_GRANTED", "duration": duration})

        asyncio.create_task(self.deliver_after_delay(samples, duration))

    async def deliver_after_delay(self, samples, delay):
        await asyncio.sleep(delay)

        y = self._apply_channel(samples)
        b64 = base64.b64encode(y.tobytes()).decode("ascii")

        dead = []
        for callsign, writer in list(self.clients.items()):
            try:
                await send_json(writer, {"type": "RX_AUDIO", "audio_b64": b64})
            except Exception:
                dead.append(callsign)
        for c in dead:
            self.clients.pop(c, None)

    def _apply_channel(self, samples: np.ndarray) -> np.ndarray:
        """Applies the REAL channel impairments to the audio: multipath echo
        and/or AWGN noise. If none are configured the signal passes through
        unchanged."""
        x = samples.astype(float)

        if self.multipath_gain > 0 and self.multipath_delay > 0:
            echo = np.zeros_like(x)
            d = self.multipath_delay
            if d < len(x):
                echo[d:] = x[:-d]
            x = x + self.multipath_gain * echo

        if self.snr_db is not None:
            sig_power = np.mean(x ** 2) or 1.0
            noise_power = sig_power / (10 ** (self.snr_db / 10))
            noise = np.random.normal(0, np.sqrt(noise_power), len(x))
            x = x + noise

        return np.clip(x, -32768, 32767).astype(np.int16)


async def main():
    ap = argparse.ArgumentParser(description="NET channel simulator (real-audio version)")
    ap.add_argument("--port", type=int, default=6000)
    ap.add_argument("--snr", type=float, default=None,
                     help="AWGN level (dB). If omitted, no noise is added.")
    ap.add_argument("--multipath-delay-ms", type=float, default=0.0,
                     help="multipath echo delay (ms). The guard interval is 8 ms.")
    ap.add_argument("--multipath-gain", type=float, default=0.0,
                     help="echo gain relative to the direct signal (0-1, e.g. 0.3)")
    args = ap.parse_args()

    server = ChannelServer(snr_db=args.snr, multipath_delay_ms=args.multipath_delay_ms,
                            multipath_gain=args.multipath_gain)
    srv = await asyncio.start_server(server.handle_client, "127.0.0.1", args.port)
    server.log(f"listening on: 127.0.0.1:{args.port}  "
               f"(snr={args.snr}, multipath={args.multipath_delay_ms}ms@{args.multipath_gain})")
    async with srv:
        await srv.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
