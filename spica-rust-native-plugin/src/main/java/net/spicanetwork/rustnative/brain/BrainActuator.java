package net.spicanetwork.rustnative.brain;

import net.spicanetwork.rustnative.api.SpicaRustAPI;
import net.spicanetwork.rustnative.nativebridge.EntityBatchArena;
import net.spicanetwork.rustnative.optimizer.EntityLodOptimizer;
import net.spicanetwork.rustnative.optimizer.FarmVillagerGovernor;
import net.spicanetwork.rustnative.optimizer.MobTrackingOptimizer;
import net.spicanetwork.rustnative.optimizer.RedstoneHopperGovernor;
import net.spicanetwork.rustnative.optimizer.SafeItemMergeHelper;
import net.spicanetwork.rustnative.optimizer.SurgeExplosionGuard;
import org.bukkit.Bukkit;
import org.bukkit.Location;
import org.bukkit.World;
import org.bukkit.configuration.file.FileConfiguration;
import org.bukkit.entity.Entity;
import org.bukkit.entity.Item;
import org.bukkit.entity.Player;
import org.bukkit.inventory.ItemStack;

import java.util.ArrayList;
import java.util.Collection;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.logging.Logger;

/**
 * The Purpur 1.21.4 Execution Body ("Actuator") driven by the Rust Brain.
 *
 * Every {@code brain.tick-interval} ticks (default: 20 ticks = 1s) on the Main Thread:
 * 1. Collects live Purpur metrics (MSPT, TPS, entities, hibernated mobs, players, loaded chunks, heap)
 * 2. Sends non-blocking telemetry to {@code spica-launcher} over IPC when connected
 * 3. Obtains the active {@link BrainDirective} (Launcher IPC or seamless in-process Rust Governor fallback)
 * 4. Safely applies dynamic AI LOD ranges, cooldown-guarded View/Simulation distance, and Rust-planned
 *    NBT-safe ground item stack merges.
 */
public final class BrainActuator {

    private static final long VIEW_DIST_COOLDOWN_MS = 15_000L;
    private static final long EMERGENCY_DROP_COOLDOWN_MS = 5_000L;
    private static final long IPC_DIRECTIVE_STALE_MS = 15_000L;
    private static final int MAX_ITEM_MERGE_PAIRS_PER_WORLD = 512;

    private final SpicaRustAPI api;
    private final EntityLodOptimizer optimizer;
    private final FarmVillagerGovernor farmGovernor;
    private final SurgeExplosionGuard surgeGuard;
    private final MobTrackingOptimizer mobTrackingOptimizer;
    private final RedstoneHopperGovernor redstoneGovernor;
    private final Logger logger;
    private final EntityBatchArena brainArena = new EntityBatchArena(4096, 64);
    private final List<Item> itemScratch = new ArrayList<>(1024);
    private final List<Entity> entityScratch = new ArrayList<>(4096);
    private final Map<UUID, int[]> originalWorldDistances = new HashMap<>();
    private final Map<UUID, int[]> lastSpicaWrittenDistances = new HashMap<>();

    private BrainIpcClient ipcClient;

    // Configuration
    private boolean brainEnabled = true;
    private boolean actuationEnabled = true;
    private boolean dynamicViewDistance = true;
    private boolean rustItemMerge = true;
    private int minViewDistance = 5;
    private int maxViewDistance = 10;
    private int minSimDistance = 4;
    private int maxSimDistance = 8;

    // Live state & telemetry
    private float localEwmaMspt = 0.0f;
    private long lastViewChangeMs = 0L;
    private int lastAppliedViewDistance = -1;
    private int lastAppliedSimDistance = -1;
    private long totalItemsMerged = 0L;
    private long lastItemsMergedThisTick = 0L;
    private BrainDirective lastDirective = null;
    private String lastBrainSource = "INITIALIZING";

    private double lastMspt = 0.0;
    private double lastTps = 20.0;
    private int lastLoadedEntities = 0;
    private int lastLoadedChunks = 0;
    private int lastOnlinePlayers = 0;
    private long lastHeapUsedMb = 0L;
    private long lastHeapMaxMb = 0L;

    public BrainActuator(
        SpicaRustAPI api,
        EntityLodOptimizer optimizer,
        FarmVillagerGovernor farmGovernor,
        BrainIpcClient ipcClient,
        Logger logger
    ) {
        this(api, optimizer, farmGovernor, null, null, null, ipcClient, logger);
    }

    public BrainActuator(
        SpicaRustAPI api,
        EntityLodOptimizer optimizer,
        FarmVillagerGovernor farmGovernor,
        SurgeExplosionGuard surgeGuard,
        BrainIpcClient ipcClient,
        Logger logger
    ) {
        this(api, optimizer, farmGovernor, surgeGuard, null, null, ipcClient, logger);
    }

    public BrainActuator(
        SpicaRustAPI api,
        EntityLodOptimizer optimizer,
        FarmVillagerGovernor farmGovernor,
        SurgeExplosionGuard surgeGuard,
        MobTrackingOptimizer mobTrackingOptimizer,
        BrainIpcClient ipcClient,
        Logger logger
    ) {
        this(api, optimizer, farmGovernor, surgeGuard, mobTrackingOptimizer, null, ipcClient, logger);
    }

    public BrainActuator(
        SpicaRustAPI api,
        EntityLodOptimizer optimizer,
        FarmVillagerGovernor farmGovernor,
        SurgeExplosionGuard surgeGuard,
        MobTrackingOptimizer mobTrackingOptimizer,
        RedstoneHopperGovernor redstoneGovernor,
        BrainIpcClient ipcClient,
        Logger logger
    ) {
        this.api = api;
        this.optimizer = optimizer;
        this.farmGovernor = farmGovernor;
        this.surgeGuard = surgeGuard;
        this.mobTrackingOptimizer = mobTrackingOptimizer;
        this.redstoneGovernor = redstoneGovernor;
        this.ipcClient = ipcClient;
        this.logger = logger;
    }

    public void setIpcClient(BrainIpcClient ipcClient) {
        this.ipcClient = ipcClient;
    }

    public void reloadConfig(FileConfiguration cfg) {
        this.brainEnabled = cfg.getBoolean("brain.enabled", true);
        this.actuationEnabled = cfg.getBoolean("brain.actuation.enabled", true);
        this.dynamicViewDistance = cfg.getBoolean("brain.actuation.dynamic-view-distance", true);
        this.rustItemMerge = cfg.getBoolean("brain.actuation.rust-item-merge", true);

        int rawMinView = cfg.getInt("brain.min-view-distance", 5);
        int rawMaxView = cfg.getInt("brain.max-view-distance", 10);
        int rawMinSim = cfg.getInt("brain.min-sim-distance", 4);
        int rawMaxSim = cfg.getInt("brain.max-sim-distance", 8);

        this.minViewDistance = Math.clamp(Math.min(rawMinView, rawMaxView), 2, 32);
        this.maxViewDistance = Math.clamp(Math.max(rawMinView, rawMaxView), this.minViewDistance, 32);
        this.minSimDistance = Math.clamp(Math.min(rawMinSim, rawMaxSim), 2, 32);
        this.maxSimDistance = Math.clamp(Math.max(rawMinSim, rawMaxSim), this.minSimDistance, 32);
    }

    /**
     * Runs on the Main Thread every {@code brain.tick-interval} ticks.
     */
    public void tick() {
        if (!brainEnabled || !api.isNativeLoaded()) {
            return;
        }

        // 1) Collect live Purpur 1.21.4 server telemetry safely
        double mspt = Bukkit.getAverageTickTime();
        if (!Double.isFinite(mspt) || mspt < 0.0) {
            mspt = 0.0;
        }

        double[] tpsSamples = Bukkit.getTPS();
        double tps = (tpsSamples != null && tpsSamples.length > 0 && Double.isFinite(tpsSamples[0]))
            ? Math.min(20.0, Math.max(0.0, tpsSamples[0]))
            : 20.0;

        int onlinePlayers = Bukkit.getOnlinePlayers().size();
        int loadedEntities = 0;
        int loadedChunks = 0;
        for (World world : Bukkit.getWorlds()) {
            loadedChunks += world.getLoadedChunks().length;
            loadedEntities += world.getEntities().size();
        }
        int hibernated = optimizer.getLastHibernatedMobs();
        int farmThrottled = farmGovernor != null ? farmGovernor.getLastFarmThrottled() : 0;
        int villagersThrottled = farmGovernor != null ? farmGovernor.getLastVillagersThrottled() : 0;
        long elytraClamps = surgeGuard != null ? surgeGuard.getElytraClampsTotal() : 0L;
        long explosionItemsMerged = surgeGuard != null ? surgeGuard.getExplosionItemsMergedTotal() : 0L;
        int trackedPairsHidden = mobTrackingOptimizer != null ? mobTrackingOptimizer.getLastTotalHiddenPairs() : 0;
        int trackingTransitionsPerSec = mobTrackingOptimizer != null ? mobTrackingOptimizer.getRollingTransitionsPerSec() : 0;
        int redstoneChunksThrottled = redstoneGovernor != null ? redstoneGovernor.getThrottledChunkCount() : 0;
        long redstoneOpsBlocked = redstoneGovernor != null ? redstoneGovernor.getTotalOpsBlocked() : 0L;

        Runtime runtime = Runtime.getRuntime();
        long heapUsedMb = Math.max(0L, (runtime.totalMemory() - runtime.freeMemory()) / (1024L * 1024L));
        long heapMaxMb = Math.max(1L, runtime.maxMemory() / (1024L * 1024L));

        this.lastMspt = mspt;
        this.lastTps = tps;
        this.lastLoadedEntities = loadedEntities;
        this.lastLoadedChunks = loadedChunks;
        this.lastOnlinePlayers = onlinePlayers;
        this.lastHeapUsedMb = heapUsedMb;
        this.lastHeapMaxMb = heapMaxMb;

        // 2) Submit telemetry to spica-launcher IPC (non-blocking single slot)
        BrainDirective directive = null;
        if (ipcClient != null && ipcClient.isConnected()) {
            ipcClient.submitTelemetry(
                mspt, tps, loadedEntities, hibernated, onlinePlayers, loadedChunks, heapUsedMb, heapMaxMb,
                farmThrottled, villagersThrottled, elytraClamps, explosionItemsMerged,
                trackedPairsHidden, trackingTransitionsPerSec,
                redstoneChunksThrottled, redstoneOpsBlocked
            );
            BrainDirective ipcDir = ipcClient.getLatestDirective();
            long lastDirTs = ipcClient.getLastDirectiveTimestampMs();
            long ipcAgeMs = System.currentTimeMillis() - lastDirTs;
            if (ipcDir != null && lastDirTs > 0L && ipcAgeMs <= IPC_DIRECTIVE_STALE_MS) {
                directive = ipcDir;
                if (Float.isFinite(ipcDir.ewmaMspt()) && ipcDir.ewmaMspt() > 0f) {
                    this.localEwmaMspt = ipcDir.ewmaMspt();
                }
                this.lastBrainSource = "LAUNCHER IPC";
            } else if (lastDirTs > 0L && ipcAgeMs > IPC_DIRECTIVE_STALE_MS) {
                // Y8: Force reconnect on stale/half-open IPC socket while falling back seamlessly
                ipcClient.forceReconnect();
            }
        }

        // Seamless fallback to in-process Rust Brain Governor when standalone or awaiting first IPC reply
        if (directive == null) {
            if (this.localEwmaMspt <= 0.0f) {
                this.localEwmaMspt = (float) mspt;
            }
            directive = api.evaluateGovernor(
                brainArena,
                (float) mspt,
                this.localEwmaMspt,
                loadedEntities,
                onlinePlayers,
                optimizer.getBaseFullAiRange(),
                optimizer.getBaseHibernateRange(),
                minViewDistance,
                maxViewDistance,
                minSimDistance,
                maxSimDistance
            );
            this.localEwmaMspt = directive.ewmaMspt();
            this.lastBrainSource = "IN-PROCESS RUST";
        }

        this.lastDirective = directive;
        this.lastItemsMergedThisTick = 0L;

        // 3) Actuate directive on Purpur 1.21.4
        if (!actuationEnabled) {
            return;
        }

        // 3a) Dynamic AI activation & hibernation ranges
        optimizer.setDynamicRanges(directive.fullAiRange(), directive.hibernateRange());

        // 3b) Cooldown-guarded dynamic View & Simulation Distance
        if (dynamicViewDistance) {
            applyViewAndSimDistance(directive);
        }

        // 3c) Rust Spatial-Hash NBT-safe ground item merging
        if (rustItemMerge && directive.itemMergeEnabled()) {
            long merged = executeRustItemMerge(directive.itemMergeRadius());
            this.lastItemsMergedThisTick = merged;
            this.totalItemsMerged += merged;
        }
    }

    private void applyViewAndSimDistance(BrainDirective directive) {
        int targetView = Math.clamp(directive.viewDistance(), minViewDistance, maxViewDistance);
        int targetSim = Math.clamp(directive.simDistance(), minSimDistance, maxSimDistance);
        targetView = Math.clamp(targetView, 2, 32);
        targetSim = Math.clamp(Math.min(targetSim, targetView), 2, 32);

        long now = System.currentTimeMillis();
        boolean isEmergencyDrop = directive.stateCode() >= BrainDirective.STATE_RED
            && ((lastAppliedViewDistance > 0 && targetView < lastAppliedViewDistance)
                || (lastAppliedSimDistance > 0 && targetSim < lastAppliedSimDistance));
        long requiredCooldown = isEmergencyDrop ? EMERGENCY_DROP_COOLDOWN_MS : VIEW_DIST_COOLDOWN_MS;

        if (lastViewChangeMs > 0L && (now - lastViewChangeMs) < requiredCooldown) {
            return;
        }

        boolean changedAnyWorld = false;
        for (World world : Bukkit.getWorlds()) {
            UUID worldId = world.getUID();
            int currentView = world.getViewDistance();
            int currentSim = world.getSimulationDistance();

            // Y6: Respect runtime external admin/plugin distance overrides — if the live value
            // differs from Spica's last written value, yield ownership of that axis instead of clobbering it.
            int[] lastWritten = lastSpicaWrittenDistances.get(worldId);
            boolean viewOwned = (lastWritten == null) || (lastWritten[0] == currentView);
            boolean simOwned = (lastWritten == null) || (lastWritten[1] == currentSim);

            if (!viewOwned && !simOwned) {
                lastWritten[0] = Integer.MIN_VALUE;
                lastWritten[1] = Integer.MIN_VALUE;
                continue;
            }

            int desiredView = viewOwned ? targetView : currentView;
            int desiredSim = simOwned ? Math.min(targetSim, desiredView) : currentSim;
            if (currentView == desiredView && currentSim == desiredSim) {
                if (lastWritten != null) {
                    if (!viewOwned) {
                        lastWritten[0] = Integer.MIN_VALUE;
                    }
                    if (!simOwned) {
                        lastWritten[1] = Integer.MIN_VALUE;
                    }
                }
                continue;
            }

            originalWorldDistances.putIfAbsent(worldId, new int[]{currentView, currentSim});

            try {
                int writtenView = viewOwned ? currentView : Integer.MIN_VALUE;
                int writtenSim = simOwned ? currentSim : Integer.MIN_VALUE;
                if (viewOwned && currentView != desiredView) {
                    world.setViewDistance(desiredView);
                    writtenView = desiredView;
                    changedAnyWorld = true;
                }
                if (simOwned && currentSim != desiredSim) {
                    world.setSimulationDistance(desiredSim);
                    writtenSim = desiredSim;
                    changedAnyWorld = true;
                }
                lastSpicaWrittenDistances.put(worldId, new int[]{writtenView, writtenSim});
            } catch (UnsupportedOperationException | IllegalArgumentException ex) {
                logger.warning(
                    "[SpicaRustNative] Dunya '" + world.getName()
                        + "' icin dinamik mesafe ayarlanamadi: " + ex.getMessage()
                );
            }
        }

        if (changedAnyWorld) {
            this.lastViewChangeMs = now;
            this.lastAppliedViewDistance = targetView;
            this.lastAppliedSimDistance = targetSim;
        } else if (lastAppliedViewDistance < 0) {
            this.lastAppliedViewDistance = targetView;
            this.lastAppliedSimDistance = targetSim;
        }
    }

    private long executeRustItemMerge(float mergeRadius) {
        float safeRadius = Float.isFinite(mergeRadius) && mergeRadius > 0.25f ? Math.min(mergeRadius, 12.0f) : 3.5f;
        long mergedEntities = 0L;

        for (World world : Bukkit.getWorlds()) {
            Collection<Item> groundItems = world.getEntitiesByClass(Item.class);
            if (groundItems.size() < 2) {
                continue;
            }

            itemScratch.clear();
            for (Item item : groundItems) {
                if (SafeItemMergeHelper.isEligibleCandidate(item)) {
                    itemScratch.add(item);
                }
            }

            if (itemScratch.size() >= 2) {
                mergedEntities += SafeItemMergeHelper.planAndApplySafeMerges(
                    api,
                    brainArena,
                    itemScratch,
                    safeRadius,
                    MAX_ITEM_MERGE_PAIRS_PER_WORLD
                );
            }
            itemScratch.clear();
        }

        return mergedEntities;
    }

    /**
     * Runs Rust Spatial-Hash collision density + O(N) Chunk Heatmap aggregation on a live world
     * and returns the top-K laggiest/densest chunks.
     */
    public ChunkHeatmapReport analyzeHotChunks(World world, int topK) {
        long start = System.nanoTime();
        int clampedTopK = Math.clamp(topK, 1, EntityBatchArena.MAX_HOT_CHUNKS);

        entityScratch.clear();
        for (Entity entity : world.getEntities()) {
            if (!entity.isValid() || entity.isDead() || entity instanceof Player) {
                continue;
            }
            entityScratch.add(entity);
        }

        int n = entityScratch.size();
        if (n == 0) {
            double totalMs = (System.nanoTime() - start) / 1_000_000.0;
            return new ChunkHeatmapReport(world.getName(), 0, 0.0, totalMs, List.of());
        }

        brainArena.ensureEntityCapacity(n);
        for (int i = 0; i < n; i++) {
            Entity entity = entityScratch.get(i);
            Location loc = entity.getLocation();
            float radius = (float) Math.max(0.35, entity.getBoundingBox().getWidthX() * 0.55);
            brainArena.setEntityPosition(i, (float) loc.getX(), (float) loc.getY(), (float) loc.getZ(), radius);
        }

        long rustStart = System.nanoTime();
        long colliding = api.computeCollisions(brainArena, n, 2.0f, 0.5f, 0.5f);
        if (colliding < 0L) {
            double rustMs = (System.nanoTime() - rustStart) / 1_000_000.0;
            entityScratch.clear();
            double totalMs = (System.nanoTime() - start) / 1_000_000.0;
            return new ChunkHeatmapReport(world.getName(), n, rustMs, totalMs, List.of());
        }
        int hotCount = api.analyzeChunks(brainArena, n, clampedTopK);
        double rustMs = (System.nanoTime() - rustStart) / 1_000_000.0;

        int safeCount = Math.clamp(hotCount, 0, clampedTopK);
        List<HotChunkEntry> entries = new ArrayList<>(safeCount);
        for (int i = 0; i < safeCount; i++) {
            entries.add(new HotChunkEntry(
                brainArena.chunkX.get(i),
                brainArena.chunkZ.get(i),
                brainArena.chunkCount.get(i),
                brainArena.chunkCrowded.get(i)
            ));
        }

        entityScratch.clear();
        double totalMs = (System.nanoTime() - start) / 1_000_000.0;
        return new ChunkHeatmapReport(world.getName(), n, rustMs, totalMs, entries);
    }

    /**
     * Restores dynamic AI ranges and any modified world view/simulation distances on disable/reload.
     */
    public void restoreDefaults() {
        optimizer.resetDynamicRanges();

        for (Map.Entry<UUID, int[]> entry : originalWorldDistances.entrySet()) {
            World world = Bukkit.getWorld(entry.getKey());
            if (world == null) {
                continue;
            }
            int[] orig = entry.getValue();
            int[] lastWritten = lastSpicaWrittenDistances.get(entry.getKey());
            try {
                // Y6: Only restore view/sim distance if Spica's last written value is still active
                // (never clobber manual admin or third-party plugin distance adjustments)
                if (lastWritten != null && world.getViewDistance() == lastWritten[0] && world.getViewDistance() != orig[0]) {
                    world.setViewDistance(orig[0]);
                }
                if (lastWritten != null && world.getSimulationDistance() == lastWritten[1] && world.getSimulationDistance() != orig[1]) {
                    world.setSimulationDistance(orig[1]);
                }
            } catch (UnsupportedOperationException | IllegalArgumentException ignored) {
                // Ignore during shutdown if world is unloading
            }
        }
        originalWorldDistances.clear();
        lastSpicaWrittenDistances.clear();
        lastAppliedViewDistance = -1;
        lastAppliedSimDistance = -1;
        lastViewChangeMs = 0L;
    }

    public boolean isBrainEnabled() {
        return brainEnabled;
    }

    public boolean isActuationEnabled() {
        return actuationEnabled;
    }

    public boolean isLauncherConnected() {
        return ipcClient != null && ipcClient.isConnected();
    }

    public String getIpcEndpoint() {
        return ipcClient != null ? ipcClient.getEndpoint() : "disabled";
    }

    public String getLastBrainSource() {
        return lastBrainSource;
    }

    public BrainDirective getLastDirective() {
        return lastDirective;
    }

    public int getCurrentBrainStateCode() {
        return lastDirective != null ? lastDirective.stateCode() : BrainDirective.STATE_GREEN;
    }

    public long getTotalItemsMerged() {
        return totalItemsMerged;
    }

    public long getLastItemsMergedThisTick() {
        return lastItemsMergedThisTick;
    }

    public double getLastMspt() {
        return lastMspt;
    }

    public double getLastTps() {
        return lastTps;
    }

    public int getLastLoadedEntities() {
        return lastLoadedEntities;
    }

    public int getLastLoadedChunks() {
        return lastLoadedChunks;
    }

    public int getLastOnlinePlayers() {
        return lastOnlinePlayers;
    }

    public long getLastHeapUsedMb() {
        return lastHeapUsedMb;
    }

    public long getLastHeapMaxMb() {
        return lastHeapMaxMb;
    }

    public int getLastAppliedViewDistance() {
        return lastAppliedViewDistance;
    }

    public int getLastAppliedSimDistance() {
        return lastAppliedSimDistance;
    }

    public record HotChunkEntry(
        int chunkX,
        int chunkZ,
        int entityCount,
        int crowdedCount
    ) {
        public int blockX() {
            return (chunkX << 4) + 8;
        }

        public int blockZ() {
            return (chunkZ << 4) + 8;
        }
    }

    public record ChunkHeatmapReport(
        String worldName,
        int totalEntities,
        double rustComputeMs,
        double totalMs,
        List<HotChunkEntry> hotChunks
    ) {
    }
}
