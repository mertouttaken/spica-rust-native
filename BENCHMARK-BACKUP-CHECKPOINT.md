# Live Purpur backup checkpoint benchmark

Four pairs ran in AB, BA, AB, BA order on November 25, 2026. A was `f26700d`. B was the staged checkpoint in the commit containing this report.

- Both versions ran Purpur 1.21.4 build 2416 with Java 21, a 2 GiB G1GC heap and four native worker threads. The host had eight logical CPUs.
- Each run used fresh copies of the same heavy Overworld snapshot and the same Nether and End folders. Both sides used the same local bot binary and the same copy-based Windows snapshot test provider. The isolated server allowed flying on both sides so the floating check did not kick stationary bots.
- After all 20 bots joined, the server warmed up for 20 seconds. It then recorded 20 seconds of normal play and 20 seconds from the backup command. All 20 bots stayed connected in the measured windows. Each backup produced one verified archive containing the level data for all three worlds.
- The same Paper listener measured every tick. CPU seconds sum the JVM, launcher, and snapshot provider processes. Final flush time is measured between Purpur's saving and saved console messages. The backup time includes preparation, the final flush, snapshot creation, archiving, and snapshot release.

## Results

Each duration or CPU value is the median of four run-level results. Tick counts are summed. The percent change is relative to A.

| Window | Metric | A | B | Change |
| --- | --- | ---: | ---: | ---: |
| Normal, 20 s | Mean MSPT | 11.64 ms | 15.80 ms | +35.8% |
| Normal, 20 s | Server CPU | 14.69 CPU-s | 17.68 CPU-s | +20.4% |
| Normal, 20 s | JVM CPU | 14.62 CPU-s | 17.62 CPU-s | +20.5% |
| Normal, 20 s | p99 tick | 22.68 ms | 40.45 ms | +78.4% |
| Normal, 20 s | Peak tick | 33.98 ms | 69.33 ms | +104.0% |
| Normal, 20 s | Ticks over 50 ms | 0 | 15 | n/a |
| Backup, 20 s | Mean MSPT | 12.19 ms | 19.58 ms | +60.7% |
| Backup, 20 s | Server CPU | 12.49 CPU-s | 17.45 CPU-s | +39.6% |
| Backup, 20 s | JVM CPU | 12.06 CPU-s | 16.70 CPU-s | +38.5% |
| Backup, 20 s | p99 tick | 19.96 ms | 77.03 ms | +286.0% |
| Backup, 20 s | Peak tick | 768.67 ms | 462.17 ms | -39.9% |
| Backup, 20 s | Ticks over 50 ms | 4 | 34 | +750.0% |
| Backup, first 5 s | Mean MSPT | 18.88 ms | 23.92 ms | +26.7% |
| Backup, first 5 s | Server CPU | 6.88 CPU-s | 8.98 CPU-s | +30.5% |
| Backup, first 5 s | JVM CPU | 6.51 CPU-s | 8.34 CPU-s | +28.1% |
| Backup, first 5 s | p99 tick | 21.39 ms | 206.09 ms | +863.5% |
| Backup, first 5 s | Peak tick | 768.67 ms | 462.17 ms | -39.9% |
| Backup, first 5 s | Ticks over 50 ms | 4 | 17 | +325.0% |
| Backup command | Final flush | 748.10 ms | 418.47 ms | -44.1% |
| Backup command | Request to start of final flush | 23.96 ms | 703.29 ms | +2835.4% |
| Backup command | Request to completion | 1204.18 ms | 1897.67 ms | +57.6% |

| Pair | Normal MSPT change | Backup MSPT change | Backup CPU change | Backup peak tick, A to B | Final flush, A to B | Completion, A to B |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | +7.9% | +28.4% | +26.3% | 831.1 to 457.7 ms | 808.2 to 420.4 ms | 1256.6 to 1748.4 ms |
| 2 | -6.2% | +4.6% | +2.0% | 788.4 to 442.8 ms | 768.1 to 416.6 ms | 1212.3 to 1566.8 ms |
| 3 | +64.3% | +98.7% | +62.6% | 678.8 to 466.7 ms | 658.4 to 415.7 ms | 1103.5 to 2046.9 ms |
| 4 | +147.9% | +104.6% | +63.9% | 748.9 to 686.4 ms | 728.1 to 633.9 ms | 1196.1 to 3074.1 ms |

The Windows snapshot helper produced a separate directory copy for this benchmark. End-to-end timing includes that helper's capture and release costs.

## Host load

| Pair | Normal host CPU, A to B | Backup host CPU, A to B |
| --- | ---: | ---: |
| 1 | 13.1% to 26.1% | 16.5% to 41.2% |
| 2 | 14.6% to 10.7% | 12.3% to 12.9% |
| 3 | 12.9% to 55.5% | 13.3% to 65.2% |
| 4 | 12.1% to 92.2% | 12.0% to 92.1% |

Host CPU includes benchmark processes and other work on the machine. In the closest backup pair, pair 2, the peak tick fell from 788.4 to 442.8 ms. Server CPU rose 2.0% and first-five-second p99 rose from 20.3 to 178.6 ms in that pair. Large host load changes in the other pairs limit attribution of the overall CPU and mean MSPT differences.

## Data verification

A separate Purpur test restored a block changed after the preparatory save and a block from a world with autosave disabled. The test also verified that the worlds retained their respective autosave flags. A simulated region-write error at Purpur's log boundary rejected the request before the snapshot provider ran and preserved the previous archive.
