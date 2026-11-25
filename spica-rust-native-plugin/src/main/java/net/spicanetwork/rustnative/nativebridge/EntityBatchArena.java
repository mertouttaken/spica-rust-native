package net.spicanetwork.rustnative.nativebridge;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.FloatBuffer;
import java.nio.IntBuffer;
import java.util.Objects;
import java.util.concurrent.locks.ReentrantLock;
import java.util.function.Consumer;

/**
 * Reusable off-heap Structure-of-Arrays (SoA) memory arena for zero-copy Rust FFI calls.
 *
 * Ownership & Concurrency Contract (R1):
 * - Zero JVM heap allocations per tick once warmed up.
 * - Native CPU endianness ({@code ByteOrder.nativeOrder()}) so Rust reads raw f32/i32 slices directly.
 * - An {@code EntityBatchArena} MUST either be confined to a single thread (e.g. the Bukkit Main Thread)
 *   OR accessed exclusively inside {@link #withExclusiveAccess(Consumer)} when shared across threads.
 * - All setter helpers and resize operations reject mutation while a native FFI call holds
 *   {@link #acquireNativeLease()}. Direct public buffer views are retained for zero-allocation hot-path
 *   compatibility and MUST NOT be mutated concurrently while another thread executes a native call on
 *   the same arena.
 */
public final class EntityBatchArena {

    public static final int MAX_HOT_CHUNKS = 128;
    public static final int GOV_INT_SLOTS = 4;
    public static final int GOV_FLOAT_SLOTS = 4;
    private static final int MAX_CAPACITY_ELEMENTS = Integer.MAX_VALUE / Float.BYTES;

    private final ReentrantLock nativeLeaseLock = new ReentrantLock();
    private volatile boolean inNativeCall = false;
    private int entityCapacity;
    private int targetCapacity;

    // Entity SoA inputs
    public ByteBuffer xBuf, yBuf, zBuf;
    public ByteBuffer vxBuf, vyBuf, vzBuf;
    public ByteBuffer radiusBuf;
    public ByteBuffer typeIdBuf, amountBuf;
    public ByteBuffer flagsBuf;

    public FloatBuffer x, y, z;
    public FloatBuffer vx, vy, vz;
    public FloatBuffer radius;
    public IntBuffer typeId, amount;
    public IntBuffer flags;

    // Target/Player SoA inputs
    public ByteBuffer targetXBuf, targetYBuf, targetZBuf;
    public FloatBuffer targetX, targetY, targetZ;

    // Outputs (collisions / boids / AI targets / radius queries / item merges)
    public ByteBuffer outXBuf, outYBuf, outZBuf;
    public ByteBuffer outDistSqBuf;
    public ByteBuffer outIntABuf, outIntBBuf;

    public FloatBuffer outX, outY, outZ;
    public FloatBuffer outDistSq;
    public IntBuffer outIntA, outIntB;

    // Chunk heatmap outputs (fixed capacity = 128 hot chunks)
    public ByteBuffer chunkXBuf, chunkZBuf, chunkCountBuf, chunkCrowdedBuf;
    public IntBuffer chunkX, chunkZ, chunkCount, chunkCrowded;

    // In-process Rust Brain governor outputs (16 bytes -> 4 ints, 16 bytes -> 4 floats)
    public ByteBuffer govIntBuf, govFloatBuf;
    public IntBuffer govInt;
    public FloatBuffer govFloat;

    public EntityBatchArena(int initialEntityCapacity, int initialTargetCapacity) {
        allocateEntityBuffers(Math.max(256, initialEntityCapacity));
        allocateTargetBuffers(Math.max(64, initialTargetCapacity));
        allocateBrainAuxBuffers();
    }

    /**
     * Executes {@code action} while holding exclusive lock ownership over this arena (R1).
     * Use this when sharing an arena across threads to serialize fill -> native call -> read-output
     * sequences so no peer thread can mutate direct buffers while Rust holds raw pointers.
     */
    public void withExclusiveAccess(Consumer<EntityBatchArena> action) {
        Objects.requireNonNull(action, "action");
        nativeLeaseLock.lock();
        try {
            if (inNativeCall) {
                throw new IllegalStateException("Cannot acquire exclusive arena access during an active native FFI call.");
            }
            action.accept(this);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    /**
     * Acquires an exclusive ownership lease on this arena for the duration of a single native
     * Rust FFI invocation. Prevents concurrent JNI calls or buffer reallocation while Rust
     * reads/writes off-heap memory.
     */
    public NativeLease acquireNativeLease() {
        nativeLeaseLock.lock();
        if (inNativeCall) {
            nativeLeaseLock.unlock();
            throw new IllegalStateException("Reentrant native FFI call on the same EntityBatchArena is forbidden.");
        }
        inNativeCall = true;
        return new NativeLease(this);
    }

    private void assertNotInNativeCall() {
        if (inNativeCall) {
            throw new IllegalStateException(
                "EntityBatchArena buffers cannot be mutated while a native Rust FFI call is active."
            );
        }
    }

    private void releaseNativeLease() {
        if (!nativeLeaseLock.isHeldByCurrentThread()) {
            throw new IllegalMonitorStateException(
                "EntityBatchArena.NativeLease must be closed on the thread that acquired it."
            );
        }
        inNativeCall = false;
        nativeLeaseLock.unlock();
    }

    public static final class NativeLease implements AutoCloseable {
        private final EntityBatchArena arena;
        private boolean closed = false;

        private NativeLease(EntityBatchArena arena) {
            this.arena = arena;
        }

        @Override
        public void close() {
            if (!closed) {
                closed = true;
                arena.releaseNativeLease();
            }
        }
    }

    public void ensureEntityCapacity(int needed) {
        if (needed > MAX_CAPACITY_ELEMENTS) {
            throw new IllegalArgumentException("Requested entity capacity exceeds direct ByteBuffer limits: " + needed);
        }
        nativeLeaseLock.lock();
        try {
            if (inNativeCall) {
                throw new IllegalStateException("Cannot resize EntityBatchArena while a native Rust call is active.");
            }
            if (needed <= entityCapacity) {
                return;
            }
            int next = entityCapacity;
            while (next < needed) {
                if (next >= MAX_CAPACITY_ELEMENTS / 2) {
                    next = needed;
                    break;
                }
                next = Math.max(next * 2, needed);
            }
            allocateEntityBuffers(next);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public void ensureTargetCapacity(int needed) {
        if (needed > MAX_CAPACITY_ELEMENTS) {
            throw new IllegalArgumentException("Requested target capacity exceeds direct ByteBuffer limits: " + needed);
        }
        nativeLeaseLock.lock();
        try {
            if (inNativeCall) {
                throw new IllegalStateException("Cannot resize EntityBatchArena while a native Rust call is active.");
            }
            if (needed <= targetCapacity) {
                return;
            }
            int next = targetCapacity;
            while (next < needed) {
                if (next >= MAX_CAPACITY_ELEMENTS / 2) {
                    next = needed;
                    break;
                }
                next = Math.max(next * 2, needed);
            }
            allocateTargetBuffers(next);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public int getEntityCapacity() {
        return entityCapacity;
    }

    public int getTargetCapacity() {
        return targetCapacity;
    }

    public void setEntityPosition(int index, float px, float py, float pz, float r) {
        assertNotInNativeCall();
        x.put(index, px);
        y.put(index, py);
        z.put(index, pz);
        radius.put(index, r);
    }

    public void setEntityVelocity(int index, float velX, float velY, float velZ) {
        assertNotInNativeCall();
        vx.put(index, velX);
        vy.put(index, velY);
        vz.put(index, velZ);
    }

    public void setTargetPosition(int index, float px, float py, float pz) {
        assertNotInNativeCall();
        targetX.put(index, px);
        targetY.put(index, py);
        targetZ.put(index, pz);
    }

    public void setEntityPositionAndFlags(int index, float px, float py, float pz, int entityFlags) {
        assertNotInNativeCall();
        x.put(index, px);
        y.put(index, py);
        z.put(index, pz);
        flags.put(index, entityFlags);
    }

    public void setItem(int index, float px, float py, float pz, int itemTypeId, int stackAmount) {
        assertNotInNativeCall();
        x.put(index, px);
        y.put(index, py);
        z.put(index, pz);
        typeId.put(index, itemTypeId);
        amount.put(index, stackAmount);
    }

    private void allocateEntityBuffers(int cap) {
        if (cap <= 0 || cap > MAX_CAPACITY_ELEMENTS) {
            throw new IllegalArgumentException("Invalid entity arena capacity: " + cap);
        }
        int floatBytes = cap * Float.BYTES;
        int intBytes = cap * Integer.BYTES;

        // Allocate all buffers first so a DirectBuffer OOM never leaves entityCapacity/fields half-updated
        ByteBuffer newXBuf = allocNative(floatBytes);
        ByteBuffer newYBuf = allocNative(floatBytes);
        ByteBuffer newZBuf = allocNative(floatBytes);
        ByteBuffer newVxBuf = allocNative(floatBytes);
        ByteBuffer newVyBuf = allocNative(floatBytes);
        ByteBuffer newVzBuf = allocNative(floatBytes);
        ByteBuffer newRadiusBuf = allocNative(floatBytes);
        ByteBuffer newTypeIdBuf = allocNative(intBytes);
        ByteBuffer newAmountBuf = allocNative(intBytes);
        ByteBuffer newFlagsBuf = allocNative(intBytes);

        ByteBuffer newOutXBuf = allocNative(floatBytes);
        ByteBuffer newOutYBuf = allocNative(floatBytes);
        ByteBuffer newOutZBuf = allocNative(floatBytes);
        ByteBuffer newOutDistSqBuf = allocNative(floatBytes);
        ByteBuffer newOutIntABuf = allocNative(intBytes);
        ByteBuffer newOutIntBBuf = allocNative(intBytes);

        this.xBuf = newXBuf;
        this.yBuf = newYBuf;
        this.zBuf = newZBuf;
        this.vxBuf = newVxBuf;
        this.vyBuf = newVyBuf;
        this.vzBuf = newVzBuf;
        this.radiusBuf = newRadiusBuf;
        this.typeIdBuf = newTypeIdBuf;
        this.amountBuf = newAmountBuf;
        this.flagsBuf = newFlagsBuf;

        this.outXBuf = newOutXBuf;
        this.outYBuf = newOutYBuf;
        this.outZBuf = newOutZBuf;
        this.outDistSqBuf = newOutDistSqBuf;
        this.outIntABuf = newOutIntABuf;
        this.outIntBBuf = newOutIntBBuf;

        this.x = xBuf.asFloatBuffer();
        this.y = yBuf.asFloatBuffer();
        this.z = zBuf.asFloatBuffer();
        this.vx = vxBuf.asFloatBuffer();
        this.vy = vyBuf.asFloatBuffer();
        this.vz = vzBuf.asFloatBuffer();
        this.radius = radiusBuf.asFloatBuffer();
        this.typeId = typeIdBuf.asIntBuffer();
        this.amount = amountBuf.asIntBuffer();
        this.flags = flagsBuf.asIntBuffer();

        this.outX = outXBuf.asFloatBuffer();
        this.outY = outYBuf.asFloatBuffer();
        this.outZ = outZBuf.asFloatBuffer();
        this.outDistSq = outDistSqBuf.asFloatBuffer();
        this.outIntA = outIntABuf.asIntBuffer();
        this.outIntB = outIntBBuf.asIntBuffer();
        this.entityCapacity = cap;
    }

    private void allocateTargetBuffers(int cap) {
        if (cap <= 0 || cap > MAX_CAPACITY_ELEMENTS) {
            throw new IllegalArgumentException("Invalid target arena capacity: " + cap);
        }
        int bytes = cap * Float.BYTES;

        ByteBuffer newTargetXBuf = allocNative(bytes);
        ByteBuffer newTargetYBuf = allocNative(bytes);
        ByteBuffer newTargetZBuf = allocNative(bytes);

        this.targetXBuf = newTargetXBuf;
        this.targetYBuf = newTargetYBuf;
        this.targetZBuf = newTargetZBuf;

        this.targetX = targetXBuf.asFloatBuffer();
        this.targetY = targetYBuf.asFloatBuffer();
        this.targetZ = targetZBuf.asFloatBuffer();
        this.targetCapacity = cap;
    }

    private void allocateBrainAuxBuffers() {
        int chunkBytes = MAX_HOT_CHUNKS * Integer.BYTES;
        this.chunkXBuf = allocNative(chunkBytes);
        this.chunkZBuf = allocNative(chunkBytes);
        this.chunkCountBuf = allocNative(chunkBytes);
        this.chunkCrowdedBuf = allocNative(chunkBytes);

        this.chunkX = chunkXBuf.asIntBuffer();
        this.chunkZ = chunkZBuf.asIntBuffer();
        this.chunkCount = chunkCountBuf.asIntBuffer();
        this.chunkCrowded = chunkCrowdedBuf.asIntBuffer();

        this.govIntBuf = allocNative(GOV_INT_SLOTS * Integer.BYTES);
        this.govFloatBuf = allocNative(GOV_FLOAT_SLOTS * Float.BYTES);

        this.govInt = govIntBuf.asIntBuffer();
        this.govFloat = govFloatBuf.asFloatBuffer();
    }

    private static ByteBuffer allocNative(int bytes) {
        return ByteBuffer.allocateDirect(bytes).order(ByteOrder.nativeOrder());
    }
}
