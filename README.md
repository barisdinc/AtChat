# NET protokol simülasyonu (gerçek ses sinyali sürümü)

Bu klasör, sohbetimizde tasarladığımız telsiz-NET protokolünü **gerçek
radyo/SDR olmadan**, localhost üzerinde test etmenizi sağlar - ama artık
soyut bir simülasyon değil: istasyonlar **gerçekten OFDM ile modüle
edilmiş ses üretir**, kanal bu sesi taşır (isteğe bağlı gerçek gürültü/
çoklu-yol bozulmasıyla), ve alıcı istasyonlar bu sesi **gerçekten
demodüle ederek** çözer. `monitor.py` ile bu gerçek sesi dinleyebilirsiniz.

Bağımlılıklar: Python 3.9+ ve `numpy` (`pip install numpy`).

> **Rust portu + GUI (`rust/`):** Bu Python simülasyonunun tamamı Rust'a
> portlandı ve **çapraz-platform tek pencere bir egui uygulamasıyla**
> birleştirildi — Kanal / İstasyonlar / Monitör sekmeleri, canlı
> scope + spectrum + waterfall ve (cpal ile) kanal sesi. Modem Python
> ile **bit-birebir** doğrulandı; Rust `atchat-channeld` ile Python
> `client.py`/`monitor.py` aynı telde konuşabiliyor. Kurulum ve kullanım:
> [`rust/README.md`](rust/README.md). Hızlı başlangıç:
>
> ```
> cd rust && cargo run -p atchat-gui
> ```
>
> Python tarafı bu dizinde **hiç değişmeden** durur — hem referans hem
> çapraz-doğrulama için.

## Hemen dinlemek isterseniz

Hiçbir şey kurmadan, `test_files/ornek_net_sesi.wav` dosyasını doğrudan
çalabilirsiniz - gerçek protokol çerçevelerinin (katılım isteği, beacon,
sohbet mesajları, bir veri bloğu) gerçekten OFDM ile modüle edilmiş
hâli, aralarında kısa boşluklarla art arda. Bu, aşağıdaki `monitor.py`
ile canlı olarak duyacağınız sesin ne olduğunu gösteren statik bir örnek.

## Mimari

```
channel_server.py   "Kanal fiziği": GERÇEK ses örneklerini taşır, yarı
                     çift yönlü kısıtı zorunlu kılar, isteğe bağlı olarak
                     gerçek AWGN gürültü ve çoklu-yol yankısı ekler.
                     Protokol mantığı İÇERMEZ.

client.py            Asıl istasyon yazılımı: LBT+backoff, master seçimi/
                     failover, roster, sohbet, blok+CRC+ARQ ile görüntü/
                     dosya transferi, kopma/yeniden bağlanma. Çerçeveleri
                     modem.py ile GERÇEKTEN modüle/demodüle eder.

modem.py              Gerçek bir OFDM modülatör/demodülatör: 8000 Hz
                     örnekleme, 256-nokta FFT, 8ms koruma aralığı, 77 alt
                     taşıyıcı, Schmidl-Cox tarzı senkronizasyon,
                     frekans-domeninde diferansiyel BPSK/QPSK kodlama.

protocol.py           Ortak sabitler ve JSON çerçeveleme yardımcıları.

monitor.py            Kanaldaki GERÇEK sesi dinleyen ve çalan pasif bir
                     izleyici (opsiyonel). Kendi demodülatörüyle çözmeyi
                     de dener, çözemezse bunu açıkça belirtir.
```

Bu ayrım kasıtlı: ileride gerçek SDR/telsiz donanımına geçtiğinizde büyük
ihtimalle sadece `channel_server.py`'yi (ses kartı/SDR I/O ile) değiştirirsiniz
— `client.py`'deki protokol mantığı ve `modem.py`'deki modülasyon aynı kalabilir.

## modem.py hakkında dürüst bir not

Bu **gerçek** bir OFDM modemi - uydurma ton değil, gerçek IFFT/FFT ile
modüle/demodüle ediyor, gerçek bit hatalarına gerçekten maruz kalıyor. Ama
bilinçli basitleştirmeleri var:

- **Adaptif bit yükleme yok**: header her zaman BPSK (sağlam), veri bloğu
  BPSK ya da QPSK (sabit, `client.py`'de seçilen `mode`'a göre).
- **Kanal kestirimi/ekolayzır yok**: koruma aralığı (8ms) içindeki *hafif*
  çoklu-yol yankılarına (test edildi: ~-16dB kazanç, 7ms'ye kadar
  gecikme) dayanıklı, ama güçlü yankılarda (test edildi: -10dB kazanç,
  3ms+) ya da koruma aralığı dışına taşan gecikmelerde bilerek bozuluyor
  - kanal kestirimi eklemek doğal bir sonraki adım.
- **Kanal kodlaması (LDPC/RS) yok**: bütünlük sadece CRC32 ile
  denetleniyor, hata düzeltme üst katmandaki blok bazlı ARQ'ya
  bırakılıyor. Ölçülen davranış: temiz sinyalde %100, ~18dB SNR'ye kadar
  mükemmel, ~10-12dB civarında keskin bir "uçurum" (FEC'siz QPSK için
  beklenen davranış - tasarım sohbetimizdeki "COFDM cliff-edge" konusunu
  hatırlarsanız, tam olarak bunu görüyorsunuz).

## Kurulum ve çalıştırma

1. **Kanal sunucusunu başlatın** (1 terminal):
   ```
   python3 channel_server.py --port 6000
   ```
   Parametreler (artık gerçek ses bozulmaları):
   - `--snr 15`  → AWGN gürültü seviyesi (dB). Vermezseniz gürültü eklenmez
     (temiz kanal). Düşürdükçe (ör. 10-12dB) ARQ'nun devreye girdiğini
     göreceksiniz; çok düşükte (< ~8dB) hiçbir şey geçemez.
   - `--multipath-delay-ms 3 --multipath-gain 0.2`  → gerçek bir yankı
     ekler. Koruma aralığı 8ms - bunun altındaki gecikmeler (özellikle
     düşük kazançta) OFDM'in avantajını gösterir, üstündekiler bozulmayı.

   **Not:** Artık gerçek ses taşındığı için aktarım süreleri GERÇEK -
   eski `--speed` hızlandırma parametresi kaldırıldı. Master seçimi hâlâ
   gerçek ~24 saniye sürer (bu, `protocol.py`'deki bir tasarım
   parametresi, ses hızından bağımsız).

2. **(İsteğe bağlı ama önerilir) Kanalı dinleyin:**
   ```
   python3 monitor.py --port 6000
   ```
   Artık gerçekten alınan sesi çalıyor - kanalda gürültü/bozulma varsa
   onu da duyacaksınız. Ayrıca kendi demodülatörüyle çözmeyi dener ve
   kimin ne gönderdiğini metin olarak da loglar; çözemezse "çözülemedi"
   der (gerçek bir operatörün "bir şey duydum ama çözemedim" demesi gibi).
   Ek bağımlılık yok; Linux'ta `aplay`/`paplay`, Mac'te `afplay`,
   Windows'ta `winsound` otomatik kullanılır - hiçbiri yoksa sessizce
   metin günlüğüyle devam eder.

3. **4 ayrı terminalde istasyonları açın:**
   ```
   python3 client.py TA1ABC
   python3 client.py TA2DEF
   python3 client.py TA3GHI
   python3 client.py TA4JKL
   ```
   İsterseniz her istasyona farklı varsayılan modülasyon verin: `--mode BPSK`
   (sohbet zaten hep BPSK gider, bu bayrak sadece `/sendimage`/`/sendfile`
   ile gönderdiğiniz bulk transferlerin varsayılan modunu belirler).

4. Herhangi bir istasyon terminaline şu komutları yazabilirsiniz:

   | Komut | Açıklama |
   |---|---|
   | `/chat merhaba` | Ortak sohbete mesaj |
   | `/msg TA2DEF selam` | Özel mesaj (adreslenmiş, şifreli değil) |
   | `/sendimage test_files/grup_gorseli.bin` | Herkese görüntü/veri gönder |
   | `/sendimage test_files/grup_gorseli.bin TA3GHI` | Belirli birine gönder |
   | `/sendfile test_files/belge.bin TA4JKL` | Dosya transferi (özel) |
   | `/status` | Rol (LISTENER/MASTER/BACKUP), roster, transfer durumları |
   | `/drop` | **Ani kopmayı simüle et** (bağlantı kesilir, süreç kapanmaz) |
   | `/reconnect` | Yeniden bağlan, kaldığı yerden devam et |
   | `/quit` | Çık |

   `test_files/` klasöründe hazır iki veri dosyası var: `grup_gorseli.bin`
   (12 KB) ve `belge.bin` (45 KB) — gerçek bir görüntü/dosyanız olmasa da
   protokolü hemen test edebilirsiniz. **Not:** artık gerçek ses
   taşındığından, `belge.bin` gibi büyük dosyalar QPSK'de gerçekten
   dakikalar sürebilir (bkz. aşağıdaki "gerçek süreler" notu) - ilk
   denemede `grup_gorseli.bin` ya da daha küçük bir dosyayla başlamanızı
   öneririm.

## Önerilen test senaryoları

**1) Sesi duyun:** `monitor.py`'yi açık tutup başka bir terminalde
`/chat merhaba` yazın - kısa, gerçek bir OFDM patlaması duyacaksınız.

**2) Master seçimi:** Sadece server + 1 client açın, ~24 saniye bekleyin,
`/status` yazın — `rol=MASTER` görmelisiniz.

**3) Gürültüde ARQ:** Sunucuyu `--snr 13` ile başlatıp `/sendimage`
deneyin — bazı bloklar CRC hatası verecek, terminalde `X/Y blok eksik,
isteniyor` mesajlarını ve tekrar gönderim turlarını göreceksiniz.
`monitor.py` çalışıyorsa bu bloklar için "çözülemedi" dediğini de
duyacaksınız.

**4) Çoklu-yol (multipath) sınırını görün:** `--multipath-delay-ms 5
--multipath-gain 0.15` (koruma aralığı içinde, hafif) ile transfer
deneyin - başarılı olmalı. Sonra `--multipath-delay-ms 15` (koruma
aralığı dışına taşıyor) ile deneyin - başarısızlığı gözlemleyin. Bu, tam
olarak OFDM'i seçme gerekçemizdi.

**5) Ani kopma + devam etme:** Bir transfer başlatın, birkaç saniye sonra
gönderen tarafta `/drop` yazın, birkaç saniye sonra `/reconnect` yazın -
alıcının otomatik olarak sadece eksik blokları istediğini göreceksiniz.

**6) Master düşüşü:** Master rolündeki istasyonda `/drop` yazın, ~24
saniye bekleyin, yedek master'ın otomatik devraldığını doğrulayın.

## Gerçek süreler hakkında

Artık ses gerçek olduğu için süreler de gerçek - hızlandırma yok. Kaba
fikir: QPSK'de ~250 B/s, BPSK'de ~125 B/s (temiz kanalda). 12 KB'lık
`grup_gorseli.bin` QPSK'de ~50 saniye, 45 KB'lık `belge.bin` ~3 dakika
sürer - tasarım sohbetimizdeki hesaplarla tutarlı, çünkü artık gerçekten
o kadar veriyi gerçekten moduluyoruz.

## Bilinen sınırlamalar (bilinçli MVP kararları)

- Failover şu an sadece **atanmış yedek master**'a kadar zincirleniyor;
  yedek de düşerse üçüncü bir istasyonun devralması için roster'daki
  "sıradaki aktif istasyon" mantığını eklemek gerekir.
- `modem.py`'de adaptif bit yükleme, kanal kestirimi/ekolayzır ve
  LDPC/RS kanal kodlaması yok (yukarıda "dürüst not" bölümüne bakın) -
  bunlar doğal bir sonraki geliştirme adımı.
- Senkronizasyon, her aktarımı bağımsız bir "patlama" (burst) olarak
  işliyor (sürekli bir ses akışını herhangi bir noktada dinlemeye
  başlama değil) - bu, half-duplex kanalımızın doğasına uygun ama gerçek
  bir SDR alıcısı sürekli akan IQ örnekleriyle çalışacağından, o geçişte
  bu kısmın yeniden ele alınması gerekir.

## SDR'a geçiş için yol haritası

Artık `client.py` ve `modem.py` gerçek ses üretip/çözdüğü için, bir
sonraki adım aslında sandığınızdan daha küçük: `channel_server.py`'nin
"sesi TCP üzerinden JSON+base64 ile taşı" kısmını, gerçek bir ses
kartı/SDR I/O katmanıyla (ör. `sounddevice` ile mikrofon/hoparlör, ya da
bir SDR'ın IQ akışı) değiştirmeniz yeterli olabilir - `modem.py`'nin
`modulate()`/`demodulate()` fonksiyonları ve `client.py`'nin tüm
protokol mantığı büyük ölçüde aynı kalır.
