"""
modem.py - Gerçek bir OFDM ses modemi (modülatör + demodülatör).

Tasarım sohbetimizdeki PHY parametreleriyle uyumlu: 8000 Hz örnekleme,
256 nokta FFT (~31.25 Hz alt taşıyıcı aralığı), 64 örnek (8ms) koruma
aralığı, ~2.7 kHz bant içinde 77 veri alt taşıyıcısı (~312-2688 Hz).

Bilinçli basitleştirmeler (MVP - bir sonraki geliştirme adımları):
  - Adaptif bit yükleme yok: header BPSK (sağlam), veri QPSK (sabit hız).
  - Kanal kestirimi/ekolayzır yok: koruma aralığı (8ms) içindeki
    gecikmelere (multipath) doğal olarak dayanıklı, ama onun dışına
    taşan gecikmelerde performans bilerek düşer - tam da OFDM'in
    avantajını VE sınırını gösterir.
  - Kanal kodlaması (LDPC/RS) yok: bütünlük sadece CRC32 ile denetleniyor,
    hata düzeltme üst katmandaki blok bazlı ARQ'ya bırakılıyor.

Senkronizasyon Schmidl-Cox yöntemine benzer bir öz-korelasyon ile yapılıyor:
preamble sadece çift indeksli alt taşıyıcılarda enerji taşır, bu da zaman
domeninde iki özdeş yarımdan oluşan bir sembol üretir - alıcı bu simetriyi
arayarak aktarımın başlangıcını (örnek hassasiyetinde) bulur.
"""
import numpy as np
import zlib

SAMPLE_RATE = 8000
N = 256                     # FFT boyutu
CP_LEN = 64                 # koruma aralığı / cyclic prefix (8ms)
SYMBOL_LEN = N + CP_LEN      # bir OFDM sembolünün toplam örnek sayısı

DATA_CARRIERS = list(range(10, 87))                     # 77 alt taşıyıcı
PREAMBLE_CARRIERS = list(range(2, N // 2, 2))            # Schmidl-Cox: çift indeksler

HEADER_BITS = 17            # 16-bit uzunluk + 1-bit mod bayrağı (0=QPSK,1=BPSK)
HEADER_REPEAT = 4           # tekrar sayısı (76 taşıyıcıya sığacak kadar)

_rng = np.random.RandomState(1234)  # TX/RX'in bildiği SABİT preamble
_PREAMBLE_SYMBOLS = _rng.choice([1.0, -1.0], size=len(PREAMBLE_CARRIERS))


# ------------------------------------------------------------------ #
# Ortak yardımcılar
# ------------------------------------------------------------------ #
def _spectrum_to_time(spectrum: dict) -> np.ndarray:
    """{taşıyıcı_indeksi: karmaşık_değer} -> N örnekli REEL zaman sinyali
    (Hermitian simetri ile: X[N-k] = conj(X[k]))."""
    X = np.zeros(N, dtype=complex)
    for k, v in spectrum.items():
        X[k] = v
        X[N - k] = np.conj(v)
    return np.fft.ifft(X).real


def _add_cp(symbol: np.ndarray) -> np.ndarray:
    return np.concatenate([symbol[-CP_LEN:], symbol])


def _make_preamble() -> np.ndarray:
    spec = dict(zip(PREAMBLE_CARRIERS, _PREAMBLE_SYMBOLS))
    return _add_cp(_spectrum_to_time(spec))


def _bits_from_bytes(data: bytes) -> np.ndarray:
    return np.unpackbits(np.frombuffer(data, dtype=np.uint8))


def _bytes_from_bits(bits: np.ndarray) -> bytes:
    n = (len(bits) // 8) * 8
    return np.packbits(bits[:n]).tobytes()


# ------------------------------------------------------------------ #
# Header sembolü (BPSK, frekans-domeninde DİFERANSİYEL kodlama)
#
# İlk taşıyıcı (DATA_CARRIERS[0]) sabit bir referans (1+0j) olarak kalır,
# bilgi taşımaz. Sonraki her taşıyıcı, bir önceki taşıyıcıya göre FAZ
# FARKI olarak kodlanır. Bu sayede senkronizasyondaki küçük bir örnek
# kayması (k'ye bağlı doğrusal faz kayması yaratır) bitişik taşıyıcılar
# arasında büyük ölçüde iptal olur - sadece küçük, sabit bir kalıntı
# kalır. Kanal kestirimi/ekolayzır olmadan gürültüye dayanıklılık için
# gereken standart bir teknik (differential OFDM).
# ------------------------------------------------------------------ #
def _make_header_symbol(payload_len: int, mode: str) -> np.ndarray:
    len_bits = [(payload_len >> (16 - 1 - i)) & 1 for i in range(16)]
    mode_bit = [1 if mode == "BPSK" else 0]
    header_bits = np.array(len_bits + mode_bit)  # 17 bit
    repeated = np.tile(header_bits, HEADER_REPEAT)
    n_info = len(DATA_CARRIERS) - 1
    padded = np.zeros(n_info, dtype=int)
    padded[:min(len(repeated), n_info)] = repeated[:n_info]
    steps = np.where(padded == 0, 1.0 + 0j, -1.0 + 0j)  # BPSK adım çarpanı

    seq = [1.0 + 0j]
    cur = 1.0 + 0j
    for v in steps:
        cur = cur * v
        seq.append(cur)
    spec = dict(zip(DATA_CARRIERS, seq))
    return _add_cp(_spectrum_to_time(spec))


def _decode_header_symbol(symbol_no_cp: np.ndarray):
    X = np.fft.fft(symbol_no_cp)
    vals = np.array([X[k] for k in DATA_CARRIERS])
    diffs = vals[1:] * np.conj(vals[:-1])
    bits = (diffs.real < 0).astype(int)

    n_info = len(DATA_CARRIERS) - 1
    reps = min(HEADER_REPEAT, n_info // HEADER_BITS)
    if reps < 1:
        return None, None
    mat = bits[:reps * HEADER_BITS].reshape(reps, HEADER_BITS)
    votes = mat.sum(axis=0)
    decoded = (votes > (reps / 2)).astype(int)
    val = 0
    for b in decoded[:16]:
        val = (val << 1) | int(b)
    mode = "BPSK" if decoded[16] == 1 else "QPSK"
    return val, mode


# ------------------------------------------------------------------ #
# Veri sembolleri (QPSK varsayılan, BPSK opsiyonel - daha sağlam/yavaş)
# ------------------------------------------------------------------ #
QPSK_MAP = {
    (0, 0): (1 + 1j) / np.sqrt(2),
    (0, 1): (1 - 1j) / np.sqrt(2),
    (1, 0): (-1 + 1j) / np.sqrt(2),
    (1, 1): (-1 - 1j) / np.sqrt(2),
}
BPSK_STEP = {0: 1.0 + 0j, 1: -1.0 + 0j}


def _make_data_symbols(bits: np.ndarray, mode: str):
    n_info = len(DATA_CARRIERS) - 1
    bits_per_carrier = 1 if mode == "BPSK" else 2
    bits_per_symbol = bits_per_carrier * n_info
    pad = (-len(bits)) % bits_per_symbol
    if pad:
        bits = np.concatenate([bits, np.zeros(pad, dtype=int)])
    n_symbols = len(bits) // bits_per_symbol
    symbols = []
    for s in range(n_symbols):
        chunk = bits[s * bits_per_symbol:(s + 1) * bits_per_symbol]
        if mode == "BPSK":
            steps = [BPSK_STEP[int(b)] for b in chunk]
        else:
            pairs = chunk.reshape(n_info, 2)
            steps = [QPSK_MAP[(int(b0), int(b1))] for b0, b1 in pairs]

        seq = [1.0 + 0j]
        cur = 1.0 + 0j
        for v in steps:
            cur = cur * v
            seq.append(cur)
        spec = dict(zip(DATA_CARRIERS, seq))
        symbols.append(_add_cp(_spectrum_to_time(spec)))
    return symbols


def _decode_data_symbol(symbol_no_cp: np.ndarray, mode: str) -> np.ndarray:
    X = np.fft.fft(symbol_no_cp)
    vals = np.array([X[k] for k in DATA_CARRIERS])
    diffs = vals[1:] * np.conj(vals[:-1])
    bits = []
    if mode == "BPSK":
        for d in diffs:
            bits.append(0 if d.real >= 0 else 1)
    else:
        for d in diffs:
            bits.append(0 if d.real >= 0 else 1)
            bits.append(0 if d.imag >= 0 else 1)
    return np.array(bits)


# ------------------------------------------------------------------ #
# Modem
# ------------------------------------------------------------------ #
class Modem:
    """OFDM modülatör/demodülatör. Durumsuz: her çağrı bağımsız bir
    aktarımı (preamble + header + veri) baştan sona işler."""

    def modulate(self, payload: bytes, mode: str = "QPSK") -> np.ndarray:
        crc = zlib.crc32(payload) & 0xFFFFFFFF
        full = payload + crc.to_bytes(4, "big")
        bits = _bits_from_bytes(full)

        preamble = _make_preamble()
        header = _make_header_symbol(len(full), mode)
        data_symbols = _make_data_symbols(bits, mode)

        waveform = np.concatenate([preamble, header] + data_symbols)

        # Küçük, rastgele bir "sessizlik" öne ekleniyor - alıcı gerçekten
        # korelasyonla senkronizasyon yapmak zorunda kalsın diye (sample 0'ın
        # her zaman başlangıç olduğunu varsaymasın). Sona da küçük sabit bir
        # tampon ekliyoruz - sync tahmini birkaç örnek kayarsa (gürültüde
        # olağan), son sembolün arabellek dışına taşmasını önlemek için.
        lead_in = np.zeros(np.random.randint(20, 300))
        trailing_pad = np.zeros(CP_LEN)
        waveform = np.concatenate([lead_in, waveform, trailing_pad])

        peak = np.max(np.abs(waveform)) or 1.0
        waveform = waveform / peak * 0.7 * 32767
        return waveform.astype(np.int16)

    def demodulate(self, samples: np.ndarray):
        """samples: int16/float dizisi (BİR aktarımın tamamı).
        Başarılıysa payload bytes döner, çözülemezse None (gerçek radyoda
        da böyle olurdu: hiçbir şey duymamış gibi davranırsınız)."""
        x = np.asarray(samples, dtype=float)
        if len(x) < SYMBOL_LEN * 2:
            return None

        half = N // 2
        search_end = min(len(x) - SYMBOL_LEN * 2, 400)
        if search_end <= 0:
            return None

        raw_scores = np.zeros(search_end)
        for ps in range(search_end):
            a = x[ps:ps + half]
            b = x[ps + half:ps + 2 * half]
            denom = (np.linalg.norm(a) * np.linalg.norm(b)) or 1.0
            raw_scores[ps] = abs(np.dot(a, b)) / denom

        # CP, periyodik preamble'ın bir kopyası olduğundan ham korelasyon
        # skoru gerçek başlangıçtan CP_LEN kadar önce başlayan bir "plato"
        # oluşturur - gürültüde bu platonun tam kenarını bulmak kırılgandır.
        # Standart düzeltme: skoru CP_LEN genişliğinde bir hareketli
        # ortalamayla yumuşatmak, platoyu TEK ve gürültüye dayanıklı bir
        # tepe noktasına dönüştürür (klasik Schmidl-Cox pratiği).
        if len(raw_scores) < CP_LEN:
            return None
        window = CP_LEN
        csum = np.cumsum(np.insert(raw_scores, 0, 0.0))
        smoothed = (csum[window:] - csum[:-window]) / window
        best_idx = int(np.argmax(smoothed))
        best_score = smoothed[best_idx]
        best_ps = best_idx + window  # platonun sonuna karşılık gelir

        if best_score < 0.25:  # yeterince güçlü bir preamble bulunamadı -> "sessizlik"
            return None

        preamble_cp_start = best_ps - CP_LEN
        if preamble_cp_start < 0:
            return None

        def symbol_at(index):
            start = preamble_cp_start + index * SYMBOL_LEN + CP_LEN
            end = start + N
            if end > len(x):
                return None
            return x[start:end]

        header_sym = symbol_at(1)
        if header_sym is None:
            return None
        payload_len, mode = _decode_header_symbol(header_sym)
        if payload_len is None or mode is None or not (4 <= payload_len <= 20000):
            return None

        bits_needed = payload_len * 8
        bits_per_carrier = 1 if mode == "BPSK" else 2
        bits_per_symbol = bits_per_carrier * (len(DATA_CARRIERS) - 1)
        n_data_symbols = -(-bits_needed // bits_per_symbol)  # ceil

        all_bits = []
        for i in range(n_data_symbols):
            sym = symbol_at(2 + i)
            if sym is None:
                return None
            all_bits.append(_decode_data_symbol(sym, mode))
        bits = np.concatenate(all_bits)[:bits_needed]
        full = _bytes_from_bits(bits)
        if len(full) < payload_len:
            return None

        payload = full[:payload_len - 4]
        crc_recv = int.from_bytes(full[payload_len - 4:payload_len], "big")
        if zlib.crc32(payload) & 0xFFFFFFFF != crc_recv:
            return None
        return payload


def airtime_seconds(payload_len_bytes: int, mode: str = "QPSK") -> float:
    """Bir payload'ın gerçekte kaç saniye 'havada' kalacağını (lead-in
    hariç) hesaplar - PHY tasarım tablosuyla karşılaştırma için."""
    full_len = payload_len_bytes + 4
    bits_needed = full_len * 8
    bits_per_carrier = 1 if mode == "BPSK" else 2
    bits_per_symbol = bits_per_carrier * (len(DATA_CARRIERS) - 1)
    n_data_symbols = -(-bits_needed // bits_per_symbol)
    n_symbols = 2 + n_data_symbols  # preamble + header + veri
    return n_symbols * SYMBOL_LEN / SAMPLE_RATE
