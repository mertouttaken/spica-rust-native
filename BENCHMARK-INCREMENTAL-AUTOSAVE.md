# Load-matched Purpur backup benchmark

Measured November 26, 2026. A used `5f66f59`. B used the incremental autosave implementation in this commit.

- Purpur 1.21.4 build 2416, Java 21, Aikar G1GC with a 2 GiB heap, four native worker threads and the same local bot binary.
- Every run started with a fresh copy of the same heavy Overworld, Nether and End. Both revisions used the same Windows directory-copy snapshot test provider.
- Twenty clients stayed online without early disconnects. Each test warmed up for 20 seconds and then measured 20 seconds of normal play and 20 seconds after the backup command. An identical Paper listener recorded each tick.
- Windows Job Objects pinned the launcher, JVM and snapshot helper to the same three physical cores with the same priority. Bot clients used the remaining physical core. Server CPU seconds include the job's child processes.
- Background CPU is the busy share of the server CPUs after subtracting the job's own CPU time. The experiment targeted four quiet pairs, with a maximum background gap of 5 percentage points and ceiling of 15% in each 20-second window. The first five backup seconds used a gap of 8 points and ceiling of 20%.

6 pairs were attempted. 1 passed the full quiet criteria. The backup comparison also uses eligible backup windows from other attempts where the same two backup gap limits held. Those additional windows can have higher ambient CPU. Pairing maximizes the number of matches and minimizes the summed load gaps using each run once. Only background CPU was used to select pairs. The final retry waited for a quiet starting window and timed out before starting Purpur.

## Results

Normal values use one full pair, with 14.4% background CPU for A and 9.8% for B. Backup values use 3 backup-matched pairs. Duration and CPU values are run-level medians. Tick counts are summed. Change is relative to A.

| Window | Metric | A | B | Change |
| --- | --- | ---: | ---: | ---: |
| Normal, 20 s | Mean MSPT | 14.12 ms | 12.43 ms | -11.9% |
| Normal, 20 s | Server CPU | 15.11 CPU-s | 14.61 CPU-s | -3.3% |
| Normal, 20 s | p99 tick | 37.69 ms | 24.83 ms | -34.1% |
| Normal, 20 s | Peak tick | 62.98 ms | 27.56 ms | -56.2% |
| Normal, 20 s | Ticks over 50 ms | 1 | 0 | -100.0% |
| Backup, 20 s | Mean MSPT | 14.40 ms | 13.19 ms | -8.4% |
| Backup, 20 s | Server CPU | 13.95 CPU-s | 9.45 CPU-s | -32.3% |
| Backup, 20 s | p99 tick | 31.26 ms | 25.04 ms | -19.9% |
| Backup, 20 s | Peak tick | 681.46 ms | 366.85 ms | -46.2% |
| Backup, 20 s | Ticks over 50 ms | 7 | 3 | -57.1% |
| Backup, first 5 s | Mean MSPT | 24.69 ms | 15.72 ms | -36.3% |
| Backup, first 5 s | Server CPU | 8.92 CPU-s | 3.50 CPU-s | -60.8% |
| Backup, first 5 s | p99 tick | 127.80 ms | 24.67 ms | -80.7% |
| Backup, first 5 s | Peak tick | 681.46 ms | 366.85 ms | -46.2% |
| Backup, first 5 s | Ticks over 50 ms | 7 | 3 | -57.1% |
| Backup command | Final flush | 626.19 ms | 343.55 ms | -45.1% |
| Backup command | Request to completion | 2062.37 ms | 980.81 ms | -52.4% |

## Matched backup windows

| Pair | Run, A / B | Backup background CPU, A to B | First-five-second background CPU, A to B | First-five-second p99, A to B | Peak backup tick, A to B |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | run-01-pair-1-A-5f66f59 / run-02-pair-1-B-incremental-candidate | 4.9% to 7.9% | 7.1% to 8.4% | 120.3 to 24.7 ms | 471.2 to 361.5 ms |
| 2 | run-04-pair-2-A-5f66f59 / r03b04 | 15.8% to 18.4% | 20.4% to 15.5% | 127.8 to 30.2 ms | 707.7 to 385.1 ms |
| 3 | r04a04 / r03b02 | 17.2% to 20.9% | 22.6% to 19.4% | 142.7 to 24.1 ms | 681.5 to 366.9 ms |

## Data verification

In a separate Purpur run, the region file contained the first block edit before any backup command. The final checkpoint preserved a newer edit and a Nether edit with autosave disabled. Purpur restored both edits from the archive. The source Paper settings and live autosave flags were preserved. A simulated write error before the next checkpoint blocked the snapshot and preserved the earlier archive.
