package net.spicanetwork.rustnative.api;

import net.spicanetwork.rustnative.brain.BrainDirective;
import net.spicanetwork.rustnative.nativebridge.EntityBatchArena;
import net.spicanetwork.rustnative.nativebridge.NativeLoader;
import net.spicanetwork.rustnative.nativebridge.RustNative;

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

    private void ensureLoaded() {
        if (!NativeLoader.isLoaded()) {
            throw new IllegalStateException("Spica Rust native kutuphanesi yuklu degil.");
        }
    }
}
