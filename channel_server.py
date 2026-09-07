"""
channel_server.py - "Kanal fiziği" simülatörü (GERÇEK SES SİNYALİ sürümü).

Önceki sürüm soyut bir "airtime formülü + olasılıksal paket kaybı"
kullanıyordu. Bu sürüm artık istemcilerin GERÇEKTEN modüle ettiği (bkz.
modem.py) ham ses örneklerini taşıyor ve üzerine GERÇEK kanal bozulmaları
uygulayabiliyor: AWGN gürültü (--snr) ve çoklu-yol yankısı (--multipath-*).
Aktarım süresi artık bir formülden değil, doğrudan örnek sayısından
(n_samples / SAMPLE_RATE) hesaplanıyor - çünkü artık gerçek bir sinyal var.

Protokol mantığının (master seçimi, ARQ, sohbet vb.) HİÇBİRİ burada YOK -
hepsi client.py'de. channel_server.py sadece: (1) yarı çift yönlü erişimi
zorunlu kılar, (2) sesi olduğu gibi (ya da bozularak) tüm istasyonlara
yayınlar. Gerçek bir SDR/telsize geçerken değişmesi gereken kısım büyük
ölçüde burasıdır.
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
        print(f"[KANAL {time.strftime('%H:%M:%S')}] {msg}")

    async def handle_client(self, reader, writer):
        callsign = None
        try:
            hello = await read_json(reader)
            if not hello or hello.get("cmd") != "HELLO":
                writer.close()
                return
            callsign = hello["callsign"]
            self.clients[callsign] = writer
            self.log(f"{callsign} bağlandı ({len(self.clients)} istasyon aktif)")

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
                self.log(f"{callsign} bağlantısı koptu ({len(self.clients)} istasyon aktif)")
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

        self.log(f"{src:8s} -> ALL      | ses | {n:6d} örnek | süre={duration:.2f}sn")

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
        """GERÇEK kanal bozulmalarını sese uygular: çoklu-yol yankısı ve/veya
        AWGN gürültü. Hiçbiri ayarlanmadıysa sinyal olduğu gibi geçer."""
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
    ap = argparse.ArgumentParser(description="NET kanal simülatörü (gerçek ses sürümü)")
    ap.add_argument("--port", type=int, default=6000)
    ap.add_argument("--snr", type=float, default=None,
                     help="AWGN gürültü seviyesi (dB). Verilmezse gürültü eklenmez.")
    ap.add_argument("--multipath-delay-ms", type=float, default=0.0,
                     help="çoklu-yol yankısının gecikmesi (ms). Koruma aralığı 8ms.")
    ap.add_argument("--multipath-gain", type=float, default=0.0,
                     help="yankının doğrudan sinyale göre kazancı (0-1 arası, ör. 0.3)")
    args = ap.parse_args()

    server = ChannelServer(snr_db=args.snr, multipath_delay_ms=args.multipath_delay_ms,
                            multipath_gain=args.multipath_gain)
    srv = await asyncio.start_server(server.handle_client, "127.0.0.1", args.port)
    server.log(f"dinleniyor: 127.0.0.1:{args.port}  "
               f"(snr={args.snr}, multipath={args.multipath_delay_ms}ms@{args.multipath_gain})")
    async with srv:
        await srv.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
