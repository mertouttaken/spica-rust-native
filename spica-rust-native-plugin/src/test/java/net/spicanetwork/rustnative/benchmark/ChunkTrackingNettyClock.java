package net.spicanetwork.rustnative.benchmark;

import java.util.concurrent.ConcurrentLinkedQueue;

/** CPU-cycle sampling for the pinned benchmark agent. Loaded by the bootstrap loader only in that run. */
public final class ChunkTrackingNettyClock {
    public static final int ENCODE = 0;
    public static final int COMPRESSION = 1;
    public static final int FLUSH = 2;
    public static final int EVENT_LOOP = 3;
    public static final int QUEUE = 4;
    public static final int BUFFER = 5;

    private static final int[] PRIORITY = {2, 2, 2, 0, 1, 1};
    private static final int[] SAMPLE_MASK = {31, 0, 31, 15, 127, 127};
    private static final long UNSAMPLED = -2;
    private static final ConcurrentLinkedQueue<Worker> WORKERS = new ConcurrentLinkedQueue<>();
    private static final ThreadLocal<Worker> LOCAL = ThreadLocal.withInitial(() -> {
        if (!Thread.currentThread().getName().startsWith("Netty Server IO ")) {
            return null;
        }
        Worker worker = new Worker();
        WORKERS.add(worker);
        return worker;
    });

    private ChunkTrackingNettyClock() {}

    public static void load(String library) {
        System.load(library);
        if (cycles() < 0 || threadId() <= 0) {
            throw new IllegalStateException("Windows thread cycle clock is unavailable");
        }
    }

    private static native long cycles();
    private static native long threadId();

    public static Worker enter(int stage) {
        Worker worker = LOCAL.get();
        if (worker == null) {
            return null;
        }
        int parent = worker.depth - 1;
        if (parent >= 0 && PRIORITY[worker.stage[parent]] >= PRIORITY[stage]) {
            return null;
        }
        int next = worker.random;
        next ^= next << 13;
        next ^= next >>> 17;
        next ^= next << 5;
        worker.random = next;
        boolean sampled = (next & SAMPLE_MASK[stage]) == 0;
        boolean parentSampled = parent >= 0 && worker.start[parent] != UNSAMPLED;
        long at = sampled || parentSampled ? cycles() : UNSAMPLED;
        if (parentSampled) {
            worker.addSegment(parent, at);
        }
        int slot = worker.depth++;
        worker.stage[slot] = stage;
        worker.start[slot] = sampled ? at : UNSAMPLED;
        worker.elapsed[slot] = 0;
        return worker;
    }

    public static Worker enterCompression(int bytes, int threshold) {
        return enter(bytes >= threshold ? COMPRESSION : BUFFER);
    }

    public static void exit(Worker worker) {
        if (worker == null) {
            return;
        }
        int slot = --worker.depth;
        if (worker.start[slot] != UNSAMPLED) {
            worker.addSegment(slot, cycles());
            worker.record(worker.stage[slot], worker.elapsed[slot]);
        }
        int parent = slot - 1;
        if (parent >= 0 && worker.start[parent] != UNSAMPLED) {
            worker.start[parent] = cycles();
        }
    }

    public static String snapshot() {
        StringBuilder output = new StringBuilder().append(System.currentTimeMillis());
        for (Worker worker : WORKERS) {
            worker.append(output);
        }
        return output.toString();
    }

    public static final class Worker {
        private final long id = threadId();
        private final long overhead = calibrate();
        private final long[] used = new long[PRIORITY.length];
        private final long[] count = new long[PRIORITY.length];
        // Each accepted child has a strictly higher priority, so there are at most three scopes.
        private final int[] stage = new int[3];
        private final long[] start = new long[3];
        private final long[] elapsed = new long[3];
        private int random = (int) id ^ 0x6e747479;
        private int depth;
        private long errors;

        private static long calibrate() {
            long minimum = Long.MAX_VALUE;
            for (int iteration = 0; iteration < 64; iteration++) {
                long first = cycles();
                long last = cycles();
                if (first >= 0 && last >= first) {
                    minimum = Math.min(minimum, last - first);
                }
            }
            return minimum;
        }

        private void addSegment(int slot, long end) {
            if (start[slot] < 0 || end < start[slot] || overhead == Long.MAX_VALUE) {
                elapsed[slot] = -1;
            } else if (elapsed[slot] >= 0) {
                elapsed[slot] += Math.max(0, end - start[slot] - overhead);
            }
        }

        private synchronized void record(int stage, long elapsed) {
            if (elapsed < 0) {
                errors++;
                return;
            }
            used[stage] += elapsed;
            count[stage]++;
        }

        private synchronized void append(StringBuilder output) {
            output.append(' ').append(id).append(':').append(overhead).append(':').append(errors);
            for (int index = 0; index < used.length; index++) {
                output.append(':').append(used[index]).append(':').append(count[index]);
            }
        }
    }
}
