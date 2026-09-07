"""
protocol.py - Ortak protokol sabitleri ve yardımcı fonksiyonlar.

channel_server.py (kanal fiziği) ve client.py (istasyon) tarafından ortak
kullanılır. Buradaki sayılar, tasarım sohbetimizdeki PHY/süper-çerçeve
tablolarıyla birebir eşleşiyor.
"""
import json
import zlib
import base64

# --- PHY katmanı parametreleri -------------------------------------------
# Efektif bayt/sn (FEC sonrası, tasarım tablosundaki sayılar)
RATE_TABLE = {
    "BPSK": 125,
    "QPSK": 250,
    "16QAM": 500,
}
PREAMBLE_OVERHEAD = 0.3  # sn - her aktarımın sabit senkron+header maliyeti


def airtime(size_bytes: int, mode: str) -> float:
    """Bir çerçevenin 'havada kalma süresi' (saniye)."""
    rate = RATE_TABLE.get(mode, RATE_TABLE["QPSK"])
    return PREAMBLE_OVERHEAD + size_bytes / rate


# --- Süper-çerçeve / NET zamanlama parametreleri --------------------------
BEACON_INTERVAL = 8.0                      # sn
BEACON_TIMEOUT = BEACON_INTERVAL * 3       # master sessiz kalırsa yedek devralır
LOST_TIMEOUT = 30.0                        # roster: "kayıp" işaretleme eşiği
REMOVE_TIMEOUT = 120.0                     # roster: tamamen silme eşiği
BLOCK_SIZE = 220                           # bayt (base64/JSON payı için küçültülmüş)


# --- CRC --------------------------------------------------------------
def crc32(data: bytes) -> int:
    return zlib.crc32(data) & 0xFFFFFFFF


# --- Satır bazlı JSON çerçeveleme (TCP üzerinden) --------------------------
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
