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

    // Elytra / Chunk-Load Surge Guard actions
    public static final int SURGE_OK = 0;
    public static final int SURGE_WARN = 1;
    public static final int SURGE_CLAMP_VELOCITY = 2;

    // Mob Visibility & Tracking Optimizer per-mob input flags
    public static final int TRACK_FLAG_ALWAYS_VISIBLE = 1;
    public static final int TRACK_FLAG_HOSTILE = 2;
    public static final int TRACK_FLAG_PASSIVE = 4;

    // Mob Visibility & Tracking Optimizer delta actions
    public static final int TRACK_ACTION_HIDE = 0;
    public static final int TRACK_ACTION_SHOW = 1;

    // Redstone & Hopper Lag-Machine Governor action bitmask values
    public static final int REDSTONE_ACTION_NORMAL = 0;
    public static final int REDSTONE_ACTION_THROTTLE_REDSTONE = 1;
    public static final int REDSTONE_ACTION_THROTTLE_HOPPERS = 2;
    public static final int REDSTONE_ACTION_LOCKDOWN = 3;

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

    /**
     * Evaluates gliding players for Elytra / chunk-load velocity surges and outputs
     * per-player {@code SURGE_*} actions and smoothly clamped velocity vectors.
     */
    public static native int nativeEvaluatePlayerSurges(
        int playerCount,
        ByteBuffer px,
        ByteBuffer py,
        ByteBuffer pz,
        ByteBuffer vx,
        ByteBuffer vy,
        ByteBuffer vz,
        ByteBuffer isGliding,
        int brainStateCode,
        float baseMaxElytraSpeed,
        ByteBuffer outActions,
        ByteBuffer outClampedVx,
        ByteBuffer outClampedVy,
        ByteBuffer outClampedVz
    );

    /**
     * Computes a 2D Dijkstra integration distance field and normalized steering flow-field
     * around obstacles toward {@code (goalX, goalZ)}.
     */
    public static native int nativeComputeFlowField(
        int width,
        int depth,
        ByteBuffer costGrid,
        int goalX,
        int goalZ,
        ByteBuffer outDist,
        ByteBuffer outDirX,
        ByteBuffer outDirZ
    );

    /**
     * Generates a 2D multi-octave Fractal Brownian Motion (FBM) world-generation noise grid
     * in parallel via Rust Rayon.
     */
    public static native int nativeGenerateNoise2D(
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
    );

    /**
     * Evaluates per-player mob visibility (distance, vertical delta, rear-FOV cone, and per-player
     * Top-K budget) in parallel zero-copy Rust and outputs only state-transition deltas
     * ({@code TRACK_ACTION_HIDE} / {@code TRACK_ACTION_SHOW}) against {@code visibilityBitset}.
     */
    public static native int nativeComputeMobTracking(
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
    );

    /**
     * Evaluates per-chunk Redstone & Hopper activity rates and thermal heat scores in zero-copy
     * Rust, returning per-chunk {@code REDSTONE_ACTION_*} bitmasks, updated heat scores,
     * fractional throttle ratios, and the indices of the hottest chunks into {@code outTopIndices}.
     */
    public static native int nativeEvaluateRedstoneChunks(
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
    );

    /**
     * Computes a parallel zero-copy 3D block diff between {@code currentBlocks} and {@code targetBlocks}
     * (int32 block-state IDs in YZX order: {@code idx = (y * depth + z) * width + x}), returning total
     * differing blocks and writing up to {@code maxDiffs} changed coordinates & target block IDs into
     * {@code outRelX}, {@code outRelY}, {@code outRelZ}, and {@code outNewBlockId}.
     */
    public static native int nativeComputeBlockDiff(
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
    );
}
