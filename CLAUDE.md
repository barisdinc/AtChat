# CLAUDE.md — Telsiz NET Protokolü Simülasyonu

Bu dosya, bu projeye başka bir oturumda devam ederken bağlamı yeniden
anlatmak zorunda kalmamak için hazırlandı. Claude bu dosyayı okuyunca
projenin tamamını, alınan kararları, bulunan hataları ve mevcut durumu
bilmiş olmalı.

## Projenin amacı

Amatör telsiz üzerinden çalışacak, SSTV kadar basit ama daha dayanıklı,
KGSTV/EasyPal/HSModem'den ilham alan, 2.7 kHz SSB bant genişliğinde
çalışan; görüntü/dosya transferi + hata düzeltme (ARQ) + çok istasyonlu
sohbet (NET) yapabilen bir dijital mod **tasarlandı ve yazılımla
simüle edildi**. Henüz gerçek SDR/telsiz donanımına bağlanmadı (bu,
en sondaki "Sonraki adımlar" bölümünde tarif ediliyor).

Konuşma şu sırayla ilerledi:
1. Modülasyon seçimi ve gerekçesi (COFDM neden seçildi)
2. NET protokolü tasarımı: çoklu istasyon, dinamik master seçimi,
   ortak+özel sohbet, zaman dilimli kanal erişimi
3. Somut bir senaryo simülasyonu (3-4 istasyonlu örnek, süre hesapları)
4. **Python ile gerçek bir test ortamı** kuruldu: TCP tabanlı kanal
   sunucusu + istasyon istemcileri (önce JSON/soyut simülasyon)
5. **Gerçek bir OFDM modemi** yazıldı (`modem.py`) - artık gerçekten
   ses üretip gerçekten demodüle ediyor, soyut simülasyon değil
6. `monitor.py` ile kanaldaki gerçek sesi dinleme eklendi
7. Gerçek kullanımda bulunan iki ciddi hata düzeltildi (aşağıda detaylı)

## Mimari

```
netproto.py          Ortak sabitler + JSON çerçeveleme yardımcıları
                      (send_json/read_json, CRC32, süper-çerçeve
                      zamanlama sabitleri). NOT: dosya adı bilerek
                      "protocol.py" DEĞİL - kullanıcının sisteminde
                      aynı isimde başka bir PyPI paketiyle çakıştığı
                      için "netproto.py" olarak yeniden adlandırıldı.
                      Bir sonraki oturumda da bu isim korunmalı.

modem.py              GERÇEK bir OFDM modülatör/demodülatör. Uydurma
                      ton değil - gerçek IFFT/FFT, gerçek bit hatalarına
                      gerçekten maruz kalıyor. Detaylar aşağıda.

channel_server.py     "Kanal fiziği": istasyonların gerçekten ürettiği
                      ses örneklerini taşır, yarı çift yönlü erişimi
                      zorunlu kılar (aynı anda tek istasyon), isteğe
                      bağlı GERÇEK AWGN gürültü (--snr) ve çoklu-yol
                      yankısı (--multipath-delay-ms/--multipath-gain)
                      ekler. Hiçbir protokol mantığı (master seçimi,
                      ARQ, sohbet vb.) İÇERMEZ - kasıtlı katman ayrımı.

client.py             Asıl istasyon yazılımı. TÜM protokol mantığı
                      burada: LBT+backoff kanal erişimi, dinamik master
                      seçimi/failover, roster, ortak+özel sohbet,
                      blok+CRC+ARQ ile görüntü/dosya transferi, ani
                      kopma/yeniden bağlanma. Çerçeveleri modem.py ile
                      gerçekten modüle/demodüle eder.

monitor.py             Kanaldaki GERÇEK sesi dinleyen pasif izleyici
                      (opsiyonel). Roster'da görünmez (hiç göndermez).
                      Kendi demodülatörüyle çözmeyi dener, çözemezse
                      "çözülemedi" der.

README.md              Kullanıcıya yönelik kurulum/kullanım talimatı
                      (bu dosyadan farklı - CLAUDE.md geliştirme
                      bağlamı için, README.md son kullanıcı için).

test_files/             grup_gorseli.bin (12KB), belge.bin (45KB) -
                      hazır test verileri. ornek_net_sesi.wav - gerçek
                      modüle edilmiş protokol çerçevelerinin (JOIN,
                      BEACON, 2x CHAT, BULK_META, BULK_BLOCK) art arda
                      dizilmiş, doğrudan çalınabilir örneği.
```

Kasıtlı katman ayrımı: `channel_server.py` = kanalın FİZİĞİ,
`client.py` = protokol MANTIĞI, `modem.py` = MODÜLASYON. Gerçek SDR'a
geçerken büyük ihtimalle sadece `channel_server.py` değişir.

## modem.py — PHY tasarım detayları

Tasarım sohbetindeki PHY tablosuyla uyumlu:

- `SAMPLE_RATE = 8000` Hz, `N = 256` (FFT boyutu), `CP_LEN = 64` (8ms
  koruma aralığı), `SYMBOL_LEN = 320`
- `DATA_CARRIERS = range(10, 87)` → 77 alt taşıyıcı (~312-2688 Hz,
  2.7kHz hedefine uygun)
- Senkronizasyon: Schmidl-Cox tarzı - preamble sadece çift indeksli alt
  taşıyıcılarda enerji taşıyor (zaman domeninde iki özdeş yarım),
  alıcı bunu öz-korelasyonla arıyor
- **Frekans-domeninde diferansiyel kodlama**: bitler mutlak faz değil,
  bitişik alt taşıyıcılar arası faz FARKI olarak taşınıyor (ilk
  taşıyıcı = sabit referans, bilgi taşımıyor). Bu, kanal
  kestirimi/ekolayzır olmadan senkronizasyon hatalarına karşı
  dayanıklılık sağlıyor (aşağıdaki hata #2'ye bakın)
- Header sembolü: her zaman BPSK, `HEADER_BITS=17` (16-bit uzunluk +
  1-bit mod bayrağı: 0=QPSK,1=BPSK), `HEADER_REPEAT=4` tekrar +
  çoğunluk oylaması ile decode ediliyor
- Veri sembolleri: `mode` parametresine göre BPSK ya da QPSK
  (`Modem.modulate(payload, mode)`)
- Bütünlük: CRC32 (payload'a eklenip gönderiliyor), FEC/LDPC/RS YOK -
  hata düzeltme üst katman ARQ'ya bırakılıyor (bilinçli tasarım)

**Ölçülen gerçek performans** (test edildi, uydurulmadı):
- Gürültüsüz: %100 başarı (1B - 5000B arası çeşitli boyutlarda test edildi)
- AWGN: temiz sinyalden 18dB SNR'ye kadar %100, ~14-16dB'de hafif
  düşüş, ~10-12dB'de keskin "uçurum" (FEC'siz QPSK için beklenen -
  tasarım sohbetindeki "COFDM cliff-edge" konusunun somut kanıtı)
- BPSK, QPSK'den belirgin daha dayanıklı ama ~%67 daha yavaş (gerçek
  ölçüm: 10dB'de QPSK 0/20 başarı, BPSK 19/20 başarı)
- Multipath: koruma aralığı (8ms) içinde HAFİF yankılarda (-16dB
  kazanç, 7ms'ye kadar gecikme) sağlam; GÜÇLÜ yankılarda (-10dB
  kazanç, 3ms+) ya da koruma aralığı dışına taşan gecikmelerde
  bilerek bozuluyor (kanal kestirimi olmadığı için - bkz. sınırlamalar)

## client.py — Protokol tasarımı

**Çerçeve tipleri:** `JOIN_REQUEST`, `BEACON`, `MASTER_CLAIM`(zımni,
BEACON içinde), `CHAT` (broadcast/unicast, DST alanıyla), `BULK_META`,
`BULK_BLOCK`, `BULK_END`, `BULK_STATUS`.

**Master seçimi/failover:** İlk bağlanan istasyon, `BEACON_TIMEOUT`
(24sn, `netproto.py`) süresince beacon duymazsa kendini master ilan
eder. Master her `BEACON_INTERVAL` (8sn) bir beacon yayınlar (roster +
atanmış yedek master ile). Yedek master, ana master'dan 24sn beacon
gelmezse otomatik devralır. İki istasyon aynı anda master olursa,
alfabetik olarak küçük çağrı işareti kazanır (basit tie-break).
**Bilinen sınırlama:** şu an sadece TEK bir atanmış yedek master'a
kadar zincirleniyor - o da düşerse roster'daki bir sonraki istasyonun
devralması için ek mantık YOK (bkz. sonraki adımlar).

**Roster:** Her istasyon local olarak `{callsign: {last_seen, status}}`
tutuyor. `LOST_TIMEOUT` (30sn) sonra "kayıp" işaretleniyor,
`REMOVE_TIMEOUT` (120sn) sonra tamamen siliniyor.

**ARQ (blok bazlı):** Dosya/görüntü `BLOCK_SIZE=220` baytlık bloklara
bölünüyor, her blok CRC32 ile korunuyor. Alıcı `BULK_END` sonrası eksik
blokların listesini (`BULK_STATUS`) gönderiyor, gönderen sadece o
blokları tekrar gönderiyor - tüm transfer baştan başlamıyor.

**Ani kopma/yeniden bağlanma:** `/drop` TCP bağlantısını kapatır ama
process/state RAM'de canlı kalır. `/reconnect` yeniden bağlanır,
aktif beacon duyulursa ASLA kendini master ilan etmez (sadece
JOIN_REQUEST gönderir). Alıcı taraf, gönderen istasyonun
JOIN_REQUEST ile geri döndüğünü gördüğünde (roster'da "kayıp"
olsun ya da olmasın - `handle_frame`'in JOIN_REQUEST dalında)
otomatik olarak yarım kalan transferler için eksik blokları ister.
Gönderen tarafta da (`on_bulk_status`) gecikmeli bir BULK_STATUS
gelirse, aktif bir ARQ döngüsü olmasa bile elindeki bloklarla
karşılık verir (arka plan görevi olarak, `_resend_missing`).

**Kontrol pencereleri (ÇOK ÖNEMLİ, gerçek bir hatanın düzeltmesi):**
`_send_blocks` içinde her `CONTROL_WINDOW_EVERY=3` blokta bir,
`CONTROL_WINDOW_PAUSE=1.2` saniyelik bilinçli bir duraklama var. Bu
OLMADAN bulk transfer kanalı sürekli işgal eder - sohbet mesajları VE
BEACON'LAR bile açlıktan ölür, bu da yanlış master seçimi
çakışmalarına yol açar (gerçekte gördük, aşağıdaki Hata #3'e bakın).
Bu parametreleri düşürmeyin/artırmayın demiyorum ama neden bu değerde
olduğunu bilmeden değiştirmeyin - matematiği yorumda (`client.py`
içinde `_send_blocks` üstündeki blok) açıklanmış durumda.

## Bulunan ve düzeltilen gerçek hatalar (önemli, tekrar düşmeyin)

Bunlar TAHMİN değil, gerçekten test edilip gözlemlenip düzeltilmiş
hatalar:

1. **`receive_loop` deadlock'u** (ilk JSON-tabanlı sürümde): Gelen bir
   çerçeveye tetiklenen yanıtlar (`on_bulk_end`, `on_bulk_status`)
   `send_frame`'i DOĞRUDAN `await` ediyordu; ama `send_frame`'in kendi
   yanıtını (TX_GRANTED) işleyecek olan da `receive_loop`'un kendisiydi
   - kendi kendini bekliyordu. **Çözüm:** gelen bir çerçeveye tetiklenen
   TÜM gönderimler `asyncio.create_task(...)` ile arka planda
   başlatılmalı, asla `receive_loop`'un çağrı zincirinde `await`
   edilmemeli.

2. **OFDM senkronizasyon hataları** (modem.py geliştirilirken, 3 ayrı
   hata):
   - CP (cyclic prefix), periyodik preamble'ın bir kopyası olduğundan
     korelasyon skorunda gerçek başlangıçtan CP_LEN kadar ÖNCE başlayan
     bir "plato" oluşuyordu → hareketli ortalama (moving average) ile
     düzeltildi.
   - Mutlak faz tabanlı demodülasyon, 1 örneklik senkron hatasında bile
     yüksek indeksli alt taşıyıcılarda büyük faz kaymasına yol
     açıyordu → frekans-domeninde diferansiyel kodlamaya geçildi
     (yukarıda açıklandı).
   - Küçük bir senkron kayması, son sembolü arabellek dışına
     taşırıyordu → waveform'un sonuna `CP_LEN` örneklik bir tampon
     (padding) eklendi.

3. **Bulk transfer kanalı boğuyordu** (gerçek kullanımda kullanıcı
   tarafından bulundu): İlk düzeltmede kontrol penceresi çok kısa/
   seyrekti (her 6 blokta 0.5sn) - rakip istasyonların yeniden deneme
   zamanlaması sunucudan gelen `retry_after`'a göre "mevcut bloğun
   bitişine" senkronize oluyor ama pencerenin TAM olarak ne zaman
   açılacağını bilemiyor, bu yüzden pencereyi büyük ihtimalle
   kaçırıyordu. Gerçek sonuç: sadece sohbet değil, BEACON'LAR bile
   kaçırılıyor, iki istasyon da birbirini "duyamayıp" kendini master
   ilan ediyordu (gerçek log'da görüldü: `master çakışması`, `kayıp
   olarak işaretlendi`). **Çözüm:** pencere sıklaştırıldı ve
   genişletildi (her 3 blokta 1.2sn), yeniden deneme sayısı 20'den
   40'a çıkarıldı. 45 bloklu gerçek bir transferle (README.md, 9815B)
   yeniden test edildi: SIFIR master çakışması (başlangıçtaki tek
   seferlik normal seçim çakışması hariç), iki sohbet mesajı da (1.3sn
   ve 3.7sn içinde) başarıyla iletildi, dosya bit-eşleşerek tamamlandı.

4. **`protocol.py` isim çakışması**: Kullanıcının sisteminde
   (`~/Library/Python/3.9/site-packages/protocol/`) aynı isimde başka
   bir paket kuruluymuş, yerel `protocol.py`'nin önüne geçiyordu.
   **Çözüm:** dosya `netproto.py` olarak yeniden adlandırıldı, tüm
   importlar güncellendi. Sahte bir çakışan paket simüle edilerek
   (bilerek `sys.path`'in en önüne konularak) düzeltme doğrulandı.

## Test durumu (doğrulanmış senaryolar)

Hepsi gerçekten çalıştırılıp doğrulandı (uydurulmadı):

- ✅ 2 istasyon: temel bağlantı, JOIN_REQUEST, master seçimi
- ✅ Ortak (broadcast) ve özel (unicast) sohbet, gerçek ses üzerinden
- ✅ Küçük (600B) ve orta (1200-9815B) dosya transferi, gerçek OFDM
  modülasyon/demodülasyon ile, bit-eşleşen sonuç
- ✅ 14dB AWGN gürültü altında canlı ARQ kurtarma (1 blok bozuldu,
  2 tur ARQ ile düzeldi, dosya bit-eşleşti)
- ✅ Ani kopma (`/drop`) + yeniden bağlanma (`/reconnect`) + otomatik
  eksik blok isteme + sadece eksik kısmın tamamlanması (baştan
  başlamadan) - hem master hem normal istasyon rolünde test edildi
- ✅ Master düşüşü + yedek master'ın otomatik devralması
- ✅ 3 istasyonlu senaryo (master + 2 istasyon, roster senkronizasyonu)
- ✅ 45 bloklu (9815B) gerçek dosya transferi sürerken sohbet VE
  beacon'ların güvenilir şekilde iletilmesi (kontrol penceresi
  düzeltmesi sonrası)
- ✅ `netproto.py` isim çakışması senaryosu (kasıtlı simüle edildi)
- ⚠️ Multipath testi sadece `modem.py` seviyesinde (standalone) test
  edildi, tam client/server entegrasyonunda `--multipath-*`
  parametreleriyle UÇTAN UCA henüz test edilmedi (server tarafında
  kod var ve mantıken doğru olmalı ama gerçek bir transferle
  doğrulanmadı)
- ❌ 4 istasyonlu tam NET senaryosu (grup görüntüsü + özel dosya +
  ani kopma - orijinal sohbetteki senaryo) gerçek ses üzerinden HENÜZ
  uçtan uca test edilmedi (sadece 2-3 istasyonla test edildi)
- ❌ Gerçek ses kartı/mikrofon loopback testi yapılmadı (hâlâ TCP
  üzerinden base64 ile taşınıyor)

## Bilinen sınırlamalar / sonraki adımlar (öncelik sırasıyla değil)

1. **Failover zinciri tek yedekle sınırlı** - roster'daki bir sonraki
   aktif istasyonun devralması için mantık eklenmeli.
2. **Multipath uçtan uca doğrulanmadı** - yukarıya bakın.
3. **4 istasyonlu tam senaryo gerçek ses ile test edilmedi.**
4. **Adaptif bit yükleme yok** - `mode` (BPSK/QPSK) şu an manuel/sabit
   seçiliyor, SNR'ye göre otomatik seçim yok.
5. **Kanal kestirimi/ekolayzır yok** - multipath sınırlamasının kök
   nedeni, pilot tabanlı kanal kestirimi eklemek doğal bir sonraki adım.
6. **LDPC/RS FEC yok** - bütünlük sadece CRC32, hata düzeltme ARQ'ya
   bırakılmış (bilinçli MVP kararı, ama gerçek bir sonraki adım).
7. **Gerçek ses kartı/mikrofon I/O yok** - hâlâ TCP+JSON+base64 ile
   örnekler taşınıyor, gerçek `sounddevice` ile ses donanımına
   bağlanmadı.
8. **Gerçek SDR/RF entegrasyonu yok** - README'deki "SDR'a geçiş için
   yol haritası" bölümüne bakın; `channel_server.py`'nin değişmesi,
   `client.py`/`modem.py`'nin büyük ölçüde aynı kalması bekleniyor.
9. **Kontrol penceresi (CONTROL_WINDOW_EVERY/PAUSE) sabit** - trafiğe
   göre adaptif hale getirilebilir (ör. bekleyen sohbet varsa pencere
   sıklığını artırmak gibi).

## Nasıl çalıştırılır (özet, detaylar README.md'de)

```
pip install numpy   # tek dış bağımlılık

# terminal 1
python3 channel_server.py --port 6000
# isteğe bağlı gerçek bozulma: --snr 15 --multipath-delay-ms 3 --multipath-gain 0.2

# terminal 2 (opsiyonel ama önerilir - gerçek sesi dinlemek için)
python3 monitor.py --port 6000

# terminal 3, 4, 5, 6 - istasyonlar
python3 client.py TA1ABC
python3 client.py TA2DEF
# komutlar: /chat, /msg, /sendimage, /sendfile, /status, /drop, /reconnect, /quit
```

## Rust portu + egui GUI (`rust/` dizini)

Kullanıcı "client ve channel_server için UI, mümkünse çapraz-platform
derlenebilir bir şey (Rust), monitor için de UI — havadaki dalgayı
scope + spectrum + waterfall olarak göstersin" dedi. Kararlar (kullanıcı
onaylı): **tam Rust portu** (modem + protokol + kanal + GUI), **egui/eframe**,
**hepsi tek uygulamada**, kod `rust/` alt dizininde, monitörde **cpal ile
ses de var**, ve Rust tarafı **Python ile aynı JSON TCP telini** konuşur.

Python dosyaları (`modem.py`, `client.py`, `channel_server.py`,
`monitor.py`, `netproto.py`) kökte **dokunulmadan** duruyor — referans +
çapraz-doğrulama.

### Workspace (`rust/`, Cargo workspace)

```
crates/netproto   sabitler, Frame/ClientMsg/ServerMsg (serde), CRC32, satır-JSON çerçeveleme
crates/modem      OFDM mod/demod — modem.py'nin BİT-BİREBİR portu (rustfft). Sabit preamble gömülü.
crates/channel    ChannelCore (yarı çift yönlü, AWGN, multipath) + Link (LinkTx/LinkRx ayrık) +
                  InProc/Tcp Connector + tcp_server (channel_server.py tel-uyumlu) +
                  pasif monitör demod -> ChannelEvent::Decoded. TEST hook'u: cfg.corrupt_burst_nums.
crates/protocol   Station = client.py'nin tokio async portu. StationConfig ile zamanlamalar
                  test için kısaltılabilir (GUI gerçek 24 sn seçim kullanır).
crates/dsp-viz    ScopeBuf (min/max zarf) + SpectrumAnalyzer (Hann+Welch EWMA+peak-hold) +
                  Waterfall (dB->RGB) + Colormap (256 LUT). GUI çatısından bağımsız.
apps/atchat-channeld  headless TCP kanal — channel_server.py argümanları birebir
apps/atchat-gui       eframe: Kanal | İstasyonlar | Monitör sekmeleri + cpal ses.
                      Motor (engine.rs) ayrı thread'de tokio runtime; GUI↔motor mpsc + Arc<Mutex<Snapshot>>.
```

### Doğrulama durumu (hepsi geçiyor)

- `modem`: gürültüsüz roundtrip %100; **çift yönlü Python çapraz-vektör
  bit-birebir** (`tools/dump_vectors.py` + `tests/cross_vectors.rs` +
  `tools/check_vectors.py`); AWGN eğrisi CLAUDE.md'deki uçurumu yeniden
  üretiyor (`tests/awgn_sweep.rs`, `#[ignore]`).
- `channel`: 8 test + **Python `client.py`/`monitor.py` Rust `atchat-channeld`'e
  bağlanıp sohbet/ARQ/dosya bit-birebir** (elle doğrulandı).
- `protocol`: 6 senaryo — election, chat, bulk bit-birebir, ARQ (kayıp blok),
  drop/reconnect resume, backup takeover. `#[ignore]`'lı tam-boyut testi de var.
- `dsp-viz`: 11 birim testi. `atchat-gui`: motor↔GUI glue testi.
- `cargo clippy --workspace` temiz, `cargo fmt` uygulanmış.

### Port sırasında bulunan/korunan hatalar

- **CLAUDE.md #1 (deadlock)**: gelen çerçeveye tetiklenen TÜM gönderimler
  `tokio::spawn` — `receive_loop` zincirinde asla `await` edilmez.
- **CLAUDE.md #3 (kontrol penceresi)**: `_send_blocks`'ta her 3 blokta 1.2 sn.
- **YENİ (Rust'a özgü)**: `receive_loop`, `rx_slot` (tokio Mutex) guard'ını
  `notified().await` boyunca tutuyordu → `reconnect` kilidi alamıyor →
  kilitlenme. Düzeltme: guard yalnız `take()` süresince tutulur.
- **Bilinen sınırlama (client.py'den miras)**: ARQ turundan sonraki
  BULK_END kaybolursa transfer, gönderen yeniden bağlanana kadar askıda
  kalır. Bu yüzden `arq_recovers_from_lost_blocks` testi saf AWGN yerine
  `cfg.corrupt_burst_nums` ile deterministik blok bozma kullanır.

### Kalan (Faz 6)

- `cargo-dist` ile paket üretimi (CI matris `.github/workflows/ci.yml` HAZIR).
- Kök CLAUDE.md/README.md Rust bölümü (BU bölüm).
- Waterfall frekans-zoom (0–2.76 kHz) — EKLENDİ.

## Konuşma dili

Kullanıcıyla tüm konuşma Türkçe yürütüldü, teknik terimler genelde
Türkçe+İngilizce karışık kullanıldı (ör. "airtime", "backoff",
"multipath" gibi terimler olduğu gibi bırakıldı). Yeni oturumda da
aynı dil/üslupla devam edilmesi bekleniyor. Kullanıcı gerçekten
kod çalıştırıp test ediyor, sonuçları/log'ları paylaşıp gerçek
hataları bildiriyor - yani bu proje "kağıt üzerinde" değil, aktif
olarak elle test edilen bir proje. Bir sonraki oturumda muhtemelen
ya yeni bir gerçek hata bildirecek, ya yukarıdaki "sonraki adımlar"
listesinden birine geçmek isteyecek, ya da gerçek ses kartı/SDR
entegrasyonuna geçmek isteyecek.
