# Chunk tabanlı mob tracking gölge pilotu

Pilot, aynı dünyadaki her oyuncu için yatayda en fazla 48 blok uzakta bulunan mobların bit kümesini üretir. Bu, mob tracking’in aday seçimi aşamasıdır. Test eklentisi `src/test` altında yer alır ve ayrı JAR ile başlatılır. Purpur gerçek entity tracking ve paket gönderimini sürdürür.

| Yol | İşlem |
|---|---|
| Tam tarama | Java her oyuncuyu o dünyadaki tüm geçerli moblarla karşılaştırır. |
| Chunk indeksli Java | Java her tick mobları chunk tablosuna yerleştirir ve oyuncunun menziline değen chunk’ları sorgular. |
| Chunk indeksli Rust | Java aynı chunk tablosunu kurar. Rust, paylaşılan direct buffer’lar üzerinden dünya başına tek senkron JNI çağrısıyla aynı sorguyu çalıştırır. |

Her üç yol aynı tipte yeniden kullanılan `nativeOrder` tamponlara konumları yazar ve aynı oyuncu sıralı bit kümesini üretir. Ayrı `VERIFY` koşusu, her girdi için üç çıktının sayısını ve bitlerini birebir karşılaştırır. Negatif chunk koordinatları ile 48 blok sınırı ayrıca sınanır.

## Ölçüm düzeni

- Purpur 1.21.4 ve Java 21, 2 GiB sabit heap ve G1GC. Her JVM koşusu aynı kayıtlı ağır dünya kopyasından açılır. Windows CPU affinity, JVM ve botları ayrı fiziksel çekirdeklere yerleştirir. Ölçülen JVM CPU süresi o süreçte çalışan native kodu da kapsar.
- Yerel testte 20 bot spectator modunda durur. Yoğun senaryoda spawn’daki farmların yanında toplanırlar. Dağınık senaryoda dört kişilik beş grup mob kümelerine dağılır. Teleport senaryosunda 20 bot iki saniyede bir iki konum arasında taşınır.
- Hesap süresi, Paper üzerinden oyuncu ve mob toplama, koordinat yazma, varsa indeks kurma, sorgu ve JNI dahil süreyi ölçer. JVM CPU 20 saniyeye normalize edilir. Tick p99, Paper `ServerTickEndEvent` üzerinden tüm sunucu tick’inin süresidir.
- Botlar izole test kopyasında spectator moduna alınır ve test koordinatlarına taşınır. Tüm bağlantılar localhost üzerinden kurulur.
- Bağımsız ölçümde üç yöntem üç farklı sırada, toplam dokuz JVM’de çalıştırıldı. Her senaryo koşu başına 24 saniye ölçüldü. Ayrılan çekirdeklerde JVM dışı CPU kullanımı pencereler arasında %7,7–43,8’e yayıldı.
- Dönüşümlü ölçüm aynı JVM’de her yöntem için altı adet dört saniyelik pencere kullanır. Her yöntem ilk, orta ve son sırada ikişer kez yer alır. Bir üçlüde JVM dışı yükün en yüksek ve en düşük ölçümü arasında en fazla 5 yüzde puan varsa üçlü eşleşmiş kabul edilir. Tick p99 seçili pencerelerdeki bütün tick’lerden hesaplanır.

## Dönüşümlü ölçüm

Aynı JVM’de 54 ölçüm penceresinde 4.322 tick kaydedildi. 20 botun tamamı hazır kaldı ve erken kopma olmadı. Tüm pencere medyanlarında dünya başına 1.030 mob kaydedildi. Kaynak dünya dosyalarının 42 SHA-256 özeti bağımsız koşularla aynıydı.

Tablo arka plan yükü eşleşen üçlüleri kullanır. Parantezdeki değişimler aynı senaryodaki tam taramaya göredir. Toplam süre tick başına medyandır. CPU, pencere toplamlarından 20 saniyeye normalize edilmiştir.

| Senaryo | Yöntem | Tick örneği | Toplam µs/tick | JVM CPU-s / 20 sn | Tick p99 (ms) |
|---|---|---:|---:|---:|---:|
| Yoğun farm (1/6) | Tam tarama | 80 | 724,0 | 36,98 | 27,18 |
| | Chunk Java | 80 | 796,0 (+%9,9) | 41,15 (+%11,3) | 32,01 (+%17,7) |
| | Chunk Rust | 80 | 771,3 (+%6,5) | 35,08 (−%5,1) | 51,10 (+%88,0) |
| Dağınık (2/6) | Tam tarama | 161 | 761,5 | 33,70 | 28,80 |
| | Chunk Java | 160 | 688,2 (−%9,6) | 24,85 (−%26,3) | 21,65 (−%24,8) |
| | Chunk Rust | 160 | 695,0 (−%8,7) | 26,58 (−%21,1) | 25,74 (−%10,6) |
| Teleport (4/6) | Tam tarama | 320 | 701,6 | 14,93 | 31,24 |
| | Chunk Java | 320 | 697,0 (−%0,7) | 13,50 (−%9,6) | 25,60 (−%18,1) |
| | Chunk Rust | 320 | 726,8 (+%3,6) | 13,50 (−%9,6) | 27,90 (−%10,7) |

| Senaryo | Java indeksli sorgu | Rust sorgusu + JNI | Fark |
|---|---:|---:|---:|
| Yoğun farm | 50,6 µs | 66,6 µs | +%31,5 |
| Dağınık | 26,4 µs | 46,7 µs | +%76,7 |
| Teleport | 16,8 µs | 35,9 µs | +%113,7 |

Dağınık senaryo indeksleme için en iyi sonucu verdi. Rust’ın Java’ya göre uçtan uca farkı yoğun farmda −%3,1, dağınık senaryoda +%1,0 ve teleportta +%4,3 oldu. Tam taramanın saf sorgusu 27–41 µs sürerken uçtan uca maliyet 702–762 µs bandındaydı. Veri toplama ve tampon doldurma toplam süreye hâkimdi.

CPU ve tick p99 değerleri gözlemdir. Özellikle yoğun farmdaki 80 tick örneğinde p99 en uzun tick’e karşılık gelir. Arka plan yükü eşleşmesine rağmen bu ölçekteki CPU ve p99 farklarını sadece sorgu yöntemine bağlamak için veri yetersizdir. Purpur’un kendi tracker’ı ve ağ trafiği her ölçümde normal şekilde devam etti.

## Bağımsız JVM koşuları

Dokuz koşuda 12.989 tick kaydedildi. Tüm koşularda 20 bot hazırdı ve erken kopma sayısı sıfırdı. Ayrı `-Xcheck:jni` koşusunda 401 canlı girdi için üç aday kümesi eşleşti. Dünya kayıtlarının SHA-256 özetleri koşuların başında ve sonunda aynıydı.

Hücreler sırasıyla tam tarama, chunk indeksli Java ve chunk indeksli Rust değerlerini gösterir. Her değer üç koşu penceresinin medyanıdır. Bu tablodaki CPU ve p99 farkları değişken arka plan yükünden etkilenir.

| Senaryo | Toplam µs/tick | JVM CPU-s / 20 sn | Tick p99 (ms) |
|---|---:|---:|---:|
| Yoğun farm | 830,7 / 1.368,7 / 998,4 | 43,30 / 54,71 / 49,16 | 47,69 / 94,94 / 105,89 |
| Dağınık | 766,3 / 920,1 / 907,0 | 33,52 / 38,09 / 34,79 | 55,77 / 59,19 / 85,46 |
| Teleport | 739,4 / 693,6 / 875,8 | 18,04 / 17,73 / 23,72 | 47,08 / 49,13 / 79,11 |

## Tekrar çalıştırma

Windows’ta Python 3.12+, Java 21, Purpur 1.21.4 test dizini ve `world-heavy-v24-snapshot` gerekir. Test dizini Purpur’un önbelleğini, yan dünyaları ve ayarlarını içermelidir. `mvn -B clean package` komutunu Java proje dizininde, `cargo build --release -p spica-rust-native -p spica-bot-bench` komutunu repo kökünde çalıştır. Ardından aşağıdaki komutta JVM ve botlar için fiziksel çekirdek paylaşmayan maskeler ver.

```powershell
python benchmarks/chunk_tracking_shadow.py --seed .test-server --output $out `
  --java $java --native target/release/spica_rust_native.dll `
  --bots target/release/spica-bot-bench.exe --server-mask 0x3f --bot-mask 0xc0 `
  --interleave
```

`--output` yeni bir dizin olmalı. Dönüşümlü ölçümü tek başına çalıştırmak için `--rounds 0 --skip-verify` eklenebilir. Her koşu kendi stdout, bot raporu ve tick CSV’sini yazar. `results.json` dünya dosyalarının SHA-256 özetlerini ve pencere metriklerini içerir.
