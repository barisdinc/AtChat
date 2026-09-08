"""
modem.py - A real OFDM audio modem (modulator + demodulator).

Matches the PHY parameters from our design discussion: 8000 Hz sample rate,
256-point FFT (~31.25 Hz subcarrier spacing), a 64-sample (8 ms) guard
interval, 77 data subcarriers inside a ~2.7 kHz band (~312-2688 Hz).

Deliberate simplifications (MVP - the next development steps):
  - No adaptive bit loading: the header is BPSK (robust), the data QPSK
    (a fixed rate).
  - No channel estimation/equaliser: naturally robust to delays (multipath)
    within the guard interval (8 ms), but performance drops on purpose for
    delays beyond it - which shows OFDM's advantage AND its limit.
  - No channel coding (LDPC/RS): integrity is checked with CRC32 only, and
    error correction is left to the upper layer's block-based ARQ.

Synchronisation is done with an autocorrelation similar to the Schmidl-Cox
method: the preamble carries energy only on the even-indexed subcarriers,
which produces a symbol made of two identical halves in the time domain -
the receiver searches for that symmetry to find the start of the
transmission (to sample precision).
"""
import numpy as np
import zlib

SAMPLE_RATE = 8000
N = 256                     # FFT size
CP_LEN = 64                 # guard interval / cyclic prefix (8 ms)
SYMBOL_LEN = N + CP_LEN      # total sample count of one OFDM symbol

DATA_CARRIERS = list(range(10, 87))                     # 77 subcarriers
PREAMBLE_CARRIERS = list(range(2, N // 2, 2))            # Schmidl-Cox: even indices

HEADER_BITS = 17            # 16-bit length + 1-bit mode flag (0=QPSK,1=BPSK)
HEADER_REPEAT = 4           # repeat count (enough to fit 76 carriers)

_rng = np.random.RandomState(1234)  # the FIXED preamble both TX and RX know
_PREAMBLE_SYMBOLS = _rng.choice([1.0, -1.0], size=len(PREAMBLE_CARRIERS))


# ------------------------------------------------------------------ #
# Shared helpers
# ------------------------------------------------------------------ #
def _spectrum_to_time(spectrum: dict) -> np.ndarray:
    """{carrier_index: complex_value} -> an N-sample REAL time-domain signal
    (with Hermitian symmetry: X[N-k] = conj(X[k]))."""
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
# The header symbol (BPSK, frequency-domain DIFFERENTIAL coding)
#
# The first carrier (DATA_CARRIERS[0]) stays a fixed reference (1+0j) and
# carries no information. Every following carrier is coded as the PHASE
# DIFFERENCE relative to the previous carrier. This way a small sample slip
# in synchronisation (which creates a k-dependent linear phase shift) mostly
# cancels between adjacent carriers - only a small, constant residue
# remains. A standard technique for noise robustness without channel
# estimation/equalisation (differential OFDM).
# ------------------------------------------------------------------ #
def _make_header_symbol(payload_len: int, mode: str) -> np.ndarray:
    len_bits = [(payload_len >> (16 - 1 - i)) & 1 for i in range(16)]
    mode_bit = [1 if mode == "BPSK" else 0]
    header_bits = np.array(len_bits + mode_bit)  # 17 bit
    repeated = np.tile(header_bits, HEADER_REPEAT)
    n_info = len(DATA_CARRIERS) - 1
    padded = np.zeros(n_info, dtype=int)
    padded[:min(len(repeated), n_info)] = repeated[:n_info]
    steps = np.where(padded == 0, 1.0 + 0j, -1.0 + 0j)  # BPSK step multiplier

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
# Data symbols (QPSK by default, BPSK optional - more robust/slower)
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
    """OFDM modulator/demodulator. Stateless: every call handles one whole
    transmission (preamble + header + data) from start to finish."""

    def modulate(self, payload: bytes, mode: str = "QPSK") -> np.ndarray:
        crc = zlib.crc32(payload) & 0xFFFFFFFF
        full = payload + crc.to_bytes(4, "big")
        bits = _bits_from_bytes(full)

        preamble = _make_preamble()
        header = _make_header_symbol(len(full), mode)
        data_symbols = _make_data_symbols(bits, mode)

        waveform = np.concatenate([preamble, header] + data_symbols)

        # A small, random "silence" is prepended so the receiver really has
        # to synchronise by correlation (it must not assume sample 0 is always
        # the start). A small fixed pad is also appended - so that if the sync
        # estimate slips a few samples (common under noise), the last symbol
        # does not run past the buffer.
        lead_in = np.zeros(np.random.randint(20, 300))
        trailing_pad = np.zeros(CP_LEN)
        waveform = np.concatenate([lead_in, waveform, trailing_pad])

        peak = np.max(np.abs(waveform)) or 1.0
        waveform = waveform / peak * 0.7 * 32767
        return waveform.astype(np.int16)

    def demodulate(self, samples: np.ndarray):
        """samples: an int16/float array (ONE whole transmission).
        Returns payload bytes on success, None if it cannot be decoded (this
        is how a real radio behaves too: you act as if you heard nothing)."""
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

        # Because the CP is a copy of the periodic preamble, the raw
        # correlation score forms a "plateau" that starts CP_LEN before the
        # true start - finding the exact edge of that plateau under noise is
        # fragile. The standard fix: smoothing the score with a moving average
        # CP_LEN wide turns the plateau into a SINGLE, noise-robust peak (the
        # classic Schmidl-Cox practice).
        if len(raw_scores) < CP_LEN:
            return None
        window = CP_LEN
        csum = np.cumsum(np.insert(raw_scores, 0, 0.0))
        smoothed = (csum[window:] - csum[:-window]) / window
        best_idx = int(np.argmax(smoothed))
        best_score = smoothed[best_idx]
        best_ps = best_idx + window  # corresponds to the end of the plateau

        if best_score < 0.25:  # no strong enough preamble found -> "silence"
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
    """Computes how many seconds a payload actually stays 'on the air'
    (lead-in excluded) - for comparing against the PHY design table."""
    full_len = payload_len_bytes + 4
    bits_needed = full_len * 8
    bits_per_carrier = 1 if mode == "BPSK" else 2
    bits_per_symbol = bits_per_carrier * (len(DATA_CARRIERS) - 1)
    n_data_symbols = -(-bits_needed // bits_per_symbol)
    n_symbols = 2 + n_data_symbols  # preamble + header + data
    return n_symbols * SYMBOL_LEN / SAMPLE_RATE
