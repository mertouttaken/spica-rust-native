# Matched Java and Rust JNI kernel benchmark

Measured November 26, 2026 on the Windows x86-64 host with eight logical processors and Temurin Java 21. Each pair ran in the same standalone JVM with fixed processor affinity, priority and heap settings. There was no Purpur tick loop or gameplay actuation. The fixture was built against the direct JNI declarations on revision 5966d01. The DLL uses the Windows distribution build flag `-C target-cpu=native`.

- The Java implementation and native kernel received the same deterministic inputs. Counts, ordered indices, states and the tracking bitset matched exactly. Float fields matched within 0.00005 before and after timing. A separate run with -Xcheck:jni passed all 15 cases.
- Java used on-heap arrays and Rust used preallocated, native-order direct buffers. The measured native time includes the JNI entry, buffer validation, and dispatch into the Rust worker pool where applicable. Fixture construction, initial upload, final output verification and game-world reads or writes take place outside the timer. Both paths reset the tracking bitset inside the timer.
- At four workers Java used the calling thread plus three ForkJoin workers for parallel kernels. Rust used four Rayon workers. At one worker both computations were serial. Kernels that use a serial implementation keep that implementation in both runs.
- Both paths warmed up before each timed block. Each fork alternated Java-first and Rust-first batches calibrated to roughly the same duration. Each reported time is normalized per invocation. The table uses the median batch time per call. The percentage is the change in duration relative to Java, so a negative value is faster.
- Windows Job Objects pinned both paths to one logical CPU per physical core, with affinity mask 0x6a and the same priority. The controller measured background CPU separately for each path and kept pairs with a gap of at most 10 percentage points. It retained 386 of 584 pairs. Every row and worker profile have at least ten retained pairs spanning at least two JVM starts.

| Kernel | Input | Java 1, µs | Rust JNI 1, µs | 1-worker change | Java 4, µs | Rust JNI 4, µs | 4-worker change |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Entity collision and density | 5000 | 4612.727 | 6281.075 | +36.2% | 1422.265 | 1606.586 | +13.0% |
| Nearest target, steering and LOD | 5000x100 | 1046.838 | 1355.434 | +29.5% | 468.139 | 588.497 | +25.7% |
| Boids steering | 5000 | 9071.058 | 10684.850 | +17.8% | 4235.846 | 4149.950 | -2.0% |
| Radius query | 5000 | 8.943 | 10.881 | +21.7% | 7.931 | 8.918 | +12.5% |
| Chunk heatmap (top 8) | 5000 | 309.138 | 190.408 | -38.4% | 152.351 | 97.203 | -36.2% |
| Item merge plan | 5000 | 7671.989 | 7152.700 | -6.8% | 4013.454 | 4381.158 | +9.2% |
| MSPT governor | 1 | 0.018 | 3.749 | +20983.3% | 0.011 | 1.974 | +18163.1% |
| Farm and villager actions | 5000x100 | 9773.950 | 10127.780 | +3.6% | 2728.555 | 2820.719 | +3.4% |
| Elytra speed actions | 100 | 0.385 | 5.616 | +1356.7% | 0.336 | 4.722 | +1305.1% |
| Flow-field distances and steering | 128x128 | 3009.406 | 2744.722 | -8.8% | 2727.505 | 2341.012 | -14.2% |
| Four-octave FBM | 256x256x4 | 6682.064 | 6529.719 | -2.3% | 2263.303 | 2382.262 | +5.3% |
| Mob visibility and bitset deltas (cap 512) | 2000x100 | 2961.209 | 3906.639 | +31.9% | 1270.515 | 1515.094 | +19.3% |
| Redstone and hopper actions (top 8) | 1024 | 33.531 | 42.622 | +27.1% | 25.983 | 46.219 | +77.9% |
| Block diff extraction | 64x64x64 | 901.233 | 592.131 | -34.3% | 793.349 | 176.363 | -77.8% |
| Block diff count | 64x64x64 | 149.515 | 94.694 | -36.7% | 67.137 | 35.479 | -47.2% |

Rust library SHA-256 is c985e4d04b5be63c6c2ff174033b0620b2ff6aabb7cb1813d6acbf7ee8f9067e.

The standalone test class is `net.spicanetwork.rustnative.nativebridge.KernelParityBenchmark`. Run it with the absolute DLL or shared library path, worker count and optional number of rounds, execution order and comma-delimited case names. A round count of zero runs parity only. For four workers set the JVM system property `java.util.concurrent.ForkJoinPool.common.parallelism=3`.
