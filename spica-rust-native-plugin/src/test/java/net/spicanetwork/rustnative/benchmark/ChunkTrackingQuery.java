package net.spicanetwork.rustnative.benchmark;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.DoubleBuffer;
import java.nio.IntBuffer;
import java.nio.LongBuffer;
import net.spicanetwork.rustnative.nativebridge.RustNative;

/** Reusable buffers for all three synchronous paths. Owned by the probe's server thread. */
final class ChunkTrackingQuery {
    static final int MAX_PLAYERS = 64;
    static final int MAX_MOBS = 16_384;
    private static final double RANGE = 48.0;

    private final ByteBuffer playerBytes = direct(2 * MAX_PLAYERS * Double.BYTES);
    private final ByteBuffer mobBytes = direct(2 * MAX_MOBS * Double.BYTES);
    private final ByteBuffer tableBytes = direct(4 * MAX_MOBS * Long.BYTES);
    private final ByteBuffer nextBytes = direct(MAX_MOBS * Integer.BYTES);
    private final ByteBuffer pairBytes = direct(MAX_PLAYERS * MAX_MOBS / Byte.SIZE);
    private final DoubleBuffer players = playerBytes.asDoubleBuffer();
    private final DoubleBuffer mobs = mobBytes.asDoubleBuffer();
    private final LongBuffer table = tableBytes.asLongBuffer();
    private final IntBuffer next = nextBytes.asIntBuffer();
    private final LongBuffer pairs = pairBytes.asLongBuffer();
    private final long[] expected = new long[pairs.capacity()];
    private int slots;

    static ByteBuffer direct(int bytes) {
        return ByteBuffer.allocateDirect(bytes).order(ByteOrder.nativeOrder());
    }

    void player(int index, double x, double z) {
        players.put(2 * index, x).put(2 * index + 1, z);
    }

    void mob(int index, double x, double z) {
        mobs.put(2 * index, x).put(2 * index + 1, z);
    }

    ByteBuffer playerInput() {
        return playerBytes;
    }

    boolean candidate(int player, int mobCount, int mob) {
        return isSet(pairs, player * mobCount + mob);
    }

    static boolean isSet(LongBuffer pairs, int bit) {
        return (pairs.get(bit >>> 6) & (1L << (bit & 63))) != 0;
    }

    private static int chunk(double coord) {
        return (int) Math.floor(coord) >> 4;
    }

    private static long key(int x, int z) {
        return ((long) x << 32) | Integer.toUnsignedLong(z);
    }

    private int bucket(long key) {
        long mixed = (key ^ (key >>> 33)) * 0xff51afd7ed558ccdL;
        int mask = slots - 1;
        int index = (int) (mixed ^ (mixed >>> 33)) & mask;
        while (table.get(2 * index + 1) != -1 && table.get(2 * index) != key) {
            index = (index + 1) & mask;
        }
        return index;
    }

    void buildIndex(int mobCount) {
        slots = Integer.highestOneBit(2 * mobCount - 1) << 1;
        for (int slot = 0; slot < slots; slot++) {
            table.put(2 * slot + 1, -1);
        }
        for (int m = 0; m < mobCount; m++) {
            long key = key(chunk(mobs.get(2 * m)), chunk(mobs.get(2 * m + 1)));
            int index = bucket(key);
            next.put(m, (int) table.get(2 * index + 1));
            table.put(2 * index, key).put(2 * index + 1, m);
        }
    }

    private void clearPairs(int playerCount, int mobCount) {
        for (int word = 0; word < words(playerCount, mobCount); word++) {
            pairs.put(word, 0);
        }
    }

    private static int words(int players, int mobs) {
        return (players * mobs + 63) / 64;
    }

    private int include(double x, double z, int mobIndex, int bit) {
        double dx = x - mobs.get(2 * mobIndex);
        double dz = z - mobs.get(2 * mobIndex + 1);
        if (dx * dx + dz * dz <= RANGE * RANGE) {
            int word = bit >>> 6;
            pairs.put(word, pairs.get(word) | (1L << (bit & 63)));
            return 1;
        }
        return 0;
    }

    int fullScan(int playerCount, int mobCount) {
        clearPairs(playerCount, mobCount);
        int count = 0;
        for (int p = 0; p < playerCount; p++) {
            double x = players.get(2 * p);
            double z = players.get(2 * p + 1);
            for (int m = 0; m < mobCount; m++) {
                count += include(x, z, m, p * mobCount + m);
            }
        }
        return count;
    }

    int indexedJava(int playerCount, int mobCount) {
        clearPairs(playerCount, mobCount);
        int count = 0;
        for (int p = 0; p < playerCount; p++) {
            double x = players.get(2 * p);
            double z = players.get(2 * p + 1);
            int lastX = chunk(x + RANGE);
            int lastZ = chunk(z + RANGE);
            for (int cx = chunk(x - RANGE); cx <= lastX; cx++) {
                for (int cz = chunk(z - RANGE); cz <= lastZ; cz++) {
                    int m = (int) table.get(2 * bucket(key(cx, cz)) + 1);
                    for (; m != -1; m = next.get(m)) {
                        count += include(x, z, m, p * mobCount + m);
                    }
                }
            }
        }
        return count;
    }

    int indexedRust(int playerCount, int mobCount) {
        int count = RustNative.nativeQueryTrackingChunks(
            playerCount, playerBytes, mobCount, mobBytes, tableBytes, nextBytes, pairBytes
        );
        if (count < 0) {
            throw new IllegalStateException("Native chunk query rejected the snapshot");
        }
        return count;
    }

    int verify(int playerCount, int mobCount) {
        int count = fullScan(playerCount, mobCount);
        int words = words(playerCount, mobCount);
        pairs.get(0, expected, 0, words);
        buildIndex(mobCount);
        compare("Java", count, indexedJava(playerCount, mobCount), words);
        compare("Rust", count, indexedRust(playerCount, mobCount), words);
        return count;
    }

    private void compare(String backend, int expectedCount, int actualCount, int words) {
        if (expectedCount != actualCount) {
            throw new IllegalStateException(backend + " candidate count differs from the full scan");
        }
        for (int word = 0; word < words; word++) {
            if (pairs.get(word) != expected[word]) {
                throw new IllegalStateException(backend + " candidate set differs at word " + word);
            }
        }
    }
}
