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

## Measured Benchmark Results (Purpur 1.21.4)

All tests below were executed on the same host (**Windows x86_64, 8 logical cores, 16 GB RAM, OpenJDK 21.0.13 LTS, `-Xms2048M -Xmx2048M`**) against an identical cloned Purpur 1.21.4 stress world (**809 persistent AI mobs + ~240 ground items = 1,052 entities, 169 force-loaded chunks**).

### 1. Live Purpur 1.21.4 Heavy Stress A/B Test (v2.4.0 — Dense Mob Farms + 8-TNT Burst)

Executed on an identical cloned heavy stress world (**360 hyper-cramped mob-farm entities in 3×3 chambers + ~609 roaming mobs + 300 ground items, 169 force-loaded chunks, plus an 8-TNT detonation on 486 cobblestone blocks at t=25s**; same OpenJDK 21, `-Xms2048M -Xmx2048M`):

| Phase / Metric | Pure Purpur 1.21.4 (No Rust) | Purpur + `SpicaRustNative v2.4.0` | Improvement |
| :--- | :---: | :---: | :---: |
| **Server Boot Time (`Done`)** | `43.53 s` | **`31.78 s`** | **27.0% faster** (`-11.75 s`) |
| **Phase 1: Pre-Explosion Avg MSPT (t=10–25s)** | `13.53 ms` | **`6.08 ms`** | **55.1% lower MSPT (2.23x)** |
| **Phase 2: 8-TNT Burst 5s Window Avg MSPT** | `25.2 ms` (10s: `17.8 ms`) | **`11.2 ms`** (10s: `7.4 ms`) | **55.6% lower MSPT (2.25x)** |
| **Phase 3: Post-Explosion Steady Avg MSPT (t=35–50s)** | `12.03 ms` (rising to `14.8 ms`) | **`2.50 ms`** (steady `2.3 ms`) | **79.2% lower MSPT (4.81x)** |
| **Overall 50s Mean MSPT (all 9 samples)** | `14.16 ms` | **`5.06 ms`** | **64.3% lower MSPT (2.80x)** |
| **1-Minute Cumulative Avg MSPT (t=50s)** | `14.9 ms` | **`8.2 ms`** | **45.0% lower MSPT** |
| **Steady-State Min Tick Time** | `6.9 – 9.9 ms` | **`1.0 – 1.1 ms`** | **~7x lower floor** |
| **Rust Actions Taken Live** | — | `498` farm mobs collision+AI throttled, `283` ground items merged | 0.426 ms Rust compute |

**Full 5-second `/mspt` avg timeline (t=10s → t=50s, 8-TNT burst at t=25s):**
- **Without Rust:** `18.6` → `15.5` → `9.6` → `10.4` → `[TNT] 25.2` → `8.6` → `11.3` → `13.4` → `14.8 ms`
- **With Rust v2.4.0:** `10.7` → `5.1` → `4.8` → `3.7` → `[TNT] 11.2` → `2.7` → `2.6` → `2.4` → `2.3 ms`

> **Methodology note:** With 0 connected players during the automated headless test, distant mobs were eligible for AI LOD hibernation (`Mob#setAware(false)`) alongside dense farm collision suppression (`setCollidable(false)`). On populated production servers where mobs close to players stay fully active, real-world MSPT savings typically land in the **20%–35%** range during normal gameplay and **40%–65%+** during mob-farm / explosion / high-density spikes.

### 2. Live In-JVM Benchmark (`/rustbridge bench` & `/rustbridge kernels`)

Measured directly inside the running Purpur 1.21.4 JVM (**4 Rayon workers**):

| Workload | Pure Java (Main-Thread Style) | Rust Native (Zero-Copy + Rayon) | Speedup |
| :--- | :---: | :---: | :---: |
| **Entity Collisions / Density (5,000 entities)** | `57.756 ms` | **`0.772 ms`** | **74.8x faster (-98.7%)** |
| **AI Nearest Target + LOD (5,000 × 100)** | `0.784 ms` | **`0.355 ms`** | **2.2x faster (-54.7%)** |
| **Custom Boids / Flock AI (5,000 entities)** | — | **`2.334 ms/tick`** | 4.7% of a 50ms tick |
| **Mob Tracking (100p × 2,000m = 200,000 pairs)** | — | **`1.823 ms`** (standalone: `1.117 ms`) | ~0.30 ms/tick @ 6-tick interval |
| **2D Flow-Field Pathfinding (128×128 w/ wall)** | — | **`2.096 ms`** (standalone: `1.869 ms`) | O(1) per-mob lookup |
| **Parallel 4-Octave FBM Noise (256×256 = 65,536)** | — | **`2.531 ms`** (standalone: `3.025 ms`) | 256 chunks of terrain |
| **3D Arena Block Diff (64×64×64 = 262k blocks)** | `4.094 ms` | **`1.144 ms`** (1M standalone: `1.114 ms`) | **3.6x faster (-72.1%)** |

### 3. Live 20-Bot Network Stress Test & World Backup Benchmark (v2.6.0)

Executed against a live Purpur 1.21.4 server (~1,169 entities) using the bundled **`spica-bot-bench`** async Minecraft 1.21.4 (**Protocol 769**) bot client (`20/20` bots connected, `0` failures, `0` early disconnects, continuous 250 ms look-vector rotation):

| Metric (20 Live Bots, 16s Window) | Mob Tracking OFF | Mob Tracking ON (`v2.6.0`) | Impact |
| :--- | :---: | :---: | :---: |
| **Connected Bots / Early Drops** | `20 / 0` | **`20 / 0`** | 100% stable Protocol 769 |
| **Live Server TPS / MSPT** | `20.00` | **`20.00` / `16.5 ms`** | `0.280 ms` Rust compute |
| **Culled `(player, mob)` Tracking Pairs** | `0` | **`15,292` hidden pairs** | Top-K + Rear-FOV + Cave |
| **Entity Movement Packets (`move/rot/tp`)** | `92,704` | **`76,346`** | **-17.6% movement packets** |
| **Total Clientbound Packets** | `311,546` (`19,459 pkt/s`) | **`280,944` (`17,548 pkt/s`)** | **-9.8% total packets** |
| **Server Outbound Bandwidth** | `666.0 KB/s` | **`556.0 KB/s`** | **-16.5% outbound bandwidth** |

- **Historical archive benchmark:** The original v2.6.0 archive stage compressed 122 files across three worlds to 1.97 MB in 142 ms. The current live backup adds filesystem snapshot creation before archiving.

### 4. Live World Diagnostics & Standalone Rust Kernels

- **Live World Collision Scan (`/rustbridge scan world` — 800 mobs):** `0.276 ms` in Rust
- **Live O(N) Chunk Heatmap (`/rustbridge chunks world` — 1,033 entities):** `0.487 ms` in Rust
- **Standalone Rust Engine (`cargo run --release -p spica-rust-native --bin bench`):**
  - Chunk Density Heatmap (5,000 entities): `0.117 ms/tick`
  - Item Merge Planner (2,000 items → 1,568 pairs): `0.434 ms/tick`
  - Adaptive MSPT Governor evaluation: `4.1 ns/eval`
  - Farm & Villager Governor (5,000 entities × 100 players): `1.761 ms` (`~0.09 ms/tick` amortized at 1s interval)
  - **Parallel Mob Tracking (100 players × 2,000 mobs = 200,000 pairs):** **`0.775 ms`** (`~0.13 ms/tick` amortized at 6-tick interval; Cave + Rear-FOV + Hostile-Priority Top-K Budget + Delta Bitset)
  - **Redstone & Hopper Governor (2,000 active chunks):** **`0.077 ms/eval`** (asymmetric EWMA heat + smooth fractional drop)
  - **Parallel 3D Arena Block Diff (128×64×128 = 1,048,576 blocks):** **`1.114 ms/eval`** (52,429 changed blocks extracted)
  - 2D Flow-Field Pathfinding (128×128 = 16,384 cells w/ obstacles): `2.350 ms/eval` (O(1) per-mob lookup)
  - Parallel 4-Octave FBM WorldGen Noise (256×256 = 65,536 blocks): `3.025 ms/eval`
- **Unit Test Suite:** `62/62` Rust unit tests passing (`spica-rust-native`: 39/39, `spica-launcher`: 19/19, `spica-bot-bench`: 4/4) and **16/16 JNI symbol parity verified**.

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
