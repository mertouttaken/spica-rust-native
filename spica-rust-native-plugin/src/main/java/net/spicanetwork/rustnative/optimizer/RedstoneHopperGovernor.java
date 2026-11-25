package net.spicanetwork.rustnative.optimizer;

import net.spicanetwork.rustnative.api.SpicaRustAPI;
import net.spicanetwork.rustnative.brain.BrainDirective;
import net.spicanetwork.rustnative.nativebridge.RustNative;
import org.bukkit.Bukkit;
import org.bukkit.Material;
import org.bukkit.Tag;
import org.bukkit.World;
import org.bukkit.block.Block;
import org.bukkit.configuration.file.FileConfiguration;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.block.BlockRedstoneEvent;
import org.bukkit.event.inventory.InventoryMoveItemEvent;
import org.bukkit.event.world.WorldUnloadEvent;
import org.bukkit.inventory.BlockInventoryHolder;
import org.bukkit.inventory.Inventory;
import org.bukkit.inventory.InventoryHolder;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.FloatBuffer;
import java.nio.IntBuffer;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.Iterator;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.UUID;
import java.util.logging.Logger;

/**
 * Live Rust-powered Redstone & Hopper Lag-Machine Governor.
 *
 * Tracks per-chunk Redstone updates and Hopper/Block-Inventory item transfers with zero-allocation
 * event handlers on the Purpur 1.21.4 Main Thread, evaluates chunk thermal heat and throttling
 * actions once per second in a single zero-copy Rust call ({@link RustNative#nativeEvaluateRedstoneChunks}),
 * and applies smooth deterministic fractional throttling without breaking blocks or interfering
 * with player-operated triggers (buttons, levers, pressure plates, tripwire hooks).
 */
public final class RedstoneHopperGovernor implements Listener {

    private static final int MAX_TOP_CHUNKS = 8;
    private static final int INITIAL_CHUNK_CAPACITY = 256;
    private static final int MAX_DROP_THRESHOLD_256 = 245;
    private static final long LOCKDOWN_ALERT_COOLDOWN_MS = 30_000L;

    private final SpicaRustAPI api;
    private final Logger logger;

    // Per-world chunk activity tables: world UUID -> packed (cx, cz) -> mutable ChunkActivity
    private final Map<UUID, HashMap<Long, ChunkActivity>> worldActivities = new HashMap<>();

    // Reusable per-tick evaluation scratch lists (Main Thread only)
    private final List<ChunkActivity> evalScratch = new ArrayList<>(INITIAL_CHUNK_CAPACITY);
    private final List<String> evalWorldNames = new ArrayList<>(INITIAL_CHUNK_CAPACITY);

    // Dedicated auto-growing off-heap DirectByteBuffers (ByteOrder.nativeOrder())
    private int chunkCapacity;
    private ByteBuffer chunkXBuf;
    private ByteBuffer chunkZBuf;
    private ByteBuffer redstoneRateBuf;
    private ByteBuffer hopperRateBuf;
    private ByteBuffer prevHeatBuf;
    private ByteBuffer outActionsBuf;
    private ByteBuffer outNewHeatBuf;
    private ByteBuffer outThrottleRatioBuf;

    private IntBuffer chunkX;
    private IntBuffer chunkZ;
    private IntBuffer redstoneRate;
    private IntBuffer hopperRate;
    private FloatBuffer prevHeat;
    private IntBuffer outActions;
    private FloatBuffer outNewHeat;
    private FloatBuffer outThrottleRatio;

    private final ByteBuffer outTopIndicesBuf;
    private final IntBuffer outTopIndices;

    // Configuration
    private boolean enabled = true;
    private boolean monitorOnly = true;
    private int redstoneLimitPerSec = 320;
    private int hopperLimitPerSec = 240;
    private boolean protectPlayerTriggers = true;
    private boolean alertAdminsOnLockdown = true;

    // Monotonic elapsed-time normalization for accurate per-second rates under TPS drops (Y4)
    private long lastEvalNanos = System.nanoTime();

    // Live telemetry
    private long lastRustNanos = 0L;
    private long lastTotalNanos = 0L;
    private int lastActiveChunkCount = 0;
    private int lastThrottledChunkCount = 0;
    private long redstoneBlockedTotal = 0L;
    private long hopperBlockedTotal = 0L;
    private int lastBrainStateCode = BrainDirective.STATE_GREEN;
    private List<HotRedstoneChunk> hotChunks = List.of();

    public RedstoneHopperGovernor(SpicaRustAPI api, Logger logger) {
        this.api = api;
        this.logger = logger;

        allocateChunkBuffers(INITIAL_CHUNK_CAPACITY);
        this.outTopIndicesBuf = allocNative(MAX_TOP_CHUNKS * Integer.BYTES);
        this.outTopIndices = outTopIndicesBuf.asIntBuffer();
    }

    public void reloadConfig(FileConfiguration cfg) {
        this.enabled = cfg.getBoolean("redstone-governor.enabled", true);
        this.monitorOnly = cfg.getBoolean("redstone-governor.monitor-only", true);
        this.redstoneLimitPerSec = Math.clamp(
            cfg.getInt("redstone-governor.redstone-limit-per-sec", 320),
            16,
            100_000
        );
        this.hopperLimitPerSec = Math.clamp(
            cfg.getInt("redstone-governor.hopper-limit-per-sec", 240),
            16,
            100_000
        );
        this.protectPlayerTriggers = cfg.getBoolean("redstone-governor.protect-player-triggers", true);
        this.alertAdminsOnLockdown = cfg.getBoolean("redstone-governor.alert-admins-on-lockdown", true);
        this.lastEvalNanos = System.nanoTime();
    }

    @EventHandler(priority = EventPriority.HIGH)
    public void onBlockRedstone(BlockRedstoneEvent event) {
        if (!enabled || event.getOldCurrent() == event.getNewCurrent()) {
            return;
        }

        Block block = event.getBlock();
        if (protectPlayerTriggers) {
            Material type = block.getType();
            if (Tag.BUTTONS.isTagged(type)
                || Tag.PRESSURE_PLATES.isTagged(type)
                || type == Material.LEVER
                || type == Material.TRIPWIRE_HOOK) {
                return;
            }
        }

        ChunkActivity entry = getOrCreateActivity(block.getWorld(), block.getX() >> 4, block.getZ() >> 4);
        entry.redstoneEvents++;

        if (!monitorOnly && (entry.action & RustNative.REDSTONE_ACTION_THROTTLE_REDSTONE) != 0) {
            int slot = (entry.redstoneDropCounter++ * 97) & 255;
            if (slot < entry.dropThreshold256) {
                event.setNewCurrent(event.getOldCurrent());
                redstoneBlockedTotal++;
            }
        }
    }

    @EventHandler(priority = EventPriority.HIGH, ignoreCancelled = true)
    public void onInventoryMoveItem(InventoryMoveItemEvent event) {
        if (!enabled) {
            return;
        }

        Inventory initiator = event.getInitiator();
        if (initiator == null) {
            return;
        }

        // Paper 1.21.4 getHolder(false) avoids allocating a BlockState snapshot on every hopper tick
        InventoryHolder holder = initiator.getHolder(false);
        if (!(holder instanceof BlockInventoryHolder bih)) {
            return;
        }

        Block block = bih.getBlock();
        if (block == null) {
            return;
        }

        ChunkActivity entry = getOrCreateActivity(block.getWorld(), block.getX() >> 4, block.getZ() >> 4);
        entry.hopperMoves++;

        if (!monitorOnly && (entry.action & RustNative.REDSTONE_ACTION_THROTTLE_HOPPERS) != 0) {
            int slot = (entry.hopperDropCounter++ * 97) & 255;
            if (slot < entry.dropThreshold256) {
                event.setCancelled(true);
                hopperBlockedTotal++;
            }
        }
    }

    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onWorldUnload(WorldUnloadEvent event) {
        worldActivities.remove(event.getWorld().getUID());
    }

    /**
     * 1-second Rust evaluation tick running on the Main Thread every 20 ticks.
     */
    public void tick(int brainStateCode) {
        int safeBrainState = Math.clamp(brainStateCode, BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);
        this.lastBrainStateCode = safeBrainState;

        if (!enabled || !api.isNativeLoaded()) {
            this.lastActiveChunkCount = 0;
            this.lastThrottledChunkCount = 0;
            this.lastRustNanos = 0L;
            this.lastTotalNanos = 0L;
            this.hotChunks = List.of();
            return;
        }

        long startNanos = System.nanoTime();
        double elapsedSecs = Math.clamp((startNanos - lastEvalNanos) / 1_000_000_000.0, 0.25, 30.0);
        this.lastEvalNanos = startNanos;
        long nowMs = System.currentTimeMillis();

        evalScratch.clear();
        evalWorldNames.clear();

        Iterator<Map.Entry<UUID, HashMap<Long, ChunkActivity>>> worldIt = worldActivities.entrySet().iterator();
        while (worldIt.hasNext()) {
            Map.Entry<UUID, HashMap<Long, ChunkActivity>> worldEntry = worldIt.next();
            World world = Bukkit.getWorld(worldEntry.getKey());
            if (world == null) {
                worldIt.remove();
                continue;
            }
            HashMap<Long, ChunkActivity> chunkMap = worldEntry.getValue();
            if (chunkMap.isEmpty()) {
                worldIt.remove();
                continue;
            }
            String worldName = world.getName();
            for (ChunkActivity activity : chunkMap.values()) {
                evalScratch.add(activity);
                evalWorldNames.add(worldName);
            }
        }

        int chunkCount = evalScratch.size();
        if (chunkCount == 0) {
            this.lastActiveChunkCount = 0;
            this.lastThrottledChunkCount = 0;
            this.hotChunks = List.of();
            this.lastRustNanos = 0L;
            this.lastTotalNanos = System.nanoTime() - startNanos;
            return;
        }

        ensureChunkCapacity(chunkCount);

        for (int i = 0; i < chunkCount; i++) {
            ChunkActivity entry = evalScratch.get(i);
            int rsPerSec = (int) Math.min(Integer.MAX_VALUE, Math.round(entry.redstoneEvents / elapsedSecs));
            int hopPerSec = (int) Math.min(Integer.MAX_VALUE, Math.round(entry.hopperMoves / elapsedSecs));
            chunkX.put(i, entry.cx);
            chunkZ.put(i, entry.cz);
            redstoneRate.put(i, rsPerSec);
            hopperRate.put(i, hopPerSec);
            prevHeat.put(i, entry.heat);
        }

        long rustStart = System.nanoTime();
        int nativeThrottled = api.evaluateRedstoneChunks(
            chunkCount,
            chunkXBuf,
            chunkZBuf,
            redstoneRateBuf,
            hopperRateBuf,
            prevHeatBuf,
            redstoneLimitPerSec,
            hopperLimitPerSec,
            safeBrainState,
            outActionsBuf,
            outNewHeatBuf,
            outThrottleRatioBuf,
            MAX_TOP_CHUNKS,
            outTopIndicesBuf
        );
        long rustNanos = System.nanoTime() - rustStart;

        if (nativeThrottled < 0) {
            for (int i = 0; i < chunkCount; i++) {
                ChunkActivity entry = evalScratch.get(i);
                entry.action = RustNative.REDSTONE_ACTION_NORMAL;
                entry.dropThreshold256 = 0;
            }
            resetWindowAndPrune();
            this.lastActiveChunkCount = chunkCount;
            this.lastThrottledChunkCount = 0;
            this.hotChunks = List.of();
            this.lastRustNanos = rustNanos;
            this.lastTotalNanos = System.nanoTime() - startNanos;
            return;
        }

        int throttledCount = 0;
        for (int i = 0; i < chunkCount; i++) {
            ChunkActivity entry = evalScratch.get(i);
            int newAction = Math.clamp(
                outActions.get(i),
                RustNative.REDSTONE_ACTION_NORMAL,
                RustNative.REDSTONE_ACTION_LOCKDOWN
            );

            float newHeat = outNewHeat.get(i);
            if (!Float.isFinite(newHeat) || newHeat < 0.0f) {
                newHeat = 0.0f;
            }

            float throttleRatio = outThrottleRatio.get(i);
            float safeRatio = Float.isFinite(throttleRatio) ? Math.clamp(throttleRatio, 0.0f, 1.0f) : 0.0f;
            int newDropThreshold = Math.clamp(Math.round(safeRatio * 256.0f), 0, MAX_DROP_THRESHOLD_256);

            entry.action = newAction;
            entry.heat = newHeat;
            entry.dropThreshold256 = newDropThreshold;

            if (!monitorOnly && newAction != RustNative.REDSTONE_ACTION_NORMAL) {
                throttledCount++;
            }

            if (alertAdminsOnLockdown
                && logger != null
                && newAction == RustNative.REDSTONE_ACTION_LOCKDOWN
                && (nowMs - entry.lastAlertMs) >= LOCKDOWN_ALERT_COOLDOWN_MS) {
                entry.lastAlertMs = nowMs;
                int blockX = (entry.cx << 4) + 8;
                int blockZ = (entry.cz << 4) + 8;
                int throttlePct = monitorOnly ? 0 : Math.clamp(Math.round(safeRatio * 100.0f), 0, 100);
                String modePrefix = monitorOnly ? "[MONITOR] REDSTONE LOCKDOWN ADAYI" : "REDSTONE LOCKDOWN";
                logger.warning(String.format(
                    Locale.US,
                    "[SpicaRustNative] %s -> %s Chunk(%d, %d) [Blok %d, %d] | Redstone: %d/sn | Hopper: %d/sn | Heat: %.1f | Kisitlama: %d%%",
                    modePrefix,
                    evalWorldNames.get(i),
                    entry.cx,
                    entry.cz,
                    blockX,
                    blockZ,
                    redstoneRate.get(i),
                    hopperRate.get(i),
                    newHeat,
                    throttlePct
                ));
            }
        }

        List<HotRedstoneChunk> nextHotChunks = new ArrayList<>(MAX_TOP_CHUNKS);
        for (int t = 0; t < MAX_TOP_CHUNKS; t++) {
            int idx = outTopIndices.get(t);
            if (idx < 0 || idx >= chunkCount) {
                break;
            }
            ChunkActivity entry = evalScratch.get(idx);
            float ratio = outThrottleRatio.get(idx);
            float safeRatio = Float.isFinite(ratio) ? Math.clamp(ratio, 0.0f, 1.0f) : 0.0f;
            int throttlePercent = monitorOnly ? 0 : Math.clamp(Math.round(safeRatio * 100.0f), 0, 100);
            nextHotChunks.add(new HotRedstoneChunk(
                evalWorldNames.get(idx),
                entry.cx,
                entry.cz,
                redstoneRate.get(idx),
                hopperRate.get(idx),
                entry.heat,
                entry.action,
                throttlePercent
            ));
        }

        resetWindowAndPrune();

        this.hotChunks = List.copyOf(nextHotChunks);
        this.lastActiveChunkCount = chunkCount;
        this.lastThrottledChunkCount = throttledCount;
        this.lastRustNanos = rustNanos;
        this.lastTotalNanos = System.nanoTime() - startNanos;
    }

    /**
     * Clears all per-chunk throttle states cleanly on plugin reload or disable.
     *
     * @return number of chunks that were actively throttled prior to reset
     */
    public int restoreAll() {
        int restoredChunks = 0;
        for (HashMap<Long, ChunkActivity> chunkMap : worldActivities.values()) {
            for (ChunkActivity entry : chunkMap.values()) {
                if (entry.action != RustNative.REDSTONE_ACTION_NORMAL) {
                    restoredChunks++;
                }
            }
        }
        worldActivities.clear();
        evalScratch.clear();
        evalWorldNames.clear();
        this.hotChunks = List.of();
        this.lastActiveChunkCount = 0;
        this.lastThrottledChunkCount = 0;
        this.lastRustNanos = 0L;
        this.lastTotalNanos = 0L;
        return restoredChunks;
    }

    private void resetWindowAndPrune() {
        Iterator<Map.Entry<UUID, HashMap<Long, ChunkActivity>>> worldIt = worldActivities.entrySet().iterator();
        while (worldIt.hasNext()) {
            HashMap<Long, ChunkActivity> chunkMap = worldIt.next().getValue();
            Iterator<ChunkActivity> chunkIt = chunkMap.values().iterator();
            while (chunkIt.hasNext()) {
                ChunkActivity entry = chunkIt.next();
                entry.redstoneEvents = 0;
                entry.hopperMoves = 0;
                if (entry.heat < 1.0f && entry.action == RustNative.REDSTONE_ACTION_NORMAL) {
                    chunkIt.remove();
                }
            }
            if (chunkMap.isEmpty()) {
                worldIt.remove();
            }
        }
    }

    private ChunkActivity getOrCreateActivity(World world, int cx, int cz) {
        UUID worldId = world.getUID();
        HashMap<Long, ChunkActivity> chunkMap = worldActivities.get(worldId);
        if (chunkMap == null) {
            chunkMap = new HashMap<>(64);
            worldActivities.put(worldId, chunkMap);
        }
        long key = packChunkKey(cx, cz);
        ChunkActivity entry = chunkMap.get(key);
        if (entry == null) {
            entry = new ChunkActivity(cx, cz);
            chunkMap.put(key, entry);
        }
        return entry;
    }

    private static long packChunkKey(int cx, int cz) {
        return (((long) cx) << 32) | (cz & 0xFFFFFFFFL);
    }

    private void ensureChunkCapacity(int needed) {
        if (needed <= chunkCapacity) {
            return;
        }
        int next = chunkCapacity;
        while (next < needed) {
            next = Math.max(next * 2, needed);
        }
        allocateChunkBuffers(next);
    }

    private void allocateChunkBuffers(int cap) {
        this.chunkCapacity = cap;
        int bytes = cap * Integer.BYTES;

        this.chunkXBuf = allocNative(bytes);
        this.chunkZBuf = allocNative(bytes);
        this.redstoneRateBuf = allocNative(bytes);
        this.hopperRateBuf = allocNative(bytes);
        this.prevHeatBuf = allocNative(bytes);
        this.outActionsBuf = allocNative(bytes);
        this.outNewHeatBuf = allocNative(bytes);
        this.outThrottleRatioBuf = allocNative(bytes);

        this.chunkX = chunkXBuf.asIntBuffer();
        this.chunkZ = chunkZBuf.asIntBuffer();
        this.redstoneRate = redstoneRateBuf.asIntBuffer();
        this.hopperRate = hopperRateBuf.asIntBuffer();
        this.prevHeat = prevHeatBuf.asFloatBuffer();
        this.outActions = outActionsBuf.asIntBuffer();
        this.outNewHeat = outNewHeatBuf.asFloatBuffer();
        this.outThrottleRatio = outThrottleRatioBuf.asFloatBuffer();
    }

    private static ByteBuffer allocNative(int bytes) {
        return ByteBuffer.allocateDirect(bytes).order(ByteOrder.nativeOrder());
    }

    private static float limitMultiplierForState(int stateCode) {
        return switch (stateCode) {
            case BrainDirective.STATE_GREEN -> 1.00f;
            case BrainDirective.STATE_YELLOW -> 0.85f;
            case BrainDirective.STATE_ORANGE -> 0.70f;
            case BrainDirective.STATE_RED -> 0.55f;
            case BrainDirective.STATE_EMERGENCY -> 0.40f;
            default -> 1.00f;
        };
    }

    public boolean isEnabled() {
        return enabled;
    }

    public boolean isMonitorOnly() {
        return monitorOnly;
    }

    public double getLastRustMs() {
        return lastRustNanos / 1_000_000.0;
    }

    public double getLastTotalMs() {
        return lastTotalNanos / 1_000_000.0;
    }

    public int getActiveChunkCount() {
        return lastActiveChunkCount;
    }

    public int getThrottledChunkCount() {
        return lastThrottledChunkCount;
    }

    public long getRedstoneBlockedTotal() {
        return redstoneBlockedTotal;
    }

    public long getHopperBlockedTotal() {
        return hopperBlockedTotal;
    }

    public long getTotalOpsBlocked() {
        return redstoneBlockedTotal + hopperBlockedTotal;
    }

    public List<HotRedstoneChunk> getHotChunks() {
        return hotChunks;
    }

    public int getEffectiveRedstoneLimit() {
        return Math.max(16, Math.round(redstoneLimitPerSec * limitMultiplierForState(lastBrainStateCode)));
    }

    public int getEffectiveHopperLimit() {
        return Math.max(16, Math.round(hopperLimitPerSec * limitMultiplierForState(lastBrainStateCode)));
    }

    public int getRedstoneLimitPerSec() {
        return redstoneLimitPerSec;
    }

    public int getHopperLimitPerSec() {
        return hopperLimitPerSec;
    }

    public boolean isProtectPlayerTriggers() {
        return protectPlayerTriggers;
    }

    public boolean isAlertAdminsOnLockdown() {
        return alertAdminsOnLockdown;
    }

    public int getLastBrainStateCode() {
        return lastBrainStateCode;
    }

    private static final class ChunkActivity {
        int cx;
        int cz;
        int redstoneEvents;
        int hopperMoves;
        float heat;
        int action;
        int dropThreshold256;
        int redstoneDropCounter;
        int hopperDropCounter;
        long lastAlertMs;

        ChunkActivity(int cx, int cz) {
            this.cx = cx;
            this.cz = cz;
        }
    }

    public record HotRedstoneChunk(
        String worldName,
        int chunkX,
        int chunkZ,
        int redstonePerSec,
        int hopperPerSec,
        float heat,
        int action,
        int throttlePercent
    ) {
        public int blockX() {
            return (chunkX << 4) + 8;
        }

        public int blockZ() {
            return (chunkZ << 4) + 8;
        }

        public String actionName() {
            return switch (action) {
                case RustNative.REDSTONE_ACTION_NORMAL -> "NORMAL";
                case RustNative.REDSTONE_ACTION_THROTTLE_REDSTONE -> "THROTTLE_RS";
                case RustNative.REDSTONE_ACTION_THROTTLE_HOPPERS -> "THROTTLE_HOP";
                case RustNative.REDSTONE_ACTION_LOCKDOWN -> "LOCKDOWN";
                default -> "UNKNOWN";
            };
        }
    }
}
