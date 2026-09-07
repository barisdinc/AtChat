# AtCHAT — Rust portu + egui GUI

Kökteki Python simülasyonunun (`modem.py`, `client.py`, `channel_server.py`,
`monitor.py`) çapraz-platform derlenebilir Rust portu. Hedef: `client`,
`channel_server` ve `monitor` işlevlerini **tek pencerede** toplayan bir
egui/eframe uygulaması; `monitor` sekmesi havadaki dalgayı **scope**
(zaman domeni), **spectrum** (FFT) ve **waterfall** olarak gösterir ve
kanal sesini hoparlöre çalar.

Python kodu kökte **dokunulmadan** kalır — hem referans hem çapraz-doğrulama
için (Rust ↔ Python aynı JSON telini konuşur).

## Durum (faz faz)

| Faz | Kapsam | Durum |
|-----|--------|-------|
| 0 | Workspace iskeleti | ✅ |
| 1 | `netproto` + `modem` (OFDM portu) + testler | ✅ Python ile çift yönlü bit-birebir doğrulandı |
| 2 | `channel` + `atchat-channeld` (tel-uyumlu) | ✅ Python `client.py`/`monitor.py` interop'u doğrulandı |
| 3 | `protocol` (`Station`) + entegrasyon testleri | ✅ 6 senaryo geçti (election, chat, bulk bit-birebir, ARQ, drop/reconnect, backup takeover) |
| 4 | `dsp-viz` (scope / spectrum / waterfall) | ✅ 11 birim testi (Hann+Welch+peak-hold, min/max zarf, colormap LUT, RGB waterfall) |
| 5 | `atchat-gui` (eframe, 3 sekme, cpal ses) | ✅ derleniyor + çalışıyor; motor↔GUI glue testi geçiyor |
| 6 | Cila: ön ayarlar, CI, paketleme, doküman | ✅ kanal ön ayarları · matris CI · cargo-dist sürüm iş akışı · kök doküman |

## Önkoşul: Rust toolchain

`rustup` ile stable kuruldu (bu makinede `~/.cargo/bin` ve
`/opt/homebrew/opt/rustup/bin` PATH'te olmalı). Yeni kurulum için:

```
brew install rustup && rustup default stable      # veya:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
rustc --version   # >= 1.75
```

## Derleme / test

```
cd rust

# Faz 1 kapısı — modem Python ile bit-birebir mi?
cargo test -p netproto
cargo test -p modem

# Çapraz vektörler (önce Python tarafında üret — bir kez):
python3 tools/dump_vectors.py          # -> crates/modem/tests/vectors/*.i16
cargo test -p modem --test cross_vectors

# Ters yön (Rust modüle -> Python demodüle):
cargo run -p modem --example emit_vectors
python3 tools/check_vectors.py

# AWGN performans eğrisi (yavaş, CLAUDE.md'deki uçurumu yeniden üretir):
cargo test -p modem --test awgn_sweep -- --ignored --nocapture
```

# Faz 2 kapısı — kanal fiziği + tel katmanı
cargo test -p channel

# Python interop (elle): Rust kanal + Python istasyonlar + Python monitör
cargo build -p atchat-channeld
./target/debug/atchat-channeld --port 6000 &
python3 ../monitor.py --port 6000 --quiet &
python3 ../client.py TA1ABC --port 6000        # ayrı terminal
python3 ../client.py TA2DEF --port 6000        # ayrı terminal
#   /chat merhaba   ve   /sendfile <yol> TA2DEF   -> monitör hepsini "çözüldü" diye loglar
```

# Faz 3 kapısı — istasyon protokol mantığı (client.py portu)
cargo test -p protocol                                  # 6 senaryo (~30 sn)
cargo test -p protocol -- --ignored full_size_image     # gerçek boyut (~75 sn)

# Faz 4/5
cargo test -p dsp-viz -p atchat-gui                     # DSP + motor↔GUI glue

# GUI'yi çalıştır
cargo run -p atchat-gui                                 # tek pencere: Kanal | İstasyonlar | Monitör
cargo build --release                                   # tek binary (hedef platform)
```

## Sürüm paketleri

`v*` biçiminde bir git etiketi push'lanınca iki iş akışı çalışır ve
çıktıları aynı GitHub Release'e yüklenir (`atchat-gui` + `atchat-channeld`):

| Kaynak | Çıktı |
|---|---|
| `release.yml` (cargo-dist, `dist-workspace.toml`) | macOS arm64/x64 · Linux arm64/x64 · Windows x64 arşivleri (`.tar.xz` / `.zip`) + **Windows `.msi`** + sha256 + `source.tar.gz` |
| `deb.yml` (cargo-deb) | Ubuntu/Debian **`.deb`** (x86_64); `atchat-gui` masaüstü girişiyle |

```
# rust/Cargo.toml'da version'ı bump et, sonra:
git tag v0.1.0 && git push origin v0.1.0      # sürümü tetikler
dist plan                                      # yerelde ne üretileceğini gösterir
```

## GUI kullanımı

- **İstasyonlar**: soldan çağrı işareti + mod (QPSK/BPSK) girip "＋ Ekle"
  (✕ ile kaldır). Seçili istasyonda rol rozeti, roster, ilerleme çubuklu
  transferler, sohbet (hedef seçici + Enter), "Kopar"/"Yeniden bağlan",
  filtreli günlük. "Dosya/görüntü gönder…" yerel dosya seçici açar.
- **NET** (toplu kontrol): sekme değiştirmeden herhangi bir istasyondan
  konuş / dosya gönder; **"Tümü konuşsun"** ve **"Tümü göndersin…"** ile
  bağlı her istasyon aynı anda; **Otomatik sohbet** (rastgele istasyon →
  ALL, ayarlı aralık) ile waterfall'ı canlı izle. Birleşik NET sohbet
  akışı + tüm istasyonların aktif transferleri tek listede.
- **Kanal**: AWGN dB + multipath gecikme/kazanç kaydırıcıları (anında
  uygulanır), ön ayarlar (Temiz / 13 dB–ARQ / Multipath sınırı), canlı
  meşgul durumu, olay günlüğü.
- **Monitör**: FFT boyutu, colormap, taban/tavan dB, scope penceresi, peak
  sıfırla, **Ses** (cpal — cihaz yoksa sessizce kapanır). Altında scope
  (zaman), spektrum (0–4 kHz, veri bandı gölgeli, peak-hold), waterfall ve
  pasif çözüm şeridi.

## Workspace düzeni

```
crates/netproto   sabitler, çerçeve tipleri (serde), CRC32, satır-JSON çerçeveleme
crates/modem      OFDM modülatör/demodülatör — modem.py'nin bit seviyesinde sadık portu
crates/channel    kanal fiziği (yarı çift yönlü, AWGN, multipath) + Link (InProc/Tcp)
crates/protocol   Station: master seçimi, roster, sohbet, ARQ (client.py portu)
crates/dsp-viz    scope zarfı, spektrum analizörü, waterfall + colormap LUT'lar
apps/atchat-channeld   headless TCP kanal sunucusu (channel_server.py tel-uyumlu)
apps/atchat-gui        eframe uygulaması: Kanal | İstasyonlar | Monitör
```

## Python ile interop

Rust `atchat-channeld` ve Python `channel_server.py` aynı satır-ayraçlı
JSON telini konuşur (`HELLO` / `TRANSMIT_AUDIO` / `TX_GRANTED` /
`CHANNEL_BUSY` / `RX_AUDIO`). Dolayısıyla Python `client.py` Rust kanala,
Rust istasyon Python kanala bağlanabilir — kademeli geçiş ve ek bir
çapraz-doğrulama katmanı.
