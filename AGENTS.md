# AGENTS.md — spica-rust-native Geliştirme Kuralları

Bu dosyadaki kurallar `spica-rust-native` deposu üzerinde çalışan tüm ajanlar ve geliştirme oturumları için zorunludur.

## 1. Zorunlu Git Commit & Push Kuralı

- **Kullanıcının istediği her görev veya değişiklik tamamlandığında, mutlaka `git add`, açıklayıcı bir commit mesajıyla `git commit` ve `git push` çalıştır.**
- Commit mesajlarında Conventional Commits biçimi kullan (`feat:`, `fix:`, `perf:`, `docs:`, `refactor:`, `chore:`).
- `target/`, `dist/`, `.test-server/` ve derleme çıktılarını (`.gitignore` kapsamındaki dosyalar) asla repoya commit etme.
- Push öncesinde ilgili doğrulamaları çalıştır:
  - Rust değişikliklerinde: `cargo test --workspace --release`
  - Java değişikliklerinde: `mvn -B clean package` (veya `.\build-windows.ps1` / `./build-linux.sh`)

## 2. Mimari ve Güvenlik Kuralları

- **Çekirdek Purpur, Beyin Rust:** Ağır toplu hesaplamalar, donanım profilleme, adaptif MSPT kararları ve metrik servisi Rust tarafında (`spica-native` / `spica-launcher`) kalmalıdır; Purpur eklentisi yalnızca güvenli icra gövdesi (actuator) olarak çalışır.
- **JNI & IPC Sözleşme Bütünlüğü:**
  - `RustNative.java` içindeki tüm `native` metodlar `spica-native/src/jni_bridge.rs` içindeki `Java_net_spicanetwork_rustnative_nativebridge_RustNative_*` sembolleri ve parametre sayılarıyla birebir eşleşmelidir.
  - `BrainIpcClient.java` ile `spica-launcher/src/{ipc,brain}.rs` arasındaki `hello` / `telemetry` / `directive` NDJSON alan adları (`camelCase`) senkron tutulmalıdır.
- **Sıfır Kopya & Sıfır GC:** Off-heap `DirectByteBuffer` tamponları daima `ByteOrder.nativeOrder()` ile ayrılmalı ve sıcak döngülerde yeniden kullanılmalıdır.
- **Panic-Safe FFI:** Her `extern "system"` ve `extern "C"` giriş noktası `std::panic::catch_unwind` ile sarılmalı, tüm buffer kapasiteleri/hizalamaları doğrulanmalıdır.
- **Güvenli Sunucu Durumu:** Main Thread asla bloklayıcı ağ/disk I/O yapmamalı; `onDisable()` / reload sırasında uyutulan moblar (`setAware(true)`) ve değiştirilen `viewDistance`/`simulationDistance` değerleri mutlaka geri yüklenmelidir.
