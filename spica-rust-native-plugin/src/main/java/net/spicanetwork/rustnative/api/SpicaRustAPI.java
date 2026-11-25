package net.spicanetwork.rustnative.api;

import net.spicanetwork.rustnative.brain.BrainDirective;
import net.spicanetwork.rustnative.nativebridge.EntityBatchArena;
import net.spicanetwork.rustnative.nativebridge.NativeLoader;
import net.spicanetwork.rustnative.nativebridge.RustNative;

import java.nio.ByteBuffer;
import java.util.Locale;

/**
 * Public Developer API registered via Bukkit ServicesManager.
 *
 * Other Purpur/Paper 1.21.4 plugins can retrieve this service via:
 * {@code Bukkit.getServicesManager().load(SpicaRustAPI.class)}
 */
public final class SpicaRustAPI {

    public boolean isNativeLoaded() {
        return NativeLoader.isLoaded();
    }

    public String getNativeInfo() {
        return NativeLoader.isLoaded() ? RustNative.nativeGetInfo() : "unavailable";
    }

    /**
     * Allocate a thread-confined zero-copy SoA arena for custom batch workloads.
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
        ensureLoaded();
        if (entityCount <= 0) {
            return 0L;
        }
        return RustNative.nativeComputeCollisions(
            entityCount,
            arena.xBuf, arena.yBuf, arena.zBuf, arena.radiusBuf,
            cellSize, pushStrength, maxPush,
            arena.outXBuf, arena.outYBuf, arena.outZBuf, arena.outIntABuf
        );
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
        ensureLoaded();
        if (mobCount <= 0) {
            return 0;
        }
        return RustNative.nativeComputeAiTargets(
            mobCount,
            arena.xBuf, arena.yBuf, arena.zBuf,
            targetCount,
            arena.targetXBuf, arena.targetYBuf, arena.targetZBuf,
            fullRange, reducedRange,
            arena.outIntABuf, arena.outDistSqBuf,
            arena.outXBuf, arena.outYBuf, arena.outZBuf,
            arena.outIntBBuf
        );
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
        ensureLoaded();
        if (entityCount <= 0) {
            return 0;
        }
        return RustNative.nativeComputeBoids(
            entityCount,
            arena.xBuf, arena.yBuf, arena.zBuf,
            arena.vxBuf, arena.vyBuf, arena.vzBuf,
            goalX, goalY, goalZ,
            perceptionRadius,
            sepWeight, alignWeight, cohesionWeight, goalWeight, maxForce,
            arena.outXBuf, arena.outYBuf, arena.outZBuf
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
        ensureLoaded();
        if (entityCount <= 0 || maxResults <= 0) {
            return 0;
        }
        return RustNative.nativeQueryRadius(
            entityCount,
            arena.xBuf, arena.yBuf, arena.zBuf,
            qx, qy, qz, radius,
            Math.min(maxResults, arena.getEntityCapacity()),
            arena.outIntABuf
        );
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
        ensureLoaded();
        if (entityCount <= 0 || maxHotChunks <= 0) {
            return 0;
        }
        int clampedCount = Math.min(entityCount, arena.getEntityCapacity());
        int clampedMax = Math.min(maxHotChunks, EntityBatchArena.MAX_HOT_CHUNKS);
        return RustNative.nativeAnalyzeChunks(
            clampedCount,
            arena.xBuf,
            arena.zBuf,
            arena.outIntABuf,
            clampedMax,
            arena.chunkXBuf,
            arena.chunkZBuf,
            arena.chunkCountBuf,
            arena.chunkCrowdedBuf
        );
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
        ensureLoaded();
        if (itemCount <= 1 || maxStackSize <= 1 || mergeRadius <= 0f || maxPairs <= 0) {
            return 0;
        }
        int clampedItems = Math.min(itemCount, arena.getEntityCapacity());
        int clampedPairs = Math.min(maxPairs, arena.getEntityCapacity());
        return RustNative.nativePlanItemMerges(
            clampedItems,
            arena.xBuf,
            arena.yBuf,
            arena.zBuf,
            arena.typeIdBuf,
            arena.amountBuf,
            maxStackSize,
            mergeRadius,
            clampedPairs,
            arena.outIntABuf,
            arena.outIntBBuf
        );
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

        int ret = RustNative.nativeEvaluateGovernor(
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
            arena.govIntBuf,
            arena.govFloatBuf
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

        int stateCode = Math.clamp(arena.govInt.get(0), BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);
        int viewDist = Math.clamp(arena.govInt.get(1), safeMinView, safeMaxView);
        int simDist = Math.clamp(arena.govInt.get(2), safeMinSim, safeMaxSim);
        boolean itemMergeEnabled = arena.govInt.get(3) != 0;

        float nextEwma = arena.govFloat.get(0);
        if (!Float.isFinite(nextEwma) || nextEwma < 0f) {
            nextEwma = safeCurrentMspt;
        }
        float fullRange = arena.govFloat.get(1);
        if (!Float.isFinite(fullRange) || fullRange <= 0f) {
            fullRange = safeBaseFull;
        }
        float hibRange = arena.govFloat.get(2);
        if (!Float.isFinite(hibRange) || hibRange < fullRange) {
            hibRange = Math.max(safeBaseHib, fullRange + 4.0f);
        }
        float mergeRadius = arena.govFloat.get(3);
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
        ensureLoaded();
        if (entityCount <= 0) {
            return 0;
        }
        int clampedEntities = Math.min(entityCount, arena.getEntityCapacity());
        int clampedPlayers = Math.clamp(playerCount, 0, arena.getTargetCapacity());
        float safeCrowdRadius = Float.isFinite(crowdRadius) && crowdRadius > 0.25f ? crowdRadius : 3.5f;
        int safeMild = Math.max(1, mildCrowdThreshold);
        int safeSevere = Math.max(safeMild, severeCrowdThreshold);
        int safeChunkLimit = Math.max(4, chunkMobLimit);
        float safeWakeRange = Float.isFinite(villagerWakeRange) && villagerWakeRange >= 0f ? villagerWakeRange : 8.0f;
        int safeVillagerCrowd = Math.max(1, villagerCrowdThreshold);
        int safeState = Math.clamp(brainStateCode, BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);

        return RustNative.nativeEvaluateFarmThrottling(
            clampedEntities,
            arena.xBuf,
            arena.yBuf,
            arena.zBuf,
            arena.flagsBuf,
            clampedPlayers,
            arena.targetXBuf,
            arena.targetYBuf,
            arena.targetZBuf,
            safeCrowdRadius,
            safeMild,
            safeSevere,
            safeChunkLimit,
            safeWakeRange,
            safeVillagerCrowd,
            safeState,
            arena.outIntABuf,
            arena.outIntBBuf
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
        ensureLoaded();
        if (playerCount <= 0) {
            return 0;
        }
        int clampedPlayers = Math.min(playerCount, arena.getEntityCapacity());
        int safeState = Math.clamp(brainStateCode, BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);
        float safeBaseSpeed = Float.isFinite(baseMaxElytraSpeed) && baseMaxElytraSpeed > 0.1f
            ? baseMaxElytraSpeed
            : 1.8f;

        return RustNative.nativeEvaluatePlayerSurges(
            clampedPlayers,
            arena.xBuf,
            arena.yBuf,
            arena.zBuf,
            arena.vxBuf,
            arena.vyBuf,
            arena.vzBuf,
            arena.flagsBuf,
            safeState,
            safeBaseSpeed,
            arena.outIntABuf,
            arena.outXBuf,
            arena.outYBuf,
            arena.outZBuf
        );
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
        ensureLoaded();
        if (width <= 0 || depth <= 0) {
            return 0;
        }
        if (costGrid == null || outDist == null || outDirX == null || outDirZ == null) {
            throw new IllegalArgumentException("Flow-field ByteBuffers must not be null.");
        }
        if (!costGrid.isDirect() || !outDist.isDirect() || !outDirX.isDirect() || !outDirZ.isDirect()) {
            throw new IllegalArgumentException("Flow-field ByteBuffers must be direct (ByteBuffer.allocateDirect).");
        }
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

        return RustNative.nativeComputeFlowField(
            width,
            depth,
            costGrid,
            goalX,
            goalZ,
            outDist,
            outDirX,
            outDirZ
        );
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
        ensureLoaded();
        if (width <= 0 || depth <= 0) {
            return 0;
        }
        if (outHeights == null || !outHeights.isDirect()) {
            throw new IllegalArgumentException("outHeights must be a non-null direct ByteBuffer.");
        }
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

        return RustNative.nativeGenerateNoise2D(
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
            outHeights
        );
    }

    /**
     * Evaluates per-player mob visibility (distance, vertical delta, rear-FOV cone, and per-player
     * Top-K budget) in parallel zero-copy Rust and emits only state-transition deltas compared
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
        ensureLoaded();
        if (mobCount <= 0 || playerCount <= 0) {
            return 0;
        }

        requireDirectBuffer(mobXs, "mobXs");
        requireDirectBuffer(mobYs, "mobYs");
        requireDirectBuffer(mobZs, "mobZs");
        requireDirectBuffer(mobFlags, "mobFlags");
        requireDirectBuffer(playerXs, "playerXs");
        requireDirectBuffer(playerYs, "playerYs");
        requireDirectBuffer(playerZs, "playerZs");
        requireDirectBuffer(playerLookXs, "playerLookXs");
        requireDirectBuffer(playerLookYs, "playerLookYs");
        requireDirectBuffer(playerLookZs, "playerLookZs");
        requireDirectBuffer(visibilityBitset, "visibilityBitset");
        requireDirectBuffer(outDeltaPlayerIdx, "outDeltaPlayerIdx");
        requireDirectBuffer(outDeltaMobIdx, "outDeltaMobIdx");
        requireDirectBuffer(outDeltaAction, "outDeltaAction");
        requireDirectBuffer(outStats, "outStats");

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

        return RustNative.nativeComputeMobTracking(
            mobCount,
            mobXs,
            mobYs,
            mobZs,
            mobFlags,
            playerCount,
            playerXs,
            playerYs,
            playerZs,
            playerLookXs,
            playerLookYs,
            playerLookZs,
            safeMaxTrack,
            safeNear,
            safeVert,
            safeRearRange,
            safeRearDot,
            safeMaxMobs,
            safeState,
            visibilityBitset,
            safeMaxDeltas,
            outDeltaPlayerIdx,
            outDeltaMobIdx,
            outDeltaAction,
            outStats
        );
    }

    /**
     * Evaluates per-chunk Redstone & Hopper activity rates and thermal heat scores in zero-copy
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
        ensureLoaded();
        if (chunkCount <= 0) {
            return 0;
        }

        requireDirectBuffer(chunkXs, "chunkXs");
        requireDirectBuffer(chunkZs, "chunkZs");
        requireDirectBuffer(redstonePerSec, "redstonePerSec");
        requireDirectBuffer(hopperPerSec, "hopperPerSec");
        requireDirectBuffer(prevHeat, "prevHeat");
        requireDirectBuffer(outActions, "outActions");
        requireDirectBuffer(outNewHeat, "outNewHeat");
        requireDirectBuffer(outThrottleRatio, "outThrottleRatio");
        requireDirectBuffer(outTopIndices, "outTopIndices");

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

        return RustNative.nativeEvaluateRedstoneChunks(
            chunkCount,
            chunkXs,
            chunkZs,
            redstonePerSec,
            hopperPerSec,
            prevHeat,
            safeRedstoneLimit,
            safeHopperLimit,
            safeState,
            outActions,
            outNewHeat,
            outThrottleRatio,
            safeMaxTop,
            outTopIndices
        );
    }

    /**
     * Computes a parallel zero-copy 3D arena/region block diff in Rust (YZX layout:
     * {@code idx = (y * depth + z) * width + x}).
     *
     * Returns the total number of differing blocks (which may exceed {@code maxDiffs} if truncated).
     * When {@code maxDiffs == 0}, runs in count-only mode and output buffers may be empty direct buffers.
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
        ensureLoaded();
        if (width <= 0 || height <= 0 || depth <= 0) {
            return 0;
        }
        long volume = (long) width * (long) height * (long) depth;
        if (volume > 16_777_216L) {
            throw new IllegalArgumentException("Block diff volume exceeds 16,777,216 block safety limit.");
        }
        requireDirectBuffer(currentBlocks, "currentBlocks");
        requireDirectBuffer(targetBlocks, "targetBlocks");
        long blockBytes = volume * Integer.BYTES;
        if (currentBlocks.capacity() < blockBytes || targetBlocks.capacity() < blockBytes) {
            throw new IllegalArgumentException("Block buffers are smaller than width * height * depth * 4 bytes.");
        }

        int safeMaxDiffs = Math.max(0, maxDiffs);
        requireDirectBuffer(outRelX, "outRelX");
        requireDirectBuffer(outRelY, "outRelY");
        requireDirectBuffer(outRelZ, "outRelZ");
        requireDirectBuffer(outNewBlockId, "outNewBlockId");
        long diffBytes = (long) safeMaxDiffs * Integer.BYTES;
        if (outRelX.capacity() < diffBytes
            || outRelY.capacity() < diffBytes
            || outRelZ.capacity() < diffBytes
            || outNewBlockId.capacity() < diffBytes) {
            throw new IllegalArgumentException("Block diff output buffers are smaller than maxDiffs * 4 bytes.");
        }

        return RustNative.nativeComputeBlockDiff(
            width,
            height,
            depth,
            currentBlocks,
            targetBlocks,
            safeMaxDiffs,
            outRelX,
            outRelY,
            outRelZ,
            outNewBlockId
        );
    }

    private static void requireDirectBuffer(ByteBuffer buf, String name) {
        if (buf == null || !buf.isDirect()) {
            throw new IllegalArgumentException(name + " must be a non-null direct ByteBuffer.");
        }
    }

    private void ensureLoaded() {
        if (!NativeLoader.isLoaded()) {
            throw new IllegalStateException("Spica Rust native kutuphanesi yuklu degil.");
        }
    }
}
