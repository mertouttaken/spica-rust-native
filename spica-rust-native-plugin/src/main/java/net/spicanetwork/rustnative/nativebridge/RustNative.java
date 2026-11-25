package net.spicanetwork.rustnative.nativebridge;

import java.nio.ByteBuffer;

/**
 * Low-level JNI declarations binding directly to `spica-native` Rust kernels.
 *
 * All buffer parameters MUST be direct ByteBuffers (`ByteBuffer.allocateDirect`)
 * ordered with `ByteOrder.nativeOrder()`. Zero copying happens across the JNI boundary.
 */
public final class RustNative {

    public static final int LOD_FULL = 0;
    public static final int LOD_REDUCED = 1;
    public static final int LOD_HIBERNATE = 2;

    // Farm & Villager Governor action bitmask values
    public static final int ACTION_NORMAL = 0;
    public static final int ACTION_DISABLE_COLLISION = 1;
    public static final int ACTION_THROTTLE_AI = 2;
    public static final int ACTION_FARM_SUPPRESS = 3;

    // Farm & Villager Governor per-entity input flags
    public static final int FLAG_PROTECTED = 1;
    public static final int FLAG_VILLAGER = 2;
    public static final int FLAG_TRAPPED_BOX = 4;
    public static final int FLAG_RECENT_INTERACT = 8;

    private RustNative() {
    }

    /**
     * Initialize the global Rust Rayon thread pool (0 = automatic balanced count).
     */
    public static native int nativeInitPool(int numThreads);

    /**
     * Returns version, OS/arch, and active Rayon worker count from Rust.
     */
    public static native String nativeGetInfo();

    /**
     * Batch Spatial-Hash Entity-Entity soft collision & neighbor density calculation.
     */
    public static native long nativeComputeCollisions(
        int count,
        ByteBuffer xs,
        ByteBuffer ys,
        ByteBuffer zs,
        ByteBuffer radii,
        float cellSize,
        float pushStrength,
        float maxPush,
        ByteBuffer outPushX,
        ByteBuffer outPushY,
        ByteBuffer outPushZ,
        ByteBuffer outNeighborCounts
    );

    /**
     * Batch Nearest-Target & AI LOD tier evaluation for N mobs against M targets/players.
     */
    public static native int nativeComputeAiTargets(
        int mobCount,
        ByteBuffer mobX,
        ByteBuffer mobY,
        ByteBuffer mobZ,
        int targetCount,
        ByteBuffer targetX,
        ByteBuffer targetY,
        ByteBuffer targetZ,
        float fullRange,
        float reducedRange,
        ByteBuffer outNearestIdx,
        ByteBuffer outDistSq,
        ByteBuffer outDirX,
        ByteBuffer outDirY,
        ByteBuffer outDirZ,
        ByteBuffer outLodTier
    );

    /**
     * Batch Boids / Flock AI steering (separation + alignment + cohesion + goal seek).
     */
    public static native int nativeComputeBoids(
        int count,
        ByteBuffer xs,
        ByteBuffer ys,
        ByteBuffer zs,
        ByteBuffer vxs,
        ByteBuffer vys,
        ByteBuffer vzs,
        float goalX,
        float goalY,
        float goalZ,
        float perceptionRadius,
        float sepWeight,
        float alignWeight,
        float cohesionWeight,
        float goalWeight,
        float maxForce,
        ByteBuffer outAx,
        ByteBuffer outAy,
        ByteBuffer outAz
    );

    /**
     * Batch AOE / Radius proximity search over SoA coordinates.
     */
    public static native int nativeQueryRadius(
        int count,
        ByteBuffer xs,
        ByteBuffer ys,
        ByteBuffer zs,
        float qx,
        float qy,
        float qz,
        float radius,
        int maxResults,
        ByteBuffer outIndices
    );

    /**
     * O(N) Chunk Heatmap analysis returning the top-K densest/most crowded chunks.
     */
    public static native int nativeAnalyzeChunks(
        int entityCount,
        ByteBuffer xs,
        ByteBuffer zs,
        ByteBuffer neighborCounts,
        int maxHotChunks,
        ByteBuffer outChunkX,
        ByteBuffer outChunkZ,
        ByteBuffer outEntityCount,
        ByteBuffer outCrowdedCount
    );

    /**
     * Spatial-Hash item merge planner outputting safe (targetIdx, sourceIdx) stack-merge pairs.
     */
    public static native int nativePlanItemMerges(
        int itemCount,
        ByteBuffer xs,
        ByteBuffer ys,
        ByteBuffer zs,
        ByteBuffer typeIds,
        ByteBuffer stackAmounts,
        int maxStackSize,
        float mergeRadius,
        int maxPairs,
        ByteBuffer outTargetIdx,
        ByteBuffer outSourceIdx
    );

    /**
     * In-process Rust Brain Governor evaluating live server load into concrete Purpur actuation parameters.
     */
    public static native int nativeEvaluateGovernor(
        float currentMspt,
        float ewmaMspt,
        int loadedEntities,
        int onlinePlayers,
        float baseFullRange,
        float baseHibernateRange,
        int minViewDist,
        int maxViewDist,
        int minSimDist,
        int maxSimDist,
        ByteBuffer outIntState,
        ByteBuffer outFloatParams
    );

    /**
     * Spatial-Hash Mob Farm & Trading-Hall Villager throttling evaluator.
     *
     * Outputs per-entity action bitmasks ({@code ACTION_*}) into {@code outActions}
     * and neighbor counts into {@code outNeighborCounts}.
     */
    public static native int nativeEvaluateFarmThrottling(
        int entityCount,
        ByteBuffer xs,
        ByteBuffer ys,
        ByteBuffer zs,
        ByteBuffer entityFlags,
        int playerCount,
        ByteBuffer playerXs,
        ByteBuffer playerYs,
        ByteBuffer playerZs,
        float crowdRadius,
        int mildCrowdThreshold,
        int severeCrowdThreshold,
        int chunkMobLimit,
        float villagerWakeRange,
        int villagerCrowdThreshold,
        int brainStateCode,
        ByteBuffer outActions,
        ByteBuffer outNeighborCounts
    );
}
