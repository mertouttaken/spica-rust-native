# spica-rust-native — Proje Dokümanı

> **Vizyon:** Minecraft 1.21.4 **Purpur** sunucusunun %100 eklenti uyumluluğunu koruyarak, başlatma/yönetim ve ağır hesaplama beynini **Rust**’a taşıyan yüksek performanslı hibrit sunucu platformu.  
> **Kısa Özet:** *Çekirdek Purpur, Beyin Rust.*

---

## 1. Projenin Amacı

Standart Minecraft Java sunucuları (Paper / Purpur), tek thread (Main Thread) mimarisi, nesne tabanlı bellek düzeni (kötü CPU cache locality) ve Garbage Collector (GC) baskısı nedeniyle yüksek oyuncu ve mob yoğunluğunda MSPT sıçramaları ve TPS düşüşleri yaşar. Öte yandan sıfırdan Rust ile yazılan sunucular mevcut Bukkit/Paper/Purpur eklenti ekosistemini desteklemez.

**`spica-rust-native` bu iki dünyanın en iyi yönlerini birleştirir:**
- **Mevcut tüm 1.21.4 Purpur/Paper eklentileri %100 çalışmaya devam eder.**
- **Sunucu başlatma, JVM/GC optimizasyonu, canlı telemetri, adaptif MSPT yönetimi ve crash watchdog** tamamen Rust (`spica-launcher`) tarafından yürütülür.
- **Yoğun entity çarpışma, AI hedef/LOD, chunk yoğunluk heatmap, sürü (Boids) AI ve eşya birleştirme planlaması** sıfır kopyalı off-heap bellek üzerinden Rust (`spica-native` + Rayon + $O(N)$ Spatial Hash) ile çok çekirdekli olarak hesaplanır.

---

## 2. 3 Katmanlı Mimari

| Katman | Modül | Dil / Teknoloji | Görevi |
| :--- | :--- | :--- | :--- |
| **1. Başlatıcı & Beyin Daemon** | `spica-launcher` | Rust 2021 + Tokio + `sysinfo` | Donanımı ölçer, optimal ZGC / Aikar G1GC bayrakları üretir, Purpur’u yönetir, NDJSON IPC ile adaptif beyin direktifleri gönderir ve sıfır JVM yüküyle HTTP `/status` & `/metrics` sunar. |
| **2. İcra Gövdesi (Plugin)** | `spica-rust-native-plugin` | Java 21 + Paper/Purpur 1.21.4 API | Rust Beynin direktiflerini güvenli biçimde dünyalara uygular (dinamik AI LOD, cooldown korumalı view/sim distance, NBT korumalı eşya birleştirme) ve diğer eklentilere `SpicaRustAPI` sunar. |
| **3. In-Process Native Motor** | `spica-native` | Rust 2021 + Rayon + Zero-Copy JNI / C ABI | `DirectByteBuffer` üzerinden 0 kopya ve 0 GC ile $O(N)$ Spatial Hash, AI hedef/LOD, Chunk Heatmap, Boids AI, Item Merge planlayıcı ve in-process Governor çalıştırır. |

### Adaptif Beyin Durum Makinesi (`GREEN` → `EMERGENCY`)
Rust Beyin, Purpur’dan gelen canlı MSPT/TPS verisini EWMA ($\alpha = 0.25$) ve 2-tick histerezis ile işler:
- **`GREEN (0)`**: Maksimum görüş/simülasyon mesafesi, tam AI menzili.
- **`YELLOW (1)` / `ORANGE (2)`**: Yumuşak view/sim mesafesi ve AI menzili ölçeklemesi + Rust destekli eşya yığını birleştirme.
- **`RED (3)` / `EMERGENCY (4)`**: Agresif AI uyutma, minimum güvenli görüş/simülasyon mesafesi ve yoğun eşya birleştirme ile 20 TPS’i koruma.

---

## 3. Ölçülmüş Performans Sonuçları (Purpur 1.21.4 A/B Testi)

Test ortamı: **Windows x86_64, 8 çekirdek, OpenJDK 21, 2 GB RAM, 809 kalıcı AI mob + ~240 eşya (1.052 entity), 169 force-loaded chunk**.

| Ölçüm | Rust’suz Saf Purpur | `spica-rust-native` ile | İyileşme |
| :--- | :---: | :---: | :---: |
| **Açılış Süresi (`Done`)** | `19.04 s` | `18.16 s` | **%4.6 daha hızlı** |
| **İlk Pik (Max Tick Spike)** | `51.0 ms` | `14.3 ms` | **%72.0 daha düşük spike** |
| **Oturmuş Ortalama MSPT** | `4.03 ms` | `2.47 ms` | **%38.7 daha düşük MSPT** |
| **1 Dakikalık Kümülatif MSPT** | `4.5 ms` | `3.1 ms` | **%31.1 daha düşük MSPT** |
| **5.000 Entity Çarpışma (JVM içi)** | `79.129 ms` (Java) | `0.915 ms` (Rust) | **86.5x daha hızlı** |
| **5.000×100 AI Hedef + LOD** | `1.036 ms` (Java) | `0.514 ms` (Rust) | **2.0x daha hızlı** |
| **Canlı Chunk Heatmap (1.033 entity)** | — | `0.487 ms` (Rust) | Tek tick’in <%1’i |

---

## 4. Dizin Yapısı

```text
spica-rust-native/
├── Cargo.toml                  # Kök Rust workspace (fat LTO, opt-level=3)
├── PROJECT.md                  # Proje amacı, mimari, benchmarklar ve yol haritası
├── AGENTS.md                   # Geliştirme ve otomatik git commit/push kuralları
├── README.md                   # Hızlı kurulum, derleme ve komut rehberi
├── build-windows.ps1           # Windows x86_64 tek komut build (.exe + .dll + .jar)
├── build-linux.sh              # Linux x86_64 / aarch64 üretim build (.so + .jar)
├── Dockerfile.linux            # Docker ile temiz Linux x86_64 paketleme
├── spica-native/               # In-process Rust beyin motoru (cdylib + bench)
├── spica-launcher/             # Rust başlatıcı, supervisor, IPC & HTTP metrik daemon
└── spica-rust-native-plugin/   # Purpur 1.21.4 Java 21 eklentisi ve SpicaRustAPI
```

---

## 5. Hedef Platformlar & Kullanım Akışı

- **Geliştirme / Yerel Test:** Windows x86_64 (`.\build-windows.ps1`)
- **Üretim Sunucuları:** Linux x86_64 / aarch64 (`./build-linux.sh` veya `Dockerfile.linux`)
- **Özel Eklenti Entegrasyonu:** Diğer Purpur eklentileri `Bukkit.getServicesManager().load(SpicaRustAPI.class)` üzerinden Rust çekirdeklerini doğrudan çağırabilir.

---

## 6. Geliştirme Kuralları

1. **Otomatik Commit & Push Kuralı:** Kullanıcı tarafından istenen her görev tamamlandığında (ve ilgili derleme/test kontrolleri geçtiğinde) değişiklikler mutlaka açıklayıcı bir commit mesajıyla `git commit` edilip `origin main` dalına `git push` yapılır (bkz. `AGENTS.md`).
2. **Sıfır Kopya & Sıfır GC:** Sıcak tick yollarında (`EntityBatchArena`) yeni JVM nesne tahsisi yapılmaz, `ByteOrder.nativeOrder()` zorunludur.
3. **FFI Güvenliği:** Hiçbir Rust `panic!` JNI/C sınırını geçemez (`catch_unwind` zorunludur); tüm `DirectByteBuffer` kapasite ve hizalamaları doğrulanır.
4. **Geri Alınabilirlik:** Eklenti devre dışı kalırken veya reload edilirken uyutulmuş moblar ve değiştirilmiş dünya görüş/simülasyon mesafeleri eksiksiz geri yüklenir.

---

## 7. Yol Haritası (Roadmap)

- [x] Zero-copy JNI + C ABI in-process Rust motoru (`spica-native`)
- [x] $O(N)$ Spatial Hash çarpışma, AI hedef/LOD, Boids, Chunk Heatmap ve Item Merge planlayıcı
- [x] Rust başlatıcı + ZGC/Aikar profilleyici + Brain IPC + HTTP `/status` & `/metrics` (`spica-launcher`)
- [x] Purpur 1.21.4 A/B performans testi ve doğrulama
- [ ] Linux x86_64 / aarch64 CI (GitHub Actions) ile otomatik çift platform `.jar` (hem `.dll` hem `.so` gömülü)
- [ ] Web panel / Grafana hazır Prometheus dashboard şablonu
- [ ] Özel chunk/şematik toplu blok hesaplayıcı ve Rust tabanlı anti-bot/paket ön filtresi
