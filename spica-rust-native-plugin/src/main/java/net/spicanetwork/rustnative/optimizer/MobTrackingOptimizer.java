package net.spicanetwork.rustnative.optimizer;

import net.spicanetwork.rustnative.api.SpicaRustAPI;
import net.spicanetwork.rustnative.brain.BrainDirective;
import net.spicanetwork.rustnative.nativebridge.RustNative;
import org.bukkit.Bukkit;
import org.bukkit.GameMode;
import org.bukkit.Location;
import org.bukkit.World;
import org.bukkit.configuration.file.FileConfiguration;
import org.bukkit.entity.Boss;
import org.bukkit.entity.Enemy;
import org.bukkit.entity.Entity;
import org.bukkit.entity.EntityType;
import org.bukkit.entity.Mob;
import org.bukkit.entity.Monster;
import org.bukkit.entity.Player;
import org.bukkit.entity.Tameable;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.entity.EntityDeathEvent;
import org.bukkit.event.entity.EntityRemoveEvent;
import org.bukkit.event.entity.EntityTargetLivingEntityEvent;
import org.bukkit.event.player.PlayerChangedWorldEvent;
import org.bukkit.event.player.PlayerQuitEvent;
import org.bukkit.event.player.PlayerRespawnEvent;
import org.bukkit.event.player.PlayerTeleportEvent;
import org.bukkit.event.world.EntitiesUnloadEvent;
import org.bukkit.plugin.Plugin;
import org.bukkit.util.Vector;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.FloatBuffer;
import java.nio.IntBuffer;
import java.util.ArrayList;
import java.util.Collection;
import java.util.EnumSet;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.UUID;

/**
 * Live Rust-powered Mob Visibility & Tracking Optimizer (ZERO NMS).
 *
 * Uses Paper 1.21.4's native {@link Player#hideEntity(Plugin, Entity)} and
 * {@link Player#showEntity(Plugin, Entity)} API to remove out-of-range, vertically isolated,
 * rear-FOV, and over-budget mobs directly from Purpur's server-side {@code TrackedEntity.seenBy}
 * broadcast set, slashing entity metadata/movement packet CPU & bandwidth while keeping
 * nearby, aggroing, boss, tamed, named, mounted, and leashed mobs 100% visible.
 */
public final class MobTrackingOptimizer implements Listener {

    public static final String TRACKING_BYPASS_PERMISSION = "spicarustnative.bypass.tracking";

    private static final long TRANSITION_WINDOW_MS = 1_000L;
    private static final int STATS_INT_SLOTS = 4;

    private final Plugin plugin;
    private final SpicaRustAPI api;

    // Reusable per-tick scratch structures (Main Thread only)
    private final List<Player> playerScratch = new ArrayList<>(128);
    private final List<Mob> mobScratch = new ArrayList<>(2048);
    private final Map<UUID, Integer> mobIndexScratch = new HashMap<>(2048);
    private final Location scratchLoc = new Location(null, 0.0, 0.0, 0.0);

    // Per-player hidden mob UUID tracking
    private final Map<UUID, Set<UUID>> hiddenMobsByPlayer = new HashMap<>();
    private final Set<EntityType> alwaysVisibleTypes = EnumSet.noneOf(EntityType.class);

    // Dedicated reusable off-heap DirectByteBuffers (ByteOrder.nativeOrder())
    private int mobCapacity;
    private ByteBuffer mobXBuf, mobYBuf, mobZBuf, mobFlagsBuf;
    private FloatBuffer mobX, mobY, mobZ;
    private IntBuffer mobFlags;

    private int playerCapacity;
    private ByteBuffer playerXBuf, playerYBuf, playerZBuf;
    private ByteBuffer lookXBuf, lookYBuf, lookZBuf;
    private FloatBuffer playerX, playerY, playerZ;
    private FloatBuffer lookX, lookY, lookZ;

    private int bitsetByteCapacity;
    private ByteBuffer hiddenBitsetBuf;

    private int deltaCapacity;
    private ByteBuffer deltaPlayerBuf, deltaMobBuf, deltaActionBuf;
    private IntBuffer deltaPlayer, deltaMob, deltaAction;

    private final ByteBuffer statsBuf;
    private final IntBuffer statsView;

    // Configuration
    private boolean enabled = true;
    private long intervalTicks = 6L;
    private float maxTrackRange = 48.0f;
    private float safetyNearRange = 12.0f;
    private float maxVerticalDelta = 24.0f;
    private float rearCullRange = 16.0f;
    private float rearCullDotThreshold = -0.25f;
    private int maxMobsPerPlayer = 72;
    private int maxDeltasPerTick = 512;
    private boolean alwaysVisibleNamed = true;
    private boolean alwaysVisibleTamed = true;

    // Rolling 1-second transition window (Main Thread only)
    private long transitionWindowStartMs = System.currentTimeMillis();
    private int transitionsCurrentSec = 0;
    private int rollingTransitionsPerSec = 0;

    // Live telemetry
    private long lastRunNanos = 0L;
    private long lastRustNanos = 0L;
    private int lastTotalHiddenPairs = 0;
    private int lastTotalVisiblePairs = 0;
    private int lastCulledByBudget = 0;
    private int lastDeltasApplied = 0;
    private long totalTransitionsApplied = 0L;
    private int lastBrainStateCode = BrainDirective.STATE_GREEN;

    public MobTrackingOptimizer(Plugin plugin, SpicaRustAPI api) {
        this.plugin = plugin;
        this.api = api;

        setDefaultAlwaysVisibleTypes();
        allocateMobBuffers(2048);
        allocatePlayerBuffers(64);
        allocateBitsetBuffer(16_384);
        allocateDeltaBuffers(512);

        this.statsBuf = allocNative(STATS_INT_SLOTS * Integer.BYTES);
        this.statsView = statsBuf.asIntBuffer();
    }

    public void reloadConfig(FileConfiguration cfg) {
        this.enabled = cfg.getBoolean("mob-tracking.enabled", true);
        this.intervalTicks = Math.max(1L, cfg.getLong("mob-tracking.interval-ticks", 6L));

        float rawMaxTrack = (float) cfg.getDouble("mob-tracking.max-track-range", 48.0);
        this.maxTrackRange = Float.isFinite(rawMaxTrack) ? Math.clamp(rawMaxTrack, 12.0f, 160.0f) : 48.0f;

        float rawSafetyNear = (float) cfg.getDouble("mob-tracking.safety-near-range", 12.0);
        this.safetyNearRange = Float.isFinite(rawSafetyNear)
            ? Math.clamp(rawSafetyNear, 4.0f, Math.max(4.0f, this.maxTrackRange - 2.0f))
            : Math.min(12.0f, this.maxTrackRange);

        float rawMaxVert = (float) cfg.getDouble("mob-tracking.max-vertical-delta", 24.0);
        this.maxVerticalDelta = Float.isFinite(rawMaxVert) ? Math.clamp(rawMaxVert, 6.0f, 128.0f) : 24.0f;

        float rawRearRange = (float) cfg.getDouble("mob-tracking.rear-cull-range", 16.0);
        this.rearCullRange = Float.isFinite(rawRearRange)
            ? Math.clamp(rawRearRange, this.safetyNearRange, this.maxTrackRange)
            : Math.clamp(16.0f, this.safetyNearRange, this.maxTrackRange);

        float rawRearDot = (float) cfg.getDouble("mob-tracking.rear-cull-dot-threshold", -0.25);
        this.rearCullDotThreshold = Float.isFinite(rawRearDot)
            ? Math.clamp(rawRearDot, -1.0f, 0.5f)
            : -0.25f;

        this.maxMobsPerPlayer = Math.clamp(cfg.getInt("mob-tracking.max-mobs-per-player", 72), 8, 512);
        this.maxDeltasPerTick = Math.clamp(cfg.getInt("mob-tracking.max-deltas-per-tick", 512), 16, 16_384);
        ensureDeltaCapacity(this.maxDeltasPerTick);

        this.alwaysVisibleNamed = cfg.getBoolean("mob-tracking.always-visible-named", true);
        this.alwaysVisibleTamed = cfg.getBoolean("mob-tracking.always-visible-tamed", true);

        alwaysVisibleTypes.clear();
        if (cfg.isList("mob-tracking.always-visible-types")) {
            for (String name : cfg.getStringList("mob-tracking.always-visible-types")) {
                try {
                    alwaysVisibleTypes.add(EntityType.valueOf(name.trim().toUpperCase(Locale.ROOT)));
                } catch (IllegalArgumentException ignored) {
                    // Ignore unknown entity types across minor Minecraft versions
                }
            }
        } else {
            setDefaultAlwaysVisibleTypes();
        }
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void onPlayerQuit(PlayerQuitEvent event) {
        hiddenMobsByPlayer.remove(event.getPlayer().getUniqueId());
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void onPlayerChangedWorld(PlayerChangedWorldEvent event) {
        Player player = event.getPlayer();
        Set<UUID> hidden = hiddenMobsByPlayer.remove(player.getUniqueId());
        if (hidden != null && !hidden.isEmpty()) {
            for (UUID mobId : hidden) {
                Entity entity = Bukkit.getEntity(mobId);
                if (entity != null && entity.isValid()) {
                    player.showEntity(plugin, entity);
                }
            }
        }
    }

    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onEntityDeath(EntityDeathEvent event) {
        purgeEntityUuid(event.getEntity().getUniqueId());
    }

    /**
     * Y2: Keeps Spica's hidden-mob tracking map 100% synchronized with Purpur's native
     * {@code CraftPlayer.onEntityRemove} whenever mobs despawn or are discarded.
     */
    @EventHandler(priority = EventPriority.MONITOR)
    public void onEntityRemove(EntityRemoveEvent event) {
        if (!(event.getEntity() instanceof Mob)) {
            return;
        }
        purgeEntityUuid(event.getEntity().getUniqueId());
    }

    /**
     * Y1: Immediately reveal hidden mobs after same-world respawn so players never wait
     * an interval to see nearby mobs around their bed/spawn point.
     */
    @EventHandler(priority = EventPriority.MONITOR)
    public void onPlayerRespawn(PlayerRespawnEvent event) {
        if (!enabled) {
            return;
        }
        restorePlayerHiddenMobs(event.getPlayer());
    }

    /**
     * Y2: Cleans up unloaded mob UUIDs so re-loaded entities never drift out of sync with Purpur.
     */
    @EventHandler(priority = EventPriority.MONITOR)
    public void onEntitiesUnload(EntitiesUnloadEvent event) {
        if (hiddenMobsByPlayer.isEmpty()) {
            return;
        }
        for (Entity entity : event.getEntities()) {
            if (entity instanceof Mob) {
                purgeEntityUuid(entity.getUniqueId());
            }
        }
    }

    /**
     * Y1: Immediately reveals hidden mobs after a long-distance teleport so players never wait
     * for the next tracking interval to see threats around their landing spot.
     */
    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onPlayerTeleport(PlayerTeleportEvent event) {
        if (!enabled) {
            return;
        }
        Location from = event.getFrom();
        Location to = event.getTo();
        if (to == null || from.getWorld() != to.getWorld() || from.distanceSquared(to) >= 256.0) {
            restorePlayerHiddenMobs(event.getPlayer());
        }
    }

    private void purgeEntityUuid(UUID mobId) {
        if (hiddenMobsByPlayer.isEmpty()) {
            return;
        }
        hiddenMobsByPlayer.values().removeIf(set -> {
            set.remove(mobId);
            return set.isEmpty();
        });
    }

    /**
     * Immediately reveals any mob that acquires a player as its living target so aggroing mobs
     * are never invisible for even a single tick.
     */
    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onEntityTargetLivingEntity(EntityTargetLivingEntityEvent event) {
        if (!enabled) {
            return;
        }
        if (!(event.getEntity() instanceof Mob mob) || !(event.getTarget() instanceof Player player)) {
            return;
        }
        if (!player.isOnline() || !player.isValid() || !mob.isValid() || mob.isDead()) {
            return;
        }

        UUID playerId = player.getUniqueId();
        UUID mobId = mob.getUniqueId();
        Set<UUID> hidden = hiddenMobsByPlayer.get(playerId);
        if (hidden != null && hidden.remove(mobId)) {
            if (hidden.isEmpty()) {
                hiddenMobsByPlayer.remove(playerId);
            }
            player.showEntity(plugin, mob);
            this.totalTransitionsApplied++;
            this.transitionsCurrentSec++;
        }
    }

    /**
     * Runs on the Main Thread every {@code mob-tracking.interval-ticks} ticks (default: 6).
     */
    public void tick(int brainStateCode) {
        long nowMs = System.currentTimeMillis();
        int safeBrainState = Math.clamp(brainStateCode, BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);
        this.lastBrainStateCode = safeBrainState;

        if (!enabled || !api.isNativeLoaded()) {
            rotateTransitionWindow(nowMs);
            return;
        }

        long startNanos = System.nanoTime();
        long rustNanos = 0L;
        int totalHiddenPairs = 0;
        int totalVisiblePairs = 0;
        int totalCulledByBudget = 0;
        int deltasAppliedThisTick = 0;
        int hidesAppliedThisTick = 0;

        for (World world : Bukkit.getWorlds()) {
            List<Player> worldPlayers = world.getPlayers();
            if (worldPlayers.isEmpty()) {
                continue;
            }

            playerScratch.clear();
            for (Player player : worldPlayers) {
                if (!player.isOnline() || !player.isValid()) {
                    continue;
                }
                if (player.getGameMode() == GameMode.SPECTATOR
                    || player.hasPermission(TRACKING_BYPASS_PERMISSION)) {
                    restorePlayerHiddenMobs(player);
                    continue;
                }
                playerScratch.add(player);
            }

            int playerCount = playerScratch.size();
            if (playerCount == 0) {
                continue;
            }

            Collection<Mob> worldMobs = world.getEntitiesByClass(Mob.class);
            if (worldMobs.isEmpty()) {
                continue;
            }

            mobScratch.clear();
            for (Mob mob : worldMobs) {
                if (mob.isValid() && !mob.isDead()) {
                    mobScratch.add(mob);
                }
            }

            int mobCount = mobScratch.size();
            if (mobCount == 0) {
                continue;
            }

            long totalPairsLong = (long) playerCount * (long) mobCount;
            if (totalPairsLong > (long) Integer.MAX_VALUE - 8L) {
                continue;
            }

            int neededBitsetBytes = (int) ((totalPairsLong + 7L) >>> 3);
            ensureMobCapacity(mobCount);
            ensurePlayerCapacity(playerCount);
            ensureBitsetCapacity(neededBitsetBytes);
            ensureDeltaCapacity(maxDeltasPerTick);

            // Check if any active player in this world currently has hidden mobs before indexing UUIDs
            boolean anyHiddenInWorld = false;
            for (int p = 0; p < playerCount; p++) {
                Set<UUID> hidden = hiddenMobsByPlayer.get(playerScratch.get(p).getUniqueId());
                if (hidden != null && !hidden.isEmpty()) {
                    anyHiddenInWorld = true;
                    break;
                }
            }

            // Snapshot players (position + unit look vector)
            for (int p = 0; p < playerCount; p++) {
                Player player = playerScratch.get(p);
                player.getLocation(scratchLoc);
                playerX.put(p, (float) scratchLoc.getX());
                playerY.put(p, (float) scratchLoc.getY());
                playerZ.put(p, (float) scratchLoc.getZ());

                Vector look = scratchLoc.getDirection();
                float lx = (float) look.getX();
                float ly = (float) look.getY();
                float lz = (float) look.getZ();
                if (!Float.isFinite(lx) || !Float.isFinite(ly) || !Float.isFinite(lz)) {
                    lx = 0.0f;
                    ly = 0.0f;
                    lz = 1.0f;
                }
                lookX.put(p, lx);
                lookY.put(p, ly);
                lookZ.put(p, lz);
            }

            // Snapshot mobs (position + visibility/classification flags)
            mobIndexScratch.clear();
            for (int m = 0; m < mobCount; m++) {
                Mob mob = mobScratch.get(m);
                mob.getLocation(scratchLoc);

                int flags = (mob instanceof Monster || mob instanceof Enemy)
                    ? RustNative.TRACK_FLAG_HOSTILE
                    : RustNative.TRACK_FLAG_PASSIVE;
                if (isAlwaysVisibleMob(mob)) {
                    flags |= RustNative.TRACK_FLAG_ALWAYS_VISIBLE;
                }

                mobX.put(m, (float) scratchLoc.getX());
                mobY.put(m, (float) scratchLoc.getY());
                mobZ.put(m, (float) scratchLoc.getZ());
                mobFlags.put(m, flags);

                if (anyHiddenInWorld) {
                    mobIndexScratch.put(mob.getUniqueId(), m);
                }
            }

            // Clear visibility bitset [0..neededBitsetBytes) and mark currently hidden pairs with bit=1
            clearBitsetBytes(neededBitsetBytes);
            if (anyHiddenInWorld) {
                for (int p = 0; p < playerCount; p++) {
                    Set<UUID> hidden = hiddenMobsByPlayer.get(playerScratch.get(p).getUniqueId());
                    if (hidden == null || hidden.isEmpty()) {
                        continue;
                    }
                    int rowBase = p * mobCount;
                    for (UUID hiddenMobId : hidden) {
                        Integer mobIdxObj = mobIndexScratch.get(hiddenMobId);
                        if (mobIdxObj != null) {
                            int bitIndex = rowBase + mobIdxObj;
                            int byteIndex = bitIndex >>> 3;
                            byte current = hiddenBitsetBuf.get(byteIndex);
                            hiddenBitsetBuf.put(byteIndex, (byte) (current | (1 << (bitIndex & 7))));
                        }
                    }
                }
            }

            statsView.put(0, 0);
            statsView.put(1, 0);
            statsView.put(2, 0);
            statsView.put(3, 0);

            int remainingHideBudget = Math.max(0, maxDeltasPerTick - hidesAppliedThisTick);
            // Y1: Always reserve ample room for SHOW transitions (which Rust emits FIRST before HIDE)
            int callMaxDeltas = Math.max(remainingHideBudget + Math.min(256, maxDeltasPerTick), 128);
            ensureDeltaCapacity(callMaxDeltas);

            long rustStart = System.nanoTime();
            int deltaCount = api.computeMobTracking(
                mobCount,
                mobXBuf,
                mobYBuf,
                mobZBuf,
                mobFlagsBuf,
                playerCount,
                playerXBuf,
                playerYBuf,
                playerZBuf,
                lookXBuf,
                lookYBuf,
                lookZBuf,
                maxTrackRange,
                safetyNearRange,
                maxVerticalDelta,
                rearCullRange,
                rearCullDotThreshold,
                maxMobsPerPlayer,
                safeBrainState,
                hiddenBitsetBuf,
                callMaxDeltas,
                deltaPlayerBuf,
                deltaMobBuf,
                deltaActionBuf,
                statsBuf
            );
            rustNanos += (System.nanoTime() - rustStart);

            if (deltaCount < 0) {
                continue;
            }

            totalVisiblePairs += Math.max(0, statsView.get(1));
            totalCulledByBudget += Math.max(0, statsView.get(3));

            int applyCount = Math.min(deltaCount, callMaxDeltas);
            for (int d = 0; d < applyCount; d++) {
                int pIdx = deltaPlayer.get(d);
                int mIdx = deltaMob.get(d);
                int action = deltaAction.get(d);

                if (pIdx < 0 || pIdx >= playerCount || mIdx < 0 || mIdx >= mobCount) {
                    continue;
                }

                Player player = playerScratch.get(pIdx);
                Mob mob = mobScratch.get(mIdx);
                if (!player.isOnline() || !player.isValid() || !mob.isValid() || mob.isDead()) {
                    continue;
                }

                UUID playerId = player.getUniqueId();
                UUID mobId = mob.getUniqueId();

                if (action == RustNative.TRACK_ACTION_SHOW) {
                    // Y1: SHOW transitions ALWAYS execute unconditionally (never blocked by HIDE quota)
                    Set<UUID> hiddenSet = hiddenMobsByPlayer.get(playerId);
                    if (hiddenSet != null) {
                        hiddenSet.remove(mobId);
                        if (hiddenSet.isEmpty()) {
                            hiddenMobsByPlayer.remove(playerId);
                        }
                    }
                    player.showEntity(plugin, mob);
                    deltasAppliedThisTick++;
                } else if (action == RustNative.TRACK_ACTION_HIDE) {
                    if (hidesAppliedThisTick >= maxDeltasPerTick) {
                        continue;
                    }
                    // Main-thread re-verification before hiding (Y1)
                    if (isAlwaysVisibleMob(mob)) {
                        continue;
                    }
                    Set<UUID> hiddenSet = hiddenMobsByPlayer.computeIfAbsent(playerId, k -> new HashSet<>());
                    if (hiddenSet.add(mobId)) {
                        player.hideEntity(plugin, mob);
                        hidesAppliedThisTick++;
                        deltasAppliedThisTick++;
                    }
                }
            }
        }

        // Release scratch references so unloaded/dead entities are never retained (Y2)
        playerScratch.clear();
        mobScratch.clear();
        mobIndexScratch.clear();

        // Compute exact live hidden-pair count from actual tracked sets (never inflated by skipped HIDEs)
        int actualHiddenPairs = 0;
        for (Set<UUID> hiddenSet : hiddenMobsByPlayer.values()) {
            actualHiddenPairs += hiddenSet.size();
        }

        this.lastTotalHiddenPairs = actualHiddenPairs;
        this.lastTotalVisiblePairs = totalVisiblePairs;
        this.lastCulledByBudget = totalCulledByBudget;
        this.lastDeltasApplied = deltasAppliedThisTick;
        this.totalTransitionsApplied += deltasAppliedThisTick;
        this.transitionsCurrentSec += deltasAppliedThisTick;
        rotateTransitionWindow(nowMs);
        this.lastRustNanos = rustNanos;
        this.lastRunNanos = System.nanoTime() - startNanos;
    }

    /**
     * Restores visibility for every hidden (player, mob) pair and clears internal state.
     * Called on {@code onDisable()} and {@code reloadBridgeConfig()}.
     */
    public int restoreAll() {
        int restoredPairs = 0;
        for (Map.Entry<UUID, Set<UUID>> entry : hiddenMobsByPlayer.entrySet()) {
            Player player = Bukkit.getPlayer(entry.getKey());
            if (player == null || !player.isOnline()) {
                continue;
            }
            for (UUID mobId : entry.getValue()) {
                Entity entity = Bukkit.getEntity(mobId);
                if (entity != null && entity.isValid()) {
                    player.showEntity(plugin, entity);
                    restoredPairs++;
                }
            }
        }

        hiddenMobsByPlayer.clear();
        lastTotalHiddenPairs = 0;
        lastTotalVisiblePairs = 0;
        lastCulledByBudget = 0;
        lastDeltasApplied = 0;
        transitionsCurrentSec = 0;
        rollingTransitionsPerSec = 0;
        transitionWindowStartMs = System.currentTimeMillis();
        return restoredPairs;
    }

    private void restorePlayerHiddenMobs(Player player) {
        Set<UUID> hidden = hiddenMobsByPlayer.remove(player.getUniqueId());
        if (hidden == null || hidden.isEmpty()) {
            return;
        }
        for (UUID mobId : hidden) {
            Entity entity = Bukkit.getEntity(mobId);
            if (entity != null && entity.isValid()) {
                player.showEntity(plugin, entity);
            }
        }
    }

    private boolean isAlwaysVisibleMob(Mob mob) {
        if (mob instanceof Boss || alwaysVisibleTypes.contains(mob.getType())) {
            return true;
        }
        if (mob.hasMetadata("NPC")) {
            return true;
        }
        if (alwaysVisibleNamed && mob.customName() != null) {
            return true;
        }
        if (alwaysVisibleTamed && mob instanceof Tameable tameable && tameable.isTamed()) {
            return true;
        }
        if (!mob.getPassengers().isEmpty()) {
            return true;
        }
        if (mob.isLeashed()) {
            return true;
        }
        if (mob.getTarget() instanceof Player) {
            return true;
        }
        return false;
    }

    private void clearBitsetBytes(int byteCount) {
        int idx = 0;
        int longLimit = byteCount & ~7;
        while (idx < longLimit) {
            hiddenBitsetBuf.putLong(idx, 0L);
            idx += 8;
        }
        while (idx < byteCount) {
            hiddenBitsetBuf.put(idx, (byte) 0);
            idx++;
        }
    }

    private void rotateTransitionWindow(long nowMs) {
        long elapsedMs = nowMs - transitionWindowStartMs;
        if (elapsedMs >= TRANSITION_WINDOW_MS) {
            if (elapsedMs >= TRANSITION_WINDOW_MS * 2L) {
                this.rollingTransitionsPerSec = (int) Math.round((transitionsCurrentSec * 1_000.0) / elapsedMs);
            } else {
                this.rollingTransitionsPerSec = transitionsCurrentSec;
            }
            this.transitionsCurrentSec = 0;
            this.transitionWindowStartMs = nowMs;
        } else if (transitionsCurrentSec > rollingTransitionsPerSec) {
            this.rollingTransitionsPerSec = transitionsCurrentSec;
        }
    }

    private void ensureMobCapacity(int needed) {
        if (needed <= mobCapacity) {
            return;
        }
        int next = mobCapacity;
        while (next < needed) {
            next = Math.max(next * 2, needed);
        }
        allocateMobBuffers(next);
    }

    private void ensurePlayerCapacity(int needed) {
        if (needed <= playerCapacity) {
            return;
        }
        int next = playerCapacity;
        while (next < needed) {
            next = Math.max(next * 2, needed);
        }
        allocatePlayerBuffers(next);
    }

    private void ensureBitsetCapacity(int neededBytes) {
        if (neededBytes <= bitsetByteCapacity) {
            return;
        }
        int next = bitsetByteCapacity;
        while (next < neededBytes) {
            next = Math.max(next * 2, neededBytes);
        }
        allocateBitsetBuffer(next);
    }

    private void ensureDeltaCapacity(int neededDeltas) {
        if (neededDeltas <= deltaCapacity) {
            return;
        }
        int next = Math.max(64, neededDeltas);
        allocateDeltaBuffers(next);
    }

    private void allocateMobBuffers(int cap) {
        this.mobCapacity = cap;
        int bytes = cap * Float.BYTES;
        this.mobXBuf = allocNative(bytes);
        this.mobYBuf = allocNative(bytes);
        this.mobZBuf = allocNative(bytes);
        this.mobFlagsBuf = allocNative(cap * Integer.BYTES);

        this.mobX = mobXBuf.asFloatBuffer();
        this.mobY = mobYBuf.asFloatBuffer();
        this.mobZ = mobZBuf.asFloatBuffer();
        this.mobFlags = mobFlagsBuf.asIntBuffer();
    }

    private void allocatePlayerBuffers(int cap) {
        this.playerCapacity = cap;
        int bytes = cap * Float.BYTES;
        this.playerXBuf = allocNative(bytes);
        this.playerYBuf = allocNative(bytes);
        this.playerZBuf = allocNative(bytes);
        this.lookXBuf = allocNative(bytes);
        this.lookYBuf = allocNative(bytes);
        this.lookZBuf = allocNative(bytes);

        this.playerX = playerXBuf.asFloatBuffer();
        this.playerY = playerYBuf.asFloatBuffer();
        this.playerZ = playerZBuf.asFloatBuffer();
        this.lookX = lookXBuf.asFloatBuffer();
        this.lookY = lookYBuf.asFloatBuffer();
        this.lookZ = lookZBuf.asFloatBuffer();
    }

    private void allocateBitsetBuffer(int bytes) {
        this.bitsetByteCapacity = bytes;
        this.hiddenBitsetBuf = allocNative(bytes);
    }

    private void allocateDeltaBuffers(int cap) {
        this.deltaCapacity = cap;
        int bytes = cap * Integer.BYTES;
        this.deltaPlayerBuf = allocNative(bytes);
        this.deltaMobBuf = allocNative(bytes);
        this.deltaActionBuf = allocNative(bytes);

        this.deltaPlayer = deltaPlayerBuf.asIntBuffer();
        this.deltaMob = deltaMobBuf.asIntBuffer();
        this.deltaAction = deltaActionBuf.asIntBuffer();
    }

    private static ByteBuffer allocNative(int bytes) {
        return ByteBuffer.allocateDirect(bytes).order(ByteOrder.nativeOrder());
    }

    private void setDefaultAlwaysVisibleTypes() {
        alwaysVisibleTypes.clear();
        alwaysVisibleTypes.add(EntityType.ENDER_DRAGON);
        alwaysVisibleTypes.add(EntityType.WITHER);
        alwaysVisibleTypes.add(EntityType.WARDEN);
        alwaysVisibleTypes.add(EntityType.ELDER_GUARDIAN);
        alwaysVisibleTypes.add(EntityType.VILLAGER);
        alwaysVisibleTypes.add(EntityType.IRON_GOLEM);
    }

    private static float rangeMultiplierForState(int stateCode) {
        return switch (stateCode) {
            case BrainDirective.STATE_GREEN -> 1.00f;
            case BrainDirective.STATE_YELLOW -> 0.90f;
            case BrainDirective.STATE_ORANGE -> 0.78f;
            case BrainDirective.STATE_RED -> 0.65f;
            case BrainDirective.STATE_EMERGENCY -> 0.52f;
            default -> 1.00f;
        };
    }

    private static float vertMultiplierForState(int stateCode) {
        return switch (stateCode) {
            case BrainDirective.STATE_GREEN -> 1.00f;
            case BrainDirective.STATE_YELLOW -> 0.90f;
            case BrainDirective.STATE_ORANGE -> 0.80f;
            case BrainDirective.STATE_RED -> 0.70f;
            case BrainDirective.STATE_EMERGENCY -> 0.60f;
            default -> 1.00f;
        };
    }

    private static float budgetMultiplierForState(int stateCode) {
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

    public long getIntervalTicks() {
        return intervalTicks;
    }

    public float getMaxTrackRange() {
        return maxTrackRange;
    }

    public float getEffectiveMaxTrackRange() {
        return Math.max(safetyNearRange + 4.0f, maxTrackRange * rangeMultiplierForState(lastBrainStateCode));
    }

    public float getSafetyNearRange() {
        return safetyNearRange;
    }

    public float getMaxVerticalDelta() {
        return maxVerticalDelta;
    }

    public float getEffectiveMaxVerticalDelta() {
        return Math.max(8.0f, maxVerticalDelta * vertMultiplierForState(lastBrainStateCode));
    }

    public float getRearCullRange() {
        return rearCullRange;
    }

    public float getEffectiveRearCullRange() {
        return Math.max(safetyNearRange, rearCullRange);
    }

    public float getRearCullDotThreshold() {
        return rearCullDotThreshold;
    }

    public int getMaxMobsPerPlayer() {
        return maxMobsPerPlayer;
    }

    public int getEffectiveMaxMobsPerPlayer() {
        return Math.max(8, Math.round(Math.max(8, maxMobsPerPlayer) * budgetMultiplierForState(lastBrainStateCode)));
    }

    public int getMaxDeltasPerTick() {
        return maxDeltasPerTick;
    }

    public double getLastRunMs() {
        return lastRunNanos / 1_000_000.0;
    }

    public double getLastRustMs() {
        return lastRustNanos / 1_000_000.0;
    }

    public int getLastTotalHiddenPairs() {
        return lastTotalHiddenPairs;
    }

    public int getLastTotalVisiblePairs() {
        return lastTotalVisiblePairs;
    }

    public int getLastCulledByBudget() {
        return lastCulledByBudget;
    }

    public int getLastDeltasApplied() {
        return lastDeltasApplied;
    }

    public long getTotalTransitionsApplied() {
        return totalTransitionsApplied;
    }

    public int getRollingTransitionsPerSec() {
        return rollingTransitionsPerSec;
    }

    public int getLastBrainStateCode() {
        return lastBrainStateCode;
    }

    public int getTrackedHiddenPairsSize() {
        int total = 0;
        for (Set<UUID> set : hiddenMobsByPlayer.values()) {
            total += set.size();
        }
        return total;
    }
}
