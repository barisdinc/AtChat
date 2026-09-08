"""
client.py - A NET station (the protocol client).

Connects to channel_server.py and implements the NET protocol we designed:
  - Channel access via LBT + backoff ("PTT")
  - Dynamic master election / backup master / failover
  - Roster (who is active, who is lost)
  - Common (broadcast) and directed (unicast) chat
  - Image/file transfer via block + CRC + ARQ
  - The ability to resume after a sudden drop (/drop) and reconnect (/reconnect)

Usage:
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
    """A bulk transfer this station is sending (image/file)."""
    def __init__(self, transfer_id, filename, dst, blocks, mode):
        self.transfer_id = transfer_id
        self.filename = filename
        self.dst = dst
        self.blocks = blocks  # {seq: bytes}
        self.mode = mode


class TransferIn:
    """A bulk transfer this station is receiving."""
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
        self.status_waiters = {}      # transfer_id -> asyncio.Future (an active ARQ loop)

        self.tx_lock = asyncio.Lock()  # this station's "PTT": one frame at a time
        self.tx_reply_waiter = None    # carries the TRANSMIT reply (TX_GRANTED/CHANNEL_BUSY)
        self.modem = Modem()           # the real OFDM modulator/demodulator
        self.stdin_q = queue.Queue()

        # Adaptive control window (see _control_window / _send_blocks):
        self._chat_waiting = 0         # local chat/msg commands queued to go out
        self._foreign_ctrl_ts = 0.0    # last time another station sent a control frame

        os.makedirs("received", exist_ok=True)

    # ------------------------------------------------------------------ #
    # Connection management
    # ------------------------------------------------------------------ #
    async def connect(self):
        self.reader, self.writer = await asyncio.open_connection(self.host, self.port)
        await send_json(self.writer, {"cmd": "HELLO", "callsign": self.callsign})
        self.connected = True
        self.last_beacon_time = time.time()  # the timeout counter starts here
        self.log(f"connected to the channel ({self.host}:{self.port})")

    async def drop(self):
        """Simulate a sudden drop: the TCP connection closes, the process/state stays alive."""
        self.connected = False
        try:
            self.writer.close()
        except Exception:
            pass
        self.log("!! link dropped (simulated) - state is preserved, you can come back with /reconnect")

    async def reconnect(self):
        if self.connected:
            self.log("already connected")
            return
        await self.connect()
        # Rejoin rule: never declare yourself master if an active master beacon is heard.
        self.role = "LISTENER"
        await self.send_frame({"type": "JOIN_REQUEST", "src": self.callsign, "dst": "ALL"})
        self._resume_pending_receives()

    def _resume_pending_receives(self):
        """Receiver side: re-request the missing blocks for half-finished transfers."""
        for t in self.transfers_in.values():
            if not t.complete:
                missing = self._missing_blocks(t)
                if missing:
                    self.log(f"[{t.transfer_id}] we are back, requesting "
                             f"{len(missing)} missing blocks from {t.src}")
                    asyncio.create_task(self.send_frame({
                        "type": "BULK_STATUS", "src": self.callsign, "dst": t.src,
                        "transfer_id": t.transfer_id, "missing": missing,
                    }))

    # ------------------------------------------------------------------ #
    # Low-level send (LBT + backoff = the channel-access logic)
    # ------------------------------------------------------------------ #
    async def send_frame(self, frame: dict, mode=None):
        if not self.connected:
            return False
        mode = mode or self.mode

        # ---- REAL MODULATION: JSON frame -> audio samples ----
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
                    self.log(f"!! send failed, the link is treated as down ({e})")
                    return False

            if reply.get("type") == "TX_GRANTED":
                await asyncio.sleep(reply["duration"])  # the real "time on air"
                return True
            elif reply.get("type") == "CHANNEL_BUSY":
                wait = reply.get("retry_after", backoff)
                await asyncio.sleep(wait + jitter())
                backoff = min(backoff * 1.7, 3.0)
                continue
        self.log("!! the channel stayed busy, the send was given up")
        return False

    # ------------------------------------------------------------------ #
    # Receive loop (the SINGLE reader - dispatches both TRANSMIT replies and RX_AUDIO)
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
                # ---- REAL DEMODULATION: audio samples -> JSON frame ----
                raw = base64.b64decode(msg["audio_b64"])
                samples = np.frombuffer(raw, dtype=np.int16)
                payload_bytes = self.modem.demodulate(samples)
                if payload_bytes is None:
                    continue  # could not decode - on a real radio this is "not heard" too
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
            # A competing control frame -> widen our bulk control window so the
            # next one from that station almost certainly finds an open slot.
            if ftype in ("CHAT", "JOIN_REQUEST", "BULK_STATUS"):
                self._foreign_ctrl_ts = time.time()

        if ftype == "BEACON":
            await self.on_beacon(frame)
        elif ftype == "JOIN_REQUEST" and not is_self:
            if self.role == "MASTER":
                self.log(f"{src} joined the net")
            # If this station disappeared while sending us something, request the
            # missing blocks for the half-finished transfers the moment it rejoins.
            for t in self.transfers_in.values():
                if t.src == src and not t.complete:
                    missing = self._missing_blocks(t)
                    if missing:
                        self.log(f"[{t.transfer_id}] {src} came back, requesting "
                                 f"{len(missing)} missing blocks")
                        asyncio.create_task(self.send_frame({
                            "type": "BULK_STATUS", "src": self.callsign, "dst": src,
                            "transfer_id": t.transfer_id, "missing": missing,
                        }))
        elif ftype == "CHAT" and not is_self and dst in ("ALL", self.callsign):
            tag = "all" if dst == "ALL" else "private"
            print(f"\n[CHAT/{tag}] {src}: {frame['text']}")
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
            self.log(f"{callsign} became visible again")
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
                self.log(f"{c} marked as lost")

    # ------------------------------------------------------------------ #
    # Master election / beacon / failover
    # ------------------------------------------------------------------ #
    async def on_beacon(self, frame):
        src = frame["src"]
        now = time.time()

        if self.role == "MASTER" and src != self.callsign:
            # A rare case: two stations became master at the same time. A simple
            # tie-break: the alphabetically smaller callsign wins, the other backs off.
            if src < self.callsign:
                self.log(f"master conflict: {src} continues, I am backing off")
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

            if self.role != "MASTER" and self._should_take_over(now):
                pos = 0 if self.master is None else self._succession_pos()
                self.log(f"beacon timed out -> taking the master role (succession #{pos})")
                self.role = "MASTER"
                self.master = self.callsign
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

    # --- Multi-level failover ---------------------------------------------
    # The old logic only let ONE assigned backup take over; if that station was
    # also gone, the net had no master. Instead every station derives the SAME
    # ordered "succession line" from its local roster and takes over according
    # to its position in it - so the chain continues past a single backup with
    # no extra signalling (the roster is already in every BEACON).
    def _succession_line(self):
        """Sorted {self + active peers} minus the presumed-dead master."""
        cands = {self.callsign}
        for c, i in self.roster.items():
            if c != self.master and i["status"] == "active":
                cands.add(c)
        return sorted(cands)

    def _succession_pos(self):
        line = self._succession_line()
        return line.index(self.callsign) if self.callsign in line else len(line)

    def _should_take_over(self, now):
        silent = now - self.last_beacon_time
        if silent <= BEACON_TIMEOUT:
            return False
        if self.master is None:
            return True  # bootstrap: the first station on an empty channel
        # One extra BEACON_INTERVAL of grace per position in the line, so the
        # highest-priority survivor keys up first. Dead peers ahead of us age
        # out to "lost", drop off the line and we move up. A genuine
        # simultaneous claim is still resolved by the tie-break in on_beacon.
        return silent > BEACON_TIMEOUT + self._succession_pos() * BEACON_INTERVAL

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
    # Bulk transfer - send
    # ------------------------------------------------------------------ #
    async def send_bulk(self, path, dst="ALL"):
        if not os.path.exists(path):
            self.log(f"file not found: {path}")
            return
        data = open(path, "rb").read()
        n_blocks = max(1, (len(data) + BLOCK_SIZE - 1) // BLOCK_SIZE)
        blocks = {i: data[i * BLOCK_SIZE:(i + 1) * BLOCK_SIZE] for i in range(n_blocks)}
        transfer_id = uuid.uuid4().hex[:8]
        t = TransferOut(transfer_id, os.path.basename(path), dst, blocks, self.mode)
        self.transfers_out[transfer_id] = t

        self.log(f"[{transfer_id}] {t.filename} -> {dst} starting "
                 f"({len(data)} B, {n_blocks} blocks, {self.mode})")

        if not await self.send_frame({
            "type": "BULK_META", "src": self.callsign, "dst": dst,
            "transfer_id": transfer_id, "filename": t.filename,
            "total_blocks": n_blocks, "total_size": len(data),
        }):
            self.log(f"[{transfer_id}] could not start (no link)")
            return

        if not await self._send_blocks(t, list(blocks.keys())):
            self.log(f"[{transfer_id}] the link dropped, the transfer is suspended")
            return
        if not await self.send_frame({"type": "BULK_END", "src": self.callsign, "dst": dst,
                                       "transfer_id": transfer_id}):
            return

        sent_last_round = n_blocks
        for round_no in range(6):
            missing = await self._wait_for_status(transfer_id, timeout=6.0)
            if not self.connected:
                self.log(f"[{transfer_id}] the link dropped, the transfer is suspended")
                return
            if missing is None:
                self.log(f"[{transfer_id}] no status reply (round {round_no + 1})")
                continue
            if not missing:
                self.log(f"[{transfer_id}] complete (round {round_no + 1})")
                return
            self._adapt_mode(t, len(missing), sent_last_round)
            sent_last_round = len(missing)
            self.log(f"[{transfer_id}] resending {len(missing)} blocks (round {round_no + 1}, {t.mode})")
            if not await self._send_blocks(t, missing):
                return
            await self.send_frame({"type": "BULK_END", "src": self.callsign, "dst": dst,
                                    "transfer_id": transfer_id})
        self.log(f"[{transfer_id}] reached the round limit; it will resume automatically if the receiver comes back")

    # Every N blocks we deliberately clear the channel (so chat/control
    # messages AND BEACONS can get through) - the real counterpart of the
    # "control window" idea from our design discussion.
    #
    # Why this frequent and this long: a competing station's retry timing
    # syncs to "the end of the current block" from the server's retry_after
    # value, but it does NOT KNOW exactly when the window opens. If the window
    # is short/sparse (the previous version: 0.5 s every 6 blocks) a retry
    # will most likely MISS it - it can even miss beacons (this was exactly
    # the real problem we saw: neither station heard the other's beacon often
    # enough during a 45-block transfer, and both declared themselves master).
    # By making the window more frequent and wider we make catching it almost
    # certain in practice - the cost is a ~25-30% drop in overall throughput,
    # but that was exactly our original design goal: not raw speed, but the
    # guarantee of "being able to pass a message in between".
    #
    # ADAPTIVE (next-step #9): the values above are the QUIET baseline - enough
    # to keep beacons alive during an otherwise idle transfer. When the channel
    # is actually contended (we have local chat queued, or another station just
    # sent a CHAT/JOIN/BULK_STATUS) we switch to the BUSY values: a window after
    # every single block, held a touch longer. Throughput drops further while
    # the contention lasts, then it relaxes back on its own.
    CONTROL_WINDOW_EVERY = 3
    CONTROL_WINDOW_PAUSE = 1.2       # s
    CONTROL_WINDOW_EVERY_BUSY = 1
    CONTROL_WINDOW_PAUSE_BUSY = 1.5  # s
    CONTROL_CONTENDED_FOR = 6.0      # s to stay in BUSY after the last foreign control frame

    # Adaptive modulation (next-step #4): there is no pilot-based per-carrier
    # bit loading yet (that needs the channel estimator, #5), but the ARQ loop
    # already gives us a real link-quality signal - the fraction of a round's
    # blocks that did not make it. If a QPSK round loses more than
    # ADAPT_DOWNSHIFT_FRAC of what we sent, the link will not sustain QPSK
    # without FEC (the "COFDM cliff" from the design notes), so we drop to BPSK
    # for the rest of the transfer. Downshift only - we never oscillate back up.
    ADAPT_DOWNSHIFT_FRAC = 0.15

    def _adapt_mode(self, t: TransferOut, n_missing, n_sent_last_round):
        if t.mode == "QPSK" and n_sent_last_round > 0 \
                and n_missing / n_sent_last_round > self.ADAPT_DOWNSHIFT_FRAC:
            t.mode = "BPSK"
            self.log(f"[{t.transfer_id}] link adaptation: {n_missing}/{n_sent_last_round} "
                     f"blocks lost at QPSK -> switching to BPSK")

    def _control_window(self):
        """(every, pause) for the control window - widened while contended."""
        contended = (self._chat_waiting > 0 or
                     (time.time() - self._foreign_ctrl_ts) < self.CONTROL_CONTENDED_FOR)
        if contended:
            return self.CONTROL_WINDOW_EVERY_BUSY, self.CONTROL_WINDOW_PAUSE_BUSY
        return self.CONTROL_WINDOW_EVERY, self.CONTROL_WINDOW_PAUSE

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
            every, pause = self._control_window()
            if (i + 1) % every == 0:
                await asyncio.sleep(pause)
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

        # No active ARQ loop (e.g. we left earlier because the link dropped)
        # but we still hold the blocks -> service the delayed "carry on" request.
        # Here too, for the same reason (we are inside receive_loop), we use a background task.
        t = self.transfers_out.get(tid)
        if t and missing:
            self.log(f"[{tid}] delayed resume request: resending {len(missing)} blocks")
            asyncio.create_task(self._resend_missing(t, missing))

    async def _resend_missing(self, t: TransferOut, missing):
        if await self._send_blocks(t, missing):
            await self.send_frame({"type": "BULK_END", "src": self.callsign, "dst": t.dst,
                                    "transfer_id": t.transfer_id})

    # ------------------------------------------------------------------ #
    # Bulk transfer - receive
    # ------------------------------------------------------------------ #
    def on_bulk_meta(self, frame):
        tid = frame["transfer_id"]
        t = TransferIn(tid, frame["filename"], frame["total_blocks"], frame["src"], frame["dst"])
        self.transfers_in[tid] = t
        self.log(f"[{tid}] {frame['src']} started a transfer: {t.filename} "
                 f"({t.total_blocks} blocks)")

    def on_bulk_block(self, frame):
        t = self.transfers_in.get(frame["transfer_id"])
        if not t or t.complete:
            return
        data = b64d(frame["data"])
        if crc32(data) != frame["crc"]:
            return  # CRC mismatch -> ignored, ARQ will re-request it
        t.received[frame["seq"]] = data

    def _missing_blocks(self, t: TransferIn):
        return [s for s in range(t.total_blocks) if s not in t.received]

    async def on_bulk_end(self, frame):
        t = self.transfers_in.get(frame["transfer_id"])
        if not t or t.complete:
            return
        missing = self._missing_blocks(t)
        if missing:
            self.log(f"[{t.transfer_id}] {len(missing)}/{t.total_blocks} blocks missing, requesting them")
        else:
            self._save_transfer(t)
        # IMPORTANT: because we are called from inside receive_loop we do NOT
        # AWAIT send_frame here (its own reply would again be awaited by
        # receive_loop, which would deadlock) - we launch it as a background task.
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
        self.log(f"[{t.transfer_id}] complete -> {out_path}")

    # ------------------------------------------------------------------ #
    # CLI
    # ------------------------------------------------------------------ #
    def log(self, msg):
        print(f"[{self.callsign} {time.strftime('%H:%M:%S')}] {msg}")

    def status(self):
        print(f"--- {self.callsign} | role={self.role} | master={self.master} | "
              f"backup={self.backup} | connected={self.connected} ---")
        for c, i in self.roster.items():
            age = time.time() - i["last_seen"]
            print(f"  {c}: {i['status']} (last seen {age:.0f}s ago)")
        for tid, t in self.transfers_in.items():
            state = "done" if t.complete else f"{len(t.received)}/{t.total_blocks}"
            print(f"  in   [{tid}] {t.filename} ({state})")
        for tid, t in self.transfers_out.items():
            print(f"  out  [{tid}] {t.filename} -> {t.dst}")

    def _stdin_reader_thread(self):
        for line in sys.stdin:
            self.stdin_q.put(line.rstrip("\n"))

    async def cli_loop(self):
        threading.Thread(target=self._stdin_reader_thread, daemon=True).start()
        print("Commands:")
        print("  /chat <message>               - chat message to everyone")
        print("  /msg <CALL> <message>         - private message")
        print("  /sendimage <path> [CALL|ALL]  - send image/data (default: ALL)")
        print("  /sendfile <path> <CALL>       - send file (private)")
        print("  /status                       - role, roster, transfer state")
        print("  /drop                         - simulate a sudden drop")
        print("  /reconnect                    - reconnect, resume where it left off")
        print("  /quit                         - quit")
        while True:
            try:
                line = self.stdin_q.get_nowait()
            except queue.Empty:
                await asyncio.sleep(0.1)
                continue
            # Run long-running commands (transfers) in the background so that
            # commands like /drop can still be processed while a transfer runs.
            asyncio.create_task(self._handle_command(line))

    async def _handle_command(self, line):
        if not line:
            return
        parts = line.split(" ", 2)
        cmd = parts[0]
        try:
            if cmd == "/chat" and len(parts) > 1:
                self._chat_waiting += 1
                try:
                    await self.chat(" ".join(parts[1:]))
                finally:
                    self._chat_waiting -= 1
            elif cmd == "/msg" and len(parts) > 2:
                self._chat_waiting += 1
                try:
                    await self.chat(parts[2], dst=parts[1].upper())
                finally:
                    self._chat_waiting -= 1
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
                print("unknown command or missing parameter")
        except Exception as e:
            self.log(f"error: {e}")


async def main():
    ap = argparse.ArgumentParser(description="NET station client")
    ap.add_argument("callsign")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=6000)
    ap.add_argument("--mode", default="QPSK", choices=["BPSK", "QPSK", "16QAM"])
    args = ap.parse_args()

    client = Client(args.callsign.upper(), args.host, args.port, args.mode)
    await client.connect()

    # IMPORTANT: we start the background tasks (especially receive_loop)
    # BEFORE the JOIN_REQUEST. send_frame relies on receive_loop to read the
    # TX_GRANTED reply from the server - if receive_loop is not running yet,
    # the first send silently times out and the client wrongly thinks it has
    # "no link" (every subsequent command then also fails silently).
    tasks = [
        asyncio.create_task(client.receive_loop()),
        asyncio.create_task(client.master_watchdog()),
        asyncio.create_task(client.beacon_loop()),
        asyncio.create_task(client.cli_loop()),
    ]
    await asyncio.sleep(0.1)  # so the background tasks get their first run

    await client.send_frame({"type": "JOIN_REQUEST", "src": client.callsign, "dst": "ALL"})

    await asyncio.gather(*tasks)


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
