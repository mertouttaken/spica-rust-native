# SpicaRustNative v2.6 — Purpur 1.21.4 Core + Rust Brain

> **Execution Core:** Purpur / Paper 1.21.4 (100% compatible with existing Bukkit/Paper/Purpur plugins)  
> **Brain, Orchestrator & Tooling:** Rust (`spica-launcher` + `spica-native` + `spica-bot-bench`)

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
│  • Crash watchdog, interactive console (:brain, :stats, :backup, :stop)│
│  • Zero-JVM-overhead Rust World Backup Engine (.tar.gz + rotation)     │
│  • Zero-JVM-overhead Embedded Web Dashboard (/) + 120s history chart   │
│    + /status (JSON) & /metrics (Prometheus)                            │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │ Localhost Token-Auth NDJSON IPC
┌──────────────────────────────────▼─────────────────────────────────────┐
│  2) SpicaRustNative Plugin (Purpur 1.21.4 Execution Body)              │
│  • Safely actuates Rust Brain directives onto live worlds:             │
│    Mob Tracking Culling, AI LOD, Mob-Farm & Villager Governor,         │
│    Redstone & Hopper Lag-Machine Governor, Elytra Surge & TNT Buffer   │
│  • Seamless fallback to in-process Rust Governor if run standalone     │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │ Zero-Copy DirectByteBuffer (0 GC)
┌──────────────────────────────────▼─────────────────────────────────────┐
│  3) spica-native (.dll / .so — In-Process Rust Compute Engine)         │
│  • Flat CSR O(N) Spatial Hash Grid + Rayon work-stealing pool          │
│  • Mob Tracking, Redstone/Hopper Governor, Farm/Villager Governor,     │
│    Collisions, AI LOD, Surge Guard, Boids, Flow-Field, FBM Noise,      │
│    and Parallel 3D Arena Block Diff Engine (16 JNI exports)            │
└────────────────────────────────────────────────────────────────────────┘
```

---

## Measured Benchmark Results (Purpur 1.21.4 — v2.6.0)

All A/B tests below were executed on the same host (**Windows x86_64, 8 logical cores, 16 GB RAM, OpenJDK 21.0.13 LTS, identical `spica-launcher.exe` Aikar Tuned G1GC `-Xms2048M -Xmx2048M` flags**) against an identical cloned Purpur 1.21.4 heavy stress world (**`world-heavy-v24-snapshot`**: **~1,126 persistent mobs** — 3×120 hyper-cramped farm mobs in 3×3 chambers, 48 trading-hall villagers, 600 outer roaming mobs — plus **300 ground items**, 169 force-loaded chunks).

### 1. Live Purpur 1.21.4 A/B Stress Test: Pure Purpur vs. `SpicaRustNative v2.6.0` (`05d2b91`)

Workload per run (identical cloned world restored before each run):
1. **Phase 1 (20 Live Steady-State Bots):** `spica-bot-bench` (Minecraft 1.21.4, Protocol 769) connecting **20/20** bots (`0` failures, `0` disconnects, `exit=0`, `3s` warmup + `15s` steady-state measurement = `300.1` active bot-seconds).
2. **Phase 2 (8-TNT Chain Explosion + 15s Recovery):** 8 TNTs detonated on a cobblestone tower, followed by 15s post-explosion recovery sampling.

| Metric (Identical World, JVM & 20-Bot Workload) | Pure Purpur 1.21.4 (No Rust) | Purpur + `SpicaRustNative v2.6.0` | Improvement |
| :--- | :---: | :---: | :---: |
| **Server Boot Time (`Done`)** | `30.93 s` | **`23.04 s`** | **25.5% faster** (`-7.89 s`) |
| **20-Bot Steady-State MSPT (5s avg)** | `35.8 ms` (`26.9`–`58.7`) | **`10.8 ms`** (`8.0`–`14.7`) | **-69.8% MSPT (3.31× lower)** |
| **20-Bot Steady-State MSPT (10s avg)** | `36.1 ms` | **`11.4 ms`** | **-68.4% MSPT (3.17× lower)** |
| **20-Bot Steady-State Max Tick Spike** | `58.7 ms` | **`14.7 ms`** | **-75.0% spike reduction** |
| **Join Burst Worst 5s TPS / Max Tick** | `8.61 TPS` / `763.2 ms` | **`12.11 TPS` / `486.2 ms`** | **+40.6% TPS**, **-36.3% peak** |
| **Post-TNT Recovery MSPT (5s avg)** | `8.0 ms` (`6.5`–`14.1`) | **`2.9 ms`** (`1.4`–`5.7`) | **-63.8% MSPT (2.76× lower)** |
| **Overall 1-Minute Avg MSPT** | `24.5 ms` | **`12.2 ms`** | **-50.2% MSPT (2.01× lower)** |
| **JVM Total CPU Time (Phase 1 / 27.4s wall)** | `111.69 cpu-s` | **`59.61 cpu-s`** | **-46.6% CPU** |
| **JVM Total CPU Time (Full Run)** | `119.66 cpu-s` | **`63.27 cpu-s`** | **-47.1% CPU** |
| **Bot p95 Join Latency (`p95JoinMs`)** | `3,922 ms` | **`2,162 ms`** | **-44.9% faster join** |
| **Entity Packets Delivered (20 bots, 15s)** | `5,996,915` (`19,984.9 /bot·s`) | **`129,718` (`432.2 /bot·s`)** | **-97.8% entity packets (46.2×)** |
| **Server Outbound Bandwidth (20 bots)** | `4,315.7 KB/s` | **`138.2 KB/s`** | **-96.8% bandwidth (31.2×)** |
| **Avg JVM RSS Memory** | `2,440.2 MB` | `2,476.8 MB` | `+36.6 MB` (+1.5% off-heap) |

#### Visual A/B Comparison Chart

```text
20-Bot Steady-State 5s MSPT (lower is better)
  Pure Purpur (No Rust)  │ ████████████████████████████████████ 35.8 ms (max 58.7 ms)
  SpicaRustNative v2.6.0 │ ███████████                          10.8 ms (max 14.7 ms)  [-69.8%]

JVM Total CPU Time Across Full A/B Run (lower is better)
  Pure Purpur (No Rust)  │ ████████████████████████████████████ 119.66 cpu-s
  SpicaRustNative v2.6.0 │ ███████████████████                   63.27 cpu-s           [-47.1%]

20-Bot Outbound Network Bandwidth (lower is better)
  Pure Purpur (No Rust)  │ ████████████████████████████████████ 4,315.7 KB/s
  SpicaRustNative v2.6.0 │ █                                      138.2 KB/s           [-96.8%]
```

**Full 5-second `/mspt` avg timeline across the A/B run (Idle → 20-Bot Join/Steady-State → 8-TNT Burst → Recovery):**
- **Without Rust:** `25.2` → `38.6` → `73.2` → `87.0` → `36.7` → `35.8` → `32.6` → `[TNT] 13.5` → `8.6` → `8.0 ms`
- **With Rust v2.6.0:** `18.2` → `31.5` → `68.9` → `16.9` → `11.5` → `10.8` → `8.1` → `[TNT] 3.4` → `3.0` → `2.9 ms`

> **Methodology & scope note:** This A/B test models a high-density mob-farm / crowded-spawn workload (360 hyper-cramped farm mobs + 600 outer mobs + 20 bots near spawn), where `FarmVillagerGovernor` (556 farm mobs throttled), `EntityLodOptimizer`, and `MobTrackingOptimizer` work together. On lightly loaded SMP servers with spread-out players and low mob density, percentage MSPT and packet reductions will naturally be smaller than in this stress world.

### 2. Live In-JVM Benchmark (`/rustbridge bench 5000 100 30` & `/rustbridge kernels`)

Measured directly inside the running Purpur 1.21.4 JVM (**4 Rayon workers, `abi=2`**):

| Workload | Pure Java (Main-Thread Style) | Rust Native (Zero-Copy + Rayon) | Speedup / Throughput |
| :--- | :---: | :---: | :---: |
| **Entity Collisions / Density (5,000 entities)** | `58.147 ms` | **`1.271 ms`** | **45.8× faster (-97.8%)** |
| **3D Arena Block Diff (64×64×64 = 262,144 blocks)** | `1.798 ms` | **`0.411 ms`** | **4.4× faster (-77.1%)** |
| **AI Nearest Target + LOD (5,000 × 100)** | `0.795 ms` | **`0.510 ms`** | **1.6× faster (-35.8%)** |
| **Mob Tracking (100p × 2,000m = 200,000 pairs)** | — | **`1.009 ms`** | ~0.17 ms/tick @ 6-tick interval |
| **2D Flow-Field Pathfinding (128×128 w/ wall)** | — | **`2.050 ms`** | 16,384 cells, O(1) lookup |
| **Parallel 4-Octave FBM Noise (256×256 = 65,536)** | — | **`2.563 ms`** | 256 chunks of terrain |
| **Custom Boids / Flock AI (5,000 entities)** | — | **`3.138 ms/tick`** | SpatialHash + Rayon |

### 3. Verification & Test Suite Status

- **Rust Unit Test Suite:** `77/77` passing (`spica-rust-native`: 44/44, `spica-launcher`: 28/28, `spica-bot-bench`: 5/5) — covers FFI aliasing/alignment/read-only guards, spatial hash bounds, mid-read backup cancellation, zero-disclosure Protocol v2 HMAC vectors, and Purpur `save-off` state preservation.
- **JNI Safety (`-Xcheck:jni`):** Verified on OpenJDK 21 with adversarial RW/WW aliasing, read-only `DirectByteBuffer`, active `EntityBatchArena` lease mutation guards, `abi=2` revision enforcement, and lazy `nativeInitPool` thread-count isolation.
- **Live World Backup Benchmark:** 122 files across 3 worlds (`world`, `world_nether`, `world_the_end`) archived to `2.08 MB` `.tar.gz` in `430–495 ms` with Purpur `save-off` / `Saved the game` ACK verification and prior `save-off` preservation.

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
- `SpicaRustNative-2.6.0.jar` — Place in `plugins/` (`spica_rust_native.dll` is bundled inside)
- `spica-bot-bench.exe` — Optional MC 1.21.4 (Protocol 769) offline-mode bot load & Mob-Tracking benchmark CLI

Run Purpur under the Rust Brain:

```powershell
# Generate default spica-launcher.toml
.\spica-launcher.exe --init

# Preview detected hardware & generated JVM flags without starting Java
.\spica-launcher.exe --dry-run

# Launch Purpur 1.21.4 with Rust Brain IPC, Backup Engine & HTTP telemetry
.\spica-launcher.exe

# Optional: Stress-test an offline-mode dev server with 20 live MC 1.21.4 bots
.\spica-bot-bench.exe --host 127.0.0.1 --port 25565 --bots 20 --duration 30
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
- `:stats` — Host CPU/RAM, JVM heap usage, TPS, entity count, hibernated mobs, Farm/Villager Governor, and World Backup stats
- `:backup` — Flushes a Purpur checkpoint, then archives a filesystem snapshot as `.tar.gz`. Each world's autosave setting is retained. Requires the snapshot provider below.
- `:restart` — Gracefully stops Purpur (`stop`) and restarts cleanly
- `:stop` — Gracefully shuts down Purpur and exits the launcher

### Zero-JVM-Overhead Embedded Web Dashboard & HTTP Endpoints
- `GET http://127.0.0.1:9120/` (or `/dashboard`) — **Embedded Live Web Dashboard** (offline-ready dark ops UI with 120s MSPT/EWMA/TPS canvas chart, Brain State badge, active directives, Farm/Villager/Redstone/Tracking/Backup & host CPU/RAM telemetry)
- `GET http://127.0.0.1:9120/history` — Atomic JSON snapshot + 120-second telemetry ring buffer
- `GET http://127.0.0.1:9120/status` — Live JSON status snapshot
- `GET http://127.0.0.1:9120/metrics` — Prometheus exposition metrics (`spica_mspt`, `spica_ewma_mspt`, `spica_tps`, `spica_entities`, `spica_hibernated_mobs`, `spica_farm_throttled_mobs`, `spica_villagers_throttled`, `spica_tracked_pairs_hidden`, `spica_redstone_chunks_throttled`, `spica_backups_completed_total`, `spica_last_backup_duration_ms`, `spica_brain_state`, `spica_host_cpu_percent`, etc.)

### Live snapshot backups

Set `backup.snapshot_command` in `spica-launcher.toml` to the executable and optional fixed arguments for your filesystem snapshot provider. The launcher disables scheduled backups and rejects manual backups while this setting is empty.

```toml
[backup]
snapshot_command = ["/absolute/path/to/snapshot-provider"]
worlds = ["world", "world_nether", "world_the_end"]
```

After a matching Purpur `save-all flush` result, the launcher runs the configured command with `create <server-root> <staging> <world>...` appended. The provider must expose one immutable point-in-time view covering all listed worlds under `<staging>/view/<world>`. Use a filesystem or volume snapshot. If the worlds span volumes, the provider must capture them as a consistent group. The provider runs under the launcher account and needs the appropriate snapshot permissions. The launcher never modifies any world's autosave flag.

The launcher archives the view and then calls the same command with `release <server-root> <staging> <world>...`. Release must also handle an interrupted or failed create. Each command has a 60-second limit. The provider should execute in the foreground and remove the view or mount during release. The launcher removes the staging directory once it is empty.

### Java native buffers

Use `EntityBatchArena` scalar setters and getters. To share an arena across threads, wrap the entire fill, compute and read sequence in `withExclusiveAccess`. The ordinary `SpicaRustAPI` overloads that accept external `ByteBuffer` instances stage the input and output through private direct memory. For zero-copy calls with exclusively owned buffers, the `WithExclusiveBuffers` overloads require the caller to keep every view inaccessible to other threads until the call returns.

### In-Game Purpur Commands (`/rustbridge`)

| Command | Description |
| :--- | :--- |
| `/rustbridge brain` | Displays Brain mode (`LAUNCHER IPC` / `IN-PROCESS RUST`), health state, EWMA MSPT, live view/sim distance, dynamic AI ranges, and total Rust-merged items. |
| `/rustbridge farms` | Displays live Mob-Farm & Villager Governor stats: throttled farm mobs, collision-disabled mobs, throttled trading-hall villagers, restock pulse countdown, and active thresholds. |
| `/rustbridge tracking` | Displays live Rust Mob Tracking stats: hidden/visible/budget-culled `(player, mob)` pairs, delta transitions/sec, and effective Brain-scaled range/cave/FOV/Top-K limits. |
| `/rustbridge redstone` | Displays live Redstone & Hopper Lag-Machine Governor stats: active/throttled chunks, blocked ops, effective Brain-scaled limits, and Top-8 hottest Redstone/Hopper chunks. |
| `/rustbridge surge` | Displays live Elytra / Chunk-Load Surge Guard & Explosion Item Buffer stats (chunks/sec, Elytra clamps, buffered explosions, merged debris items). |
| `/rustbridge kernels` | Runs an **async** live Rust benchmark of 200k-pair Mob Tracking + 128×128 Flow-Field Pathfinding + 256×256 FBM WorldGen Noise + Java vs Rust 64×64×64 Arena Block Diff inside the running JVM. |
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
