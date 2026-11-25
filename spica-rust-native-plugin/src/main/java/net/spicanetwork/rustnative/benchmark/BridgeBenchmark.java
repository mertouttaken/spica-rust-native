package net.spicanetwork.rustnative.benchmark;

import net.spicanetwork.rustnative.api.SpicaRustAPI;
import net.spicanetwork.rustnative.nativebridge.EntityBatchArena;

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
        float noiseMaxHeight
    ) {
    }

    /**
     * Benchmarks the v2.3.0 Rust native kernels asynchronously:
     * 1. 128x128 Dijkstra Flow-Field Pathfinding with an obstacle wall
     * 2. 256x256 4-octave FBM WorldGen Noise (65,536 blocks)
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
            maxH
        );
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
