package net.spicanetwork.rustnative.nativebridge;

import net.spicanetwork.rustnative.brain.BrainDirective;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.FloatBuffer;
import java.nio.IntBuffer;
import java.util.Locale;
import java.util.Objects;
import java.util.concurrent.locks.ReentrantLock;
import java.util.function.Consumer;

/**
 * Reusable, native-order SoA storage for synchronous Rust calls. The direct buffers are owned
 * by the arena and never returned to callers. Resize, scalar access and JNI use the same lock.
 * Use {@link #withExclusiveAccess(Consumer)} to share a whole fill/compute/read sequence.
 */
public final class EntityBatchArena {

    public static final int MAX_HOT_CHUNKS = 128;
    public static final int GOV_INT_SLOTS = 4;
    public static final int GOV_FLOAT_SLOTS = 4;
    private static final int MAX_CAPACITY_ELEMENTS = Integer.MAX_VALUE / Float.BYTES;

    private final ReentrantLock nativeLeaseLock = new ReentrantLock();
    private boolean inNativeCall = false;
    private int entityCapacity;
    private int targetCapacity;

    // Entity SoA inputs
    private ByteBuffer xBuf, yBuf, zBuf;
    private ByteBuffer vxBuf, vyBuf, vzBuf;
    private ByteBuffer radiusBuf;
    private ByteBuffer typeIdBuf, amountBuf;
    private ByteBuffer flagsBuf;

    private FloatBuffer x, y, z;
    private FloatBuffer vx, vy, vz;
    private FloatBuffer radius;
    private IntBuffer typeId, amount;
    private IntBuffer flags;

    // Target/Player SoA inputs
    private ByteBuffer targetXBuf, targetYBuf, targetZBuf;
    private FloatBuffer targetX, targetY, targetZ;

    // Outputs (collisions / boids / AI targets / radius queries / item merges)
    private ByteBuffer outXBuf, outYBuf, outZBuf;
    private ByteBuffer outDistSqBuf;
    private ByteBuffer outIntABuf, outIntBBuf;

    private FloatBuffer outX, outY, outZ;
    private FloatBuffer outDistSq;
    private IntBuffer outIntA, outIntB;

    // Chunk heatmap outputs (fixed capacity = 128 hot chunks)
    private ByteBuffer chunkXBuf, chunkZBuf, chunkCountBuf, chunkCrowdedBuf;
    private IntBuffer chunkX, chunkZ, chunkCount, chunkCrowded;

    // In-process Rust Brain governor outputs (16 bytes -> 4 ints, 16 bytes -> 4 floats)
    private ByteBuffer govIntBuf, govFloatBuf;
    private IntBuffer govInt;
    private FloatBuffer govFloat;

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
        inNativeCall = false;
        nativeLeaseLock.unlock();
    }

    public static final class NativeLease implements AutoCloseable {
        private final EntityBatchArena arena;
        private final Thread owner = Thread.currentThread();
        private boolean closed = false;

        private NativeLease(EntityBatchArena arena) {
            this.arena = arena;
        }

        @Override
        public void close() {
            if (Thread.currentThread() != owner) {
                throw new IllegalMonitorStateException(
                    "EntityBatchArena.NativeLease must be closed on the thread that acquired it."
                );
            }
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
        nativeLeaseLock.lock();
        try {
            return entityCapacity;
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public int getTargetCapacity() {
        nativeLeaseLock.lock();
        try {
            return targetCapacity;
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public void setEntityPosition(int index, float px, float py, float pz, float r) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            x.put(index, px);
            y.put(index, py);
            z.put(index, pz);
            radius.put(index, r);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public void setEntityVelocity(int index, float velX, float velY, float velZ) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            vx.put(index, velX);
            vy.put(index, velY);
            vz.put(index, velZ);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public void setTargetPosition(int index, float px, float py, float pz) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            targetX.put(index, px);
            targetY.put(index, py);
            targetZ.put(index, pz);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public void setEntityPositionAndFlags(int index, float px, float py, float pz, int entityFlags) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            x.put(index, px);
            y.put(index, py);
            z.put(index, pz);
            flags.put(index, entityFlags);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public void setItem(int index, float px, float py, float pz, int itemTypeId, int stackAmount) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            x.put(index, px);
            y.put(index, py);
            z.put(index, pz);
            typeId.put(index, itemTypeId);
            amount.put(index, stackAmount);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public float getOutX(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return outX.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public float getOutY(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return outY.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public float getOutZ(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return outZ.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public float getOutDistSq(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return outDistSq.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public int getOutIntA(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return outIntA.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public int getOutIntB(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return outIntB.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public int getFlags(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return flags.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public int getChunkX(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return chunkX.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public int getChunkZ(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return chunkZ.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public int getChunkCount(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return chunkCount.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public int getChunkCrowded(int index) {
        nativeLeaseLock.lock();
        try {
            assertNotInNativeCall();
            return chunkCrowded.get(index);
        } finally {
            nativeLeaseLock.unlock();
        }
    }

    public long computeCollisions(
        int entityCount,
        float cellSize,
        float pushStrength,
        float maxPush
    ) {
        ensureLoaded();
        if (entityCount <= 0) {
            return 0L;
        }
        try (NativeLease ignored = acquireNativeLease()) {
            return RustNative.nativeComputeCollisions(
                entityCount,
                xBuf, yBuf, zBuf, radiusBuf,
                cellSize, pushStrength, maxPush,
                outXBuf, outYBuf, outZBuf, outIntABuf
            );
        }
    }

    public int computeAiTargets(
        int mobCount,
        int targetCount,
        float fullRange,
        float reducedRange
    ) {
        ensureLoaded();
        if (mobCount <= 0) {
            return 0;
        }
        try (NativeLease ignored = acquireNativeLease()) {
            return RustNative.nativeComputeAiTargets(
                mobCount,
                xBuf, yBuf, zBuf,
                targetCount,
                targetXBuf, targetYBuf, targetZBuf,
                fullRange, reducedRange,
                outIntABuf, outDistSqBuf,
                outXBuf, outYBuf, outZBuf,
                outIntBBuf
            );
        }
    }

    public int computeBoidsSteering(
        int entityCount,
        float goalX,
        float goalY,
        float goalZ,
        float perceptionRadius,
        float sepWeight,
        float alignWeight,
        float cohesionWeight,
        float goalWeight,
        float maxForce
    ) {
        ensureLoaded();
        if (entityCount <= 0) {
            return 0;
        }
        try (NativeLease ignored = acquireNativeLease()) {
            return RustNative.nativeComputeBoids(
                entityCount,
                xBuf, yBuf, zBuf,
                vxBuf, vyBuf, vzBuf,
                goalX, goalY, goalZ,
                perceptionRadius,
                sepWeight, alignWeight, cohesionWeight, goalWeight, maxForce,
                outXBuf, outYBuf, outZBuf
            );
        }
    }

    public int queryRadius(
        int entityCount,
        float qx,
        float qy,
        float qz,
        float radius,
        int maxResults
    ) {
        ensureLoaded();
        if (entityCount <= 0 || maxResults <= 0) {
            return 0;
        }
        try (NativeLease ignored = acquireNativeLease()) {
            return RustNative.nativeQueryRadius(
                entityCount,
                xBuf, yBuf, zBuf,
                qx, qy, qz, radius,
                Math.min(maxResults, getEntityCapacity()),
                outIntABuf
            );
        }
    }

    public int analyzeChunks(
        int entityCount,
        int maxHotChunks
    ) {
        ensureLoaded();
        if (entityCount <= 0 || maxHotChunks <= 0) {
            return 0;
        }
        int clampedMax = Math.min(maxHotChunks, EntityBatchArena.MAX_HOT_CHUNKS);
        try (NativeLease ignored = acquireNativeLease()) {
            int clampedCount = Math.min(entityCount, getEntityCapacity());
            return RustNative.nativeAnalyzeChunks(
                clampedCount,
                xBuf,
                zBuf,
                outIntABuf,
                clampedMax,
                chunkXBuf,
                chunkZBuf,
                chunkCountBuf,
                chunkCrowdedBuf
            );
        }
    }

    public int planItemMerges(
        int itemCount,
        int maxStackSize,
        float mergeRadius,
        int maxPairs
    ) {
        ensureLoaded();
        if (itemCount <= 1 || maxStackSize <= 1 || mergeRadius <= 0f || maxPairs <= 0) {
            return 0;
        }
        try (NativeLease ignored = acquireNativeLease()) {
            int clampedItems = Math.min(itemCount, getEntityCapacity());
            int clampedPairs = Math.min(maxPairs, getEntityCapacity());
            return RustNative.nativePlanItemMerges(
                clampedItems,
                xBuf,
                yBuf,
                zBuf,
                typeIdBuf,
                amountBuf,
                maxStackSize,
                mergeRadius,
                clampedPairs,
                outIntABuf,
                outIntBBuf
            );
        }
    }

    public BrainDirective evaluateGovernor(
        float currentMspt,
        float ewmaMspt,
        int loadedEntities,
        int onlinePlayers,
        float baseFullRange,
        float baseHibernateRange,
        int minViewDist,
        int maxViewDist,
        int minSimDist,
        int maxSimDist
    ) {
        ensureLoaded();
        int safeMinView = Math.clamp(Math.min(minViewDist, maxViewDist), 2, 32);
        int safeMaxView = Math.clamp(Math.max(minViewDist, maxViewDist), safeMinView, 32);
        int safeMinSim = Math.clamp(Math.min(minSimDist, maxSimDist), 2, 32);
        int safeMaxSim = Math.clamp(Math.max(minSimDist, maxSimDist), safeMinSim, 32);

        float safeCurrentMspt = Float.isFinite(currentMspt) && currentMspt >= 0f ? currentMspt : 0f;
        float safeEwmaMspt = Float.isFinite(ewmaMspt) && ewmaMspt >= 0f ? ewmaMspt : safeCurrentMspt;
        float safeBaseFull = Float.isFinite(baseFullRange) && baseFullRange > 0f ? baseFullRange : 32.0f;
        float safeBaseHib = Float.isFinite(baseHibernateRange) && baseHibernateRange >= safeBaseFull
            ? baseHibernateRange
            : Math.max(56.0f, safeBaseFull + 8.0f);

        int ret;
        int stateCode;
        int viewDist;
        int simDist;
        boolean itemMergeEnabled;
        float nextEwma;
        float fullRange;
        float hibRange;
        float mergeRadius;

        try (NativeLease ignored = acquireNativeLease()) {
            ret = RustNative.nativeEvaluateGovernor(
                safeCurrentMspt,
                safeEwmaMspt,
                Math.max(0, loadedEntities),
                Math.max(0, onlinePlayers),
                safeBaseFull,
                safeBaseHib,
                safeMinView,
                safeMaxView,
                safeMinSim,
                safeMaxSim,
                govIntBuf,
                govFloatBuf
            );

            if (ret < 0) {
                return BrainDirective.safeDefault(
                    safeEwmaMspt,
                    safeMaxView,
                    safeMaxSim,
                    safeBaseFull,
                    safeBaseHib,
                    "In-process Rust governor fallback error code=" + ret
                );
            }

            stateCode = Math.clamp(govInt.get(0), BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);
            viewDist = Math.clamp(govInt.get(1), safeMinView, safeMaxView);
            simDist = Math.clamp(govInt.get(2), safeMinSim, safeMaxSim);
            itemMergeEnabled = govInt.get(3) != 0;

            // Slot layout matches Rust jni_bridge.rs / c_abi.rs:
            // out_floats[0..4] = [full_ai_range, hibernate_range, merge_radius, ewma_mspt]
            fullRange = govFloat.get(0);
            hibRange = govFloat.get(1);
            mergeRadius = govFloat.get(2);
            nextEwma = govFloat.get(3);
        }

        if (!Float.isFinite(nextEwma) || nextEwma < 0f) {
            nextEwma = safeCurrentMspt;
        }
        if (!Float.isFinite(fullRange) || fullRange <= 0f) {
            fullRange = safeBaseFull;
        }
        if (!Float.isFinite(hibRange) || hibRange < fullRange) {
            hibRange = Math.max(safeBaseHib, fullRange + 4.0f);
        }
        if (!Float.isFinite(mergeRadius) || mergeRadius <= 0f) {
            mergeRadius = 3.5f;
        }

        String state = BrainDirective.stateNameForCode(stateCode);
        String reason = String.format(
            Locale.US,
            "In-process Rust Governor (MSPT=%.2fms, EWMA=%.2fms, entities=%d, players=%d)",
            safeCurrentMspt, nextEwma, loadedEntities, onlinePlayers
        );

        return new BrainDirective(
            state,
            stateCode,
            nextEwma,
            viewDist,
            simDist,
            fullRange,
            hibRange,
            itemMergeEnabled,
            mergeRadius,
            reason,
            false
        );
    }

    public int evaluateFarmThrottling(
        int entityCount,
        int playerCount,
        float crowdRadius,
        int mildCrowdThreshold,
        int severeCrowdThreshold,
        int chunkMobLimit,
        float villagerWakeRange,
        int villagerCrowdThreshold,
        int brainStateCode
    ) {
        ensureLoaded();
        if (entityCount <= 0) {
            return 0;
        }
        float safeCrowdRadius = Float.isFinite(crowdRadius) && crowdRadius > 0.25f ? crowdRadius : 3.5f;
        int safeMild = Math.max(1, mildCrowdThreshold);
        int safeSevere = Math.max(safeMild, severeCrowdThreshold);
        int safeChunkLimit = Math.max(4, chunkMobLimit);
        float safeWakeRange = Float.isFinite(villagerWakeRange) && villagerWakeRange >= 0f ? villagerWakeRange : 8.0f;
        int safeVillagerCrowd = Math.max(1, villagerCrowdThreshold);
        int safeState = Math.clamp(brainStateCode, BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);

        try (NativeLease ignored = acquireNativeLease()) {
            int clampedEntities = Math.min(entityCount, getEntityCapacity());
            int clampedPlayers = Math.clamp(playerCount, 0, getTargetCapacity());
            return RustNative.nativeEvaluateFarmThrottling(
                clampedEntities,
                xBuf,
                yBuf,
                zBuf,
                flagsBuf,
                clampedPlayers,
                targetXBuf,
                targetYBuf,
                targetZBuf,
                safeCrowdRadius,
                safeMild,
                safeSevere,
                safeChunkLimit,
                safeWakeRange,
                safeVillagerCrowd,
                safeState,
                outIntABuf,
                outIntBBuf
            );
        }
    }

    public int evaluatePlayerSurges(
        int playerCount,
        int brainStateCode,
        float baseMaxElytraSpeed
    ) {
        ensureLoaded();
        if (playerCount <= 0) {
            return 0;
        }
        int safeState = Math.clamp(brainStateCode, BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);
        float safeBaseSpeed = Float.isFinite(baseMaxElytraSpeed) && baseMaxElytraSpeed > 0.1f
            ? baseMaxElytraSpeed
            : 1.8f;

        try (NativeLease ignored = acquireNativeLease()) {
            int clampedPlayers = Math.min(playerCount, getEntityCapacity());
            return RustNative.nativeEvaluatePlayerSurges(
                clampedPlayers,
                xBuf,
                yBuf,
                zBuf,
                vxBuf,
                vyBuf,
                vzBuf,
                flagsBuf,
                safeState,
                safeBaseSpeed,
                outIntABuf,
                outXBuf,
                outYBuf,
                outZBuf
            );
        }
    }

    private void ensureLoaded() {
        if (!NativeLoader.isLoaded()) {
            throw new IllegalStateException("Spica Rust native kutuphanesi yuklu degil.");
        }
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
