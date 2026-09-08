"""
protocol.py - Shared protocol constants and helper functions.

Shared by channel_server.py (the channel physics) and client.py (the station).
The numbers here match the PHY/super-frame tables from our design discussion
exactly.
"""
import json
import zlib
import base64

# --- PHY-layer parameters ---------------------------------------------
# Effective bytes/sec (post-FEC, the numbers from the design table)
RATE_TABLE = {
    "BPSK": 125,
    "QPSK": 250,
    "16QAM": 500,
}
PREAMBLE_OVERHEAD = 0.3  # s - the fixed sync+header cost of every transmission


def airtime(size_bytes: int, mode: str) -> float:
    """A frame's 'time on air' (seconds)."""
    rate = RATE_TABLE.get(mode, RATE_TABLE["QPSK"])
    return PREAMBLE_OVERHEAD + size_bytes / rate


# --- Super-frame / NET timing parameters -----------------------------
BEACON_INTERVAL = 8.0                      # s
BEACON_TIMEOUT = BEACON_INTERVAL * 3       # the backup takes over if the master goes quiet
LOST_TIMEOUT = 30.0                        # roster: the "lost" marking threshold
REMOVE_TIMEOUT = 120.0                     # roster: the full-removal threshold
BLOCK_SIZE = 220                           # bytes (shrunk to leave room for base64/JSON)


# --- CRC --------------------------------------------------------------
def crc32(data: bytes) -> int:
    return zlib.crc32(data) & 0xFFFFFFFF


# --- Line-based JSON framing (over TCP) -------------------------------
async def send_json(writer, obj: dict):
    line = json.dumps(obj, ensure_ascii=False).encode("utf-8") + b"\n"
    writer.write(line)
    await writer.drain()


async def read_json(reader):
    line = await reader.readline()
    if not line:
        return None
    return json.loads(line.decode("utf-8"))


def b64e(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def b64d(s: str) -> bytes:
    return base64.b64decode(s.encode("ascii"))
