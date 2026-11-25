package net.spicanetwork.rustnative.optimizer;

import net.kyori.adventure.text.Component;
import net.kyori.adventure.text.format.NamedTextColor;
import net.spicanetwork.rustnative.api.SpicaRustAPI;
import net.spicanetwork.rustnative.brain.BrainDirective;
import net.spicanetwork.rustnative.nativebridge.EntityBatchArena;
import net.spicanetwork.rustnative.nativebridge.RustNative;
import org.bukkit.Bukkit;
import org.bukkit.Location;
import org.bukkit.World;
import org.bukkit.configuration.file.FileConfiguration;
import org.bukkit.entity.Item;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.block.BlockExplodeEvent;
import org.bukkit.event.entity.EntityExplodeEvent;
import org.bukkit.event.player.PlayerQuitEvent;
import org.bukkit.event.world.ChunkLoadEvent;
import org.bukkit.event.world.WorldUnloadEvent;
import org.bukkit.plugin.Plugin;
import org.bukkit.scheduler.BukkitTask;
import org.bukkit.util.Vector;

import java.util.ArrayList;
import java.util.Collection;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.UUID;

/**
 * Rust-powered Elytra / Chunk-Load Surge Guard and Debounced Explosion Item-Burst Buffer.
 *
 * Responsibilities on the Purpur 1.21.4 Main Thread:
 * 1. Tracks weighted chunk loads/sec (new chunks = weight 2, existing loads = weight 1) and
 *    escalates the effective Brain state by +1 during chunk-load surges.
 * 2. Evaluates gliding players via zero-copy Rust ({@link RustNative#nativeEvaluatePlayerSurges})
 *    and smoothly clamps excessive Elytra velocities with a subtle 3-second cooldown ActionBar.
 * 3. Debounces multi-block explosions per world and executes a 1-tick delayed Rust Spatial-Hash
 *    NBT-safe ground item merge sweep to absorb TNT/Creeper entity bursts before they spike MSPT.
 */
public final class SurgeExplosionGuard implements Listener {

    public static final String ELYTRA_BYPASS_PERMISSION = "spicarustnative.bypass.elytra";

    private static final long ACTIONBAR_COOLDOWN_MS = 3_000L;
    private static final long CHUNK_WINDOW_MS = 1_000L;
    private static final int MAX_EXPLOSION_MERGE_PAIRS = 2048;

    private final Plugin plugin;
    private final SpicaRustAPI api;
    private final EntityBatchArena surgeArena = new EntityBatchArena(1024, 64);
    private final List<Player> glidingScratch = new ArrayList<>(64);
    private final List<Item> itemScratch = new ArrayList<>(1024);
    private final Location scratchLoc = new Location(null, 0.0, 0.0, 0.0);

    private final Map<UUID, List<ExplosionSpot>> pendingExplosionsByWorld = new HashMap<>();
    private final Map<UUID, BukkitTask> pendingExplosionTasks = new HashMap<>();
    private final Map<UUID, Integer> chunkLoadsByWorldCurrentSec = new HashMap<>();
    private final Map<UUID, Integer> lastChunkLoadsByWorld = new HashMap<>();
    private final Map<UUID, Long> actionbarCooldownUntilMs = new HashMap<>();
    private long resetGeneration = 0L;

    // Configuration
    private boolean enabled = true;
    private boolean elytraEnabled = true;
    private long elytraIntervalTicks = 10L;
    private float baseMaxElytraSpeed = 1.8f;
    private int chunkLoadSurgeThreshold = 40;
    private boolean actionbarWarning = true;

    private boolean explosionEnabled = true;
    private int explosionMinBlocksTrigger = 8;
    private float explosionMergeRadius = 5.0f;

    // Rolling 1-second chunk-load telemetry (Main Thread only, monotonic clock)
    private long chunkWindowStartNanos = System.nanoTime();
    private int chunkLoadsCurrentSec = 0;
    private int lastChunkLoadsPerSec = 0;

    // Live runtime telemetry
    private long elytraClampsTotal = 0L;
    private long explosionBurstsBuffered = 0L;
    private long explosionItemsMergedTotal = 0L;
    private int lastGlidingPlayers = 0;
    private int lastEffectiveBrainStateCode = BrainDirective.STATE_GREEN;
    private boolean chunkSurgeEscalated = false;
    private float activeSpeedCap = 1.8f;
    private long lastRustComputeNanos = 0L;

    public SurgeExplosionGuard(Plugin plugin, SpicaRustAPI api) {
        this.plugin = plugin;
        this.api = api;
    }

    public void reloadConfig(FileConfiguration cfg) {
        this.enabled = cfg.getBoolean("surge-guard.enabled", true);
        this.elytraEnabled = cfg.getBoolean("surge-guard.elytra.enabled", true);
        this.elytraIntervalTicks = Math.max(1L, cfg.getLong("surge-guard.elytra.interval-ticks", 10L));

        float rawBaseSpeed = (float) cfg.getDouble("surge-guard.elytra.base-max-speed", 1.8);
        this.baseMaxElytraSpeed = Float.isFinite(rawBaseSpeed)
            ? Math.clamp(rawBaseSpeed, 0.5f, 8.0f)
            : 1.8f;

        this.chunkLoadSurgeThreshold = Math.max(
            4,
            cfg.getInt("surge-guard.elytra.chunk-load-surge-threshold", 40)
        );
        this.actionbarWarning = cfg.getBoolean("surge-guard.elytra.actionbar-warning", true);

        this.explosionEnabled = cfg.getBoolean("surge-guard.explosion.enabled", true);
        this.explosionMinBlocksTrigger = Math.max(
            1,
            cfg.getInt("surge-guard.explosion.min-blocks-trigger", 8)
        );
        float rawMergeRadius = (float) cfg.getDouble("surge-guard.explosion.merge-radius", 5.0);
        this.explosionMergeRadius = Float.isFinite(rawMergeRadius)
            ? Math.clamp(rawMergeRadius, 1.0f, 16.0f)
            : 5.0f;

        this.activeSpeedCap = computeSpeedCapForState(this.lastEffectiveBrainStateCode, this.baseMaxElytraSpeed);
    }

    /**
     * Cancels scheduled explosion tasks and clears transient pending queues and cooldown maps on disable or reload (Y5).
     */
    public void resetPending() {
        resetGeneration++;
        for (BukkitTask task : pendingExplosionTasks.values()) {
            if (task != null) {
                task.cancel();
            }
        }
        pendingExplosionTasks.clear();
        pendingExplosionsByWorld.clear();
        chunkLoadsByWorldCurrentSec.clear();
        lastChunkLoadsByWorld.clear();
        actionbarCooldownUntilMs.clear();
        chunkLoadsCurrentSec = 0;
        chunkWindowStartNanos = System.nanoTime();
        lastGlidingPlayers = 0;
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void onChunkLoad(ChunkLoadEvent event) {
        if (!enabled) {
            return;
        }
        int weight = event.isNewChunk() ? 2 : 1;
        chunkLoadsCurrentSec += weight;
        UUID worldId = event.getWorld().getUID();
        chunkLoadsByWorldCurrentSec.merge(worldId, weight, Integer::sum);
    }

    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onWorldUnload(WorldUnloadEvent event) {
        UUID worldId = event.getWorld().getUID();
        BukkitTask task = pendingExplosionTasks.remove(worldId);
        if (task != null) {
            task.cancel();
        }
        pendingExplosionsByWorld.remove(worldId);
        chunkLoadsByWorldCurrentSec.remove(worldId);
        lastChunkLoadsByWorld.remove(worldId);
    }

    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onEntityExplode(EntityExplodeEvent event) {
        if (!enabled || !explosionEnabled) {
            return;
        }
        if (event.blockList().size() < explosionMinBlocksTrigger) {
            return;
        }
        Location loc = event.getLocation();
        World world = loc.getWorld();
        if (world != null) {
            scheduleExplosionBurstMerge(world, loc.getX(), loc.getY(), loc.getZ());
        }
    }

    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onBlockExplode(BlockExplodeEvent event) {
        if (!enabled || !explosionEnabled) {
            return;
        }
        if (event.blockList().size() < explosionMinBlocksTrigger) {
            return;
        }
        Location loc = event.getBlock().getLocation();
        World world = loc.getWorld();
        if (world != null) {
            scheduleExplosionBurstMerge(world, loc.getX() + 0.5, loc.getY() + 0.5, loc.getZ() + 0.5);
        }
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void onPlayerQuit(PlayerQuitEvent event) {
        actionbarCooldownUntilMs.remove(event.getPlayer().getUniqueId());
    }

    /**
     * Runs on the Main Thread every {@code surge-guard.elytra.interval-ticks} ticks (default: 10).
     */
    public void tick(int brainStateCode) {
        long nowMs = System.currentTimeMillis();
        rotateChunkLoadWindow(System.nanoTime());

        int safeBrainState = Math.clamp(brainStateCode, BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);
        boolean surgeActive = lastChunkLoadsPerSec >= chunkLoadSurgeThreshold;
        int effectiveBrainState = surgeActive
            ? Math.min(BrainDirective.STATE_EMERGENCY, safeBrainState + 1)
            : safeBrainState;

        this.chunkSurgeEscalated = surgeActive;
        this.lastEffectiveBrainStateCode = effectiveBrainState;
        this.activeSpeedCap = computeSpeedCapForState(effectiveBrainState, baseMaxElytraSpeed);

        if (!enabled || !elytraEnabled || !api.isNativeLoaded()) {
            this.lastGlidingPlayers = 0;
            return;
        }

        // Y6: Evaluate gliding players per-world so chunk surges in one world do not unfairly clamp another world
        int totalGliding = 0;
        long totalRustNanos = 0L;
        for (World world : Bukkit.getWorlds()) {
            glidingScratch.clear();
            for (Player player : world.getPlayers()) {
                if (player.isOnline()
                    && player.isValid()
                    && player.isGliding()
                    && !player.hasPermission(ELYTRA_BYPASS_PERMISSION)) {
                    glidingScratch.add(player);
                }
            }

            int count = glidingScratch.size();
            if (count == 0) {
                continue;
            }
            totalGliding += count;

            int worldChunkRate = lastChunkLoadsByWorld.getOrDefault(world.getUID(), 0);
            int worldEffState = (worldChunkRate >= chunkLoadSurgeThreshold)
                ? Math.min(BrainDirective.STATE_EMERGENCY, safeBrainState + 1)
                : safeBrainState;

            surgeArena.ensureEntityCapacity(count);
            for (int i = 0; i < count; i++) {
                Player player = glidingScratch.get(i);
                player.getLocation(scratchLoc);
                Vector vel = player.getVelocity();

                surgeArena.setEntityPositionAndFlags(
                    i, (float) scratchLoc.getX(), (float) scratchLoc.getY(), (float) scratchLoc.getZ(), 1
                );
                surgeArena.setEntityVelocity(i, (float) vel.getX(), (float) vel.getY(), (float) vel.getZ());
            }

            long rustStart = System.nanoTime();
            int evalResult = api.evaluatePlayerSurges(surgeArena, count, worldEffState, baseMaxElytraSpeed);
            totalRustNanos += (System.nanoTime() - rustStart);
            if (evalResult < 0) {
                continue;
            }

            for (int i = 0; i < count; i++) {
                int action = surgeArena.getOutIntA(i);
                if (action != RustNative.SURGE_CLAMP_VELOCITY) {
                    continue;
                }

                float cvx = surgeArena.getOutX(i);
                float cvy = surgeArena.getOutY(i);
                float cvz = surgeArena.getOutZ(i);
                if (!Float.isFinite(cvx) || !Float.isFinite(cvy) || !Float.isFinite(cvz)) {
                    continue;
                }

                Player player = glidingScratch.get(i);
                if (!player.isOnline() || !player.isValid()) {
                    continue;
                }

                player.setVelocity(new Vector(cvx, cvy, cvz));
                this.elytraClampsTotal++;

                if (actionbarWarning) {
                    UUID pid = player.getUniqueId();
                    Long cooldownUntil = actionbarCooldownUntilMs.get(pid);
                    if (cooldownUntil == null || cooldownUntil <= nowMs) {
                        actionbarCooldownUntilMs.put(pid, nowMs + ACTIONBAR_COOLDOWN_MS);
                        float clampedSpeed = (float) Math.sqrt(cvx * cvx + cvy * cvy + cvz * cvz);
                        player.sendActionBar(Component.text(
                            String.format(
                                Locale.US,
                                "Chunk yogunlugu nedeniyle Elytra hizi dengeleniyor (%.2f blok/tick)",
                                clampedSpeed
                            ),
                            NamedTextColor.GOLD
                        ));
                    }
                }
            }
        }
        glidingScratch.clear();
        this.lastGlidingPlayers = totalGliding;
        this.lastRustComputeNanos = totalRustNanos;
        if (!actionbarCooldownUntilMs.isEmpty()) {
            actionbarCooldownUntilMs.entrySet().removeIf(entry -> entry.getValue() <= nowMs);
        }
    }

    private void rotateChunkLoadWindow(long nowNanos) {
        long elapsedMs = (nowNanos - chunkWindowStartNanos) / 1_000_000L;
        if (elapsedMs >= CHUNK_WINDOW_MS) {
            double scale = 1_000.0 / Math.max(CHUNK_WINDOW_MS, elapsedMs);
            this.lastChunkLoadsPerSec = (int) Math.round(chunkLoadsCurrentSec * scale);
            this.lastChunkLoadsByWorld.clear();
            for (Map.Entry<UUID, Integer> e : chunkLoadsByWorldCurrentSec.entrySet()) {
                this.lastChunkLoadsByWorld.put(e.getKey(), (int) Math.round(e.getValue() * scale));
            }
            this.chunkLoadsCurrentSec = 0;
            this.chunkLoadsByWorldCurrentSec.clear();
            this.chunkWindowStartNanos = nowNanos;
        } else {
            if (chunkLoadsCurrentSec > lastChunkLoadsPerSec) {
                this.lastChunkLoadsPerSec = chunkLoadsCurrentSec;
            }
            for (Map.Entry<UUID, Integer> e : chunkLoadsByWorldCurrentSec.entrySet()) {
                int cur = e.getValue();
                if (cur > lastChunkLoadsByWorld.getOrDefault(e.getKey(), 0)) {
                    lastChunkLoadsByWorld.put(e.getKey(), cur);
                }
            }
        }
    }

    private void scheduleExplosionBurstMerge(World world, double x, double y, double z) {
        if (!plugin.isEnabled() || !api.isNativeLoaded()) {
            return;
        }
        UUID worldId = world.getUID();
        List<ExplosionSpot> spots = pendingExplosionsByWorld.computeIfAbsent(worldId, k -> new ArrayList<>(4));
        if (spots.size() < 32) {
            spots.add(new ExplosionSpot(x, y, z));
        }
        if (pendingExplosionTasks.containsKey(worldId)) {
            return;
        }
        long capturedGen = this.resetGeneration;
        BukkitTask task = Bukkit.getScheduler().runTaskLater(
            plugin,
            () -> processExplosionBurstMerge(worldId, capturedGen),
            1L
        );
        pendingExplosionTasks.put(worldId, task);
    }

    private void processExplosionBurstMerge(UUID worldId, long expectedGen) {
        if (expectedGen != this.resetGeneration) {
            return;
        }
        pendingExplosionTasks.remove(worldId);
        List<ExplosionSpot> spots = pendingExplosionsByWorld.remove(worldId);

        if (!enabled || !explosionEnabled || !api.isNativeLoaded()) {
            return;
        }
        if (spots == null || spots.isEmpty()) {
            return;
        }
        World world = Bukkit.getWorld(worldId);
        if (world == null) {
            return;
        }

        // Y5: Scope explosion item merge strictly to blast neighborhood so unrelated world items are untouched
        double blastRadius = Math.max(12.0, explosionMergeRadius * 3.0);
        double blastRadiusSq = blastRadius * blastRadius;

        Collection<Item> groundItems = world.getEntitiesByClass(Item.class);
        if (groundItems.size() < 2) {
            return;
        }

        itemScratch.clear();
        for (Item item : groundItems) {
            if (!SafeItemMergeHelper.isEligibleCandidate(item)) {
                continue;
            }
            item.getLocation(scratchLoc);
            if (isNearAnySpot(scratchLoc.getX(), scratchLoc.getY(), scratchLoc.getZ(), spots, blastRadiusSq)) {
                itemScratch.add(item);
            }
        }

        if (itemScratch.size() < 2) {
            itemScratch.clear();
            return;
        }

        this.explosionBurstsBuffered++;
        long merged = SafeItemMergeHelper.planAndApplySafeMerges(
            api,
            surgeArena,
            itemScratch,
            explosionMergeRadius,
            MAX_EXPLOSION_MERGE_PAIRS
        );
        itemScratch.clear();
        this.explosionItemsMergedTotal += merged;
    }

    private static boolean isNearAnySpot(
        double x,
        double y,
        double z,
        List<ExplosionSpot> spots,
        double radiusSq
    ) {
        for (int i = 0; i < spots.size(); i++) {
            ExplosionSpot s = spots.get(i);
            double dx = x - s.x;
            double dy = y - s.y;
            double dz = z - s.z;
            if (dx * dx + dy * dy + dz * dz <= radiusSq) {
                return true;
            }
        }
        return false;
    }

    private record ExplosionSpot(double x, double y, double z) {
    }

    private static float computeSpeedCapForState(int stateCode, float baseMaxSpeed) {
        float multiplier = switch (stateCode) {
            case BrainDirective.STATE_GREEN -> 1.00f;
            case BrainDirective.STATE_YELLOW -> 0.85f;
            case BrainDirective.STATE_ORANGE -> 0.70f;
            case BrainDirective.STATE_RED -> 0.55f;
            case BrainDirective.STATE_EMERGENCY -> 0.40f;
            default -> 1.00f;
        };
        return Math.max(0.4f, baseMaxSpeed * multiplier);
    }

    public boolean isEnabled() {
        return enabled;
    }

    public boolean isElytraEnabled() {
        return elytraEnabled;
    }

    public boolean isExplosionEnabled() {
        return explosionEnabled;
    }

    public long getElytraIntervalTicks() {
        return elytraIntervalTicks;
    }

    public float getBaseMaxElytraSpeed() {
        return baseMaxElytraSpeed;
    }

    public float getActiveSpeedCap() {
        return activeSpeedCap;
    }

    public int getChunkLoadSurgeThreshold() {
        return chunkLoadSurgeThreshold;
    }

    public boolean isChunkSurgeEscalated() {
        return chunkSurgeEscalated;
    }

    public int getExplosionMinBlocksTrigger() {
        return explosionMinBlocksTrigger;
    }

    public float getExplosionMergeRadius() {
        return explosionMergeRadius;
    }

    public long getElytraClampsTotal() {
        return elytraClampsTotal;
    }

    public long getExplosionBurstsBuffered() {
        return explosionBurstsBuffered;
    }

    public long getExplosionItemsMergedTotal() {
        return explosionItemsMergedTotal;
    }

    public int getLastChunkLoadsPerSec() {
        return lastChunkLoadsPerSec;
    }

    public int getLastGlidingPlayers() {
        return lastGlidingPlayers;
    }

    public int getLastEffectiveBrainStateCode() {
        return lastEffectiveBrainStateCode;
    }

    public double getLastRustComputeMs() {
        return lastRustComputeNanos / 1_000_000.0;
    }
}
