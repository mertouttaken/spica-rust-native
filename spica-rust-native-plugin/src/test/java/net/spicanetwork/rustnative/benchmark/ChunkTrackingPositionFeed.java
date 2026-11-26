package net.spicanetwork.rustnative.benchmark;

import java.util.function.IntConsumer;

/** Bootstrap-visible test bridge. The agent calls it without resolving Bukkit classes. */
public final class ChunkTrackingPositionFeed {
    private static volatile IntConsumer listener;
    private static volatile int installed;

    private ChunkTrackingPositionFeed() { }

    public static synchronized void installed(int hook) {
        installed |= hook;
    }

    public static void attach(IntConsumer consumer) {
        if (installed != 3) {
            throw new IllegalStateException("Both pinned Purpur movement hooks are required, installed=" + installed);
        }
        listener = consumer;
    }

    public static void detach() {
        listener = null;
    }

    public static boolean horizontalChanged(double oldX, double oldZ, double x, double z, boolean mob) {
        return listener != null && mob && (oldX != x || oldZ != z);
    }

    public static void position(boolean changed, int entity) {
        if (changed) {
            dirty(entity);
        }
    }

    public static void health(int entity, Object changed, Object healthKey) {
        if (changed == healthKey) {
            dirty(entity);
        }
    }

    public static void dirty(int entity) {
        IntConsumer consumer = listener;
        if (consumer != null) {
            consumer.accept(entity);
        }
    }
}
