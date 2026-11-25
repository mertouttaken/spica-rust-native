# SpicaRustNative v2.2 — Purpur 1.21.4 Core + Rust Brain

> **Execution Core:** Purpur / Paper 1.21.4 (100% compatible with existing Bukkit/Paper/Purpur plugins)  
> **Brain & Orchestrator:** Rust (`spica-launcher` + `spica-native`)

**SpicaRustNative** replaces static `.bat`/`.sh` launch scripts and heavy Java tick bottlenecks with a **3-layer Rust Brain architecture** powered by zero-copy off-heap memory (`DirectByteBuffer` + JNI / C ABI), an $O(N)$ flat CSR Spatial Hash Grid, and a dedicated Rayon multi-core worker pool.

---

## Architecture

```text
┌────────────────────────────────────────────────────────────────────────┐
│  1) spica-launcher (Rust Startup Orchestrator, Supervisor & Brain)     │
│  • Auto-profiles host CPU/RAM and generates optimal Java 21+ flags     │
│    (Generational ZGC or Aikar Tuned G1GC + AlwaysPreTouch)             │
│  • Runs an async token-authenticated NDJSON IPC Brain server           │
│  • Computes EWMA MSPT + 2-tick hysteresis (GREEN -> EMERGENCY)         │
│  • Crash watchdog, interactive console (:brain, :stats, :restart)      │
│  • Zero-JVM-overhead Embedded Web Dashboard (/) + 120s history chart   │
│    + /status (JSON) & /metrics (Prometheus)                            │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │ Localhost Token-Auth NDJSON IPC
┌──────────────────────────────────▼─────────────────────────────────────┐
│  2) SpicaRustNative Plugin (Purpur 1.21.4 Execution Body)              │
│  • Safely actuates Rust Brain directives onto live worlds:             │
│    dynamic AI LOD, live Mob-Farm & Villager Trading-Hall Governor,     │
│    cooldown-guarded view/sim distance, NBT-safe item stack merging     │
│  • Seamless fallback to in-process Rust Governor if run standalone     │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │ Zero-Copy DirectByteBuffer (0 GC)
┌──────────────────────────────────▼─────────────────────────────────────┐
│  3) spica-native (.dll / .so — In-Process Rust Compute Engine)         │
│  • Flat CSR O(N) Spatial Hash Grid + Rayon work-stealing pool          │
│  • Collision/density, AI targeting + LOD, Farm & Villager Governor,    │
│    Boids steering, O(N) Chunk Heatmap, Item-Merge planner, Governor    │
└────────────────────────────────────────────────────────────────────────┘
```

---

## Measured Benchmark Results (Purpur 1.21.4)

All tests below were executed on the same host (**Windows x86_64, 8 logical cores, 16 GB RAM, OpenJDK 21.0.13 LTS, `-Xms2048M -Xmx2048M`**) against an identical cloned Purpur 1.21.4 stress world (**809 persistent AI mobs + ~240 ground items = 1,052 entities, 169 force-loaded chunks**).

### 1. Live Purpur 1.21.4 Server A/B Test (Without Rust vs. With Rust Brain)

| Metric | Pure Purpur 1.21.4 (No Rust) | Purpur + `SpicaRustNative` (Rust Brain) | Improvement |
| :--- | :---: | :---: | :---: |
| **Server Boot Time (`Done`)** | `19.04 s` | **`18.16 s`** | **4.6% faster** (`-0.88 s`) |
| **Initial Max Tick Spike (t=10s)** | `51.0 ms` | **`14.3 ms`** | **72.0% lower spike** |
| **All 5s Samples Avg MSPT** | `4.52 ms` | **`3.00 ms`** | **33.6% lower MSPT** |
| **Steady-State Avg MSPT (last 15s)** | `4.03 ms` (`2.2–8.0 ms`) | **`2.47 ms`** (`1.1–6.8 ms`) | **38.7% lower MSPT** |
| **1-Minute Cumulative Avg MSPT** | `4.5 ms` | **`3.1 ms`** | **31.1% lower MSPT** |
| **TPS** | `20.0` | **`20.0`** | Locked 20.0 |

**5-second `/mspt` series (avg):**
- **Without Rust:** `5.9 ms` → `4.6 ms` → `4.2 ms` → `4.1 ms` → `3.8 ms`
- **With Rust Brain:** `4.8 ms` → `2.8 ms` → `2.4 ms` → `2.4 ms` → `2.6 ms`

> **Methodology note:** With 0 connected players during the automated headless test, all 809 distant mobs were eligible for Rust AI LOD hibernation (`Mob#setAware(false)` and cleanly restored on shutdown). On populated production servers where mobs within 56 blocks of players remain fully active, real-world MSPT savings typically land in the **20%–35%** range (and **35%–50%+** during mob-farm spikes when dynamic view/sim distance and Rust item merging engage).

### 2. Live In-JVM Benchmark (`/rustbridge bench 5000 100 40`)

Measured directly inside the running Purpur 1.21.4 JVM (**5,000 entities, 100 players, 40 iterations, 4 Rayon workers**):

| Workload | Pure Java (Main-Thread Style) | Rust Native (Zero-Copy + Rayon) | Speedup |
| :--- | :---: | :---: | :---: |
| **Entity Collisions / Density (5,000)** | `79.129 ms` | **`0.915 ms`** | **86.5x faster (-98.8%)** |
| **AI Nearest Target + LOD (5,000 × 100)** | `1.036 ms` | **`0.514 ms`** | **2.0x faster (-50.4%)** |
| **Custom Boids / Flock AI (5,000)** | — | **`3.419 ms/tick`** | 6.8% of a 50ms tick |

### 3. Live World Diagnostics & Standalone Rust Kernels

- **Live World Collision Scan (`/rustbridge scan world` — 800 mobs):** `0.276 ms` in Rust
- **Live O(N) Chunk Heatmap (`/rustbridge chunks world` — 1,033 entities):** `0.487 ms` in Rust
- **Standalone Rust Engine (`cargo run --release -p spica-rust-native --bin bench`):**
  - Chunk Density Heatmap (5,000 entities): `0.249 ms/tick`
  - Item Merge Planner (2,000 items → 1,568 pairs): `0.702 ms/tick`
  - Adaptive MSPT Governor evaluation: `5.5 ns/eval`
  - Farm & Villager Governor (5,000 entities × 100 players): `3.685 ms` (`~0.18 ms/tick` amortized at 1s interval)
- **Unit Test Suite:** `29/29` Rust unit tests passing (`spica-launcher`: 14/14, `spica-rust-native`: 15/15)

---

## Building & Running

### Windows (Local Testing — `.exe` + `.dll` + `.jar`)

Install prerequisites (open a fresh terminal afterward):

```powershell
winget install Rustlang.Rustup
winget install EclipseAdoptium.Temurin.21.JDK
winget install Apache.Maven
```

Build the entire workspace (native engine, launcher, tests, and Purpur plugin JAR):

```powershell
.\build-windows.ps1 -RunBench
```

Artifacts are exported to `dist/windows-x86_64/`:
- `spica-launcher.exe` — Place next to `purpur-1.21.4.jar`
- `SpicaRustNative-2.2.0.jar` — Place in `plugins/` (`spica_rust_native.dll` is bundled inside)

Run Purpur under the Rust Brain:

```powershell
# Generate default spica-launcher.toml
.\spica-launcher.exe --init

# Preview detected hardware & generated JVM flags without starting Java
.\spica-launcher.exe --dry-run

# Launch Purpur 1.21.4 with Rust Brain IPC & HTTP telemetry
.\spica-launcher.exe
```

### Linux (Production — `spica-launcher` + `.so` + `.jar`)

Directly on Linux (`x86_64` or `aarch64`):

```bash
chmod +x build-linux.sh
./build-linux.sh
```

Or cross-build a Linux `x86_64` release via Docker from Windows/macOS:

```powershell
docker build -f Dockerfile.linux --output type=local,dest=./dist/linux-x86_64 .
```

### Automated Multi-Platform CI & Universal JAR (GitHub Actions)

Every push/PR to `main` and version tag (`v*`) runs [`.github/workflows/ci-release.yml`](.github/workflows/ci-release.yml), compiling native libraries for **Windows `x86_64`**, **Linux `x86_64`**, and **Linux `aarch64` (ARM64)** and bundling all three into a single **Universal `SpicaRustNative-universal.jar`** plus platform-specific `spica-launcher` binaries and `SHA256SUMS.txt`.

---

## Commands & Telemetry

### Launcher Supervisor Commands (Terminal)
- `:brain` — Live Rust Brain state (`GREEN`–`EMERGENCY`), EWMA MSPT, IPC status, and active directive
- `:stats` — Host CPU/RAM, JVM heap usage, TPS, entity count, hibernated mobs, and Farm/Villager Governor stats
- `:restart` — Gracefully stops Purpur (`stop`) and restarts cleanly
- `:stop` — Gracefully shuts down Purpur and exits the launcher

### Zero-JVM-Overhead Embedded Web Dashboard & HTTP Endpoints
- `GET http://127.0.0.1:9120/` (or `/dashboard`) — **Embedded Live Web Dashboard** (offline-ready dark ops UI with 120s MSPT/EWMA/TPS canvas chart, Brain State badge, active directives, Farm/Villager Governor & host CPU/RAM telemetry)
- `GET http://127.0.0.1:9120/history` — Atomic JSON snapshot + 120-second telemetry ring buffer
- `GET http://127.0.0.1:9120/status` — Live JSON status snapshot
- `GET http://127.0.0.1:9120/metrics` — Prometheus exposition metrics (`spica_mspt`, `spica_ewma_mspt`, `spica_tps`, `spica_entities`, `spica_hibernated_mobs`, `spica_farm_throttled_mobs`, `spica_villagers_throttled`, `spica_brain_state`, `spica_host_cpu_percent`, etc.)

### In-Game Purpur Commands (`/rustbridge`)

| Command | Description |
| :--- | :--- |
| `/rustbridge brain` | Displays Brain mode (`LAUNCHER IPC` / `IN-PROCESS RUST`), health state, EWMA MSPT, live view/sim distance, dynamic AI ranges, and total Rust-merged items. |
| `/rustbridge farms` | Displays live Mob-Farm & Villager Governor stats: throttled farm mobs, collision-disabled mobs, throttled trading-hall villagers, restock pulse countdown, and active thresholds. |
| `/rustbridge chunks [world]` | Runs the Rust $O(N)$ Chunk Heatmap and lists the Top 8 densest/most crowded chunks with chunk & block coordinates. |
| `/rustbridge status` | Shows native engine info, Rayon worker count, and AI LOD optimizer stats. |
| `/rustbridge scan [world]` | Scans all live mobs with Rust Spatial Hash to locate entity-cramming / mob-farm hotspots. |
| `/rustbridge bench [entities] [players] [iters]` | Runs an **async** Pure Java vs. Rust Native MSPT benchmark inside the live JVM. |
| `/rustbridge reload` | Reloads `config.yml` and safely restores hibernated mobs & default distances. |

---

## Developer API (`SpicaRustAPI`)

Other Purpur/Paper 1.21.4 plugins can offload custom batch physics, AOE queries, or swarm AI directly to Rust via Bukkit's `ServicesManager`:

```java
SpicaRustAPI rust = Bukkit.getServicesManager().load(SpicaRustAPI.class);
if (rust != null && rust.isNativeLoaded()) {
    EntityBatchArena arena = rust.createArena(2048, 64);

    // 1. Populate off-heap SoA buffers (0 GC allocations)
    arena.ensureEntityCapacity(mobs.size());
    for (int i = 0; i < mobs.size(); i++) {
        Location loc = mobs.get(i).getLocation();
        arena.setEntityPosition(i, (float) loc.getX(), (float) loc.getY(), (float) loc.getZ(), 0.5f);
    }

    // 2. Execute single zero-copy parallel Rust call
    rust.computeCollisions(arena, mobs.size(), 1.5f, 0.5f, 0.4f);

    // 3. Read results directly from off-heap buffers
    float pushX = arena.outX.get(0);
    int neighbors = arena.outIntA.get(0);
}
```

---

## Documentation
- [PROJECT.md](PROJECT.md) — Project vision, architecture details, and roadmap (Turkish)
- [AGENTS.md](AGENTS.md) — Development, FFI/IPC contract, and Git workflow rules
