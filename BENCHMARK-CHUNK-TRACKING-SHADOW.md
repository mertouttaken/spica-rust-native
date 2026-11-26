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

## Artımlı Rust pilotu

Karşılaştırmanın tabanı, `3abb845` sürümündeki tam mob görüntüsü ve her tick yeniden kurulan chunk indeksiyle çalışan Rust yoludur. Artımlı yol dünyayı ilk etkinleştiğinde bir kez tarar. Dünya yaşam döngüsü olayları Java’daki mob listesini günceller. Pilot her tick bu listedeki canlı mobların X/Z koordinatlarını okur, değişen konumları ve silinen kayıtları tek bir toplu JNI çağrısında gönderir. Oyuncu koordinatları her sorguda gönderilir.

Rust dünya başına chunk indeksini çağrılar arasında tutar. Chunk içi hareket konumu günceller, chunk değişimi kaydı ilgili kovaya taşır. Dünya kapanınca ya da pilot devreden çıkınca yerel indeks serbest bırakılır. Bitiş çıktısı aynı 48 blok aday kuralını kullanır. Ölçüm kopyasında spawn, chunk’lar arası mob teleportu ve ölüm de birebir eşitlik koşusuna dahil edilir. Gölge test eklentisi Paper’ın entity tracking ve paket gönderimini etkilemeden ölçüm alır.

Yukarıdaki komutta `--interleave` yerine `--incremental --rounds 0` vererek bu karşılaştırma çalıştırılabilir. Her senaryoda aynı JVM içinde sırayla A/B ve B/A olmak üzere sekiz çift alınır. Dört saniyelik her pencereden önce iki saniye ayrılır. Bu süre ilk indeks doldurma maliyetini ölçüm penceresinin dışında tutar. Tick başına toplam hesap sürelerine Java’daki yaşam döngüsü işleyicilerinin süresi de eklenir. İki penceresi de en az 40 tick içeren ve ortam yükü en fazla 5 yüzde puan ayrışan çiftler eşleşir.

`-Xcheck:jni` ile 347 canlı tick’te aday bitleri birebir eşleşti. Zorunlu mob teleportu 48 blok menzil sınırını aştı ve ölüm sonrası bir silme kaydı gözlendi. 42 SHA-256 özeti önceki pilotun aynı dünya kopyasını doğruladı.

20 botun hazır kaldığı ve erken kopma olmayan A/B tekrarında 48 pencere ile 3.820 tick kaydedildi. Tabloda ortam yükü eşleşen 9 çiftin 1.445 tick’i kullanıldı. Hücreler önce eski Rust pilotunu, ardından artımlı yolu gösterir. Mob satırı, tick başına medyandır ve konumu güncellenen kayıtları sayar. Ölçüm pencerelerinde mob silme kaydı oluşmadı.

| Senaryo | Eşleşen çift | Toplam µs/tick | Rust’a giren mob satırı/tick | JVM CPU-s / 20 sn | Tick p99 (ms) |
|---|---:|---:|---:|---:|---:|
| Yoğun farm | 3/8 | 1.352,9 → 664,1 (−%50,9) | 1.030 → 318 (−%69,1) | 62,50 → 63,05 (+%0,9) | 122,99 → 74,61 (−%39,3) |
| Dağınık | 2/8 | 859,9 → 356,6 (−%58,5) | 1.030 → 426 (−%58,6) | 39,92 → 31,83 (−%20,3) | 77,79 → 34,80 (−%55,3) |
| Teleport | 4/8 | 724,3 → 271,6 (−%62,5) | 1.030 → 359 (−%65,1) | 14,99 → 12,18 (−%18,8) | 31,85 → 30,28 (−%4,9) |

Artımlı yol toplam aday hesaplama süresini 0,45–0,69 ms/tick azalttı. Tüm pencereler dahil edildiğinde de düşüş üç senaryoda %51,6–69,0 aralığındaydı. Kazanç dünya taramasının ve Java’da indeksin baştan kurulmasının kalkmasıyla geldi. Rust tarafında güncelleme ve sorgu birlikte çalıştığından JNI çağrısının kendi süresi eşleşen pencerelerde 36–119 µs bandından 50–149 µs bandına çıktı.

JVM CPU ve tick p99 bütün sunucuyu kapsar. Örneğin dağınık senaryonun p99’u eşleşen iki çiftte %55,3 düştü, tüm sekiz çiftte %16,0 yükseldi. Bu örneklerle aday hesaplama süresindeki düşüş tekrarlanabilirdi. JVM CPU ve p99 farklarını artımlı indekse bağlamak için daha fazla eşleşen pencere gerekir.

## Yalnız değişen mobları okuyan test kancası

Stok Purpur 1.21.4-2416’da test mobu X=48,49’dan X=48,51’e aynı blok içinde taşındı. 20 botun tümü 48 blok menzilinin içinde gördüğü adayı bir sonraki tick’te kaybetti. Konum yazımı `EntityMoveEvent`, `EntityTeleportEvent`, dünyaya ekleme/çıkarma ve `ServerCommandEvent` bildirimlerinden hiçbirini üretmedi. Kontrol teleportunda normal teleport bildirimi alındı. Sadece bu olaylarla çalışan bir indeks 20 eski adayı tutardı.

`--movement --rounds 0` seçeneği, yalnızca benchmark JVM’inde sınıf özetleri sabitlenmiş bir Java agent yükler. Agent X/Z yazımı tamamlandığında hareket eden mobu bildirir. Sağlık, ölüm ve kaldırma değişimleri de aday üyeliğini yeniden değerlendirmek için bildirilir. Java her dünyayı ilk etkinleştiğinde tarar. Sıcak yolda tick başına bir kez yalnız bildirim alan ve yaşam döngüsü değişen kayıtları okur. Aynı mobun bir tick’teki birden çok bildirimi tek kuyruğa alınır. Rust, aynı kalıcı chunk indeksini ve aynı toplu JNI sözleşmesini kullanır.

A/B aynı test JVM’sinde `5a5966e` sürümünün konum tarama döngüsü ile bildirim kuyruğunu karşılaştırır. Tarama kolunda agent yüklenmiş ve bildirim tüketicisi kapalıdır. İki kollu önceki deneyle aynı eşleştirme kuralı kullanılır. Hesap süresine Java bildirimlerinin kuyruğa alınma süresi de eklenir. Agent ve denek eklentisi test çıktılarıdır. Üretim eklentisi aynı normal paketleme yolunu kullanır.

42 dünya dosyasının SHA-256 özeti `5a5966e` ölçümüyle aynıydı. Ayrı `-Xcheck:jni` koşusunda 585 canlı tick boyunca tam tarama, taramalı Rust indeksi ve yalnız değişen kayıtları okuyan Rust indeksi birebir aynı aday kümelerini üretti. Bu kontrol aynı blok içindeki bildirimsiz konum yazımını, chunk’lar arası teleportu ve mobun kaldırılmasını içerdi.

20 botun hazır kaldığı ve erken kopma olmayan A/B koşusunda 48 pencere ile 3.878 tick kaydedildi. Tabloda arka plan yükü eşleşen 13 çiftin 2.119 tick’i kullanıldı. Metinler önce `5a5966e` taramasını, ardından kuyruklu yolu gösterir.

| Senaryo | Eşleşen çift | X/Z okunan mob/tick | Hesap µs/tick, JNI dahil | JVM CPU-s / 20 sn | Tick p99 (ms) |
|---|---:|---:|---:|---:|---:|
| Yoğun farm | 4/8 | 1.030 → 313,5 (−%69,6) | 397,9 → 269,0 (−%32,4) | 46,83 → 32,95 (−%29,6) | 35,42 → 30,24 (−%14,6) |
| Dağınık | 6/8 | 1.030 → 414,5 (−%59,8) | 370,8 → 319,0 (−%14,0) | 26,17 → 29,07 (+%11,1) | 31,32 → 34,10 (+%8,9) |
| Teleport | 3/8 | 1.030 → 349 (−%66,1) | 337,4 → 247,4 (−%26,7) | 12,30 → 13,19 (+%7,2) | 38,33 → 45,24 (+%18,0) |

Toplam aday süresi tick başına 0,052–0,129 ms azaldı. Bütün pencerelerde de aday süresi %14,4–30,1 daha düşüktü. İki yol aynı değişiklikleri toplu gönderdiği için JNI’ye giren satır sayısı benzer kaldı. Kazanç Java’nın konum okumalarını sınırlamasından geldi. JVM CPU ve tick p99 iki senaryoda yükseldi. Bu ölçüm, aday hesabındaki kazancı gösterir ve sunucu genelinde tutarlı bir CPU veya p99 düşüşü göstermedi.

## Hareket bildirimlerinin sıcak yolu

`9449861` ham tick’lerinde dağınık senaryonun p99’u 31,3151 → 34,1017 ms iken bu iki tick’te ölçülen takip işi 0,4314 → 0,4319 ms idi. Teleportta karşılık gelen sayılar 38,3273 → 45,2420 ms ve 0,3683 → 0,3819 ms idi. Ölçülen takip işi çıkarıldığında p99 farkları sırasıyla 2,786 ve 6,901 ms kaldı. Teleportta eşleşen üç çiftin ikisinde JVM CPU düştü. Bir çiftteki %35,8 artış birleşik sonucun yönünü değiştirdi. Bu kayıtlar JVM’nin hangi alt sisteminin değişime yol açtığını belirlemeye yetmiyor.

Eski hareket tüketicisi her bildirimde bir hash araması ve iki `nanoTime()` çağrısı yapıyordu. İlk denemede hash aramasından önce ikinci bir kuyrukla tekilleştirme sınandı. Bu dünyada bildirim sayısı ve kuyruktaki mob sayısı pratikte aynı kaldı. Eşleşmiş dağınık pencerelerde ikinci kuyrukla JVM CPU %3,0 ve p99 %4,0 yükseldi. Bu yol son değişikliğe alınmadı.

Son yol, tamsayı entity kimliğinden mob yuvasına doğrudan dizinle erişiyor ve sıra dışı kimliklerde hash tablosunu kullanıyor. Yuva kuyruğu aynı mob için yinelenen bildirimleri topluyor. Bildirim başına saat okumaları kaldırıldı. Sabitlenen Purpur sürümünde `CraftEntity.isValid()` zaten `isAlive()` kontrolünü yaptığı için doğrudan yol ikinci `isDead()` çağrısını da kaldırıyor. İki yol aynı agent bayt kodunu, konum kaynağını, Rust indeksini ve JNI sözleşmesini kullanıyor.

`--hook-cost --rounds 0` ile `9449861`in bildirim yolu ve doğrudan yuva erişimi aynı JVM’de karşılaştırılır. Her senaryoda A/B ve B/A sırası dörder kez çalışır, pencere dört saniye sürer. Aday süreleri Java’nın tick başındaki işi ve JNI’yi kapsar. Bu ortak sayaç, tick içinde ayrı yerden çalışan bildirim maliyetini iki kolda da dışarıda bırakır. JVM ve ana iş parçacığı CPU ölçümleri bildirimleri de kapsar. GC toplam sayısı ve süresi ayrıca kaydedilir.

İlk tekrar giriş sırası yarışını yakaladı. `ShadowBot020`, üç bot daha bağlanmadan dünyaya girdi ve ilk yerleşim yalnızca 17 botu taşıdı. Koşucu artık 20 farklı giriş kaydını bekliyor ve toplu teleportların 20 oyuncuya uygulandığını doğruluyor. `9449861`in yayımlanan A/B kaydında ilk yerleşim zaten 20 botla yapılmıştı.

Ayrı JFR teşhis koşusunda ilk bot zaman aşımından önce 24 GC duraklaması kaydedildi. En uzunu 348,8 ms sürdü. Bu aralıkta iş parçacığı yükü örneklerinde Netty trafiği ve JIT derleyicileri de belirgindi. JFR açık koşu botlar koptuğu için performans tablosuna alınmadı. Son A/B’de profil kapalı, test JVM’sinin keep-alive zaman aşımı 120 saniye ve farklı çekirdeklere sabitlenen JVM ile bot sürecinin öncelikleri normalin üstündeydi. İki kol aynı ayarları kullandı.

42 dünya dosyasının SHA-256 özeti `9449861` ölçümüyle aynıydı. `-Xcheck:jni` ile 503 canlı tick’te aday kümeleri birebir eşleşti. A/B’de 20 botun tamamı hazır kaldı, erken kopma olmadı ve 460 keep-alive yanıtı alındı. 48 pencere ve 3.867 tick içinden arka plan yükü eşleşen 10 çiftin 1.615 tick’i tabloya girdi. Her hücre önce `9449861`, ardından doğrudan yuva yoludur.

| Senaryo | Eşleşen çift | Hash araması/tick | Tick başı JNI dahil (µs) | JVM CPU-s / 20 sn | Ana iş parçacığı CPU-s / 20 sn | Tick p99 (ms) |
|---|---:|---:|---:|---:|---:|---:|
| Yoğun farm | 4/8 | 305 → 0 | 323,75 → 312,30 (−%3,5) | 64,71 → 56,02 (−%13,4) | 13,77 → 12,03 (−%12,6) | 64,66 → 59,81 (−%7,5) |
| Dağınık | 2/8 | 421 → 0 | 337,60 → 333,15 (−%1,3) | 41,81 → 42,52 (+%1,7) | 11,79 → 12,83 (+%8,8) | 51,69 → 47,37 (−%8,4) |
| Teleport | 4/8 | 342 → 0 | 230,00 → 213,20 (−%7,3) | 18,74 → 19,32 (+%3,1) | 8,14 → 8,37 (+%2,7) | 64,37 → 60,40 (−%6,2) |

Eski kolda Java bildirim işleyicisinin ölçülen medyan süresi yoğun, dağınık ve teleport pencerelerinde sırasıyla 112,4, 135,6 ve 81,3 µs/tick idi. Buna per-bildirim zamanlaması eşlik ediyordu. Yeni kolda saat okumaları kaldırıldığı için bu sayaç yeni işleyicinin süresini ölçmüyor. Hash araması ve tick başındaki JNI dahil süre her senaryoda düştü. Eşleşen pencerelerde p99 gerilerken dağınık ve teleport JVM CPU’su yükseldi.

Teleportun sekizinci çiftinde yeni kol 246 ms GC süresi kaydetti. JVM CPU 12,25 → 25,76 CPU-s/20 sn ve ana iş parçacığı CPU’su 6,01 → 7,25 CPU-s/20 sn oldu. Bu CPU farkının yaklaşık %91’i ana iş parçacığı dışındaydı. Aynı çift iki taraftan çıkarıldığında kalan teleport pencerelerinde JVM CPU 20,90 → 17,17 (−%17,9) oldu. Bu duyarlılık kontrolüdür. Ana tabloda sekizinci çift de yer alır. Dağınık senaryoda yalnız iki çift eşleşti ve CPU farkının yönü çiftler arasında değişti.

## Uzun A/B ve JVM CPU dağılımı

`python -B` ile yukarıdaki koşucuya `--cpu-attribution --rounds 0` verildiğinde aynı `c8913f4` Java, agent ve Rust koduyla iki yol dönüşümlü çalışır. `DIRTY`, `9449861`in hash araması ve bildirim zamanlaması kullanan yoludur. `DIRECT`, `c8913f4`ün doğrudan yuva yoludur. Her senaryoda altı A/B çifti alınır ve her pencere 12 saniye sürer. Kol başına toplam 72 saniye, önceki 32 saniyenin 2,25 katıdır. Önceki deneydeki 5 yüzde puanlık arka plan eşleştirmesi kullanılır.

Koşucu pencere sınırlarında Windows `GetProcessTimes` ve `GetThreadTimes` sayaçlarının farkını alır. HotSpot `jcmd Thread.print`, iş parçacıklarının adını ve işletim sistemi kimliğini pencere dışında eşler. Kimliğe oluşturulma zamanı da eklenir. Açık tutulan tutamaçlar pencere içinde sonlanan iş parçacığının biriktirdiği CPU zamanını korur. Ana iş parçacığı, G1/GC çalışanları, `Netty Server IO`, JIT ve adı bilinen Paper çalışanları ayrı ölçülür. Diğer CPU, süreç toplamından bu gruplar çıkarılarak bulunur. Bu kalem VM koordinasyonunu, adı eşlenemeyen iş parçacıklarını ve sayaç sınırlarının kısa okuma farkını da kapsar. [Windows iş parçacığı CPU sayaçları](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getthreadtimes)

GC çalışanlarının CPU süresi ve JVM’nin GC için geçen süre sayacı ayrı verilerdir. Netty kalemi, adı `Netty Server IO` olan JVM iş parçacıklarının CPU süresidir. Süreç toplamı ağın ana iş parçacığında veya başka çalışanlarda yaptığı işi de kapsar. Bu koşuda JFR kaydı açılmaz. Ölçüm kodu koşucuya aittir. Kanca, pilot ve aday hesabı derlenen `c8913f4` kaynaklarından gelir.

Önceki deneyle aynı 42 dünya dosyası SHA-256 ile doğrulandı. Ayrı `-Xcheck:jni` koşusunda aday kümeleri 589 canlı tick boyunca eşleşti. Kontrol, API bildirimi gelmeyen aynı blok içi X/Z yazımını, teleportu ve mob kaldırmayı kapsadı. A/B koşusunda 20 botun tamamı hazır kaldı, kopma olmadı ve 760 keep-alive yanıtı alındı. 36 pencerede 8.702 tick ölçüldü. İlk tablo, arka plan yükü farkı en çok 5 yüzde puan olan 6 çiftin 2.908 tick’ini kullanır.

Her hücre `DIRTY` → `DIRECT` sırasındadır. CPU değerleri 20 saniye duvar süresine ölçeklenen CPU saniyeleridir. GC ve Netty sütunlarındaki parantezler mutlak CPU farkıdır.

| Senaryo | Eşleşen çift | JVM CPU | GC çalışanı CPU | Netty CPU | Tick p99 (ms) |
|---|---:|---:|---:|---:|---:|
| Yoğun farm | 2/6 | 58,81 → 55,98 (−%4,8) | 0,19 → 1,94 (+1,75) | 41,35 → 38,09 (−3,25) | 75,25 → 65,27 (−%13,3) |
| Dağınık | 3/6 | 36,42 → 33,57 (−%7,8) | 2,25 → 1,03 (−1,22) | 23,28 → 21,75 (−1,52) | 44,90 → 47,90 (+%6,7) |
| Teleport | 1/6 | 15,45 → 15,86 (+%2,7) | 0,13 → 0,00 (−0,13) | 7,15 → 7,10 (−0,05) | 44,35 → 40,34 (−%9,0) |

Yoğun farmda Netty’deki 3,25 CPU-s düşüş, GC’deki 1,75 CPU-s artışa rağmen süreç toplamını aşağı çekti. Dağınık senaryoda Netty ve GC birlikte 2,75 CPU-s daha az çalıştı. Bu, eşleşen pencerelerdeki 2,85 CPU-s toplam düşüşün yaklaşık %96’sıydı. Teleportun tek eşleşen çiftinde en büyük artış JIT derleyicilerindeydi (+0,42 CPU-s). Ana iş parçacığı +0,16 CPU-s, GC ve Netty birlikte −0,18 CPU-s değişti. Bu çiftte toplam CPU artarken p99 düştü.

JVM GC süre sayacı, eşleşen pencerelerde yoğun farm için 42 → 95 ms, dağınık için 196 → 130 ms, teleport için 21 → 0 ms kaydetti. İş parçacığı kataloğu 11 GC ve dört Netty iş parçacığını tanıdı. Süreç ve iş parçacığı sayaçlarının bir sınırdaki birlikte okunma süresi en çok 1,96 ms idi.

| Tüm pencereler | JVM CPU | GC çalışanı CPU | Netty CPU | Tick p99 (ms) |
|---|---:|---:|---:|---:|
| Yoğun farm | 52,69 → 49,74 (−%5,6) | 0,71 → 1,62 | 37,96 → 35,35 | 60,29 → 48,70 (−%19,2) |
| Dağınık | 34,54 → 34,76 (+%0,6) | 1,31 → 1,66 | 22,79 → 22,51 | 47,15 → 46,76 (−%0,8) |
| Teleport | 16,14 → 16,00 (−%0,8) | 0,12 → 1,34 | 7,05 → 6,84 | 47,56 → 41,03 (−%13,7) |

Yoğun farmda toplam CPU ve p99 düşüşünün yönü iki tabloda da aynı. Dağınık senaryoda CPU ve p99 farkının, teleportta da CPU farkının yönü pencere seçimiyle değişiyor. Teleport p99’u iki tabloda da düşük, ancak tek eşleşen çiftle etkinin büyüklüğü belirsiz. İş parçacığı dağılımı bu uzun koşudaki farkın bileşenlerini gösteriyor. Önceki koşudaki 246 ms GC kaydı geçmişe dönük olarak bu gruplara ayrıştırılamıyor.

## Netty CPU ve paket hacmi

`--packet-attribution --rounds 0` seçeneği `b7eab50`’deki aynı `DIRTY` ve `DIRECT` yollarını tek Purpur JVM’sinde karşılaştırır. Senaryo başına on çift alınır. Her çiftin iki kolu da sekiz saniye çalışır, A/B ve B/A sırası dönüşümlüdür. JVM `0x3f`, 20 bot `0xc0` maskesine sabitlenir. Eşleştirme için her iki işlemci kümesinde de ilgili benchmark sürecinin CPU’su çıkarılır. Bir çift, iki kümede de kalan arka plan yükü farkı en çok 5 yüzde puansa eşleşir.

Koşucu pencere sınırlarında bot istemine `snapshot` yazar. Bot, mevcut birikimli Minecraft Play paket, ağ baytı ve entity paket sayaçlarını döndürür. Sayaçlar, hazır botların sunucudan alıp ayrıştırdığı Minecraft çerçevelerini ve çerçeve başlıkları dahil ağ baytlarını kapsar. Bu ölçümde iki kola da aynı bot derlemesi yük verir. Paket başına Netty CPU, ölçülen Netty I/O CPU/s değerinin paket/s değerine bölümüdür. Bu ortalamaya paket türü, boyutu, gruplama ve iş parçacığı zamanlaması da yansıyabilir.

Paket isteği iş parçacığı okumasının hemen ardından gönderilir. Arka plan sayaçları bu iki okumanın dışında alınır. Paket okuma aralığının iki ucuna olan uzaklığın üst sınırı 100 ms’yi aşarsa iki kol da özetten çıkarılır. Bu eşik sekiz saniyelik pencerenin %1,25’idir. Ham sayaçlar her pencere için korunur. İlk denemede botların sabit ölçüm süresi son teleport çifti bitmeden doldu. Tam koşuda koşucu son tick kaydı yazıldıktan sonra `finish` komutuyla bot ölçümünü kapatır.

Netty CPU/s, paket/s ile ortalama CPU/paket çarpımı olarak ayrıştırılır. Hacim etkisi paket hızındaki farkın iki kolun ortalama paket maliyetiyle çarpımıdır. Birim maliyet etkisi, paket maliyetindeki farkın iki kolun ortalama paket hızıyla çarpımıdır. İkisi bir araya geldiğinde gözlenen Netty CPU farkını verir. Bu hesap trafik hacmi ile normalize edilmiş maliyeti ayırır. Maliyet değişiminin hangi Netty fonksiyonundan kaynaklandığını belirlemek için işlev seviyesinde ayrı bir profil gerekir.

Üç koşuda kullanılan 42 dünya dosyasının SHA-256 özeti `b7eab50` ölçümüyle aynıydı. Ayrı `-Xcheck:jni` doğrulamalarında tam tarama ile `DIRTY` 397, `DIRECT` 521 canlı tick boyunca aynı aday kümelerini üretti. Her doğrulama API bildirimi gelmeyen aynı blok içi X/Z değişimini ve yaşam döngüsü deneğini içerdi. Üretim JAR’ı benchmark kancası ve denek sınıfları açısından kontrol edildi.

Tam A/B’de 20 botun tümü hazır kaldı, kopma olmadı ve 900 keep-alive yanıtı alındı. 60 pencere boyunca 9.893 tick kaydedildi. Tüm pencereler zamanlama eşiğini geçti. Sayaçlar arasındaki uzaklığın en yüksek üst sınırı 74,96 ms oldu. Arka plan yükü eşleşen 16 çiftin 5.300 tick’i aşağıdaki tablolara girdi. Eşleşen çiftlerde arka plan yükü farkı JVM kümesinde en çok 3,00, bot kümesinde 4,87 yüzde puandı.

Hücreler `DIRTY` → `DIRECT` sırasındadır. Netty CPU ve iki etki 20 saniye duvar süresine ölçeklenmiş CPU saniyeleridir.

| Senaryo | Eşleşen çift | Paket/sn | Netty CPU | Netty µs/paket | Ağ baytı/paket |
|---|---:|---:|---:|---:|---:|
| Yoğun farm | 8/10 | 317.289 → 312.639 (−%1,47) | 33,76 → 33,59 (−%0,50) | 5,320 → 5,372 (+%0,98) | 11,400 → 11,416 |
| Dağınık | 6/10 | 234.326 → 234.959 (+%0,27) | 27,64 → 27,03 (−%2,21) | 5,898 → 5,753 (−%2,47) | 10,787 → 10,801 |
| Teleport | 2/10 | 37.226 → 37.159 (−%0,18) | 8,64 → 7,75 (−%10,30) | 11,604 → 10,428 (−%10,14) | 24,420 → 24,547 |

| Senaryo | Paket hacmi etkisi | Birim maliyet etkisi | Netty CPU farkı | JVM CPU-s / 20 sn | Tick p99 (ms) |
|---|---:|---:|---:|---:|---:|
| Yoğun farm | −0,497 | +0,328 | −0,169 | 46,27 → 47,43 (+%2,51) | 55,59 → 61,68 (+%10,95) |
| Dağınık | +0,074 | −0,685 | −0,611 | 43,73 → 42,88 (−%1,96) | 94,25 → 100,59 (+%6,73) |
| Teleport | −0,015 | −0,875 | −0,890 | 25,12 → 19,65 (−%21,76) | 77,59 → 72,93 (−%6,01) |

Yoğun farmda hacim azaldı, paket başına CPU arttı. Kalan %0,50 CPU düşüşü tüm pencereler alındığında %0,51 artışa döndü. Önceki %7,9 Netty CPU düşüşü bu koşuda tekrarlanmadı. Dağınık senaryoda altı çiftin dördünde Netty CPU azaldı. Birleşik düşüşün kaynağı paket başına CPU’ydu ve tüm pencerelerde Netty CPU %2,23 düştü. Teleportta eşleşen iki çiftte de paket başına CPU aşağıdaydı. Tüm pencereler alındığında Netty CPU düşüşü %5,24 oldu. İki eşleşen çift, teleport etkisinin büyüklüğünü genellemek için sınırlı. Paketlerin ortalama ağ boyutu üç senaryoda da %0,53’ten az değişti.
