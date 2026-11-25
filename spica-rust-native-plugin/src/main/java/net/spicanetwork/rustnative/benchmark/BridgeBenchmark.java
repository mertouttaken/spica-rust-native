package net.spicanetwork.rustnative.benchmark;

import net.spicanetwork.rustnative.api.SpicaRustAPI;
import net.spicanetwork.rustnative.brain.BrainDirective;
import net.spicanetwork.rustnative.nativebridge.EntityBatchArena;
import net.spicanetwork.rustnative.nativebridge.RustNative;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.FloatBuffer;
import java.nio.IntBuffer;
import java.util.SplittableRandom;

/**
 * In-server benchmark comparing Pure Java (Main-Thread style) vs Rust Native
 * (Zero-Copy DirectByteBuffer + Spatial Hash + Rayon) on identical entity datasets.
 *
 * Safe to run asynchronously so live server TPS is never impacted during testing.
 */
public final class BridgeBenchmark {

    private BridgeBenchmark() {
    }

    public record BenchmarkReport(
        int entities,
        int players,
        int iterations,
        double javaCollisionMs,
        double rustCollisionMs,
        double javaAiMs,
        double rustAiMs,
        double rustBoidsMs,
        long rustPairs
    ) {
        public double collisionSpeedup() {
            return rustCollisionMs > 0 ? javaCollisionMs / rustCollisionMs : 0.0;
        }

        public double aiSpeedup() {
            return rustAiMs > 0 ? javaAiMs / rustAiMs : 0.0;
        }
    }

    public record KernelBenchmarkReport(
        int flowWidth,
        int flowDepth,
        int flowIterations,
        double flowFieldMs,
        int flowReachableCells,
        float sampleDirX,
        float sampleDirZ,
        int noiseWidth,
        int noiseDepth,
        int noiseOctaves,
        int noiseIterations,
        double noiseMs,
        int noiseSamples,
        float noiseMinHeight,
        float noiseMaxHeight,
        int trackingPlayers,
        int trackingMobs,
        int trackingIterations,
        double trackingMs,
        int trackingHiddenPairs,
        int trackingVisiblePairs,
        int trackingBudgetCulled,
        int trackingDeltas
    ) {
    }

    /**
     * Benchmarks the v2.4.0 Rust native kernels asynchronously:
     * 1. 128x128 Dijkstra Flow-Field Pathfinding with an obstacle wall
     * 2. 256x256 4-octave FBM WorldGen Noise (65,536 blocks)
     * 3. Mob Visibility & Tracking Optimizer (100 players x 2,000 mobs = 200,000 pairs)
     */
    public static KernelBenchmarkReport runKernelsBenchmark(SpicaRustAPI api) {
        final int flowW = 128;
        final int flowD = 128;
        final int flowCells = flowW * flowD;
        final int goalX = 96;
        final int goalZ = 64;
        final int flowIters = 20;

        ByteBuffer outDist = ByteBuffer.allocateDirect(flowCells * Float.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer outDirX = ByteBuffer.allocateDirect(flowCells * Float.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer outDirZ = ByteBuffer.allocateDirect(flowCells * Float.BYTES).order(ByteOrder.nativeOrder());

        // Prepare u8 cost grid (1 = walkable, 255 = obstacle wall at x=64, z=16..111)
        ByteBuffer costGrid = ByteBuffer.allocateDirect(flowCells).order(ByteOrder.nativeOrder());
        for (int z = 0; z < flowD; z++) {
            for (int x = 0; x < flowW; x++) {
                boolean isWall = (x == 64 && z >= 16 && z < 112);
                costGrid.put(z * flowW + x, isWall ? (byte) 255 : (byte) 1);
            }
        }

        // Probe costGrid format cleanly (u8 default, fallback to i32 / f32 if kernel requires 4-byte cells)
        int probe = api.computeFlowField(flowW, flowD, costGrid, goalX, goalZ, outDist, outDirX, outDirZ);
        if (probe < 0) {
            ByteBuffer costGridInt = ByteBuffer.allocateDirect(flowCells * Integer.BYTES).order(ByteOrder.nativeOrder());
            IntBuffer costIntView = costGridInt.asIntBuffer();
            for (int z = 0; z < flowD; z++) {
                for (int x = 0; x < flowW; x++) {
                    boolean isWall = (x == 64 && z >= 16 && z < 112);
                    costIntView.put(z * flowW + x, isWall ? 0 : 1);
                }
            }
            int intProbe = api.computeFlowField(flowW, flowD, costGridInt, goalX, goalZ, outDist, outDirX, outDirZ);
            if (intProbe >= 0) {
                costGrid = costGridInt;
                probe = intProbe;
            } else {
                ByteBuffer costGridFloat = ByteBuffer.allocateDirect(flowCells * Float.BYTES).order(ByteOrder.nativeOrder());
                FloatBuffer costFloatView = costGridFloat.asFloatBuffer();
                for (int z = 0; z < flowD; z++) {
                    for (int x = 0; x < flowW; x++) {
                        boolean isWall = (x == 64 && z >= 16 && z < 112);
                        costFloatView.put(z * flowW + x, isWall ? 0.0f : 1.0f);
                    }
                }
                costGrid = costGridFloat;
                probe = api.computeFlowField(flowW, flowD, costGrid, goalX, goalZ, outDist, outDirX, outDirZ);
            }
        }

        // Warmup Flow-Field
        for (int w = 0; w < 3; w++) {
            api.computeFlowField(flowW, flowD, costGrid, goalX, goalZ, outDist, outDirX, outDirZ);
        }

        int reachable = probe;
        long tFlowStart = System.nanoTime();
        for (int k = 0; k < flowIters; k++) {
            reachable = api.computeFlowField(flowW, flowD, costGrid, goalX, goalZ, outDist, outDirX, outDirZ);
        }
        double flowFieldMs = (System.nanoTime() - tFlowStart) / 1_000_000.0 / flowIters;

        int sampleIdx = 64 * flowW + 32;
        float sampleDirX = outDirX.asFloatBuffer().get(sampleIdx);
        float sampleDirZ = outDirZ.asFloatBuffer().get(sampleIdx);

        // 2) 256x256 4-octave FBM WorldGen Noise (65,536 blocks)
        final int noiseW = 256;
        final int noiseD = 256;
        final int noiseCells = noiseW * noiseD;
        final int noiseOctaves = 4;
        final int noiseIters = 20;

        ByteBuffer outHeights = ByteBuffer.allocateDirect(noiseCells * Float.BYTES).order(ByteOrder.nativeOrder());

        for (int w = 0; w < 3; w++) {
            api.generateNoise2D(0.0f, 0.0f, noiseW, noiseD, 1.0f, 0x51CA2026, noiseOctaves, 0.008f, 2.0f, 0.5f, outHeights);
        }

        int noiseSamples = 0;
        long tNoiseStart = System.nanoTime();
        for (int k = 0; k < noiseIters; k++) {
            noiseSamples = api.generateNoise2D(
                0.0f, 0.0f, noiseW, noiseD, 1.0f, 0x51CA2026, noiseOctaves, 0.008f, 2.0f, 0.5f, outHeights
            );
        }
        double noiseMs = (System.nanoTime() - tNoiseStart) / 1_000_000.0 / noiseIters;

        FloatBuffer heightView = outHeights.asFloatBuffer();
        float minH = Float.POSITIVE_INFINITY;
        float maxH = Float.NEGATIVE_INFINITY;
        for (int i = 0; i < noiseCells; i++) {
            float h = heightView.get(i);
            if (Float.isFinite(h)) {
                if (h < minH) minH = h;
                if (h > maxH) maxH = h;
            }
        }
        if (!Float.isFinite(minH)) minH = 0.0f;
        if (!Float.isFinite(maxH)) maxH = 0.0f;

        // 3) Mob Visibility & Tracking Optimizer (100 players x 2,000 mobs = 200,000 pairs)
        final int trackPlayers = 100;
        final int trackMobs = 2_000;
        final int trackIters = 20;
        final int trackMaxDeltas = 2_048;
        final int trackBitsetBytes = ((trackPlayers * trackMobs) + 7) >>> 3;

        ByteBuffer mobXsBuf = ByteBuffer.allocateDirect(trackMobs * Float.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer mobYsBuf = ByteBuffer.allocateDirect(trackMobs * Float.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer mobZsBuf = ByteBuffer.allocateDirect(trackMobs * Float.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer mobFlagsBuf = ByteBuffer.allocateDirect(trackMobs * Integer.BYTES).order(ByteOrder.nativeOrder());

        ByteBuffer pXsBuf = ByteBuffer.allocateDirect(trackPlayers * Float.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer pYsBuf = ByteBuffer.allocateDirect(trackPlayers * Float.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer pZsBuf = ByteBuffer.allocateDirect(trackPlayers * Float.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer pLookXsBuf = ByteBuffer.allocateDirect(trackPlayers * Float.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer pLookYsBuf = ByteBuffer.allocateDirect(trackPlayers * Float.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer pLookZsBuf = ByteBuffer.allocateDirect(trackPlayers * Float.BYTES).order(ByteOrder.nativeOrder());

        ByteBuffer visBitsetBuf = ByteBuffer.allocateDirect(trackBitsetBytes).order(ByteOrder.nativeOrder());
        ByteBuffer outDeltaP = ByteBuffer.allocateDirect(trackMaxDeltas * Integer.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer outDeltaM = ByteBuffer.allocateDirect(trackMaxDeltas * Integer.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer outDeltaAct = ByteBuffer.allocateDirect(trackMaxDeltas * Integer.BYTES).order(ByteOrder.nativeOrder());
        ByteBuffer outStatsBuf = ByteBuffer.allocateDirect(4 * Integer.BYTES).order(ByteOrder.nativeOrder());

        FloatBuffer mobXs = mobXsBuf.asFloatBuffer();
        FloatBuffer mobYs = mobYsBuf.asFloatBuffer();
        FloatBuffer mobZs = mobZsBuf.asFloatBuffer();
        IntBuffer mobFlags = mobFlagsBuf.asIntBuffer();

        FloatBuffer pXs = pXsBuf.asFloatBuffer();
        FloatBuffer pYs = pYsBuf.asFloatBuffer();
        FloatBuffer pZs = pZsBuf.asFloatBuffer();
        FloatBuffer pLookXs = pLookXsBuf.asFloatBuffer();
        FloatBuffer pLookYs = pLookYsBuf.asFloatBuffer();
        FloatBuffer pLookZs = pLookZsBuf.asFloatBuffer();
        IntBuffer outStats = outStatsBuf.asIntBuffer();

        SplittableRandom trackRng = new SplittableRandom(0x51CA2400L);
        for (int i = 0; i < trackMobs; i++) {
            int cluster = i % 16;
            float cx = (cluster % 4) * 64.0f;
            float cz = (cluster / 4) * 64.0f;
            mobXs.put(i, cx + (float) (trackRng.nextDouble() * 48.0));
            mobYs.put(i, 32.0f + (float) (trackRng.nextDouble() * 64.0));
            mobZs.put(i, cz + (float) (trackRng.nextDouble() * 48.0));

            int flag = (i % 2 == 0) ? RustNative.TRACK_FLAG_HOSTILE : RustNative.TRACK_FLAG_PASSIVE;
            if (i % 25 == 0) {
                flag |= RustNative.TRACK_FLAG_ALWAYS_VISIBLE;
            }
            mobFlags.put(i, flag);
        }

        for (int p = 0; p < trackPlayers; p++) {
            pXs.put(p, (float) (trackRng.nextDouble() * 256.0));
            pYs.put(p, 64.0f + (float) ((trackRng.nextDouble() - 0.5) * 16.0));
            pZs.put(p, (float) (trackRng.nextDouble() * 256.0));

            double yaw = trackRng.nextDouble() * Math.PI * 2.0;
            double pitch = (trackRng.nextDouble() - 0.5) * 0.6;
            float cosP = (float) Math.cos(pitch);
            pLookXs.put(p, (float) (-Math.sin(yaw) * cosP));
            pLookYs.put(p, (float) (-Math.sin(pitch)));
            pLookZs.put(p, (float) (Math.cos(yaw) * cosP));
        }

        // Warmup Mob Tracking kernel
        for (int w = 0; w < 3; w++) {
            clearDirectBuffer(visBitsetBuf, trackBitsetBytes);
            api.computeMobTracking(
                trackMobs, mobXsBuf, mobYsBuf, mobZsBuf, mobFlagsBuf,
                trackPlayers, pXsBuf, pYsBuf, pZsBuf, pLookXsBuf, pLookYsBuf, pLookZsBuf,
                48.0f, 12.0f, 24.0f, 16.0f, -0.25f, 72, BrainDirective.STATE_GREEN,
                visBitsetBuf, trackMaxDeltas, outDeltaP, outDeltaM, outDeltaAct, outStatsBuf
            );
        }

        long trackNanos = 0L;
        int trackDeltas = 0;
        for (int k = 0; k < trackIters; k++) {
            clearDirectBuffer(visBitsetBuf, trackBitsetBytes);
            long tTrackStart = System.nanoTime();
            trackDeltas = api.computeMobTracking(
                trackMobs, mobXsBuf, mobYsBuf, mobZsBuf, mobFlagsBuf,
                trackPlayers, pXsBuf, pYsBuf, pZsBuf, pLookXsBuf, pLookYsBuf, pLookZsBuf,
                48.0f, 12.0f, 24.0f, 16.0f, -0.25f, 72, BrainDirective.STATE_GREEN,
                visBitsetBuf, trackMaxDeltas, outDeltaP, outDeltaM, outDeltaAct, outStatsBuf
            );
            trackNanos += (System.nanoTime() - tTrackStart);
        }
        double trackingMs = trackNanos / 1_000_000.0 / trackIters;
        int trackHidden = Math.max(0, outStats.get(0));
        int trackVisible = Math.max(0, outStats.get(1));
        int trackBudgetCulled = Math.max(0, outStats.get(3));

        return new KernelBenchmarkReport(
            flowW,
            flowD,
            flowIters,
            flowFieldMs,
            reachable,
            sampleDirX,
            sampleDirZ,
            noiseW,
            noiseD,
            noiseOctaves,
            noiseIters,
            noiseMs,
            noiseSamples,
            minH,
            maxH,
            trackPlayers,
            trackMobs,
            trackIters,
            trackingMs,
            trackHidden,
            trackVisible,
            trackBudgetCulled,
            Math.max(0, trackDeltas)
        );
    }

    private static void clearDirectBuffer(ByteBuffer buf, int length) {
        int idx = 0;
        int longLimit = length & ~7;
        while (idx < longLimit) {
            buf.putLong(idx, 0L);
            idx += 8;
        }
        while (idx < length) {
            buf.put(idx, (byte) 0);
            idx++;
        }
    }

    public static BenchmarkReport run(SpicaRustAPI api, int entities, int players, int iterations) {
        int n = Math.clamp(entities, 200, 30_000);
        int m = Math.clamp(players, 1, 2_000);
        int iters = Math.clamp(iterations, 5, 200);

        EntityBatchArena arena = api.createArena(n, m);

        float[] jx = new float[n];
        float[] jy = new float[n];
        float[] jz = new float[n];
        float[] jvx = new float[n];
        float[] jvy = new float[n];
        float[] jvz = new float[n];
        float[] jr = new float[n];
        float[] jpx = new float[m];
        float[] jpy = new float[m];
        float[] jpz = new float[m];

        SplittableRandom rng = new SplittableRandom(0x51CA2026L);
        for (int i = 0; i < n; i++) {
            int cluster = i % 16;
            float cx = (cluster % 4) * 64.0f;
            float cz = (cluster / 4) * 64.0f;
            float x = cx + (float) (rng.nextDouble() * 18.0);
            float y = 64.0f + (float) (rng.nextDouble() * 6.0);
            float z = cz + (float) (rng.nextDouble() * 18.0);
            float vx = (float) ((rng.nextDouble() - 0.5) * 0.4);
            float vz = (float) ((rng.nextDouble() - 0.5) * 0.4);

            jx[i] = x;
            jy[i] = y;
            jz[i] = z;
            jvx[i] = vx;
            jvy[i] = 0.0f;
            jvz[i] = vz;
            jr[i] = 0.45f;

            arena.setEntityPosition(i, x, y, z, 0.45f);
            arena.setEntityVelocity(i, vx, 0.0f, vz);
        }

        for (int j = 0; j < m; j++) {
            float x = (float) (rng.nextDouble() * 256.0);
            float y = 64.0f;
            float z = (float) (rng.nextDouble() * 256.0);
            jpx[j] = x;
            jpy[j] = y;
            jpz[j] = z;
            arena.setTargetPosition(j, x, y, z);
        }

        // Warmup both JVM JIT and Rust Rayon pool
        float[] jOutX = new float[n];
        float[] jOutY = new float[n];
        float[] jOutZ = new float[n];
        int[] jOutCounts = new int[n];
        int[] jOutIdx = new int[n];
        float[] jOutDist = new float[n];
        int[] jOutLod = new int[n];

        for (int w = 0; w < 3; w++) {
            javaCollisionsNaive(n, jx, jy, jz, jr, jOutX, jOutY, jOutZ, jOutCounts);
            api.computeCollisions(arena, n, 1.5f, 0.5f, 0.4f);
            javaAiTargets(n, m, jx, jy, jz, jpx, jpy, jpz, 32f, 56f, jOutIdx, jOutDist, jOutLod);
            api.computeAiTargets(arena, n, m, 32f, 56f);
            api.computeBoidsSteering(arena, n, 128f, 64f, 128f, 5f, 1.2f, 0.6f, 0.4f, 1.0f, 0.5f);
        }

        // 1) Pure Java Collisions (use fewer iterations if N is very large to keep benchmark snappy)
        int javaCollIters = n > 6000 ? Math.min(iters, 5) : Math.min(iters, 20);
        long t0 = System.nanoTime();
        for (int k = 0; k < javaCollIters; k++) {
            javaCollisionsNaive(n, jx, jy, jz, jr, jOutX, jOutY, jOutZ, jOutCounts);
        }
        double javaCollMs = (System.nanoTime() - t0) / 1_000_000.0 / javaCollIters;

        // 2) Rust Spatial Hash + Rayon Collisions
        long t1 = System.nanoTime();
        long rustPairs = 0;
        for (int k = 0; k < iters; k++) {
            rustPairs = api.computeCollisions(arena, n, 1.5f, 0.5f, 0.4f);
        }
        double rustCollMs = (System.nanoTime() - t1) / 1_000_000.0 / iters;

        // 3) Pure Java AI Target + LOD
        long t2 = System.nanoTime();
        for (int k = 0; k < iters; k++) {
            javaAiTargets(n, m, jx, jy, jz, jpx, jpy, jpz, 32f, 56f, jOutIdx, jOutDist, jOutLod);
        }
        double javaAiMs = (System.nanoTime() - t2) / 1_000_000.0 / iters;

        // 4) Rust Parallel AI Target + LOD
        long t3 = System.nanoTime();
        for (int k = 0; k < iters; k++) {
            api.computeAiTargets(arena, n, m, 32f, 56f);
        }
        double rustAiMs = (System.nanoTime() - t3) / 1_000_000.0 / iters;

        // 5) Rust Boids Steering
        long t4 = System.nanoTime();
        for (int k = 0; k < iters; k++) {
            api.computeBoidsSteering(arena, n, 128f, 64f, 128f, 5f, 1.2f, 0.6f, 0.4f, 1.0f, 0.5f);
        }
        double rustBoidsMs = (System.nanoTime() - t4) / 1_000_000.0 / iters;

        return new BenchmarkReport(
            n, m, iters,
            javaCollMs, rustCollMs,
            javaAiMs, rustAiMs,
            rustBoidsMs, rustPairs
        );
    }

    private static void javaCollisionsNaive(
        int n,
        float[] xs, float[] ys, float[] zs, float[] radii,
        float[] outPx, float[] outPy, float[] outPz, int[] outCounts
    ) {
        for (int i = 0; i < n; i++) {
            float xi = xs[i], yi = ys[i], zi = zs[i], ri = radii[i];
            float px = 0f, py = 0f, pz = 0f;
            int count = 0;
            for (int j = 0; j < n; j++) {
                if (i == j) continue;
                float dx = xi - xs[j];
                float dy = yi - ys[j];
                float dz = zi - zs[j];
                float minD = ri + radii[j];
                float d2 = dx * dx + dy * dy + dz * dz;
                if (d2 < minD * minD) {
                    count++;
                    if (d2 > 1e-6f) {
                        float d = (float) Math.sqrt(d2);
                        float overlap = (minD - d) * 0.5f;
                        float inv = 1.0f / d;
                        px += dx * inv * overlap;
                        py += dy * inv * overlap * 0.25f;
                        pz += dz * inv * overlap;
                    }
                }
            }
            outPx[i] = px;
            outPy[i] = py;
            outPz[i] = pz;
            outCounts[i] = count;
        }
    }

    private static void javaAiTargets(
        int n, int m,
        float[] mx, float[] my, float[] mz,
        float[] tx, float[] ty, float[] tz,
        float fullRange, float reducedRange,
        int[] outIdx, float[] outDistSq, int[] outLod
    ) {
        float fullSq = fullRange * fullRange;
        float redSq = reducedRange * reducedRange;
        for (int i = 0; i < n; i++) {
            float x = mx[i], y = my[i], z = mz[i];
            int best = -1;
            float bestD2 = Float.MAX_VALUE;
            for (int j = 0; j < m; j++) {
                float dx = tx[j] - x;
                float dy = ty[j] - y;
                float dz = tz[j] - z;
                float d2 = dx * dx + dy * dy + dz * dz;
                if (d2 < bestD2) {
                    bestD2 = d2;
                    best = j;
                }
            }
            outIdx[i] = best;
            outDistSq[i] = bestD2;
            outLod[i] = bestD2 <= fullSq ? 0 : (bestD2 <= redSq ? 1 : 2);
        }
    }
}
