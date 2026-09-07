"""
monitor.py - NET kanalındaki GERÇEK sesi dinleyen pasif bir izleyici.

Önceki sürüm sembolik "sonifikasyon" (uydurma tonlar) çalıyordu. Bu sürüm
artık channel_server.py'nin gerçekten taşıdığı, istasyonların modem.py ile
GERÇEKTEN modüle ettiği ses örneklerini olduğu gibi çalar - yani kanalda
oluşan gürültü/bozulma varsa onu da duyarsınız. Ayrıca (mümkünse) sesi
kendi demodülatörüyle çözüp kimin ne gönderdiğini metin olarak da loglar;
çözemezse (düşük SNR, çakışma vb.) bunu da açıkça belirtir - tıpkı gerçek
bir operatörün "bir şey duydum ama çözemedim" demesi gibi.

Pasif olduğu için (hiç JOIN_REQUEST/BEACON göndermediği için) diğer
istasyonların roster'ında GÖRÜNMEZ.

Bağımlılık: numpy (client.py/channel_server.py zaten gerektiriyor).
Ses çalma için Linux'ta `aplay`/`paplay`, Mac'te `afplay`, Windows'ta
`winsound` kullanılır - hiçbiri yoksa sessizce sadece metin günlüğü verir.

Kullanım:
    python3 monitor.py --port 6000
    python3 monitor.py --port 6000 --quiet     (sessiz, sadece metin günlüğü)
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
# Ses çalma (işletim sistemine göre, ek bağımlılık yok)
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
    """Gelen GERÇEK sesi sırayla çalan işçi thread (kanal zaten yarı çift
    yönlü olduğu için sıralı çalma gerçek zamanlamayı doğru yansıtır)."""
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
                print("[DİNLEYİCİ] Ses çalma komutu bulunamadı (aplay/paplay/afplay/winsound). "
                      "Sadece metin günlüğü ile devam ediliyor.")


# ------------------------------------------------------------------ #
# Ana döngü
# ------------------------------------------------------------------ #
async def monitor_loop(host, port, name, quiet):
    reader, writer = await asyncio.open_connection(host, port)
    await send_json(writer, {"cmd": "HELLO", "callsign": name})
    print(f"[DİNLEYİCİ {name}] kanala bağlanıldı ({host}:{port}) - sadece dinliyor, "
          f"hiçbir şey göndermiyor (roster'da görünmeyecek)")

    modem = Modem()
    audio = None if quiet else AudioQueue()

    while True:
        msg = await read_json(reader)
        if msg is None:
            print("[DİNLEYİCİ] bağlantı koptu")
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
                print(f"[DİNLE {ts}] {frame.get('src','?'):8s} -> {frame.get('dst','ALL'):8s} | "
                      f"{frame.get('type','?'):12s} | {duration:.2f}sn | çözüldü")
            except Exception:
                print(f"[DİNLE {ts}] {duration:.2f}sn | ses çözüldü ama JSON değil (bozuk?)")
        else:
            print(f"[DİNLE {ts}] {duration:.2f}sn | çözülemedi (düşük SNR / çakışma / gürültü)")

        if audio:
            audio.push(samples)


def main():
    ap = argparse.ArgumentParser(description="NET kanalındaki gerçek sesi dinleyen izleyici")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=6000)
    ap.add_argument("--name", default="DINLEYICI")
    ap.add_argument("--quiet", action="store_true", help="ses çalma, sadece metin günlüğü")
    args = ap.parse_args()
    try:
        asyncio.run(monitor_loop(args.host, args.port, args.name, args.quiet))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
