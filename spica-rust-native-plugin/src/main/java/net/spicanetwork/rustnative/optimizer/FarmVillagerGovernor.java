package net.spicanetwork.rustnative.optimizer;

import net.spicanetwork.rustnative.api.SpicaRustAPI;
import net.spicanetwork.rustnative.brain.BrainDirective;
import net.spicanetwork.rustnative.nativebridge.EntityBatchArena;
import net.spicanetwork.rustnative.nativebridge.RustNative;
import org.bukkit.Bukkit;
import org.bukkit.Location;
import org.bukkit.World;
import org.bukkit.configuration.file.FileConfiguration;
import org.bukkit.entity.Entity;
import org.bukkit.entity.EntityType;
import org.bukkit.entity.Mob;
import org.bukkit.entity.Player;
import org.bukkit.entity.Tameable;
import org.bukkit.entity.Villager;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.entity.EntityDeathEvent;
import org.bukkit.event.player.PlayerInteractEntityEvent;

import java.util.ArrayList;
import java.util.Collection;
import java.util.EnumSet;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.UUID;

/**
 * Live Rust-powered Mob Farm & Trading-Hall Villager Governor.
 *
 * Detects overcrowded mob farms and boxed-in trading hall villagers in zero-copy native Rust
 * ({@link RustNative#nativeEvaluateFarmThrottling}), disabling entity-entity collision pushing
 * and heavy pathfinding/AI via {@link MobAiOwnershipManager} while preserving seamless player
 * interaction, proximity wake-up, crash/unload safety, and periodic villager restock pulses.
 */
public final class FarmVillagerGovernor implements Listener {

    private final SpicaRustAPI api;
    private final MobAiOwnershipManager aiOwnership;
    private final EntityBatchArena farmArena = new EntityBatchArena(4096, 256);
    private final List<Mob> mobScratch = new ArrayList<>(4096);
    private final Location scratchLoc = new Location(null, 0.0, 0.0, 0.0);

    private final Map<UUID, Long> interactGraceUntilMs = new HashMap<>();
    private final Set<EntityType> excludedTypes = EnumSet.noneOf(EntityType.class);

    // Configuration
    private boolean enabled = true;
    private float crowdRadius = 3.5f;
    private int mildCrowdThreshold = 8;
    private int severeCrowdThreshold = 14;
    private int chunkMobLimit = 48;

    private boolean villagerOptimizationEnabled = false;
    private float villagerWakeRange = 8.0f;
    private int villagerCrowdThreshold = 4;
    private int interactGraceSeconds = 15;
    private int restockPulseIntervalSeconds = 120;
    private int restockPulseDurationSeconds = 6;

    private boolean skipNamedMobs = true;
    private boolean skipTamedMobs = true;

    // Restock pulse scheduling
    private long restockCycleAnchorMs = System.currentTimeMillis();

    // Live telemetry
    private long lastRunNanos = 0L;
    private long lastRustComputeNanos = 0L;
    private int lastTotalMobs = 0;
    private int lastFarmThrottled = 0;
    private int lastCollisionDisabled = 0;
    private int lastVillagersThrottled = 0;
    private boolean inRestockPulse = false;
    private int secondsUntilNextRestockPulse = 0;
    private int restockPulseRemainingSeconds = 0;
    private int lastBrainStateCode = BrainDirective.STATE_GREEN;

    public FarmVillagerGovernor(SpicaRustAPI api, MobAiOwnershipManager aiOwnership) {
        this.api = api;
        this.aiOwnership = aiOwnership;
        setDefaultExcludedTypes();
    }

    public void reloadConfig(FileConfiguration cfg) {
        this.enabled = cfg.getBoolean("farm-governor.enabled", true);
        float rawCrowdRadius = (float) cfg.getDouble("farm-governor.crowd-radius", 3.5);
        this.crowdRadius = Float.isFinite(rawCrowdRadius) ? Math.clamp(rawCrowdRadius, 1.0f, 16.0f) : 3.5f;
        this.mildCrowdThreshold = Math.max(2, cfg.getInt("farm-governor.mild-crowd-threshold", 8));
        this.severeCrowdThreshold = Math.max(
            this.mildCrowdThreshold,
            cfg.getInt("farm-governor.severe-crowd-threshold", 14)
        );
        this.chunkMobLimit = Math.max(8, cfg.getInt("farm-governor.chunk-mob-limit", 48));

        this.villagerOptimizationEnabled = cfg.getBoolean("farm-governor.villager-optimization.enabled", false);
        float rawWakeRange = (float) cfg.getDouble("farm-governor.villager-optimization.wake-range", 8.0);
        this.villagerWakeRange = Float.isFinite(rawWakeRange) ? Math.clamp(rawWakeRange, 2.0f, 48.0f) : 8.0f;
        this.villagerCrowdThreshold = Math.max(
            2,
            cfg.getInt("farm-governor.villager-optimization.crowd-threshold", 4)
        );
        this.interactGraceSeconds = Math.max(
            0,
            cfg.getInt("farm-governor.villager-optimization.interact-grace-seconds", 15)
        );
        this.restockPulseIntervalSeconds = Math.max(
            0,
            cfg.getInt("farm-governor.villager-optimization.restock-pulse-interval-seconds", 120)
        );
        int rawDuration = Math.max(
            0,
            cfg.getInt("farm-governor.villager-optimization.restock-pulse-duration-seconds", 6)
        );
        if (this.restockPulseIntervalSeconds > 0 && rawDuration > 0) {
            this.restockPulseDurationSeconds = Math.min(rawDuration, Math.max(1, this.restockPulseIntervalSeconds - 1));
        } else {
            this.restockPulseDurationSeconds = 0;
        }

        this.skipNamedMobs = cfg.getBoolean("farm-governor.skip-named-mobs", true);
        this.skipTamedMobs = cfg.getBoolean("farm-governor.skip-tamed-mobs", true);

        excludedTypes.clear();
        if (cfg.isList("farm-governor.excluded-types")) {
            for (String name : cfg.getStringList("farm-governor.excluded-types")) {
                try {
                    excludedTypes.add(EntityType.valueOf(name.trim().toUpperCase(Locale.ROOT)));
                } catch (IllegalArgumentException ignored) {
                    // Ignore unknown entity types across minor Minecraft versions
                }
            }
        } else {
            setDefaultExcludedTypes();
        }

        this.restockCycleAnchorMs = System.currentTimeMillis();
        updateRestockPulseState(this.restockCycleAnchorMs);
    }

    /**
     * Immediately wakes any mob/villager right-clicked by a player (trading, breeding, shearing, feeding)
     * and protects it from re-throttling for {@code interact-grace-seconds}.
     */
    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onPlayerInteractEntity(PlayerInteractEntityEvent event) {
        if (!enabled) {
            return;
        }
        Entity clicked = event.getRightClicked();
        if (!(clicked instanceof Mob mob) || !mob.isValid() || mob.isDead()) {
            return;
        }

        UUID id = mob.getUniqueId();
        boolean wasOwnedBySpica = aiOwnership.releaseAllForInteract(mob);
        boolean protectedMob = isProtectedMob(mob) || mob.hasMetadata("NPC");

        if (interactGraceSeconds > 0 && (wasOwnedBySpica || !protectedMob)) {
            interactGraceUntilMs.put(id, System.currentTimeMillis() + (interactGraceSeconds * 1000L));
        }
    }

    /**
     * Prevents interact-grace maps from retaining dead mob UUIDs in active XP/mob grinders.
     */
    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onEntityDeath(EntityDeathEvent event) {
        interactGraceUntilMs.remove(event.getEntity().getUniqueId());
    }

    /**
     * Runs on the Main Thread every {@code farm-governor.interval-ticks} ticks (default: 20 ticks = 1s).
     */
    public void tick(int brainStateCode) {
        if (!enabled || !api.isNativeLoaded()) {
            return;
        }

        long startNanos = System.nanoTime();
        long nowMs = System.currentTimeMillis();
        int safeBrainState = Math.clamp(brainStateCode, BrainDirective.STATE_GREEN, BrainDirective.STATE_EMERGENCY);
        this.lastBrainStateCode = safeBrainState;

        updateRestockPulseState(nowMs);
        if (!interactGraceUntilMs.isEmpty()) {
            interactGraceUntilMs.entrySet().removeIf(entry -> entry.getValue() <= nowMs);
        }

        long rustNanos = 0L;
        int totalMobs = 0;
        int farmThrottledCount = 0;
        int collisionDisabledCount = 0;
        int villagersThrottledCount = 0;

        for (World world : Bukkit.getWorlds()) {
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

            List<Player> players = world.getPlayers();
            int playerCount = players.size();

            farmArena.ensureEntityCapacity(mobCount);
            farmArena.ensureTargetCapacity(Math.max(1, playerCount));

            for (int i = 0; i < mobCount; i++) {
                Mob mob = mobScratch.get(i);
                mob.getLocation(scratchLoc);
                UUID id = mob.getUniqueId();

                int flags = 0;
                boolean isVillager = mob instanceof Villager;
                boolean protectedMob = isProtectedMob(mob)
                    || mob.hasMetadata("NPC")
                    || (!villagerOptimizationEnabled && isVillager);

                if (protectedMob) {
                    flags |= RustNative.FLAG_PROTECTED;
                } else {
                    if (isVillager) {
                        flags |= RustNative.FLAG_VILLAGER;
                        if (isBoxedIn(scratchLoc)) {
                            flags |= RustNative.FLAG_TRAPPED_BOX;
                        }
                        if (inRestockPulse) {
                            flags |= RustNative.FLAG_RECENT_INTERACT;
                        }
                    }

                    Long graceUntil = interactGraceUntilMs.get(id);
                    if (graceUntil != null && graceUntil > nowMs) {
                        flags |= RustNative.FLAG_RECENT_INTERACT;
                    }
                }

                farmArena.setEntityPositionAndFlags(
                    i,
                    (float) scratchLoc.getX(),
                    (float) scratchLoc.getY(),
                    (float) scratchLoc.getZ(),
                    flags
                );
            }

            for (int j = 0; j < playerCount; j++) {
                players.get(j).getLocation(scratchLoc);
                farmArena.setTargetPosition(
                    j,
                    (float) scratchLoc.getX(),
                    (float) scratchLoc.getY(),
                    (float) scratchLoc.getZ()
                );
            }

            long rustStart = System.nanoTime();
            int evalResult = api.evaluateFarmThrottling(
                farmArena,
                mobCount,
                playerCount,
                crowdRadius,
                mildCrowdThreshold,
                severeCrowdThreshold,
                chunkMobLimit,
                villagerWakeRange,
                villagerCrowdThreshold,
                safeBrainState
            );
            rustNanos += (System.nanoTime() - rustStart);
            totalMobs += mobCount;

            if (evalResult < 0) {
                continue;
            }

            for (int i = 0; i < mobCount; i++) {
                Mob mob = mobScratch.get(i);
                UUID id = mob.getUniqueId();
                int inputFlags = farmArena.getFlags(i);
                int action = farmArena.getOutIntA(i);

                if ((inputFlags & RustNative.FLAG_PROTECTED) != 0) {
                    action = RustNative.ACTION_NORMAL;
                }

                boolean disableCollision = (action & RustNative.ACTION_DISABLE_COLLISION) != 0;
                boolean throttleAi = (action & RustNative.ACTION_THROTTLE_AI) != 0;
                boolean isVillager = (inputFlags & RustNative.FLAG_VILLAGER) != 0;

                // Bit 0: Collision pushing suppression (ownership-checked via MobAiOwnershipManager)
                boolean colManaged;
                if (disableCollision) {
                    colManaged = aiOwnership.suppressCollision(mob);
                } else {
                    aiOwnership.releaseCollision(mob);
                    colManaged = false;
                }

                // Bit 1: AI throttling (coordinated cleanly via MobAiOwnershipManager)
                boolean aiManaged;
                if (throttleAi) {
                    aiManaged = aiOwnership.suppressAi(mob, MobAiOwnershipManager.REASON_FARM);
                } else {
                    aiOwnership.releaseAi(mob, MobAiOwnershipManager.REASON_FARM);
                    aiManaged = false;
                }

                if (colManaged) {
                    collisionDisabledCount++;
                }
                if (isVillager) {
                    if (aiManaged || colManaged) {
                        villagersThrottledCount++;
                    }
                } else {
                    if (aiManaged || colManaged) {
                        farmThrottledCount++;
                    }
                }
            }
        }

        mobScratch.clear();
        this.lastTotalMobs = totalMobs;
        this.lastFarmThrottled = farmThrottledCount;
        this.lastCollisionDisabled = collisionDisabledCount;
        this.lastVillagersThrottled = villagersThrottledCount;
        this.lastRustComputeNanos = rustNanos;
        this.lastRunNanos = System.nanoTime() - startNanos;
    }

    /**
     * Restores collision and AI awareness owned by this governor (called on disable/reload).
     */
    public int restoreAll() {
        int restored = aiOwnership.restoreAllCollisions();
        restored += aiOwnership.restoreAllForReason(MobAiOwnershipManager.REASON_FARM);
        interactGraceUntilMs.clear();
        lastFarmThrottled = 0;
        lastCollisionDisabled = 0;
        lastVillagersThrottled = 0;
        return restored;
    }

    /**
     * Returns {@code true} if this governor is actively suppressing the given mob's AI awareness.
     */
    public boolean isMobManagedByFarmGovernor(UUID id) {
        return aiOwnership.isAiSuppressedByReason(id, MobAiOwnershipManager.REASON_FARM);
    }

    /**
     * Fast 4-cardinal block confinement check (zero chunk loading).
     * Marks a villager as boxed-in when at least 3 horizontal sides are blocked at feet ({@code y}) or head ({@code y+1}).
     */
    public boolean isBoxedIn(Location loc) {
        if (loc == null) {
            return false;
        }
        World world = loc.getWorld();
        if (world == null) {
            return false;
        }
        int baseX = loc.getBlockX();
        int baseY = loc.getBlockY();
        int baseZ = loc.getBlockZ();
        int minY = world.getMinHeight();
        int maxY = world.getMaxHeight();

        int blockedSides = 0;
        if (isDirectionBlocked(world, baseX + 1, baseY, baseZ, minY, maxY)) {
            blockedSides++;
        }
        if (isDirectionBlocked(world, baseX - 1, baseY, baseZ, minY, maxY)) {
            blockedSides++;
        }
        if (isDirectionBlocked(world, baseX, baseY, baseZ + 1, minY, maxY)) {
            blockedSides++;
        }
        if (isDirectionBlocked(world, baseX, baseY, baseZ - 1, minY, maxY)) {
            blockedSides++;
        }
        return blockedSides >= 3;
    }

    private static boolean isDirectionBlocked(World world, int bx, int by, int bz, int minY, int maxY) {
        if (!world.isChunkLoaded(bx >> 4, bz >> 4)) {
            return false;
        }
        if (by >= minY && by < maxY && !world.getBlockAt(bx, by, bz).isPassable()) {
            return true;
        }
        int headY = by + 1;
        return headY >= minY && headY < maxY && !world.getBlockAt(bx, headY, bz).isPassable();
    }

    private void updateRestockPulseState(long nowMs) {
        if (!villagerOptimizationEnabled || restockPulseIntervalSeconds <= 0 || restockPulseDurationSeconds <= 0) {
            this.inRestockPulse = false;
            this.secondsUntilNextRestockPulse = -1;
            this.restockPulseRemainingSeconds = 0;
            return;
        }

        long intervalMs = restockPulseIntervalSeconds * 1000L;
        long durationMs = restockPulseDurationSeconds * 1000L;
        long elapsedInCycle = Math.floorMod(nowMs - restockCycleAnchorMs, intervalMs);
        long pulseStartMs = intervalMs - durationMs;

        if (elapsedInCycle >= pulseStartMs) {
            this.inRestockPulse = true;
            this.secondsUntilNextRestockPulse = 0;
            this.restockPulseRemainingSeconds = (int) Math.max(1L, (intervalMs - elapsedInCycle + 999L) / 1000L);
        } else {
            this.inRestockPulse = false;
            this.restockPulseRemainingSeconds = 0;
            this.secondsUntilNextRestockPulse = (int) Math.max(1L, (pulseStartMs - elapsedInCycle + 999L) / 1000L);
        }
    }

    private boolean isProtectedMob(Mob mob) {
        if (excludedTypes.contains(mob.getType())) {
            return true;
        }
        if (skipNamedMobs && mob.customName() != null) {
            return true;
        }
        if (skipTamedMobs && mob instanceof Tameable tameable && tameable.isTamed()) {
            return true;
        }
        return false;
    }

    private void setDefaultExcludedTypes() {
        excludedTypes.clear();
        excludedTypes.add(EntityType.ENDER_DRAGON);
        excludedTypes.add(EntityType.WITHER);
        excludedTypes.add(EntityType.WARDEN);
        excludedTypes.add(EntityType.ELDER_GUARDIAN);
    }

    public boolean isEnabled() {
        return enabled;
    }

    public boolean isVillagerOptimizationEnabled() {
        return villagerOptimizationEnabled;
    }

    public double getLastRunMs() {
        return lastRunNanos / 1_000_000.0;
    }

    public double getLastRustComputeMs() {
        return lastRustComputeNanos / 1_000_000.0;
    }

    public int getLastTotalMobs() {
        return lastTotalMobs;
    }

    public int getLastFarmThrottled() {
        return lastFarmThrottled;
    }

    public int getLastCollisionDisabled() {
        return lastCollisionDisabled;
    }

    public int getLastVillagersThrottled() {
        return lastVillagersThrottled;
    }

    public boolean isInRestockPulse() {
        return inRestockPulse;
    }

    public int getSecondsUntilNextRestockPulse() {
        return secondsUntilNextRestockPulse;
    }

    public int getRestockPulseRemainingSeconds() {
        return restockPulseRemainingSeconds;
    }

    public float getCrowdRadius() {
        return crowdRadius;
    }

    public int getMildCrowdThreshold() {
        return mildCrowdThreshold;
    }

    public int getSevereCrowdThreshold() {
        return severeCrowdThreshold;
    }

    public int getChunkMobLimit() {
        return chunkMobLimit;
    }

    public float getVillagerWakeRange() {
        return villagerWakeRange;
    }

    public int getVillagerCrowdThreshold() {
        return villagerCrowdThreshold;
    }

    public int getInteractGraceSeconds() {
        return interactGraceSeconds;
    }

    public int getRestockPulseIntervalSeconds() {
        return restockPulseIntervalSeconds;
    }

    public int getRestockPulseDurationSeconds() {
        return restockPulseDurationSeconds;
    }

    public int getLastBrainStateCode() {
        return lastBrainStateCode;
    }

    public int getTrackedAiSuppressedSize() {
        return aiOwnership.countByAiReason(MobAiOwnershipManager.REASON_FARM);
    }

    public int getTrackedCollisionSuppressedSize() {
        return aiOwnership.getCollisionTrackedCount();
    }
}
