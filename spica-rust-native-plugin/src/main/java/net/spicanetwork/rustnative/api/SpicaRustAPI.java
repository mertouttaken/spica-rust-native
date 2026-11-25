package net.spicanetwork.rustnative.api;

import net.spicanetwork.rustnative.brain.BrainDirective;
import net.spicanetwork.rustnative.nativebridge.EntityBatchArena;
import net.spicanetwork.rustnative.nativebridge.NativeLoader;
import net.spicanetwork.rustnative.nativebridge.RustNative;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;

/**
 * Public Developer API registered via Bukkit ServicesManager.
 *
 * The ordinary ByteBuffer methods copy caller-owned data into thread-local storage before JNI.
 * Callers that own every view can choose a WithExclusiveBuffers variant to skip these copies.
 * Keep inputs and outputs stable for deterministic results and use separate output buffers.
 *
 * Other Purpur/Paper 1.21.4 plugins can retrieve this service via:
 * {@code Bukkit.getServicesManager().load(SpicaRustAPI.class)}
 */
public final class SpicaRustAPI {
    private static final ThreadLocal<ByteBuffer[]> NATIVE_COPIES = ThreadLocal.withInitial(() -> new ByteBuffer[15]);

    public boolean isNativeLoaded() {
        return NativeLoader.isLoaded();
    }

    public String getNativeInfo() {
        return NativeLoader.isLoaded() ? RustNative.nativeGetInfo() : "unavailable";
    }

    /**
     * Allocate an arena that owns its native buffers. Group shared fill/compute/read sequences with withExclusiveAccess.
     */
    public EntityBatchArena createArena(int initialEntities, int initialTargets) {
        return new EntityBatchArena(initialEntities, initialTargets);
    }

    /**
     * Runs Rust Spatial-Hash batch soft-collision & density calculation.
     *
     * Inputs in {@code arena}: {@code x, y, z, radius}.
     * Outputs in {@code arena}: push vector in {@code outX, outY, outZ}, neighbor count in {@code outIntA}.
     */
    public long computeCollisions(
        EntityBatchArena arena,
        int entityCount,
        float cellSize,
        float pushStrength,
        float maxPush
    ) {
        return arena.computeCollisions(entityCount, cellSize, pushStrength, maxPush);
    }

    /**
     * Runs Rust parallel nearest-target + AI LOD classification.
     *
     * Inputs: {@code x, y, z} (mobs) and {@code targetX, targetY, targetZ} (players/targets).
     * Outputs:
     * - {@code outIntA}: nearest target index (-1 if none)
     * - {@code outDistSq}: squared distance to nearest target
     * - {@code outX, outY, outZ}: normalized steering direction toward target
     * - {@code outIntB}: LOD tier (0=FULL, 1=REDUCED, 2=HIBERNATE)
     */
    public int computeAiTargets(
        EntityBatchArena arena,
        int mobCount,
        int targetCount,
        float fullRange,
        float reducedRange
    ) {
        return arena.computeAiTargets(mobCount, targetCount, fullRange, reducedRange);
    }

    /**
     * Runs Rust Spatial-Hash Boids / Swarm AI steering (separation + alignment + cohesion + goal seek).
     *
     * Inputs: {@code x, y, z, vx, vy, vz}.
     * Outputs: steering acceleration in {@code outX, outY, outZ}.
     */
    public int computeBoidsSteering(
        EntityBatchArena arena,
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
        return arena.computeBoidsSteering(
            entityCount, goalX, goalY, goalZ, perceptionRadius, sepWeight,
            alignWeight, cohesionWeight, goalWeight, maxForce
        );
    }

    /**
     * Runs Rust SIMD-friendly radius query over {@code x, y, z}.
     * Matching indices are written into {@code arena.outIntA}.
     */
    public int queryRadius(
        EntityBatchArena arena,
        int entityCount,
        float qx,
        float qy,
        float qz,
        float radius,
        int maxResults
    ) {
        return arena.queryRadius(entityCount, qx, qy, qz, radius, maxResults);
    }

    /**
     * Runs Rust O(N) Chunk Heatmap aggregation over entity positions and collision neighbor counts.
     *
     * Inputs in {@code arena}: {@code x, z} and neighbor counts in {@code outIntA}
     * (typically populated by {@link #computeCollisions}).
     * Outputs in {@code arena}: {@code chunkX, chunkZ, chunkCount, chunkCrowded}.
     */
    public int analyzeChunks(
        EntityBatchArena arena,
        int entityCount,
        int maxHotChunks
    ) {
        return arena.analyzeChunks(entityCount, maxHotChunks);
    }

    /**
     * Runs Rust Spatial-Hash Item Merge planning.
     *
     * Inputs in {@code arena}: {@code x, y, z, typeId, amount}.
     * Outputs in {@code arena}: merge target indices in {@code outIntA} and source indices in {@code outIntB}.
     */
    public int planItemMerges(
        EntityBatchArena arena,
        int itemCount,
        int maxStackSize,
        float mergeRadius,
        int maxPairs
    ) {
        return arena.planItemMerges(itemCount, maxStackSize, mergeRadius, maxPairs);
    }

    /**
     * Runs the in-process Rust Brain Governor kernel to evaluate live server metrics into a
     * concrete {@link BrainDirective} for Purpur 1.21.4 actuation.
     */
    public BrainDirective evaluateGovernor(
        EntityBatchArena arena,
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
        return arena.evaluateGovernor(
            currentMspt, ewmaMspt, loadedEntities, onlinePlayers, baseFullRange,
            baseHibernateRange, minViewDist, maxViewDist, minSimDist, maxSimDist
        );
    }

    /**
     * Runs Rust Spatial-Hash Mob Farm & Trading-Hall Villager throttling evaluation.
     *
     * Inputs in {@code arena}: {@code x, y, z, flags} (mobs) and {@code targetX, targetY, targetZ} (players).
     * Outputs in {@code arena}:
     * - {@code outIntA}: action bitmask ({@link RustNative#ACTION_NORMAL}, {@link RustNative#ACTION_DISABLE_COLLISION},
     *   {@link RustNative#ACTION_THROTTLE_AI}, {@link RustNative#ACTION_FARM_SUPPRESS})
     * - {@code outIntB}: spatial neighbor counts within {@code crowdRadius}
     */
    public int evaluateFarmThrottling(
        EntityBatchArena arena,
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
        return arena.evaluateFarmThrottling(
            entityCount, playerCount, crowdRadius, mildCrowdThreshold,
            severeCrowdThreshold, chunkMobLimit, villagerWakeRange,
            villagerCrowdThreshold, brainStateCode
        );
    }

    /**
     * Evaluates gliding players for Elytra / chunk-load velocity surges in Rust.
     *
     * Inputs in {@code arena}: {@code x, y, z}, {@code vx, vy, vz}, and gliding flag in {@code flags} (1 = gliding).
     * Outputs in {@code arena}:
     * - {@code outIntA}: action code ({@link RustNative#SURGE_OK}, {@link RustNative#SURGE_WARN},
     *   {@link RustNative#SURGE_CLAMP_VELOCITY})
     * - {@code outX, outY, outZ}: clamped velocity components when action is {@link RustNative#SURGE_CLAMP_VELOCITY}
     */
    public int evaluatePlayerSurges(
        EntityBatchArena arena,
        int playerCount,
        int brainStateCode,
        float baseMaxElytraSpeed
    ) {
        return arena.evaluatePlayerSurges(playerCount, brainStateCode, baseMaxElytraSpeed);
    }

    /**
     * Computes a 2D Dijkstra integration distance field and normalized steering flow-field
     * around obstacles toward {@code (goalX, goalZ)}.
     *
     * All buffers MUST be direct ByteBuffers in native byte order.
     */
    public int computeFlowField(
        int width,
        int depth,
        ByteBuffer costGrid,
        int goalX,
        int goalZ,
        ByteBuffer outDist,
        ByteBuffer outDirX,
        ByteBuffer outDirZ
    ) {
        return computeFlowField(
            width, depth, costGrid, goalX, goalZ, outDist, outDirX, outDirZ, false
        );
    }

    /**
     * Zero-copy variant. The caller must exclusively own every direct buffer for the full
     * call, including all slices and duplicates. Sharing a buffer requires external synchronization.
     */
    public int computeFlowFieldWithExclusiveBuffers(
        int width,
        int depth,
        ByteBuffer costGrid,
        int goalX,
        int goalZ,
        ByteBuffer outDist,
        ByteBuffer outDirX,
        ByteBuffer outDirZ
    ) {
        return computeFlowField(
            width, depth, costGrid, goalX, goalZ, outDist, outDirX, outDirZ, true
        );
    }

    private int computeFlowField(
        int width,
        int depth,
        ByteBuffer costGrid,
        int goalX,
        int goalZ,
        ByteBuffer outDist,
        ByteBuffer outDirX,
        ByteBuffer outDirZ,
        boolean exclusiveBuffers
    ) {
        ensureLoaded();
        if (width <= 0 || depth <= 0) {
            return 0;
        }
        requireDirectBuffer(costGrid, "costGrid");
        requireDirectNativeBuffer(outDist, "outDist", true);
        requireDirectNativeBuffer(outDirX, "outDirX", true);
        requireDirectNativeBuffer(outDirZ, "outDirZ", true);
        long cells = (long) width * (long) depth;
        if (cells > Integer.MAX_VALUE / Float.BYTES) {
            throw new IllegalArgumentException("Flow-field grid dimensions overflow buffer limits.");
        }
        int floatBytes = (int) (cells * Float.BYTES);
        if (costGrid.capacity() < cells
            || outDist.capacity() < floatBytes
            || outDirX.capacity() < floatBytes
            || outDirZ.capacity() < floatBytes) {
            throw new IllegalArgumentException("Flow-field ByteBuffers are smaller than required grid size.");
        }

        ByteBuffer[] scratch = exclusiveBuffers ? null : NATIVE_COPIES.get();
        ByteBuffer stagedCostGrid = NativeCopies.prepare(scratch, 0, costGrid, (int) cells);
        ByteBuffer stagedOutDist = NativeCopies.prepare(scratch, 1, outDist, floatBytes);
        ByteBuffer stagedOutDirX = NativeCopies.prepare(scratch, 2, outDirX, floatBytes);
        ByteBuffer stagedOutDirZ = NativeCopies.prepare(scratch, 3, outDirZ, floatBytes);
        int result = RustNative.nativeComputeFlowField(
            width,
            depth,
            stagedCostGrid,
            goalX,
            goalZ,
            stagedOutDist,
            stagedOutDirX,
            stagedOutDirZ
        );
        if (result >= 0 && scratch != null) {
            NativeCopies.copyOut(stagedOutDist, outDist, floatBytes);
            NativeCopies.copyOut(stagedOutDirX, outDirX, floatBytes);
            NativeCopies.copyOut(stagedOutDirZ, outDirZ, floatBytes);
        }
        return result;
    }

    /**
     * Generates a 2D multi-octave Fractal Brownian Motion (FBM) world-generation noise field
     * into {@code outHeights} (direct native-order {@code float} ByteBuffer of {@code width * depth * 4} bytes).
     */
    public int generateNoise2D(
        float originX,
        float originZ,
        int width,
        int depth,
        float step,
        int seed,
        int octaves,
        float frequency,
        float lacunarity,
        float persistence,
        ByteBuffer outHeights
    ) {
        return generateNoise2D(
            originX, originZ, width, depth, step, seed, octaves, frequency, lacunarity, persistence,
            outHeights, false
        );
    }

    /**
     * Zero-copy variant. The caller must exclusively own every direct buffer for the full
     * call, including all slices and duplicates. Sharing a buffer requires external synchronization.
     */
    public int generateNoise2DWithExclusiveBuffers(
        float originX,
        float originZ,
        int width,
        int depth,
        float step,
        int seed,
        int octaves,
        float frequency,
        float lacunarity,
        float persistence,
        ByteBuffer outHeights
    ) {
        return generateNoise2D(
            originX, originZ, width, depth, step, seed, octaves, frequency, lacunarity, persistence,
            outHeights, true
        );
    }

    private int generateNoise2D(
        float originX,
        float originZ,
        int width,
        int depth,
        float step,
        int seed,
        int octaves,
        float frequency,
        float lacunarity,
        float persistence,
        ByteBuffer outHeights,
        boolean exclusiveBuffers
    ) {
        ensureLoaded();
        if (width <= 0 || depth <= 0) {
            return 0;
        }
        requireDirectNativeBuffer(outHeights, "outHeights", true);
        long cells = (long) width * (long) depth;
        if (cells > Integer.MAX_VALUE / Float.BYTES || outHeights.capacity() < cells * Float.BYTES) {
            throw new IllegalArgumentException("outHeights buffer is smaller than width * depth * 4 bytes.");
        }

        float safeOriginX = Float.isFinite(originX) ? originX : 0f;
        float safeOriginZ = Float.isFinite(originZ) ? originZ : 0f;
        float safeStep = Float.isFinite(step) && step > 0f ? step : 1.0f;
        int safeOctaves = Math.clamp(octaves, 1, 16);
        float safeFreq = Float.isFinite(frequency) && frequency > 0f ? frequency : 0.01f;
        float safeLacunarity = Float.isFinite(lacunarity) && lacunarity > 0f ? lacunarity : 2.0f;
        float safePersistence = Float.isFinite(persistence) && persistence > 0f ? persistence : 0.5f;

        ByteBuffer[] scratch = exclusiveBuffers ? null : NATIVE_COPIES.get();
        ByteBuffer stagedOutHeights = NativeCopies.prepare(scratch, 0, outHeights, (int) cells * Float.BYTES);
        int result = RustNative.nativeGenerateNoise2D(
            safeOriginX,
            safeOriginZ,
            width,
            depth,
            safeStep,
            seed,
            safeOctaves,
            safeFreq,
            safeLacunarity,
            safePersistence,
            stagedOutHeights
        );
        if (result >= 0 && scratch != null) {
            NativeCopies.copyOut(stagedOutHeights, outHeights, (int) cells * Float.BYTES);
        }
        return result;
    }

    /**
     * Evaluates per-player mob visibility (distance, vertical delta, rear-FOV cone, and per-player
     * Top-K budget) in parallel Rust and emits only state-transition deltas compared
     * against {@code visibilityBitset} ({@code 1} = currently hidden, {@code 0} = currently visible).
     *
     * All ByteBuffers MUST be non-null direct ByteBuffers in native byte order.
     */
    public int computeMobTracking(
        int mobCount,
        ByteBuffer mobXs,
        ByteBuffer mobYs,
        ByteBuffer mobZs,
        ByteBuffer mobFlags,
        int playerCount,
        ByteBuffer playerXs,
        ByteBuffer playerYs,
        ByteBuffer playerZs,
        ByteBuffer playerLookXs,
        ByteBuffer playerLookYs,
        ByteBuffer playerLookZs,
        float maxTrackRange,
        float safetyNearRange,
        float maxVerticalDelta,
        float rearCullRange,
        float rearCullDotThreshold,
        int maxMobsPerPlayer,
        int brainStateCode,
        ByteBuffer visibilityBitset,
        int maxDeltas,
        ByteBuffer outDeltaPlayerIdx,
        ByteBuffer outDeltaMobIdx,
        ByteBuffer outDeltaAction,
        ByteBuffer outStats
    ) {
        return computeMobTracking(
            mobCount, mobXs, mobYs, mobZs, mobFlags, playerCount, playerXs, playerYs, playerZs,
            playerLookXs, playerLookYs, playerLookZs, maxTrackRange, safetyNearRange,
            maxVerticalDelta, rearCullRange, rearCullDotThreshold, maxMobsPerPlayer, brainStateCode,
            visibilityBitset, maxDeltas, outDeltaPlayerIdx, outDeltaMobIdx, outDeltaAction, outStats,
            false
        );
    }

    /**
     * Zero-copy variant. The caller must exclusively own every direct buffer for the full
     * call, including all slices and duplicates. Sharing a buffer requires external synchronization.
     */
    public int computeMobTrackingWithExclusiveBuffers(
        int mobCount,
        ByteBuffer mobXs,
        ByteBuffer mobYs,
        ByteBuffer mobZs,
        ByteBuffer mobFlags,
        int playerCount,
        ByteBuffer playerXs,
        ByteBuffer playerYs,
        ByteBuffer playerZs,
        ByteBuffer playerLookXs,
        ByteBuffer playerLookYs,
        ByteBuffer playerLookZs,
        float maxTrackRange,
        float safetyNearRange,
        float maxVerticalDelta,
        float rearCullRange,
        float rearCullDotThreshold,
        int maxMobsPerPlayer,
        int brainStateCode,
        ByteBuffer visibilityBitset,
        int maxDeltas,
        ByteBuffer outDeltaPlayerIdx,
        ByteBuffer outDeltaMobIdx,
        ByteBuffer outDeltaAction,
        ByteBuffer outStats
    ) {
        return computeMobTracking(
            mobCount, mobXs, mobYs, mobZs, mobFlags, playerCount, playerXs, playerYs, playerZs,
            playerLookXs, playerLookYs, playerLookZs, maxTrackRange, safetyNearRange,
            maxVerticalDelta, rearCullRange, rearCullDotThreshold, maxMobsPerPlayer, brainStateCode,
            visibilityBitset, maxDeltas, outDeltaPlayerIdx, outDeltaMobIdx, outDeltaAction, outStats,
            true
        );
    }

    private int computeMobTracking(
        int mobCount,
        ByteBuffer mobXs,
        ByteBuffer mobYs,
        ByteBuffer mobZs,
        ByteBuffer mobFlags,
        int playerCount,
        ByteBuffer playerXs,
        ByteBuffer playerYs,
        ByteBuffer playerZs,
        ByteBuffer playerLookXs,
        ByteBuffer playerLookYs,
        ByteBuffer playerLookZs,
        float maxTrackRange,
        float safetyNearRange,
        float maxVerticalDelta,
        float rearCullRange,
        float rearCullDotThreshold,
        int maxMobsPerPlayer,
        int brainStateCode,
        ByteBuffer visibilityBitset,
        int maxDeltas,
        ByteBuffer outDeltaPlayerIdx,
        ByteBuffer outDeltaMobIdx,
        ByteBuffer outDeltaAction,
        ByteBuffer outStats,
        boolean exclusiveBuffers
    ) {
        ensureLoaded();
        if (mobCount <= 0 || playerCount <= 0) {
            return 0;
        }

        requireDirectNativeBuffer(mobXs, "mobXs", false);
        requireDirectNativeBuffer(mobYs, "mobYs", false);
        requireDirectNativeBuffer(mobZs, "mobZs", false);
        requireDirectNativeBuffer(mobFlags, "mobFlags", false);
        requireDirectNativeBuffer(playerXs, "playerXs", false);
        requireDirectNativeBuffer(playerYs, "playerYs", false);
        requireDirectNativeBuffer(playerZs, "playerZs", false);
        requireDirectNativeBuffer(playerLookXs, "playerLookXs", false);
        requireDirectNativeBuffer(playerLookYs, "playerLookYs", false);
        requireDirectNativeBuffer(playerLookZs, "playerLookZs", false);
        requireDirectBuffer(visibilityBitset, "visibilityBitset");
        if (visibilityBitset.isReadOnly()) {
            throw new IllegalArgumentException("visibilityBitset must be a writable direct ByteBuffer.");
        }
        requireDirectNativeBuffer(outDeltaPlayerIdx, "outDeltaPlayerIdx", true);
        requireDirectNativeBuffer(outDeltaMobIdx, "outDeltaMobIdx", true);
        requireDirectNativeBuffer(outDeltaAction, "outDeltaAction", true);
        requireDirectNativeBuffer(outStats, "outStats", true);

        long mobBytes = (long) mobCount * Float.BYTES;
        if (mobXs.capacity() < mobBytes
            || mobYs.capacity() < mobBytes
            || mobZs.capacity() < mobBytes
            || mobFlags.capacity() < mobBytes) {
            throw new IllegalArgumentException("Mob SoA ByteBuffers are smaller than mobCount * 4 bytes.");
        }

        long playerBytes = (long) playerCount * Float.BYTES;
        if (playerXs.capacity() < playerBytes
            || playerYs.capacity() < playerBytes
            || playerZs.capacity() < playerBytes
            || playerLookXs.capacity() < playerBytes
            || playerLookYs.capacity() < playerBytes
            || playerLookZs.capacity() < playerBytes) {
            throw new IllegalArgumentException("Player SoA ByteBuffers are smaller than playerCount * 4 bytes.");
        }

        long totalPairs = (long) playerCount * (long) mobCount;
        long neededBitsetBytes = (totalPairs + 7L) >>> 3;
        if (visibilityBitset.capacity() < neededBitsetBytes) {
            throw new IllegalArgumentException("visibilityBitset is smaller than ceil(playerCount * mobCount / 8) bytes.");
        }

        int safeMaxDeltas = Math.max(0, maxDeltas);
        long deltaBytes = (long) safeMaxDeltas * Integer.BYTES;
        if (outDeltaPlayerIdx.capacity() < deltaBytes
            || outDeltaMobIdx.capacity() < deltaBytes
            || outDeltaAction.capacity() < deltaBytes) {
            throw new IllegalArgumentException("Output delta ByteBuffers are smaller than maxDeltas * 4 bytes.");
        }

        if (outStats.capacity() < 4 * Integer.BYTES) {
            throw new IllegalArgumentException("outStats ByteBuffer must be at least 16 bytes (4 ints).");
        }

        float safeMaxTrack = Float.isFinite(maxTrackRange) && maxTrackRange > 1.0f ? maxTrackRange : 48.0f;
        float safeNear = Float.isFinite(safetyNearRange) && safetyNearRange >= 0.0f
            ? Math.min(safetyNearRange, safeMaxTrack)
            : 12.0f;
        float safeVert = Float.isFinite(maxVerticalDelta) && maxVerticalDelta > 0.0f ? maxVerticalDelta : 24.0f;
        float safeRearRange = Float.isFinite(rearCullRange) && rearCullRange >= 0.0f ? rearCullRange : 16.0f;
        float safeRearDot = Float.isFinite(rearCullDotThreshold)
            ? Math.clamp(rearCullDotThreshold, -1.0f, 1.0f)
            : -0.25f;
        int safeMaxMobs = Math.max(1, maxMobsPerPlayer);
        int safeState = Math.clamp(brainStateCode, BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);

        ByteBuffer[] scratch = exclusiveBuffers ? null : NATIVE_COPIES.get();
        ByteBuffer stagedMobXs = NativeCopies.prepare(scratch, 0, mobXs, (int) mobBytes);
        ByteBuffer stagedMobYs = NativeCopies.prepare(scratch, 1, mobYs, (int) mobBytes);
        ByteBuffer stagedMobZs = NativeCopies.prepare(scratch, 2, mobZs, (int) mobBytes);
        ByteBuffer stagedMobFlags = NativeCopies.prepare(scratch, 3, mobFlags, (int) mobBytes);
        ByteBuffer stagedPlayerXs = NativeCopies.prepare(scratch, 4, playerXs, (int) playerBytes);
        ByteBuffer stagedPlayerYs = NativeCopies.prepare(scratch, 5, playerYs, (int) playerBytes);
        ByteBuffer stagedPlayerZs = NativeCopies.prepare(scratch, 6, playerZs, (int) playerBytes);
        ByteBuffer stagedPlayerLookXs = NativeCopies.prepare(scratch, 7, playerLookXs, (int) playerBytes);
        ByteBuffer stagedPlayerLookYs = NativeCopies.prepare(scratch, 8, playerLookYs, (int) playerBytes);
        ByteBuffer stagedPlayerLookZs = NativeCopies.prepare(scratch, 9, playerLookZs, (int) playerBytes);
        ByteBuffer stagedVisibilityBitset = NativeCopies.prepare(scratch, 10, visibilityBitset, (int) neededBitsetBytes);
        ByteBuffer stagedOutDeltaPlayerIdx = NativeCopies.prepare(scratch, 11, outDeltaPlayerIdx, (int) deltaBytes);
        ByteBuffer stagedOutDeltaMobIdx = NativeCopies.prepare(scratch, 12, outDeltaMobIdx, (int) deltaBytes);
        ByteBuffer stagedOutDeltaAction = NativeCopies.prepare(scratch, 13, outDeltaAction, (int) deltaBytes);
        ByteBuffer stagedOutStats = NativeCopies.prepare(scratch, 14, outStats, 4 * Integer.BYTES);
        int result = RustNative.nativeComputeMobTracking(
            mobCount,
            stagedMobXs,
            stagedMobYs,
            stagedMobZs,
            stagedMobFlags,
            playerCount,
            stagedPlayerXs,
            stagedPlayerYs,
            stagedPlayerZs,
            stagedPlayerLookXs,
            stagedPlayerLookYs,
            stagedPlayerLookZs,
            safeMaxTrack,
            safeNear,
            safeVert,
            safeRearRange,
            safeRearDot,
            safeMaxMobs,
            safeState,
            stagedVisibilityBitset,
            safeMaxDeltas,
            stagedOutDeltaPlayerIdx,
            stagedOutDeltaMobIdx,
            stagedOutDeltaAction,
            stagedOutStats
        );
        if (result >= 0 && scratch != null) {
            NativeCopies.copyOut(stagedVisibilityBitset, visibilityBitset, (int) neededBitsetBytes);
            NativeCopies.copyOut(stagedOutDeltaPlayerIdx, outDeltaPlayerIdx, result * Integer.BYTES);
            NativeCopies.copyOut(stagedOutDeltaMobIdx, outDeltaMobIdx, result * Integer.BYTES);
            NativeCopies.copyOut(stagedOutDeltaAction, outDeltaAction, result * Integer.BYTES);
            NativeCopies.copyOut(stagedOutStats, outStats, 4 * Integer.BYTES);
        }
        return result;
    }

    /**
     * Evaluates per-chunk Redstone & Hopper activity rates and thermal heat scores in
     * Rust, returning the number of top hottest chunk indices written to {@code outTopIndices}.
     *
     * All ByteBuffers MUST be non-null direct ByteBuffers in native byte order.
     */
    public int evaluateRedstoneChunks(
        int chunkCount,
        ByteBuffer chunkXs,
        ByteBuffer chunkZs,
        ByteBuffer redstonePerSec,
        ByteBuffer hopperPerSec,
        ByteBuffer prevHeat,
        int redstoneLimit,
        int hopperLimit,
        int brainStateCode,
        ByteBuffer outActions,
        ByteBuffer outNewHeat,
        ByteBuffer outThrottleRatio,
        int maxTopChunks,
        ByteBuffer outTopIndices
    ) {
        return evaluateRedstoneChunks(
            chunkCount, chunkXs, chunkZs, redstonePerSec, hopperPerSec, prevHeat, redstoneLimit,
            hopperLimit, brainStateCode, outActions, outNewHeat, outThrottleRatio, maxTopChunks,
            outTopIndices, false
        );
    }

    /**
     * Zero-copy variant. The caller must exclusively own every direct buffer for the full
     * call, including all slices and duplicates. Sharing a buffer requires external synchronization.
     */
    public int evaluateRedstoneChunksWithExclusiveBuffers(
        int chunkCount,
        ByteBuffer chunkXs,
        ByteBuffer chunkZs,
        ByteBuffer redstonePerSec,
        ByteBuffer hopperPerSec,
        ByteBuffer prevHeat,
        int redstoneLimit,
        int hopperLimit,
        int brainStateCode,
        ByteBuffer outActions,
        ByteBuffer outNewHeat,
        ByteBuffer outThrottleRatio,
        int maxTopChunks,
        ByteBuffer outTopIndices
    ) {
        return evaluateRedstoneChunks(
            chunkCount, chunkXs, chunkZs, redstonePerSec, hopperPerSec, prevHeat, redstoneLimit,
            hopperLimit, brainStateCode, outActions, outNewHeat, outThrottleRatio, maxTopChunks,
            outTopIndices, true
        );
    }

    private int evaluateRedstoneChunks(
        int chunkCount,
        ByteBuffer chunkXs,
        ByteBuffer chunkZs,
        ByteBuffer redstonePerSec,
        ByteBuffer hopperPerSec,
        ByteBuffer prevHeat,
        int redstoneLimit,
        int hopperLimit,
        int brainStateCode,
        ByteBuffer outActions,
        ByteBuffer outNewHeat,
        ByteBuffer outThrottleRatio,
        int maxTopChunks,
        ByteBuffer outTopIndices,
        boolean exclusiveBuffers
    ) {
        ensureLoaded();
        if (chunkCount <= 0) {
            return 0;
        }

        requireDirectNativeBuffer(chunkXs, "chunkXs", false);
        requireDirectNativeBuffer(chunkZs, "chunkZs", false);
        requireDirectNativeBuffer(redstonePerSec, "redstonePerSec", false);
        requireDirectNativeBuffer(hopperPerSec, "hopperPerSec", false);
        requireDirectNativeBuffer(prevHeat, "prevHeat", false);
        requireDirectNativeBuffer(outActions, "outActions", true);
        requireDirectNativeBuffer(outNewHeat, "outNewHeat", true);
        requireDirectNativeBuffer(outThrottleRatio, "outThrottleRatio", true);
        requireDirectNativeBuffer(outTopIndices, "outTopIndices", true);

        long chunkBytes = (long) chunkCount * Integer.BYTES;
        if (chunkXs.capacity() < chunkBytes
            || chunkZs.capacity() < chunkBytes
            || redstonePerSec.capacity() < chunkBytes
            || hopperPerSec.capacity() < chunkBytes
            || prevHeat.capacity() < chunkBytes
            || outActions.capacity() < chunkBytes
            || outNewHeat.capacity() < chunkBytes
            || outThrottleRatio.capacity() < chunkBytes) {
            throw new IllegalArgumentException("Redstone chunk ByteBuffers are smaller than chunkCount * 4 bytes.");
        }

        int safeMaxTop = Math.max(0, maxTopChunks);
        long topBytes = (long) safeMaxTop * Integer.BYTES;
        if (outTopIndices.capacity() < topBytes) {
            throw new IllegalArgumentException("outTopIndices ByteBuffer is smaller than maxTopChunks * 4 bytes.");
        }

        int safeRedstoneLimit = Math.max(1, redstoneLimit);
        int safeHopperLimit = Math.max(1, hopperLimit);
        int safeState = Math.clamp(brainStateCode, BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);

        ByteBuffer[] scratch = exclusiveBuffers ? null : NATIVE_COPIES.get();
        ByteBuffer stagedChunkXs = NativeCopies.prepare(scratch, 0, chunkXs, (int) chunkBytes);
        ByteBuffer stagedChunkZs = NativeCopies.prepare(scratch, 1, chunkZs, (int) chunkBytes);
        ByteBuffer stagedRedstonePerSec = NativeCopies.prepare(scratch, 2, redstonePerSec, (int) chunkBytes);
        ByteBuffer stagedHopperPerSec = NativeCopies.prepare(scratch, 3, hopperPerSec, (int) chunkBytes);
        ByteBuffer stagedPrevHeat = NativeCopies.prepare(scratch, 4, prevHeat, (int) chunkBytes);
        ByteBuffer stagedOutActions = NativeCopies.prepare(scratch, 5, outActions, (int) chunkBytes);
        ByteBuffer stagedOutNewHeat = NativeCopies.prepare(scratch, 6, outNewHeat, (int) chunkBytes);
        ByteBuffer stagedOutThrottleRatio = NativeCopies.prepare(scratch, 7, outThrottleRatio, (int) chunkBytes);
        ByteBuffer stagedOutTopIndices = NativeCopies.prepare(scratch, 8, outTopIndices, (int) topBytes);
        int result = RustNative.nativeEvaluateRedstoneChunks(
            chunkCount,
            stagedChunkXs,
            stagedChunkZs,
            stagedRedstonePerSec,
            stagedHopperPerSec,
            stagedPrevHeat,
            safeRedstoneLimit,
            safeHopperLimit,
            safeState,
            stagedOutActions,
            stagedOutNewHeat,
            stagedOutThrottleRatio,
            safeMaxTop,
            stagedOutTopIndices
        );
        if (result >= 0 && scratch != null) {
            NativeCopies.copyOut(stagedOutActions, outActions, (int) chunkBytes);
            NativeCopies.copyOut(stagedOutNewHeat, outNewHeat, (int) chunkBytes);
            NativeCopies.copyOut(stagedOutThrottleRatio, outThrottleRatio, (int) chunkBytes);
            NativeCopies.copyOut(stagedOutTopIndices, outTopIndices, (int) topBytes);
        }
        return result;
    }

    /**
     * Computes a parallel 3D arena/region block diff in Rust (YZX layout:
     * {@code idx = (y * depth + z) * width + x}).
     *
     * Return value semantics:
     * - Count-only mode ({@code maxDiffs == 0}): returns total differing blocks without writing outputs.
     * - Extraction mode ({@code maxDiffs > 0}): writes up to {@code maxDiffs} entries and returns the
     *   number of entries written (capped at {@code maxDiffs}).
     */
    public int computeBlockDiff(
        int width,
        int height,
        int depth,
        ByteBuffer currentBlocks,
        ByteBuffer targetBlocks,
        int maxDiffs,
        ByteBuffer outRelX,
        ByteBuffer outRelY,
        ByteBuffer outRelZ,
        ByteBuffer outNewBlockId
    ) {
        return computeBlockDiff(
            width, height, depth, currentBlocks, targetBlocks, maxDiffs, outRelX, outRelY, outRelZ,
            outNewBlockId, false
        );
    }

    /**
     * Zero-copy variant. The caller must exclusively own every direct buffer for the full
     * call, including all slices and duplicates. Sharing a buffer requires external synchronization.
     */
    public int computeBlockDiffWithExclusiveBuffers(
        int width,
        int height,
        int depth,
        ByteBuffer currentBlocks,
        ByteBuffer targetBlocks,
        int maxDiffs,
        ByteBuffer outRelX,
        ByteBuffer outRelY,
        ByteBuffer outRelZ,
        ByteBuffer outNewBlockId
    ) {
        return computeBlockDiff(
            width, height, depth, currentBlocks, targetBlocks, maxDiffs, outRelX, outRelY, outRelZ,
            outNewBlockId, true
        );
    }

    private int computeBlockDiff(
        int width,
        int height,
        int depth,
        ByteBuffer currentBlocks,
        ByteBuffer targetBlocks,
        int maxDiffs,
        ByteBuffer outRelX,
        ByteBuffer outRelY,
        ByteBuffer outRelZ,
        ByteBuffer outNewBlockId,
        boolean exclusiveBuffers
    ) {
        ensureLoaded();
        if (width <= 0 || height <= 0 || depth <= 0) {
            return 0;
        }
        if (width > 16_777_216 || height > 16_777_216 || depth > 16_777_216) {
            throw new IllegalArgumentException("Block diff dimensions exceed 16,777,216 block safety limit.");
        }
        long plane = (long) width * (long) depth;
        if (plane > 16_777_216L || plane * (long) height > 16_777_216L) {
            throw new IllegalArgumentException("Block diff volume exceeds 16,777,216 block safety limit.");
        }
        long volume = plane * (long) height;
        requireDirectNativeBuffer(currentBlocks, "currentBlocks", false);
        requireDirectNativeBuffer(targetBlocks, "targetBlocks", false);
        long blockBytes = volume * Integer.BYTES;
        if (currentBlocks.capacity() < blockBytes || targetBlocks.capacity() < blockBytes) {
            throw new IllegalArgumentException("Block buffers are smaller than width * height * depth * 4 bytes.");
        }

        int safeMaxDiffs = Math.max(0, maxDiffs);
        if (safeMaxDiffs > 0) {
            requireDirectNativeBuffer(outRelX, "outRelX", true);
            requireDirectNativeBuffer(outRelY, "outRelY", true);
            requireDirectNativeBuffer(outRelZ, "outRelZ", true);
            requireDirectNativeBuffer(outNewBlockId, "outNewBlockId", true);
            long diffBytes = (long) safeMaxDiffs * Integer.BYTES;
            if (outRelX.capacity() < diffBytes
                || outRelY.capacity() < diffBytes
                || outRelZ.capacity() < diffBytes
                || outNewBlockId.capacity() < diffBytes) {
                throw new IllegalArgumentException("Block diff output buffers are smaller than maxDiffs * 4 bytes.");
            }
        }

        ByteBuffer[] scratch = exclusiveBuffers ? null : NATIVE_COPIES.get();
        ByteBuffer stagedCurrentBlocks = NativeCopies.prepare(scratch, 0, currentBlocks, (int) blockBytes);
        ByteBuffer stagedTargetBlocks = NativeCopies.prepare(scratch, 1, targetBlocks, (int) blockBytes);
        ByteBuffer stagedOutRelX = safeMaxDiffs == 0 ? null : NativeCopies.prepare(scratch, 2, outRelX, safeMaxDiffs * Integer.BYTES);
        ByteBuffer stagedOutRelY = safeMaxDiffs == 0 ? null : NativeCopies.prepare(scratch, 3, outRelY, safeMaxDiffs * Integer.BYTES);
        ByteBuffer stagedOutRelZ = safeMaxDiffs == 0 ? null : NativeCopies.prepare(scratch, 4, outRelZ, safeMaxDiffs * Integer.BYTES);
        ByteBuffer stagedOutNewBlockId = safeMaxDiffs == 0 ? null : NativeCopies.prepare(scratch, 5, outNewBlockId, safeMaxDiffs * Integer.BYTES);
        int result = RustNative.nativeComputeBlockDiff(
            width,
            height,
            depth,
            stagedCurrentBlocks,
            stagedTargetBlocks,
            safeMaxDiffs,
            stagedOutRelX,
            stagedOutRelY,
            stagedOutRelZ,
            stagedOutNewBlockId
        );
        if (result > 0 && safeMaxDiffs > 0 && scratch != null) {
            int writtenBytes = result * Integer.BYTES;
            NativeCopies.copyOut(stagedOutRelX, outRelX, writtenBytes);
            NativeCopies.copyOut(stagedOutRelY, outRelY, writtenBytes);
            NativeCopies.copyOut(stagedOutRelZ, outRelZ, writtenBytes);
            NativeCopies.copyOut(stagedOutNewBlockId, outNewBlockId, writtenBytes);
        }
        return result;
    }

    private static final class NativeCopies {
        private static final int MAX_CACHED_BYTES = 8 * 1024 * 1024;
        static ByteBuffer prepare(ByteBuffer[] scratch, int slot, ByteBuffer source, int length) {
            if (scratch == null) {
                return source;
            }

            ByteBuffer buffer = scratch[slot];
            if (buffer == null || buffer.capacity() < length) {
                buffer = ByteBuffer.allocateDirect(length).order(ByteOrder.nativeOrder());
                if (length <= MAX_CACHED_BYTES) {
                    scratch[slot] = buffer;
                }
            }
            if (length > 0) {
                buffer.put(0, source.duplicate().clear(), 0, length);
            }
            return buffer;
        }

        static void copyOut(ByteBuffer staged, ByteBuffer destination, int length) {
            if (length > 0) {
                destination.duplicate().clear().put(0, staged, 0, length);
            }
        }
    }

    private static void requireDirectBuffer(ByteBuffer buf, String name) {
        if (buf == null || !buf.isDirect()) {
            throw new IllegalArgumentException(name + " must be a non-null direct ByteBuffer.");
        }
    }

    private static void requireDirectNativeBuffer(ByteBuffer buf, String name, boolean writable) {
        requireDirectBuffer(buf, name);
        if (writable && buf.isReadOnly()) {
            throw new IllegalArgumentException(name + " must be a writable direct ByteBuffer.");
        }
        if (buf.order() != ByteOrder.nativeOrder()) {
            throw new IllegalArgumentException(name + " must use ByteOrder.nativeOrder().");
        }
    }

    private void ensureLoaded() {
        if (!NativeLoader.isLoaded()) {
            throw new IllegalStateException("Spica Rust native kutuphanesi yuklu degil.");
        }
    }
}
