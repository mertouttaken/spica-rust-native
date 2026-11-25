package net.spicanetwork.rustnative.nativebridge;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.FloatBuffer;
import java.nio.IntBuffer;

/**
 * Reusable off-heap Structure-of-Arrays (SoA) memory arena for zero-copy Rust FFI calls.
 *
 * Guarantees:
 * - Zero JVM heap allocations per tick once warmed up
 * - Native CPU endianness (`ByteOrder.nativeOrder()`) so Rust reads raw f32/i32 slices directly
 * - Thread-confined usage: keep one arena per thread (e.g. one for Main Thread, one for async tasks)
 */
public final class EntityBatchArena {

    public static final int MAX_HOT_CHUNKS = 128;
    public static final int GOV_INT_SLOTS = 4;
    public static final int GOV_FLOAT_SLOTS = 4;

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

    public void ensureEntityCapacity(int needed) {
        if (needed <= entityCapacity) {
            return;
        }
        int next = entityCapacity;
        while (next < needed) {
            next = Math.max(next * 2, needed);
        }
        allocateEntityBuffers(next);
    }

    public void ensureTargetCapacity(int needed) {
        if (needed <= targetCapacity) {
            return;
        }
        int next = targetCapacity;
        while (next < needed) {
            next = Math.max(next * 2, needed);
        }
        allocateTargetBuffers(next);
    }

    public int getEntityCapacity() {
        return entityCapacity;
    }

    public int getTargetCapacity() {
        return targetCapacity;
    }

    public void setEntityPosition(int index, float px, float py, float pz, float r) {
        x.put(index, px);
        y.put(index, py);
        z.put(index, pz);
        radius.put(index, r);
    }

    public void setEntityVelocity(int index, float velX, float velY, float velZ) {
        vx.put(index, velX);
        vy.put(index, velY);
        vz.put(index, velZ);
    }

    public void setTargetPosition(int index, float px, float py, float pz) {
        targetX.put(index, px);
        targetY.put(index, py);
        targetZ.put(index, pz);
    }

    public void setEntityPositionAndFlags(int index, float px, float py, float pz, int entityFlags) {
        x.put(index, px);
        y.put(index, py);
        z.put(index, pz);
        flags.put(index, entityFlags);
    }

    public void setItem(int index, float px, float py, float pz, int itemTypeId, int stackAmount) {
        x.put(index, px);
        y.put(index, py);
        z.put(index, pz);
        typeId.put(index, itemTypeId);
        amount.put(index, stackAmount);
    }

    private void allocateEntityBuffers(int cap) {
        this.entityCapacity = cap;
        int floatBytes = cap * Float.BYTES;
        int intBytes = cap * Integer.BYTES;

        this.xBuf = allocNative(floatBytes);
        this.yBuf = allocNative(floatBytes);
        this.zBuf = allocNative(floatBytes);
        this.vxBuf = allocNative(floatBytes);
        this.vyBuf = allocNative(floatBytes);
        this.vzBuf = allocNative(floatBytes);
        this.radiusBuf = allocNative(floatBytes);
        this.typeIdBuf = allocNative(intBytes);
        this.amountBuf = allocNative(intBytes);
        this.flagsBuf = allocNative(intBytes);

        this.outXBuf = allocNative(floatBytes);
        this.outYBuf = allocNative(floatBytes);
        this.outZBuf = allocNative(floatBytes);
        this.outDistSqBuf = allocNative(floatBytes);
        this.outIntABuf = allocNative(intBytes);
        this.outIntBBuf = allocNative(intBytes);

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
    }

    private void allocateTargetBuffers(int cap) {
        this.targetCapacity = cap;
        int bytes = cap * Float.BYTES;

        this.targetXBuf = allocNative(bytes);
        this.targetYBuf = allocNative(bytes);
        this.targetZBuf = allocNative(bytes);

        this.targetX = targetXBuf.asFloatBuffer();
        this.targetY = targetYBuf.asFloatBuffer();
        this.targetZ = targetZBuf.asFloatBuffer();
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
