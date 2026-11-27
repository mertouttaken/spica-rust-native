package net.spicanetwork.rustnative.benchmark;

import java.util.concurrent.ConcurrentLinkedQueue;

/** CPU-cycle sampling for the pinned benchmark agent. Loaded by the bootstrap loader only in that run. */
public final class ChunkTrackingNettyClock {
    public static final int ENCODE = 0;
    public static final int COMPRESSION = 1;
    public static final int FLUSH = 2;

    private static final int SAMPLE_MASK = 31;
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
        if (worker == null || worker.active) {
            return null;
        }
        int next = worker.random;
        next ^= next << 13;
        next ^= next >>> 17;
        next ^= next << 5;
        worker.random = next;
        worker.active = true;
        worker.stage = stage;
        worker.start = stage == COMPRESSION || (next & SAMPLE_MASK) == 0 ? cycles() : UNSAMPLED;
        return worker;
    }

    public static Worker enterCompression(int bytes, int threshold) {
        return bytes >= threshold ? enter(COMPRESSION) : null;
    }

    public static void exit(Worker worker) {
        if (worker == null) {
            return;
        }
        long end = worker.start == UNSAMPLED ? UNSAMPLED : cycles();
        worker.active = false;
        if (end != UNSAMPLED) {
            worker.record(end);
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
        private final long[] used = new long[3];
        private final long[] count = new long[3];
        private int random = (int) id ^ 0x6e747479;
        private int stage;
        private long start;
        private long errors;
        private boolean active;

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

        private synchronized void record(long end) {
            if (start < 0 || end < start || overhead == Long.MAX_VALUE) {
                errors++;
                return;
            }
            used[stage] += Math.max(0, end - start - overhead);
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
