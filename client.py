"""
client.py - NET istasyonu (protokol istemcisi).

channel_server.py'ye bağlanır ve tasarladığımız NET protokolünü uygular:
  - LBT + backoff ile kanal erişimi ("PTT")
  - Dinamik master seçimi / yedek master / failover
  - Roster (kim aktif, kim kayıp)
  - Ortak (broadcast) ve özel (unicast) sohbet
  - Blok + CRC + ARQ ile görüntü/dosya transferi
  - Ani kopma (/drop) ve yeniden bağlanma (/reconnect) ile devam edebilme

Kullanım:
    python client.py TA1ABC
    python client.py TA2DEF --port 6000 --mode QPSK
"""
import asyncio
import argparse
import os
import sys
import time
import uuid
import random
import threading
import queue
import base64
import json

import numpy as np

from netproto import (send_json, read_json, crc32, b64e, b64d,
                       BEACON_INTERVAL, BEACON_TIMEOUT, LOST_TIMEOUT,
                       REMOVE_TIMEOUT, BLOCK_SIZE)
from modem import Modem, SAMPLE_RATE


def jitter():
    return random.uniform(0.05, 0.3)


class TransferOut:
    """Bu istasyonun gönderdiği bir bulk transfer (görüntü/dosya)."""
    def __init__(self, transfer_id, filename, dst, blocks, mode):
        self.transfer_id = transfer_id
        self.filename = filename
        self.dst = dst
        self.blocks = blocks  # {seq: bytes}
        self.mode = mode


class TransferIn:
    """Bu istasyonun aldığı bir bulk transfer."""
    def __init__(self, transfer_id, filename, total_blocks, src, dst):
        self.transfer_id = transfer_id
        self.filename = filename
        self.total_blocks = total_blocks
        self.src = src
        self.dst = dst
        self.received = {}  # {seq: bytes}
        self.complete = False


class Client:
    def __init__(self, callsign, host, port, mode="QPSK"):
        self.callsign = callsign
        self.host = host
        self.port = port
        self.mode = mode

        self.reader = None
        self.writer = None
        self.connected = False

        self.role = "LISTENER"        # LISTENER | MASTER | BACKUP
        self.master = None
        self.backup = None
        self.last_beacon_time = 0.0
        self.roster = {}              # callsign -> {"last_seen": ts, "status": ...}

        self.transfers_out = {}       # transfer_id -> TransferOut
        self.transfers_in = {}        # transfer_id -> TransferIn
        self.status_waiters = {}      # transfer_id -> asyncio.Future (aktif ARQ döngüsü)

        self.tx_lock = asyncio.Lock()  # bu istasyonun "PTT"si: aynı anda tek çerçeve
        self.tx_reply_waiter = None    # TRANSMIT yanıtını (TX_GRANTED/CHANNEL_BUSY) taşır
        self.modem = Modem()           # gerçek OFDM modülatör/demodülatör
        self.stdin_q = queue.Queue()

        os.makedirs("received", exist_ok=True)

    # ------------------------------------------------------------------ #
    # Bağlantı yönetimi
    # ------------------------------------------------------------------ #
    async def connect(self):
        self.reader, self.writer = await asyncio.open_connection(self.host, self.port)
        await send_json(self.writer, {"cmd": "HELLO", "callsign": self.callsign})
        self.connected = True
        self.last_beacon_time = time.time()  # zaman aşımı sayacı burada başlar
        self.log(f"kanala bağlanıldı ({self.host}:{self.port})")

    async def drop(self):
        """Ani kopmayı simüle et: TCP bağlantısı kapanır, süreç/durum canlı kalır."""
        self.connected = False
        try:
            self.writer.close()
        except Exception:
            pass
        self.log("!! bağlantı koptu (simüle edildi) - durum korunuyor, /reconnect ile dönebilirsiniz")

    async def reconnect(self):
        if self.connected:
            self.log("zaten bağlı")
            return
        await self.connect()
        # Rejoin kuralı: aktif bir master beacon'ı duyulursa asla kendini master ilan etme.
        self.role = "LISTENER"
        await self.send_frame({"type": "JOIN_REQUEST", "src": self.callsign, "dst": "ALL"})
        self._resume_pending_receives()

    def _resume_pending_receives(self):
        """Alıcı tarafı: yarım kalan transferler için eksik blokları tekrar iste."""
        for t in self.transfers_in.values():
            if not t.complete:
                missing = self._missing_blocks(t)
                if missing:
                    self.log(f"[{t.transfer_id}] geri döndük, {t.src}'den "
                             f"{len(missing)} eksik blok isteniyor")
                    asyncio.create_task(self.send_frame({
                        "type": "BULK_STATUS", "src": self.callsign, "dst": t.src,
                        "transfer_id": t.transfer_id, "missing": missing,
                    }))

    # ------------------------------------------------------------------ #
    # Düşük seviye gönderim (LBT + backoff = kanal erişim mantığı)
    # ------------------------------------------------------------------ #
    async def send_frame(self, frame: dict, mode=None):
        if not self.connected:
            return False
        mode = mode or self.mode

        # ---- GERÇEK MODÜLASYON: JSON çerçeve -> ses örnekleri ----
        payload_bytes = json.dumps(frame, ensure_ascii=False).encode("utf-8")
        samples = self.modem.modulate(payload_bytes, mode)
        audio_b64 = base64.b64encode(samples.tobytes()).decode("ascii")

        backoff = 0.2
        for _ in range(40):
            if not self.connected:
                return False
            async with self.tx_lock:
                loop = asyncio.get_event_loop()
                self.tx_reply_waiter = loop.create_future()
                try:
                    await send_json(self.writer, {"cmd": "TRANSMIT_AUDIO",
                                                    "audio_b64": audio_b64})
                    reply = await asyncio.wait_for(self.tx_reply_waiter, timeout=8.0)
                except Exception as e:
                    self.connected = False
                    self.log(f"!! gönderim başarısız, bağlantı kopmuş sayılıyor ({e})")
                    return False

            if reply.get("type") == "TX_GRANTED":
                await asyncio.sleep(reply["duration"])  # gerçek "havada kalma" süresi
                return True
            elif reply.get("type") == "CHANNEL_BUSY":
                wait = reply.get("retry_after", backoff)
                await asyncio.sleep(wait + jitter())
                backoff = min(backoff * 1.7, 3.0)
                continue
        self.log("!! kanal sürekli meşgul, gönderim vazgeçildi")
        return False

    # ------------------------------------------------------------------ #
    # Alım döngüsü (TEK okuyucu - hem TRANSMIT yanıtlarını hem RX_AUDIO'yu dağıtır)
    # ------------------------------------------------------------------ #
    async def receive_loop(self):
        while True:
            if not self.connected:
                await asyncio.sleep(0.5)
                continue
            try:
                msg = await read_json(self.reader)
            except Exception:
                msg = None
            if msg is None:
                self.connected = False
                continue

            mtype = msg.get("type")
            if mtype in ("TX_GRANTED", "CHANNEL_BUSY"):
                if self.tx_reply_waiter and not self.tx_reply_waiter.done():
                    self.tx_reply_waiter.set_result(msg)
            elif mtype == "RX_AUDIO":
                # ---- GERÇEK DEMODÜLASYON: ses örnekleri -> JSON çerçeve ----
                raw = base64.b64decode(msg["audio_b64"])
                samples = np.frombuffer(raw, dtype=np.int16)
                payload_bytes = self.modem.demodulate(samples)
                if payload_bytes is None:
                    continue  # çözülemedi - gerçek radyoda da "duyulmamış" sayılır
                try:
                    frame = json.loads(payload_bytes.decode("utf-8"))
                except Exception:
                    continue
                await self.handle_frame(frame)

    async def handle_frame(self, frame):
        ftype = frame.get("type")
        src = frame.get("src")
        dst = frame.get("dst", "ALL")
        is_self = (src == self.callsign)

        if not is_self:
            self._touch_roster(src)

        if ftype == "BEACON":
            await self.on_beacon(frame)
        elif ftype == "JOIN_REQUEST" and not is_self:
            if self.role == "MASTER":
                self.log(f"{src} net'e katıldı")
            # Bu istasyon daha önce bize bir şey gönderiyorken kaybolduysa,
            # rejoin anında yarım kalan transferler için eksik blokları iste.
            for t in self.transfers_in.values():
                if t.src == src and not t.complete:
                    missing = self._missing_blocks(t)
                    if missing:
                        self.log(f"[{t.transfer_id}] {src} geri döndü, "
                                 f"{len(missing)} eksik blok isteniyor")
                        asyncio.create_task(self.send_frame({
                            "type": "BULK_STATUS", "src": self.callsign, "dst": src,
                            "transfer_id": t.transfer_id, "missing": missing,
                        }))
        elif ftype == "CHAT" and not is_self and dst in ("ALL", self.callsign):
            tag = "herkese" if dst == "ALL" else "özel"
            print(f"\n[SOHBET/{tag}] {src}: {frame['text']}")
        elif ftype == "BULK_META" and not is_self and dst in ("ALL", self.callsign):
            self.on_bulk_meta(frame)
        elif ftype == "BULK_BLOCK" and dst in ("ALL", self.callsign):
            self.on_bulk_block(frame)
        elif ftype == "BULK_END" and dst in ("ALL", self.callsign):
            await self.on_bulk_end(frame)
        elif ftype == "BULK_STATUS" and dst == self.callsign:
            await self.on_bulk_status(frame)

    # ------------------------------------------------------------------ #
    # Roster
    # ------------------------------------------------------------------ #
    def _touch_roster(self, callsign):
        was_lost = self.roster.get(callsign, {}).get("status") == "lost"
        self.roster[callsign] = {"last_seen": time.time(), "status": "active"}
        if was_lost:
            self.log(f"{callsign} yeniden görünür oldu")
            for t in self.transfers_in.values():
                if t.src == callsign and not t.complete:
                    missing = self._missing_blocks(t)
                    if missing:
                        asyncio.create_task(self.send_frame({
                            "type": "BULK_STATUS", "src": self.callsign, "dst": callsign,
                            "transfer_id": t.transfer_id, "missing": missing,
                        }))

    def _age_roster(self):
        now = time.time()
        for c, info in list(self.roster.items()):
            if now - info["last_seen"] > REMOVE_TIMEOUT:
                del self.roster[c]
            elif now - info["last_seen"] > LOST_TIMEOUT and info["status"] != "lost":
                info["status"] = "lost"
                self.log(f"{c} kayıp olarak işaretlendi")

    # ------------------------------------------------------------------ #
    # Master seçimi / beacon / failover
    # ------------------------------------------------------------------ #
    async def on_beacon(self, frame):
        src = frame["src"]
        now = time.time()

        if self.role == "MASTER" and src != self.callsign:
            # Nadir durum: iki istasyon aynı anda master oldu. Basit tie-break:
            # alfabetik olarak küçük çağrı işareti kazanır, diğeri geri çekilir.
            if src < self.callsign:
                self.log(f"master çakışması: {src} devam ediyor, geri çekiliyorum")
                self.role = "LISTENER"
            else:
                return

        self.last_beacon_time = now
        self.master = src
        self.backup = frame.get("backup")
        for c in frame.get("roster", []):
            if c != self.callsign:
                self.roster.setdefault(c, {"last_seen": now, "status": "active"})
                self.roster[c]["last_seen"] = now
                self.roster[c]["status"] = "active"

        if self.role != "MASTER":
            self.role = "BACKUP" if self.backup == self.callsign else "LISTENER"

    async def master_watchdog(self):
        while True:
            await asyncio.sleep(1.0)
            if not self.connected:
                continue
            self._age_roster()
            now = time.time()

            if self.role != "MASTER" and (now - self.last_beacon_time) > BEACON_TIMEOUT:
                # Sadece "ilk gelen" (master hiç görülmedi) ya da atanmış yedek devralır.
                if self.role == "BACKUP" or self.master is None:
                    self.log("beacon zaman aşımına uğradı -> master rolü alınıyor")
                    self.role = "MASTER"
                    self.last_beacon_time = now
                    await self.send_beacon()

    async def beacon_loop(self):
        while True:
            await asyncio.sleep(BEACON_INTERVAL)
            if self.role == "MASTER" and self.connected:
                await self.send_beacon()

    def _pick_backup(self):
        active = [c for c, i in self.roster.items()
                  if i["status"] == "active" and c != self.callsign]
        return sorted(active)[0] if active else None

    async def send_beacon(self):
        self.backup = self._pick_backup()
        roster_list = [self.callsign] + list(self.roster.keys())
        await self.send_frame({
            "type": "BEACON", "src": self.callsign, "dst": "ALL",
            "backup": self.backup, "roster": roster_list,
        }, mode="BPSK")

    # ------------------------------------------------------------------ #
    # Sohbet
    # ------------------------------------------------------------------ #
    async def chat(self, text, dst="ALL"):
        return await self.send_frame({"type": "CHAT", "src": self.callsign, "dst": dst, "text": text},
                                      mode="BPSK")

    # ------------------------------------------------------------------ #
    # Bulk transfer - gönderim
    # ------------------------------------------------------------------ #
    async def send_bulk(self, path, dst="ALL"):
        if not os.path.exists(path):
            self.log(f"dosya bulunamadı: {path}")
            return
        data = open(path, "rb").read()
        n_blocks = max(1, (len(data) + BLOCK_SIZE - 1) // BLOCK_SIZE)
        blocks = {i: data[i * BLOCK_SIZE:(i + 1) * BLOCK_SIZE] for i in range(n_blocks)}
        transfer_id = uuid.uuid4().hex[:8]
        t = TransferOut(transfer_id, os.path.basename(path), dst, blocks, self.mode)
        self.transfers_out[transfer_id] = t

        self.log(f"[{transfer_id}] {t.filename} -> {dst} başlıyor "
                 f"({len(data)} B, {n_blocks} blok, {self.mode})")

        if not await self.send_frame({
            "type": "BULK_META", "src": self.callsign, "dst": dst,
            "transfer_id": transfer_id, "filename": t.filename,
            "total_blocks": n_blocks, "total_size": len(data),
        }):
            self.log(f"[{transfer_id}] başlatılamadı (bağlantı yok)")
            return

        if not await self._send_blocks(t, list(blocks.keys())):
            self.log(f"[{transfer_id}] bağlantı koptu, transfer askıda kaldı")
            return
        if not await self.send_frame({"type": "BULK_END", "src": self.callsign, "dst": dst,
                                       "transfer_id": transfer_id}):
            return

        for round_no in range(6):
            missing = await self._wait_for_status(transfer_id, timeout=6.0)
            if not self.connected:
                self.log(f"[{transfer_id}] bağlantı koptu, transfer askıda kaldı")
                return
            if missing is None:
                self.log(f"[{transfer_id}] durum yanıtı gelmedi (tur {round_no + 1})")
                continue
            if not missing:
                self.log(f"[{transfer_id}] tamamlandı (tur {round_no + 1})")
                return
            self.log(f"[{transfer_id}] {len(missing)} blok yeniden gönderiliyor (tur {round_no + 1})")
            if not await self._send_blocks(t, missing):
                return
            await self.send_frame({"type": "BULK_END", "src": self.callsign, "dst": dst,
                                    "transfer_id": transfer_id})
        self.log(f"[{transfer_id}] azami tur sayısına ulaşıldı, alıcı geri dönerse otomatik devam edecek")

    # Her kaç blokta bir kanalı bilinçli olarak boşaltacağız (sohbet/kontrol
    # mesajlarının VE BEACON'LARIN araya girebilmesi için) - tasarım
    # sohbetimizdeki "kontrol penceresi" fikrinin gerçek karşılığı.
    #
    # Neden bu kadar sık ve bu kadar uzun: rakip bir istasyonun yeniden
    # deneme zamanlaması sunucudan gelen retry_after değerine göre "mevcut
    # bloğun bitişine" senkronize olur, pencerenin TAM olarak ne zaman
    # açılacağını BİLEMEZ. Pencere kısa/seyrekse (önceki sürüm: her 6
    # blokta 0.5sn), bir deneme penceреyi büyük ihtimalle KAÇIRIR - hatta
    # beacon'lar bile kaçırabilir (gördüğümüz gerçek sorun tam buydu: iki
    # istasyon da birbirinin beacon'ını 45 bloklu bir transfer boyunca
    # yeterince sık duyamayıp ikisi de kendini master ilan etti). Pencereyi
    # sıklaştırıp genişleterek yakalama olasılığını pratikte neredeyse
    # kesinleştiriyoruz - bunun bedeli toplam verimde ~%25-30'luk bir düşüş,
    # ama bu tam olarak orijinal tasarım hedefimizdi: ham hız değil,
    # "arada mesaj alıp verebilme" garantisi.
    CONTROL_WINDOW_EVERY = 3
    CONTROL_WINDOW_PAUSE = 1.2  # sn

    async def _send_blocks(self, t: TransferOut, seqs):
        for i, seq in enumerate(seqs):
            if not self.connected:
                return False
            data = t.blocks[seq]
            ok = await self.send_frame({
                "type": "BULK_BLOCK", "src": self.callsign, "dst": t.dst,
                "transfer_id": t.transfer_id, "seq": seq,
                "data": b64e(data), "crc": crc32(data),
            }, mode=t.mode)
            if not ok:
                return False
            if (i + 1) % self.CONTROL_WINDOW_EVERY == 0:
                await asyncio.sleep(self.CONTROL_WINDOW_PAUSE)
        return True

    async def _wait_for_status(self, transfer_id, timeout):
        fut = asyncio.get_event_loop().create_future()
        self.status_waiters[transfer_id] = fut
        try:
            return await asyncio.wait_for(fut, timeout=timeout)
        except asyncio.TimeoutError:
            return None
        finally:
            self.status_waiters.pop(transfer_id, None)

    async def on_bulk_status(self, frame):
        tid = frame["transfer_id"]
        missing = frame["missing"]

        fut = self.status_waiters.get(tid)
        if fut and not fut.done():
            fut.set_result(missing)
            return

        # Aktif bir ARQ döngüsü yok (ör. daha önce bağlantı koparak çıkılmıştı)
        # ama elimizde hâlâ bloklar var -> gecikmeli "devam et" isteğini karşıla.
        # Burada da aynı sebeple (receive_loop içindeyiz) arka plan görevi kullanıyoruz.
        t = self.transfers_out.get(tid)
        if t and missing:
            self.log(f"[{tid}] gecikmeli devam isteği: {len(missing)} blok yeniden gönderiliyor")
            asyncio.create_task(self._resend_missing(t, missing))

    async def _resend_missing(self, t: TransferOut, missing):
        if await self._send_blocks(t, missing):
            await self.send_frame({"type": "BULK_END", "src": self.callsign, "dst": t.dst,
                                    "transfer_id": t.transfer_id})

    # ------------------------------------------------------------------ #
    # Bulk transfer - alım
    # ------------------------------------------------------------------ #
    def on_bulk_meta(self, frame):
        tid = frame["transfer_id"]
        t = TransferIn(tid, frame["filename"], frame["total_blocks"], frame["src"], frame["dst"])
        self.transfers_in[tid] = t
        self.log(f"[{tid}] {frame['src']} bir transfer başlattı: {t.filename} "
                 f"({t.total_blocks} blok)")

    def on_bulk_block(self, frame):
        t = self.transfers_in.get(frame["transfer_id"])
        if not t or t.complete:
            return
        data = b64d(frame["data"])
        if crc32(data) != frame["crc"]:
            return  # CRC hatası -> yok sayılır, ARQ ile yeniden istenecek
        t.received[frame["seq"]] = data

    def _missing_blocks(self, t: TransferIn):
        return [s for s in range(t.total_blocks) if s not in t.received]

    async def on_bulk_end(self, frame):
        t = self.transfers_in.get(frame["transfer_id"])
        if not t or t.complete:
            return
        missing = self._missing_blocks(t)
        if missing:
            self.log(f"[{t.transfer_id}] {len(missing)}/{t.total_blocks} blok eksik, isteniyor")
        else:
            self._save_transfer(t)
        # ÖNEMLİ: receive_loop içinden çağrıldığımız için send_frame'i burada
        # AWAIT ETMEYİZ (kendi yanıtını yine receive_loop bekleyeceğinden
        # kilitlenme olurdu) - arka plan görevi olarak fırlatıyoruz.
        asyncio.create_task(self.send_frame({
            "type": "BULK_STATUS", "src": self.callsign, "dst": frame["src"],
            "transfer_id": t.transfer_id, "missing": missing,
        }, mode="BPSK"))

    def _save_transfer(self, t: TransferIn):
        t.complete = True
        out_path = os.path.join("received", f"{t.transfer_id}_{t.filename}")
        with open(out_path, "wb") as f:
            for seq in range(t.total_blocks):
                f.write(t.received[seq])
        self.log(f"[{t.transfer_id}] tamamlandı -> {out_path}")

    # ------------------------------------------------------------------ #
    # CLI
    # ------------------------------------------------------------------ #
    def log(self, msg):
        print(f"[{self.callsign} {time.strftime('%H:%M:%S')}] {msg}")

    def status(self):
        print(f"--- {self.callsign} | rol={self.role} | master={self.master} | "
              f"backup={self.backup} | bağlı={self.connected} ---")
        for c, i in self.roster.items():
            age = time.time() - i["last_seen"]
            print(f"  {c}: {i['status']} (son görülme {age:.0f}sn önce)")
        for tid, t in self.transfers_in.items():
            state = "tamam" if t.complete else f"{len(t.received)}/{t.total_blocks}"
            print(f"  gelen  [{tid}] {t.filename} ({state})")
        for tid, t in self.transfers_out.items():
            print(f"  giden  [{tid}] {t.filename} -> {t.dst}")

    def _stdin_reader_thread(self):
        for line in sys.stdin:
            self.stdin_q.put(line.rstrip("\n"))

    async def cli_loop(self):
        threading.Thread(target=self._stdin_reader_thread, daemon=True).start()
        print("Komutlar:")
        print("  /chat <mesaj>                 - herkese sohbet mesajı")
        print("  /msg <ÇAĞRI> <mesaj>           - özel mesaj")
        print("  /sendimage <yol> [ÇAĞRI|ALL]   - görüntü/veri gönder (varsayılan: ALL)")
        print("  /sendfile <yol> <ÇAĞRI>        - dosya gönder (özel)")
        print("  /status                        - rol, roster, transfer durumu")
        print("  /drop                          - ani kopmayı simüle et")
        print("  /reconnect                     - yeniden bağlan, kaldığı yerden devam")
        print("  /quit                          - çık")
        while True:
            try:
                line = self.stdin_q.get_nowait()
            except queue.Empty:
                await asyncio.sleep(0.1)
                continue
            # Uzun süren komutları (transfer) arka planda çalıştır ki
            # /drop gibi komutlar transfer sürerken de işlenebilsin.
            asyncio.create_task(self._handle_command(line))

    async def _handle_command(self, line):
        if not line:
            return
        parts = line.split(" ", 2)
        cmd = parts[0]
        try:
            if cmd == "/chat" and len(parts) > 1:
                await self.chat(" ".join(parts[1:]))
            elif cmd == "/msg" and len(parts) > 2:
                await self.chat(parts[2], dst=parts[1].upper())
            elif cmd == "/sendimage" and len(parts) > 1:
                dst = parts[2].upper() if len(parts) > 2 else "ALL"
                await self.send_bulk(parts[1], dst)
            elif cmd == "/sendfile" and len(parts) > 2:
                await self.send_bulk(parts[1], parts[2].upper())
            elif cmd == "/status":
                self.status()
            elif cmd == "/drop":
                await self.drop()
            elif cmd == "/reconnect":
                await self.reconnect()
            elif cmd == "/quit":
                os._exit(0)
            else:
                print("bilinmeyen komut ya da eksik parametre")
        except Exception as e:
            self.log(f"hata: {e}")


async def main():
    ap = argparse.ArgumentParser(description="NET istasyon istemcisi")
    ap.add_argument("callsign")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=6000)
    ap.add_argument("--mode", default="QPSK", choices=["BPSK", "QPSK", "16QAM"])
    args = ap.parse_args()

    client = Client(args.callsign.upper(), args.host, args.port, args.mode)
    await client.connect()

    # ÖNEMLİ: arka plan görevlerini (özellikle receive_loop) JOIN_REQUEST'ten
    # ÖNCE başlatıyoruz. send_frame, sunucudan gelecek TX_GRANTED yanıtını
    # receive_loop'un okumasına güvenir - receive_loop henüz çalışmıyorsa
    # ilk gönderim sessizce zaman aşımına uğrar ve istemci kendini yanlışlıkla
    # "bağlantı yok" sanır (tüm sonraki komutlar da sessizce başarısız olur).
    tasks = [
        asyncio.create_task(client.receive_loop()),
        asyncio.create_task(client.master_watchdog()),
        asyncio.create_task(client.beacon_loop()),
        asyncio.create_task(client.cli_loop()),
    ]
    await asyncio.sleep(0.1)  # arka plan görevlerinin ilk çalışma turunu alması için

    await client.send_frame({"type": "JOIN_REQUEST", "src": client.callsign, "dst": "ALL"})

    await asyncio.gather(*tasks)


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
