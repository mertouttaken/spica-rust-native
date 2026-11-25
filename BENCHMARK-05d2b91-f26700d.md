# Purpur 1.21.4 revision benchmark

Measured November 25, 2026 on Windows with eight logical CPUs.

- A used revision `05d2b91`. B used revision `f26700d`. Both launchers and Java/JNI plugins were built from their respective revisions. All eight sessions used the same Rust bot binary.
- The Overworld was restored from `world-heavy-v24-snapshot` before each session. The Nether and End also started from one common seed copy. Each session used Purpur build 2416, Java 21, a 2 GiB G1GC heap, and four native workers.
- Four pairs ran in AB, BA, AB, BA order. Each session warmed up for 20 seconds after 20 clients joined, then recorded 20 seconds of normal play and 20 seconds starting with one manual backup. The same clients stayed online throughout both windows.
- An identical Paper tick-end listener captured each tick duration. CPU is user plus kernel CPU time for Java and the launcher, including the snapshot helper when it ran. The bot process runs separately.
- B used an isolated file-copy provider that copies all three worlds after the save checkpoint. Results cover that provider on this machine. Production providers such as VSS, ZFS and Btrfs have their own capture and release costs. Each backup archive opened and contained all three worlds' level files.

## Summary

Values are medians of four run-level statistics per revision. Tick counts above 50 ms are summed across runs. Change is B relative to A.

| Window | Metric | A | B | Change |
| --- | --- | ---: | ---: | ---: |
| Normal, 20 s | Mean MSPT | 11.96 ms | 14.36 ms | +20.1% |
| Normal, 20 s | Server CPU | 12.80 CPU-s | 13.98 CPU-s | +9.3% |
| Normal, 20 s | JVM CPU | 12.73 CPU-s | 13.88 CPU-s | +9.1% |
| Normal, 20 s | p99 tick | 27.31 ms | 29.38 ms | +7.6% |
| Normal, 20 s | Peak tick | 40.15 ms | 52.83 ms | +31.6% |
| Normal, 20 s | Ticks over 50 ms | 3 | 32 | +966.7% |
| Backup, 20 s | Mean MSPT | 13.44 ms | 16.47 ms | +22.5% |
| Backup, 20 s | Server CPU | 12.61 CPU-s | 17.23 CPU-s | +36.7% |
| Backup, 20 s | JVM CPU | 12.37 CPU-s | 16.48 CPU-s | +33.2% |
| Backup, 20 s | p99 tick | 26.81 ms | 30.38 ms | +13.3% |
| Backup, 20 s | Peak tick | 839.92 ms | 1141.89 ms | +36.0% |
| Backup, 20 s | Ticks over 50 ms | 4 | 7 | +75.0% |
| Backup, first 5 s | Mean MSPT | 19.93 ms | 28.50 ms | +43.0% |
| Backup, first 5 s | Server CPU | 6.88 CPU-s | 9.99 CPU-s | +45.3% |
| Backup, first 5 s | JVM CPU | 6.67 CPU-s | 9.29 CPU-s | +39.2% |
| Backup, first 5 s | p99 tick | 30.17 ms | 48.01 ms | +59.1% |
| Backup, first 5 s | Peak tick | 839.92 ms | 1141.89 ms | +36.0% |
| Backup, first 5 s | Ticks over 50 ms | 4 | 7 | +75.0% |
| Backup | Command to completion | 1115.76 ms | 2063.66 ms | +85.0% |
| Backup | Purpur save phase | 826.09 ms | 1102.84 ms | +33.5% |
| Backup | Save confirmation to completion | 221.93 ms | 915.50 ms | +312.5% |

The completion marker includes the autosave restoration acknowledgment in A and snapshot release in B. The save phase is measured between Purpur's saving and saved console lines. The longest tick in each backup window coincided with this phase.

## Individual pairs

| Pair | Host CPU during normal load, A → B | Normal MSPT, A → B | Normal server CPU change | Backup MSPT, A → B | Backup server CPU change | Backup peak tick change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 80.5% → 79.2% | 23.34 → 29.28 ms | +37.8% | 22.71 → 23.43 ms | +30.7% | +88.9% |
| 2 | 11.8% → 39.2% | 10.82 → 16.06 ms | +9.5% | 11.30 → 18.49 ms | +76.6% | +65.6% |
| 3 | 14.9% → 10.0% | 11.32 → 10.96 ms | -9.7% | 12.73 → 12.99 ms | +16.5% | +8.9% |
| 4 | 20.5% → 25.2% | 12.60 → 12.67 ms | +9.1% | 14.15 → 14.45 ms | +10.7% | +10.2% |

The host CPU readings include the benchmark processes and Windows background activity. Host CPU varied across runs, especially in the second pair. Treat the summary as the observed test result. The per-pair data shows the variation relevant to attributing changes to the revisions.
