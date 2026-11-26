package net.spicanetwork.rustnative.nativebridge;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.SplittableRandom;
import java.util.function.IntUnaryOperator;
import java.util.stream.IntStream;

/** Standalone, game-independent parity and timing harness for the direct JNI entry points. */
public final class KernelParityBenchmark {
    private static final int ENTITIES = 5000;
    private static final int PLAYERS = 100;
    private static final float DIAGONAL = 0.70710677f;
    private static final int[] DX = {1, -1, 0, 0, 1, -1, 1, -1};
    private static final int[] DZ = {0, 0, 1, -1, 1, 1, -1, -1};
    private static volatile long sink;
    private static int workers;

    private record Result(IntUnaryOperator java, IntUnaryOperator rust, Runnable verify) {
        void check(String name) {
            int cases = name.equals("governor") ? 128 : 1;
            try {
                for (int i = 0; i < cases; i++) {
                    if (java.applyAsInt(i) != rust.applyAsInt(i)) {
                        throw new AssertionError("return value differs at input " + i);
                    }
                    verify.run();
                }
            } catch (AssertionError mismatch) {
                throw new AssertionError(name + ": " + mismatch.getMessage(), mismatch);
            }
        }
    }

    private record Case(String name, String size, Result result) {}

    private record Floats(float[] array, ByteBuffer direct) {
        Floats(int length) { this(new float[length], buffer(4 * length)); }
        void upload() { direct.asFloatBuffer().put(array); }
        void verify() {
            var values = direct.asFloatBuffer();
            for (int i = 0; i < array.length; i++) {
                float a = array[i], b = values.get(i);
                if (a != b && (!Float.isFinite(a) || !Float.isFinite(b) || Math.abs(a - b) > 0.00005f)) {
                    throw new AssertionError("float output " + i + ": " + a + " != " + b);
                }
            }
        }
    }

    private record Ints(int[] array, ByteBuffer direct) {
        Ints(int length) { this(new int[length], buffer(4 * length)); }
        void upload() { direct.asIntBuffer().put(array); }
        void verify() {
            var values = direct.asIntBuffer();
            for (int i = 0; i < array.length; i++) {
                if (array[i] != values.get(i)) {
                    throw new AssertionError("int output " + i + ": " + array[i] + " != " + values.get(i));
                }
            }
        }
    }

    private record Points(Floats x, Floats y, Floats z) {
        Points(int length) { this(new Floats(length), new Floats(length), new Floats(length)); }
        void upload() { x.upload(); y.upload(); z.upload(); }
        void verify() { x.verify(); y.verify(); z.verify(); }
        int size() { return x.array.length; }
    }

    private static ByteBuffer buffer(int capacity) {
        return ByteBuffer.allocateDirect(capacity).order(ByteOrder.nativeOrder());
    }

    private static Points points(int count, SplittableRandom rng) {
        Points p = new Points(count);
        for (int i = 0; i < count; i++) {
            int cluster = i & 15;
            p.x.array[i] = (cluster & 3) * 40 + rng.nextFloat() * 14;
            p.y.array[i] = rng.nextFloat() * 9 + 64;
            p.z.array[i] = (cluster >> 2) * 40 + rng.nextFloat() * 14;
        }
        p.upload();
        return p;
    }

    private static float square(float a) { return a * a; }

    private static float distance(Points a, int i, Points b, int j) {
        return square(a.x.array[i] - b.x.array[j]) + square(a.y.array[i] - b.y.array[j])
            + square(a.z.array[i] - b.z.array[j]);
    }

    private static IntStream work(int size, boolean parallel) {
        IntStream indices = IntStream.range(0, size);
        return parallel && workers > 1 ? indices.parallel() : indices;
    }

    // Same CSR bucket layout, hash and encounter order used by native item merging.
    private static final class Grid {
        private static final ThreadLocal<int[]> BUCKETS = ThreadLocal.withInitial(() -> new int[27]);
        final Points points;
        final float inverse;
        final int mask;
        final int[] offsets;
        final int[] indices;

        Grid(Points points, float cellSize) {
            this.points = points;
            inverse = 1f / cellSize;
            int n = points.size();
            int buckets = Math.clamp(Integer.highestOneBit(2 * n - 1) * 2, 1024, 262144);
            mask = buckets - 1;
            offsets = new int[buckets + 1];
            indices = new int[n];
            int[] entityBuckets = new int[n];
            for (int i = 0; i < n; i++) {
                int key = key(cell(points.x.array[i]), cell(points.y.array[i]), cell(points.z.array[i]));
                entityBuckets[i] = key;
                offsets[key + 1]++;
            }
            for (int i = 1; i <= buckets; i++) offsets[i] += offsets[i - 1];
            int[] cursor = Arrays.copyOf(offsets, buckets);
            for (int i = 0; i < n; i++) indices[cursor[entityBuckets[i]]++] = i;
        }

        int cell(float coord) { return (int) Math.floor(coord * inverse); }

        int key(int x, int y, int z) { return (x * 73856093 ^ y * 19349663 ^ z * 83492791) & mask; }

        int collect(int i, int[] output) {
            int x = cell(points.x.array[i]), y = cell(points.y.array[i]), z = cell(points.z.array[i]);
            int n = 0;
            for (int dx = -1; dx <= 1; dx++) {
                for (int dy = -1; dy <= 1; dy++) {
                    for (int dz = -1; dz <= 1; dz++) {
                        int b = key(x + dx, y + dy, z + dz);
                        if (offsets[b] == offsets[b + 1]) continue;
                        int k = 0;
                        while (k < n && output[k] != b) k++;
                        if (k == n) output[n++] = b;
                    }
                }
            }
            return n;
        }
    }

    private static Result collisions(Points p) {
        int n = p.size();
        Points push = new Points(n);
        Ints counts = new Ints(n);
        Floats radius = new Floats(n);
        Arrays.fill(radius.array, .45f);
        radius.upload();
        return new Result(ignore -> {
            var grid = new Grid(p, 1.5f);
            return work(n, n >= 512).map(i -> {
                int[] buckets = Grid.BUCKETS.get();
                int length = grid.collect(i, buckets), neighbors = 0;
                float x = 0, y = 0, z = 0;
                for (int b = 0; b < length; b++) {
                    int bucket = buckets[b];
                    for (int t = grid.offsets[bucket]; t < grid.offsets[bucket + 1]; t++) {
                        int j = grid.indices[t];
                        if (i == j) continue;
                        float dx = p.x.array[i] - p.x.array[j], dy = p.y.array[i] - p.y.array[j], dz = p.z.array[i] - p.z.array[j];
                        float d = square(dx) + square(dy) + square(dz);
                        float separation = radius.array[i] + radius.array[j];
                        if (d >= square(separation)) continue;
                        neighbors++;
                        if (d > 1e-6f) {
                            float dist = (float) Math.sqrt(d);
                            float overlap = (separation - dist) * .5f, inv = 1f / dist;
                            x += dx * inv * overlap;
                            y += dy * inv * overlap * .25f;
                            z += dz * inv * overlap;
                        } else {
                            float angle = (i * (int) 2654435761L & 1023) * .006135923f;
                            x += (float) Math.cos(angle) * .05f * .5f;
                            z += (float) Math.sin(angle) * .05f * .5f;
                        }
                    }
                }
                float mag = square(x) + square(y) + square(z);
                if (mag > square(.4f)) {
                    float scale = .4f / (float) Math.sqrt(mag);
                    x *= scale;
                    y *= scale;
                    z *= scale;
                }
                push.x.array[i] = x;
                push.y.array[i] = y;
                push.z.array[i] = z;
                counts.array[i] = neighbors;
                return neighbors;
            }).sum();
        }, ignore -> Math.toIntExact(RustNative.nativeComputeCollisions(n, p.x.direct, p.y.direct, p.z.direct,
            radius.direct, 1.5f, .5f, .4f, push.x.direct, push.y.direct, push.z.direct, counts.direct)),
            () -> { push.verify(); counts.verify(); });
    }

    private static Result nearest(Points mobs, Points targets) {
        int n = mobs.size(), m = targets.size();
        Ints index = new Ints(n), tier = new Ints(n);
        Floats squared = new Floats(n);
        Points dir = new Points(n);
        return new Result(ignore -> work(n, n >= 512).map(i -> {
            int jBest = -1;
            float dBest = Float.MAX_VALUE;
            float mx = mobs.x.array[i], my = mobs.y.array[i], mz = mobs.z.array[i];
            float dxBest = 0, dyBest = 0, dzBest = 0;
            for (int j = 0; j < m; j++) {
                float dx = targets.x.array[j] - mx, dy = targets.y.array[j] - my, dz = targets.z.array[j] - mz;
                float d = square(dx) + square(dy) + square(dz);
                if (d < dBest) {
                    dBest = d;
                    jBest = j;
                    dxBest = dx;
                    dyBest = dy;
                    dzBest = dz;
                }
            }
            int level = dBest <= 32 * 32 ? 0 : dBest <= 56 * 56 ? 1 : 2;
            index.array[i] = jBest;
            squared.array[i] = dBest;
            tier.array[i] = level;
            float inv = level < 2 && dBest > 1e-6f ? 1f / (float) Math.sqrt(dBest) : 0;
            dir.x.array[i] = dxBest * inv;
            dir.y.array[i] = dyBest * inv;
            dir.z.array[i] = dzBest * inv;
            return level == 0 ? 1 : 0;
        }).sum(), ignore -> RustNative.nativeComputeAiTargets(n, mobs.x.direct, mobs.y.direct, mobs.z.direct, m,
            targets.x.direct, targets.y.direct, targets.z.direct, 32, 56, index.direct, squared.direct,
            dir.x.direct, dir.y.direct, dir.z.direct, tier.direct),
            () -> { index.verify(); squared.verify(); tier.verify(); dir.verify(); });
    }

    private static Result radius(Points p) {
        Ints hits = new Ints(p.size());
        return new Result(k -> {
            int count = 0;
            for (int i = 0; i < p.size(); i++) {
                if (square(p.x.array[i] - 50) + square(p.y.array[i] - 64) + square(p.z.array[i] - 50) <= 25 * 25) {
                    hits.array[count++] = i;
                }
            }
            return count;
        }, k -> RustNative.nativeQueryRadius(p.size(), p.x.direct, p.y.direct, p.z.direct,
            50, 64, 50, 25, hits.array.length, hits.direct), hits::verify);
    }

    private static Result diff(boolean countOnly, SplittableRandom rng) {
        int w = 64, n = w * w * w;
        Ints cur = new Ints(n), target = new Ints(n);
        for (int i = 0; i < n; i++) {
            cur.array[i] = rng.nextInt(1000);
            target.array[i] = i % 31 == 0 ? cur.array[i] + 1 : cur.array[i];
        }
        cur.upload(); target.upload();
        Ints x = new Ints(n / 30), y = new Ints(n / 30), z = new Ints(n / 30), id = new Ints(n / 30);
        return new Result(k -> {
            int found = 0;
            if (countOnly) {
                return work(n / 8192, true).map(tile -> {
                    int count = 0;
                    for (int i = tile * 8192; i < (tile + 1) * 8192; i++) {
                        count += cur.array[i] == target.array[i] ? 0 : 1;
                    }
                    return count;
                }).sum();
            }
            // Same deterministic YZX order. A serial compaction avoids a second scan on Java.
            for (int i = 0; i < n && found < id.array.length; i++) {
                if (cur.array[i] != target.array[i]) {
                    x.array[found] = i % w;
                    y.array[found] = i / (w * w);
                    z.array[found] = i / w % w;
                    id.array[found++] = target.array[i];
                }
            }
            return found;
        }, k -> RustNative.nativeComputeBlockDiff(w, w, w, cur.direct, target.direct,
            countOnly ? 0 : id.array.length, x.direct, y.direct, z.direct, id.direct),
            () -> { if (!countOnly) { x.verify(); y.verify(); z.verify(); id.verify(); } });
    }

    private static Result boids(Points p, Points velocity) {
        int n = p.size();
        Points output = new Points(n);
        return new Result(k -> {
            Grid grid = new Grid(p, 5);
            work(n, n >= 512).forEach(i -> {
                float x = p.x.array[i], y = p.y.array[i], z = p.z.array[i];
                float sx = 0, sy = 0, sz = 0, ux = 0, uy = 0, uz = 0, cx = 0, cy = 0, cz = 0;
                int nearby = 0, close = 0;
                int[] buckets = Grid.BUCKETS.get();
                int length = grid.collect(i, buckets);
                for (int b = 0; b < length; b++) {
                    int key = buckets[b];
                    for (int t = grid.offsets[key]; t < grid.offsets[key + 1]; t++) {
                        int j = grid.indices[t];
                        if (i == j) continue;
                        float dx = p.x.array[j] - x, dy = p.y.array[j] - y, dz = p.z.array[j] - z;
                        float d = square(dx) + square(dy) + square(dz);
                        if (d >= 25 || d <= 1e-6f) continue;
                        nearby++;
                        ux += velocity.x.array[j]; uy += velocity.y.array[j]; uz += velocity.z.array[j];
                        cx += p.x.array[j]; cy += p.y.array[j]; cz += p.z.array[j];
                        if (d < square(5 * .45f)) {
                            float inv = 1f / d;
                            sx -= dx * inv; sy -= dy * inv; sz -= dz * inv;
                            close++;
                        }
                    }
                }
                float sep = close == 0 ? 0 : 1.2f / close;
                float ax = sx * sep, ay = sy * sep, az = sz * sep;
                if (nearby > 0) {
                    float inv = 1f / nearby;
                    ax += (ux * inv - velocity.x.array[i]) * .6f;
                    ay += (uy * inv - velocity.y.array[i]) * .6f;
                    az += (uz * inv - velocity.z.array[i]) * .6f;
                    ax += (cx * inv - x) * .4f;
                    ay += (cy * inv - y) * .4f;
                    az += (cz * inv - z) * .4f;
                }
                float gx = 64 - x, gy = 68 - y, gz = 64 - z;
                float goal = square(gx) + square(gy) + square(gz);
                if (goal > 1e-4f) {
                    float inv = 1f / (float) Math.sqrt(goal);
                    ax += gx * inv; ay += gy * inv; az += gz * inv;
                }
                float mag = square(ax) + square(ay) + square(az);
                if (mag > .25f) {
                    float scale = .5f / (float) Math.sqrt(mag);
                    ax *= scale; ay *= scale; az *= scale;
                }
                output.x.array[i] = ax; output.y.array[i] = ay; output.z.array[i] = az;
            });
            return n;
        }, k -> RustNative.nativeComputeBoids(n, p.x.direct, p.y.direct, p.z.direct,
            velocity.x.direct, velocity.y.direct, velocity.z.direct, 64, 68, 64, 5,
            1.2f, .6f, .4f, 1f, .5f, output.x.direct, output.y.direct, output.z.direct), output::verify);
    }

    private static long chunk(Points p, int i) {
        int x = (int) Math.floor(p.x.array[i]) >> 4, z = (int) Math.floor(p.z.array[i]) >> 4;
        return (long) x << 32 | Integer.toUnsignedLong(z);
    }

    private static Result farm(Points p, Points players) {
        int n = p.size();
        Ints flags = new Ints(n), action = new Ints(n), count = new Ints(n);
        for (int i = 0; i < n; i++) {
            flags.array[i] = i % 13 == 0 ? 6 : i % 11 == 0 ? 1 : i % 9 == 0 ? 2 : i % 17 == 0 ? 8 : 0;
        }
        flags.upload();
        return new Result(k -> {
            var density = new java.util.HashMap<Long, int[]>();
            for (int i = 0; i < n; i++) density.computeIfAbsent(chunk(p, i), key -> new int[1])[0]++;
            var grid = new Grid(p, 3.5f);
            return work(n, n >= 512).map(i -> {
                int[] buckets = Grid.BUCKETS.get();
                int length = grid.collect(i, buckets), neighbors = 0;
                for (int b = 0; b < length; b++) {
                    int key = buckets[b];
                    for (int t = grid.offsets[key]; t < grid.offsets[key + 1]; t++) {
                        int j = grid.indices[t];
                        if (i != j && distance(p, i, p, j) <= square(3.5f)) neighbors++;
                    }
                }
                int f = flags.array[i], chunkCount = density.get(chunk(p, i))[0], a = 0;
                if ((f & 9) == 0) {
                    if ((f & 2) != 0) {
                        float nearest = Float.MAX_VALUE;
                        for (int j = 0; j < players.size(); j++) nearest = Math.min(nearest, distance(p, i, players, j));
                        if (nearest > 64 && ((f & 4) != 0 || neighbors >= 3 || chunkCount >= 34)) a = 3;
                    } else if (neighbors >= 10 || chunkCount >= 34) {
                        a = 3;
                    } else if (neighbors >= 6) {
                        a = 1;
                    }
                }
                count.array[i] = neighbors;
                action.array[i] = a;
                return a != 0 ? 1 : 0;
            }).sum();
        }, k -> RustNative.nativeEvaluateFarmThrottling(n, p.x.direct, p.y.direct, p.z.direct, flags.direct,
            players.size(), players.x.direct, players.y.direct, players.z.direct, 3.5f, 8, 14, 48, 8, 4, 2,
            action.direct, count.direct), () -> { action.verify(); count.verify(); });
    }

    private static Result merges(Points p, SplittableRandom rng) {
        int n = p.size();
        Ints type = new Ints(n), amount = new Ints(n), dst = new Ints(n), src = new Ints(n);
        for (int i = 0; i < n; i++) {
            type.array[i] = rng.nextInt(12);
            amount.array[i] = rng.nextInt(1, 65);
        }
        type.upload(); amount.upload();
        return new Result(k -> {
            var grid = new Grid(p, 3f);
            boolean[] consumed = new boolean[n];
            int[] remaining = new int[n];
            for (int i = 0; i < n; i++) remaining[i] = 64 - amount.array[i];
            int count = 0;
            int[] buckets = Grid.BUCKETS.get();
            for (int i = 0; i < n; i++) {
                if (consumed[i] || remaining[i] <= 0) continue;
                int length = grid.collect(i, buckets);
                for (int b = 0; b < length && remaining[i] > 0; b++) {
                    int key = buckets[b];
                    for (int t = grid.offsets[key]; t < grid.offsets[key + 1] && remaining[i] > 0; t++) {
                        int j = grid.indices[t];
                        if (j <= i || consumed[j] || type.array[j] != type.array[i] || amount.array[j] > remaining[i]) continue;
                        if (distance(p, i, p, j) <= 9) {
                            consumed[j] = true;
                            remaining[i] -= amount.array[j];
                            dst.array[count] = i;
                            src.array[count++] = j;
                        }
                    }
                }
            }
            return count;
        }, k -> RustNative.nativePlanItemMerges(n, p.x.direct, p.y.direct, p.z.direct, type.direct,
            amount.direct, 64, 3, n, dst.direct, src.direct), () -> { dst.verify(); src.verify(); });
    }

    private static int insert(int[] top, int count, int item, java.util.function.IntBinaryOperator order) {
        int pos = Math.min(count, top.length - 1);
        if (count == top.length && order.applyAsInt(item, top[pos]) >= 0) return count;
        while (pos > 0 && order.applyAsInt(item, top[pos - 1]) < 0) {
            top[pos] = top[pos - 1];
            pos--;
        }
        top[pos] = item;
        return Math.min(top.length, count + 1);
    }

    private static Result heatmap(Points p, SplittableRandom rng) {
        Ints neighbors = new Ints(p.size());
        for (int i = 0; i < p.size(); i++) neighbors.array[i] = rng.nextInt(30);
        neighbors.upload();
        int length = 8;
        Ints x = new Ints(length), z = new Ints(length), count = new Ints(length), crowded = new Ints(length);
        return new Result(k -> {
            var chunks = new java.util.HashMap<Long, int[]>();
            for (int i = 0; i < p.size(); i++) {
                long key = chunk(p, i);
                int[] row = chunks.computeIfAbsent(key, c -> new int[]{(int) (c >> 32), c.intValue(), 0, 0});
                row[2]++;
                if (neighbors.array[i] >= 6) row[3]++;
            }
            int[][] rows = chunks.values().toArray(int[][]::new);
            int[] top = new int[length];
            int found = 0;
            java.util.function.IntBinaryOperator order = (i, j) -> {
                int[] a = rows[i], b = rows[j];
                int cmp = Integer.compare(b[3] * 4 + b[2], a[3] * 4 + a[2]);
                if (cmp == 0) cmp = Integer.compare(b[2], a[2]);
                if (cmp == 0) cmp = Integer.compare(a[0], b[0]);
                return cmp == 0 ? Integer.compare(a[1], b[1]) : cmp;
            };
            for (int i = 0; i < rows.length; i++) found = insert(top, found, i, order);
            for (int i = 0; i < found; i++) {
                int[] values = rows[top[i]];
                x.array[i] = values[0]; z.array[i] = values[1];
                count.array[i] = values[2]; crowded.array[i] = values[3];
            }
            return found;
        }, k -> RustNative.nativeAnalyzeChunks(p.size(), p.x.direct, p.z.direct, neighbors.direct,
            length, x.direct, z.direct, count.direct, crowded.direct),
            () -> { x.verify(); z.verify(); count.verify(); crowded.verify(); });
    }

    private static Result surge(Points p, Points speed, SplittableRandom rng) {
        int n = p.size();
        Ints gliding = new Ints(n), action = new Ints(n);
        for (int i = 0; i < n; i++) gliding.array[i] = rng.nextInt(2);
        gliding.upload();
        Points output = new Points(n);
        return new Result(k -> {
            int count = 0;
            float cap = 1.8f * .7f;
            for (int i = 0; i < n; i++) {
                float x = speed.x.array[i], y = speed.y.array[i], z = speed.z.array[i];
                int a = 0;
                if (gliding.array[i] != 0) {
                    double h2 = (double) x * x + (double) z * z;
                    float h = (float) Math.sqrt(h2), s = (float) Math.sqrt(h2 + (double) y * y);
                    if (h > cap || s > cap * 1.2f) {
                        float factor = h > cap ? cap / h : 1;
                        x *= factor; z *= factor;
                        y = Math.clamp(y, -cap * 1.2f, cap * .65f);
                        a = 2;
                        count++;
                    } else if (h > cap * .85f) {
                        a = 1;
                    }
                }
                output.x.array[i] = x; output.y.array[i] = y; output.z.array[i] = z;
                action.array[i] = a;
            }
            return count;
        }, k -> RustNative.nativeEvaluatePlayerSurges(n, p.x.direct, p.y.direct, p.z.direct,
            speed.x.direct, speed.y.direct, speed.z.direct, gliding.direct, 2, 1.8f, action.direct,
            output.x.direct, output.y.direct, output.z.direct), () -> { action.verify(); output.verify(); });
    }

    private static Result redstone(SplittableRandom rng) {
        int n = 1024, limit = 8;
        Ints x = new Ints(n), z = new Ints(n), rs = new Ints(n), hop = new Ints(n), action = new Ints(n), top = new Ints(limit);
        Floats previous = new Floats(n), heat = new Floats(n), ratio = new Floats(n);
        for (int i = 0; i < n; i++) {
            x.array[i] = i % 32; z.array[i] = i / 32;
            rs.array[i] = rng.nextInt(640); hop.array[i] = rng.nextInt(480);
            previous.array[i] = rng.nextFloat() * 400;
        }
        x.upload(); z.upload(); rs.upload(); hop.upload(); previous.upload();
        return new Result(k -> {
            int throttled = work(n, n >= 512).map(i -> {
                float r = rs.array[i] / 224f, h = hop.array[i] / 168f, maximum = Math.max(r, h);
                float instant = Math.min(maximum * 100, 1000), prev = previous.array[i];
                float next = instant > prev ? prev * .4f + instant * .6f : prev * .7f + instant * .3f;
                int a = ((r >= 1 && next >= 85) || r >= 2 ? 1 : 0) | ((h >= 1 && next >= 85) || h >= 2 ? 2 : 0);
                action.array[i] = a;
                heat.array[i] = next;
                ratio.array[i] = a == 0 ? 0 : Math.clamp(.5f + Math.max(0, maximum - 1) * .225f, .5f, .95f);
                return a > 0 ? 1 : 0;
            }).sum();
            java.util.function.IntBinaryOperator order = (a, b) -> {
                int rank = Float.compare(heat.array[b], heat.array[a]);
                if (rank == 0) rank = Integer.compare(rs.array[b] + hop.array[b], rs.array[a] + hop.array[a]);
                if (rank == 0) rank = Integer.compare(x.array[b], x.array[a]);
                if (rank == 0) rank = Integer.compare(z.array[b], z.array[a]);
                return rank == 0 ? Integer.compare(a, b) : rank;
            };
            int size = 0;
            for (int i = 0; i < n; i++) if (heat.array[i] > 0 || rs.array[i] > 0 || hop.array[i] > 0) {
                size = insert(top.array, size, i, order);
            }
            Arrays.fill(top.array, size, limit, -1);
            return throttled;
        }, k -> RustNative.nativeEvaluateRedstoneChunks(n, x.direct, z.direct, rs.direct, hop.direct, previous.direct, 320,
            240, 2, action.direct, heat.direct, ratio.direct, limit, top.direct),
            () -> { action.verify(); heat.verify(); ratio.verify(); top.verify(); });
    }

    private static Result governor() {
        Ints state = new Ints(4);
        Floats data = new Floats(4);
        float[] scale = {1f, .85f, .7f, .55f, .4f};
        float[] merge = {2.5f, 3f, 3.5f, 4.5f, 5.5f};
        return new Result(k -> {
            float ms = 15 + (k & 63), prev = 20 + (k & 31);
            float ewma = .25f * ms + .75f * prev;
            int s = ewma < 28 ? 0 : ewma < 38 ? 1 : ewma < 45 ? 2 : ewma < 52 ? 3 : 4;
            if ((k & 1) != 0) s = Math.min(4, s + 1);
            state.array[0] = s;
            state.array[1] = s < 3 ? 10 - s : s == 3 ? 6 : 5;
            state.array[2] = s < 3 ? 8 - s : 4;
            state.array[3] = s == 0 ? 0 : 1;
            data.array[0] = Math.max(32 * scale[s], 10);
            data.array[1] = Math.max(56 * scale[s], data.array[0] + 6);
            data.array[2] = merge[s];
            data.array[3] = ewma;
            return s;
        }, k -> RustNative.nativeEvaluateGovernor(15 + (k & 63), 20 + (k & 31), (k & 1) == 0 ? 3000 : 5000,
            100, 32, 56, 5, 10, 4, 8, state.direct, data.direct),
            () -> { state.verify(); data.verify(); });
    }

    private static final float[] GRAD_X = {1, -1, 0, 0, DIAGONAL, -DIAGONAL, DIAGONAL, -DIAGONAL};
    private static final float[] GRAD_Z = {0, 0, 1, -1, DIAGONAL, DIAGONAL, -DIAGONAL, -DIAGONAL};

    private static float fade(float t) { return t * t * t * (t * (t * 6 - 15) + 10); }

    private static float gradient(int ix, int iz, int seed, float x, float z) {
        int hash = ix * 374761393 ^ iz * 668265263 ^ seed * (int) 2246822519L;
        hash = (hash ^ (hash >>> 13)) * 1274126177;
        int index = (hash ^ (hash >>> 16)) & 7;
        return GRAD_X[index] * x + GRAD_Z[index] * z;
    }

    private static float gradientNoise(float x, float z, int seed) {
        int ix = (int) Math.floor(x), iz = (int) Math.floor(z);
        float fx = x - ix, fz = z - iz, u = fade(fx), v = fade(fz);
        float a = gradient(ix, iz, seed, fx, fz);
        float b = gradient(ix, iz + 1, seed, fx, fz - 1);
        float top = a + u * (gradient(ix + 1, iz, seed, fx - 1, fz) - a);
        float bottom = b + u * (gradient(ix + 1, iz + 1, seed, fx - 1, fz - 1) - b);
        return Math.clamp((top + v * (bottom - top)) * 1.4142135f, -1f, 1f);
    }

    private static Result noise() {
        int width = 256;
        Floats out = new Floats(width * width);
        return new Result(k -> {
            work(width, true).forEach(z -> {
                float wz = -32 + z * .75f;
                for (int x = 0; x < width; x++) {
                    float wx = 64 + x * .75f;
                    float result = 0, norm = 0, weight = 1, freq = .025f;
                    for (int octave = 0; octave < 4; octave++) {
                        result += gradientNoise(wx * freq, wz * freq, 765 + octave * 1013904223) * weight;
                        norm += weight;
                        freq *= 2;
                        weight *= .5f;
                    }
                    out.array[z * width + x] = Math.clamp(result / norm, -1f, 1f);
                }
            });
            return out.array.length;
        }, k -> RustNative.nativeGenerateNoise2D(64, -32, width, width, .75f, 765, 4,
            .025f, 2, .5f, out.direct), out::verify);
    }

    private static final class Heap {
        private long[] values;
        int size;

        Heap(int capacity) { values = new long[capacity]; }

        void push(long value) {
            if (size == values.length) values = Arrays.copyOf(values, size * 2);
            int i = size++;
            while (i > 0) {
                int parent = (i - 1) >>> 1;
                if (value >= values[parent]) break;
                values[i] = values[parent];
                i = parent;
            }
            values[i] = value;
        }

        long pop() {
            long min = values[0];
            long last = values[--size];
            int i = 0;
            for (int left = 1; left < size; left = i * 2 + 1) {
                int child = left + 1 < size && values[left + 1] < values[left] ? left + 1 : left;
                if (last <= values[child]) break;
                values[i] = values[child];
                i = child;
            }
            values[i] = last;
            return min;
        }
    }

    private static int neighbor(byte[] cost, int width, int x, int z, int direction) {
        int nx = x + DX[direction], nz = z + DZ[direction];
        if (nx < 0 || nx >= width || nz < 0 || nz >= width) return -1;
        int index = nz * width + nx;
        if (cost[index] == 0 || (direction >= 4 && (cost[z * width + nx] == 0 || cost[nz * width + x] == 0))) return -1;
        return index;
    }

    private static Result flow(SplittableRandom rng) {
        int w = 128, size = w * w, goal = 100 * w + 110;
        byte[] cost = new byte[size];
        for (int i = 0; i < size; i++) cost[i] = (byte) rng.nextInt(1, 5);
        for (int z = 1; z < w - 1; z++) if (z != 60) cost[z * w + 64] = 0;
        ByteBuffer grid = buffer(size);
        grid.put(0, cost);
        Floats dist = new Floats(size), dx = new Floats(size), dz = new Floats(size);
        return new Result(k -> {
            int[] fixed = new int[size];
            Arrays.fill(fixed, Integer.MAX_VALUE);
            fixed[goal] = 0;
            Heap heap = new Heap(w * 8);
            heap.push(goal);
            while (heap.size > 0) {
                long packed = heap.pop();
                int from = (int) packed, total = (int) (packed >>> 32);
                if (total != fixed[from]) continue;
                int x = from % w, z = from / w;
                for (int d = 0; d < 8; d++) {
                    int to = neighbor(cost, w, x, z, d);
                    if (to < 0) continue;
                    int next = total + (d < 4 ? cost[to] * 100 : (cost[to] * 14142 + 50) / 100);
                    if (next < fixed[to]) {
                        fixed[to] = next;
                        heap.push((long) next << 32 | to);
                    }
                }
            }
            int reachable = 0;
            for (int i = 0; i < size; i++) {
                dist.array[i] = fixed[i] == Integer.MAX_VALUE ? Float.POSITIVE_INFINITY : fixed[i] * .01f;
                if (fixed[i] != Integer.MAX_VALUE) reachable++;
            }
            work(size, true).forEach(i -> {
                float from = dist.array[i];
                dx.array[i] = 0;
                dz.array[i] = 0;
                if (i == goal || !Float.isFinite(from)) return;
                int x = i % w, z = i / w, best = -1;
                for (int d = 0; d < 8; d++) {
                    int to = neighbor(cost, w, x, z, d);
                    if (to >= 0 && dist.array[to] < from) {
                        from = dist.array[to];
                        best = d;
                    }
                }
                if (best < 0) return;
                float ax = DX[best] * (best >= 4 ? DIAGONAL : 1), az = DZ[best] * (best >= 4 ? DIAGONAL : 1);
                boolean open = x > 0 && x < w - 1 && z > 0 && z < w - 1;
                for (int d = 0; open && d < 8; d++) open = cost[i + DZ[d] * w + DX[d]] != 0;
                if (open) {
                    float gx = dist.array[i - 1] - dist.array[i + 1], gz = dist.array[i - w] - dist.array[i + w];
                    float mag = square(gx) + square(gz);
                    if (Float.isFinite(gx) && Float.isFinite(gz) && mag > 1e-6f) {
                        float inv = 1 / (float) Math.sqrt(mag), sx = gx * inv, sz = gz * inv;
                        if (sx * ax + sz * az > .5f) {
                            ax = sx;
                            az = sz;
                        }
                    }
                }
                dx.array[i] = ax;
                dz.array[i] = az;
            });
            return reachable;
        }, k -> RustNative.nativeComputeFlowField(w, w, grid, goal % w, goal / w, dist.direct, dx.direct, dz.direct),
            () -> { dist.verify(); dx.verify(); dz.verify(); });
    }

    private record Visibility(int[] shows, int[] hides, int showCount, int hideCount, int culled) {}

    private static Result tracking(Points mobs, Points players, SplittableRandom rng) {
        int n = mobs.size(), p = players.size(), cap = 512, bytes = (n * p + 7) / 8;
        byte[] initial = new byte[bytes];
        for (int i = 0; i < bytes; i++) initial[i] = (byte) rng.nextInt(256);
        byte[] hidden = initial.clone();
        ByteBuffer directHidden = buffer(bytes);
        Ints flags = new Ints(n), who = new Ints(cap), which = new Ints(cap), action = new Ints(cap), stats = new Ints(4);
        Points look = new Points(p);
        for (int j = 0; j < p; j++) {
            look.x.array[j] = rng.nextFloat() - .5f;
            look.y.array[j] = .1f;
            look.z.array[j] = rng.nextFloat() - .5f;
        }
        for (int i = 0; i < n; i++) flags.array[i] = i % 37 == 0 ? 1 : i % 2 == 0 ? 2 : 4;
        look.upload(); flags.upload();
        int initialHidden = 0;
        for (byte b : initial) initialHidden += Integer.bitCount(b & 255);
        final int startHidden = initialHidden;
        ThreadLocal<long[]> scratch = ThreadLocal.withInitial(() -> new long[n]);
        ThreadLocal<boolean[]> visibleScratch = ThreadLocal.withInitial(() -> new boolean[n]);
        return new Result(k -> {
            System.arraycopy(initial, 0, hidden, 0, bytes);
            Visibility[] results = new Visibility[p];
            work(p, true).forEach(j -> {
                int base = j * n;
                float lx = look.x.array[j], ly = look.y.array[j], lz = look.z.array[j];
                float inv = 1f / (float) Math.sqrt(square(lx) + square(ly) + square(lz));
                lx *= inv; ly *= inv; lz *= inv;
                long[] candidates = scratch.get();
                boolean[] visible = visibleScratch.get();
                Arrays.fill(visible, false);
                int count = 0, immune = 0;
                for (int i = 0; i < n; i++) {
                    float dx = mobs.x.array[i] - players.x.array[j], dy = mobs.y.array[i] - players.y.array[j],
                        dz = mobs.z.array[i] - players.z.array[j];
                    float dist = square(dx) + square(dy) + square(dz);
                    int flag = flags.array[i], bit = base + i;
                    if ((flag & 1) != 0 || dist <= 144) {
                        visible[i] = true;
                        immune++;
                        continue;
                    }
                    boolean prev = (hidden[bit >>> 3] & (1 << (bit & 7))) != 0;
                    if (dist > (prev ? 45 * 45 : 48 * 48) || Math.abs(dy) > (prev ? 22 : 24)) continue;
                    if (dist > 256 && (dx * lx + dy * ly + dz * lz) / (float) Math.sqrt(dist)
                        < (prev ? -.25f + .12f : -.25f)) continue;
                    float score = (flag & 2) != 0 ? dist * .65f : dist;
                    candidates[count++] = (long) Float.floatToIntBits(score) << 32 | i;
                }
                int taken = Math.min(count, Math.max(0, 72 - immune));
                if (taken > 0 && taken < count) selectPrefix(candidates, count, taken);
                for (int i = 0; i < taken; i++) visible[(int) candidates[i]] = true;
                int[] shows = new int[Math.min(cap, n)], hides = new int[Math.min(cap, n)];
                int sv = 0, hv = 0;
                for (int i = 0; i < n; i++) {
                    int bit = base + i;
                    boolean prev = (hidden[bit >>> 3] & (1 << (bit & 7))) != 0;
                    if (visible[i] && prev && sv < shows.length) shows[sv++] = i;
                    if (!visible[i] && !prev && hv < hides.length) hides[hv++] = i;
                }
                results[j] = new Visibility(shows, hides, sv, hv, count - taken);
            });
            int written = 0, hiddenCount = startHidden, culled = 0;
            for (var player : results) culled += player.culled;
            for (int mode = 1; mode >= 0; mode--) {
                for (int row = 0; written < cap; row++) {
                    boolean progressed = false;
                    for (int j = 0; j < p && written < cap; j++) {
                        var r = results[j];
                        if (row >= (mode == 1 ? r.showCount : r.hideCount)) continue;
                        int i = mode == 1 ? r.shows[row] : r.hides[row];
                        int bit = j * n + i;
                        hidden[bit >>> 3] ^= (byte) (1 << (bit & 7));
                        hiddenCount += mode == 1 ? -1 : 1;
                        who.array[written] = j; which.array[written] = i; action.array[written++] = mode;
                        progressed = true;
                    }
                    if (!progressed) break;
                }
            }
            stats.array[0] = hiddenCount; stats.array[1] = n * p - hiddenCount;
            stats.array[2] = written; stats.array[3] = culled;
            return written;
        }, k -> {
            directHidden.put(0, initial);
            return RustNative.nativeComputeMobTracking(n, mobs.x.direct, mobs.y.direct, mobs.z.direct, flags.direct,
                p, players.x.direct, players.y.direct, players.z.direct, look.x.direct, look.y.direct, look.z.direct,
                48, 12, 24, 16, -.25f, 72, 0, directHidden, cap, who.direct, which.direct, action.direct, stats.direct);
        }, () -> {
            who.verify(); which.verify(); action.verify(); stats.verify();
            for (int i = 0; i < bytes; i++) if (hidden[i] != directHidden.get(i)) throw new AssertionError("Tracking bitset differs");
        });
    }

    /** Select the lowest count entries without sorting the retained prefix. */
    private static void selectPrefix(long[] data, int size, int count) {
        int lo = 0, hi = size - 1;
        while (lo < hi) {
            long pivot = data[(lo + hi) >>> 1];
            int left = lo, right = hi;
            while (left <= right) {
                while (data[left] < pivot) left++;
                while (data[right] > pivot) right--;
                if (left <= right) {
                    long old = data[left];
                    data[left++] = data[right];
                    data[right--] = old;
                }
            }
            if (count - 1 <= right) hi = right;
            else if (count - 1 >= left) lo = left;
            else return;
        }
    }

    private static long measure(IntUnaryOperator operation, int repetitions) {
        long sum = 0;
        long start = System.nanoTime();
        for (int i = 0; i < repetitions; i++) sum += operation.applyAsInt(i);
        long elapsed = System.nanoTime() - start;
        sink = sum;
        return elapsed;
    }

    private static void warm(IntUnaryOperator operation, int repetitions) {
        long elapsed = 0;
        do {
            elapsed += measure(operation, repetitions);
        } while (elapsed < 240_000_000L);
    }

    private static int repetitions(IntUnaryOperator operation, int initial, boolean governor) {
        double nanosPerCall = measure(operation, initial) / (double) initial;
        int count = Math.clamp((int) (55_000_000 / nanosPerCall), 5, 10_000_000);
        return governor ? Math.max(128, count - count % 128) : count;
    }

    private static double timedPath(BufferedReader gate, Case item, String path, int sample,
                                    IntUnaryOperator operation, int repetitions) throws IOException {
        if (gate != null) {
            System.out.printf("PATH_BEGIN,%s,%s,%d,%s,%d%n", item.name, item.size, sample, path, repetitions);
            if (!"RUN".equals(gate.readLine())) throw new IOException("Benchmark controller disconnected");
        }
        double nanos = measure(operation, repetitions) / (double) repetitions;
        if (gate != null) System.out.printf(java.util.Locale.ROOT, "PATH_END,%.3f%n", nanos);
        return nanos;
    }

    private static void run(Case item, int rounds, BufferedReader gate) throws IOException {
        Result result = item.result;
        result.check(item.name);
        int initial = item.name.equals("governor") ? 4096 : 32;
        warm(result.java, initial);
        warm(result.rust, initial);
        boolean governor = item.name.equals("governor");
        int javaRepetitions = repetitions(result.java, initial, governor);
        int rustRepetitions = repetitions(result.rust, initial, governor);
        for (int i = 0; i < rounds; i++) {
            double javaNanos, rustNanos;
            if ((i & 1) == 0) {
                javaNanos = timedPath(gate, item, "java", i, result.java, javaRepetitions);
                rustNanos = timedPath(gate, item, "rust", i, result.rust, rustRepetitions);
            } else {
                rustNanos = timedPath(gate, item, "rust", i, result.rust, rustRepetitions);
                javaNanos = timedPath(gate, item, "java", i, result.java, javaRepetitions);
            }
            System.out.printf(java.util.Locale.ROOT, "SAMPLE,%s,%d,%.3f,%.3f%n",
                item.name, i, javaNanos, rustNanos);
        }
        System.out.printf("END,%s%n", item.name);
        result.check(item.name);
    }

    public static void main(String[] args) throws IOException {
        if (args.length < 2) {
            throw new IllegalArgumentException("Usage: KernelParityBenchmark <absolute-library> <workers> [rounds] [forward|reverse] [cases]");
        }
        workers = Integer.parseInt(args[1]);
        if (workers < 1) throw new IllegalArgumentException("workers");
        int rounds = args.length > 2 ? Integer.parseInt(args[2]) : 10;
        System.load(args[0]);
        if (RustNative.nativeInitPool(workers) != workers) throw new IllegalStateException("Rust thread count");
        if (workers > 1 && java.util.concurrent.ForkJoinPool.getCommonPoolParallelism() != workers - 1) {
            throw new IllegalStateException("Set java.util.concurrent.ForkJoinPool.common.parallelism to workers - 1");
        }

        var rng = new SplittableRandom(0x53504943414cL);
        Points mobs = points(ENTITIES, rng), players = points(PLAYERS, rng);
        Points velocity = new Points(ENTITIES), playerVelocity = new Points(PLAYERS);
        for (Points v : List.of(velocity, playerVelocity)) {
            for (int i = 0; i < v.size(); i++) {
                v.x.array[i] = (rng.nextFloat() - .5f) * 4;
                v.y.array[i] = (rng.nextFloat() - .5f) * 3;
                v.z.array[i] = (rng.nextFloat() - .5f) * 4;
            }
            v.upload();
        }
        var cases = new ArrayList<>(List.of(
            new Case("collision", "5000", collisions(mobs)),
            new Case("nearest", "5000x100", nearest(mobs, players)),
            new Case("boids", "5000", boids(mobs, velocity)),
            new Case("radius", "5000", radius(mobs)),
            new Case("heatmap", "5000", heatmap(mobs, rng)),
            new Case("item_merge", "5000", merges(mobs, rng)),
            new Case("governor", "1", governor()),
            new Case("farm", "5000x100", farm(mobs, players)),
            new Case("elytra", "100", surge(players, playerVelocity, rng)),
            new Case("flow", "128x128", flow(rng)),
            new Case("noise", "256x256x4", noise()),
            new Case("tracking", "2000x100", tracking(points(2000, rng), players, rng)),
            new Case("redstone", "1024", redstone(rng)),
            new Case("diff_extract", "64x64x64", diff(false, rng)),
            new Case("diff_count", "64x64x64", diff(true, rng))
        ));
        if (args.length > 4) {
            var names = java.util.Set.of(args[4].split(","));
            cases.removeIf(item -> !names.contains(item.name));
            if (cases.size() != names.size()) throw new IllegalArgumentException("Unknown case: " + args[4]);
        }
        System.out.println("POOL," + workers + "," + RustNative.nativeGetInfo());
        if (rounds == 0) {
            for (Case item : cases) {
                item.result.check(item.name);
                System.out.println("PARITY," + item.name);
            }
        } else {
            if (args.length > 3 && args[3].equals("reverse")) java.util.Collections.reverse(cases);
            BufferedReader gate = Boolean.getBoolean("spica.benchmark.wait-for-controller")
                ? new BufferedReader(new InputStreamReader(System.in)) : null;
            for (Case item : cases) run(item, rounds, gate);
        }
    }
}
