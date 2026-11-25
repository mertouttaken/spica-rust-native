# SpicaRustNative v2.0 — Purpur 1.21.4 Çekirdek + Rust Beyin Mimarisi

> **Çekirdek (Gövde):** Purpur / Paper 1.21.4 (tüm eklentilerle %100 uyumlu)  
> **Beyin & Başlatıcı:** Rust (`spica-launcher` + `spica-native`)

Bu proje klasik `.bat`/`.sh` başlatıcısını ve ağır Java döngülerini **3 katmanlı Rust Beyin mimarisi** ile değiştirir:

```
┌──────────────────────────────────────────────────────────────────────┐
│  1) spica-launcher (Rust Başlatıcı, Supervisor & Brain Daemon)       │
│  • Donanımı (CPU/RAM) otomatik ölçer, optimal ZGC / Aikar bayrakları │
│    üretir ve Purpur 1.21.4'ü çocuk süreç olarak yönetir              │
│  • Canlı MSPT/TPS telemetrisini NDJSON IPC ile alır, EWMA +          │
│    histerezis ile dinamik direktif (view/sim/AI LOD/item-merge) yollar│
│  • Crash watchdog + interaktif konsol (:brain, :stats, :restart)     │
│  • Sıfır JVM yüküyle HTTP /status (JSON) & /metrics (Prometheus)     │
└───────────────────────────────┬──────────────────────────────────────┘
                                │ Localhost Token'lı NDJSON IPC (Async)
┌───────────────────────────────▼──────────────────────────────────────┐
│  2) SpicaRustNative Plugin (Purpur 1.21.4 İcra Gövdesi)              │
│  • Rust Beynin gönderdiği direktifleri dünyalara güvenle uygular     │
│    (dinamik view/sim distance, AI LOD, güvenli eşya birleştirme)     │
│  • Launcher olmadan başlatılırsa otomatik in-process Rust beynine    │
│    düşer (sıfır kesinti)                                             │
└───────────────────────────────┬──────────────────────────────────────┘
                                │ Zero-Copy DirectByteBuffer (0 GC/tick)
┌───────────────────────────────▼──────────────────────────────────────┐
│  3) spica-native (.dll / .so — In-Process Rust Hesaplama Motoru)     │
│  • Flat CSR O(N) Spatial Hash Grid + Rayon çok çekirdekli havuz      │
│  • Çarpışma/yoğunluk, AI hedef + LOD, Boids sürü AI, O(N) Chunk      │
│    Heatmap, Item-Merge planlayıcı ve in-process Governor             │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 1. Windows'ta Yerel Test İçin Derleme (`.exe` + `.dll` + `.jar`)

Araçlar kurulu değilse tek seferde kur (kurulum sonrası terminali yeniden aç):

```powershell
winget install Rustlang.Rustup
winget install EclipseAdoptium.Temurin.21.JDK
winget install Apache.Maven
```

Tek komutla tüm workspace’i (native engine + launcher + plugin) derle ve benchmark çalıştır:

```powershell
cd spica-rust-native
.\build-windows.ps1 -RunBench
```

Çıktılar `spica-rust-native/dist/windows-x86_64/` içine toplanır:
- `spica-launcher.exe` — Purpur klasörüne (`purpur-1.21.4.jar` yanına) koy
- `SpicaRustNative-2.0.0.jar` — `plugins/` klasörüne at (içinde `spica_rust_native.dll` gömülü gelir)

### Başlatma (Windows Test)
```powershell
# İlk kez yapılandırma oluşturmak için:
.\spica-launcher.exe --init

# Donanım analizi + üretilen optimal JVM bayraklarını görmek için (dry-run):
.\spica-launcher.exe --dry-run

# Purpur'u Rust Beyin yönetiminde başlatmak için:
.\spica-launcher.exe
```

Başlatıcı çalışırken konsoldan Purpur komutlarının yanı sıra Rust Beyin komutlarını yazabilirsin:
- `:brain` — Canlı beyin durumu (`GREEN`..`EMERGENCY`), EWMA MSPT ve aktif direktif
- `:stats` — Host CPU/RAM + JVM Heap + TPS/Mob istatistikleri
- `:restart` — Purpur’u güvenle durdurup yeniden başlatır
- `:stop` — Purpur’u güvenle kapatıp supervisor’dan çıkar

Canlı HTTP metrikleri (sıfır JVM yükü):
- `http://127.0.0.1:9120/status` (JSON)
- `http://127.0.0.1:9120/metrics` (Prometheus)

---

## 2. Linux Üretim Sunucusu İçin Derleme

### Doğrudan Linux Sunucuda
```bash
cd spica-rust-native
chmod +x build-linux.sh
./build-linux.sh
```

### Windows'tan Docker ile Linux Çıktısı Üretme
```powershell
cd spica-rust-native
docker build -f Dockerfile.linux --output type=local,dest=./dist/linux-x86_64 .
```

---

## 3. Sunucu İçi Komutlar (`/rustbridge`)

| Komut | Açıklama |
| :--- | :--- |
| `/rustbridge brain` | Rust Beyin modu (`LAUNCHER IPC` / `IN-PROCESS`), durum (`GREEN`–`EMERGENCY`), EWMA MSPT, canlı view/sim distance, AI LOD ve birleştirilen eşya sayısını gösterir. |
| `/rustbridge chunks [dunya]` | Rust $O(N)$ Chunk Heatmap ile en yoğun ve en çok sıkışık mob barındıran Top 8 chunk’ı koordinatlarıyla listeler. |
| `/rustbridge status` | Native motor, Rayon worker sayısı ve AI LOD istatistiklerini gösterir. |
| `/rustbridge scan [dunya]` | Canlı dünyadaki mob-farm sıkışma noktalarını Rust Spatial Hash ile bulur. |
| `/rustbridge bench [entity] [oyuncu] [iter]` | **Async** olarak Saf Java vs Rust Native MSPT karşılaştırması yapar. |
| `/rustbridge reload` | `config.yml` yeniler ve uyutulan mobları güvenle uyandırır. |
