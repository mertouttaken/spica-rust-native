package net.spicanetwork.rustnative.optimizer;

import net.spicanetwork.rustnative.api.SpicaRustAPI;
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

import java.util.ArrayList;
import java.util.Collection;
import java.util.EnumSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.UUID;

/**
 * Live Purpur 1.21.4 Entity AI LOD Optimizer & Mob-Cluster Hotspot Scanner.
 *
 * Snapshots loaded mobs & players into the zero-copy off-heap {@link EntityBatchArena},
 * evaluates nearest-player distances and spatial crowding inside Rust, and safely toggles
 * {@link Mob#setAware(boolean)} via {@link MobAiOwnershipManager} to prevent permanent NBT leakage.
 */
public final class EntityLodOptimizer {

    private final SpicaRustAPI api;
    private final MobAiOwnershipManager aiOwnership;
    private final EntityBatchArena mainThreadArena = new EntityBatchArena(4096, 256);
    private final List<Mob> mobScratch = new ArrayList<>(4096);
    private final Set<EntityType> excludedTypes = EnumSet.noneOf(EntityType.class);

    private boolean enabled = true;
    private float baseFullAiRange = 32.0f;
    private float baseHibernateRange = 56.0f;
    private float fullAiRange = 32.0f;
    private float hibernateRange = 56.0f;
    private int crammingAlertNeighbors = 12;
    private boolean skipNamedMobs = true;
    private boolean skipTamedMobs = true;

    // Telemetry
    private long lastRunNanos = 0L;
    private int lastTotalMobs = 0;
    private int lastFullMobs = 0;
    private int lastHibernatedMobs = 0;

    public EntityLodOptimizer(SpicaRustAPI api, MobAiOwnershipManager aiOwnership) {
        this.api = api;
        this.aiOwnership = aiOwnership;
    }

    public boolean isHibernatedByUs(UUID id) {
        return aiOwnership.isAiSuppressedByReason(id, MobAiOwnershipManager.REASON_LOD);
    }

    public void reloadConfig(FileConfiguration cfg) {
        this.enabled = cfg.getBoolean("optimizer.enabled", true);
        this.baseFullAiRange = Math.max(6.0f, (float) cfg.getDouble("optimizer.full-ai-range", 32.0));
        this.baseHibernateRange = Math.max(
            this.baseFullAiRange + 4.0f,
            (float) cfg.getDouble("optimizer.hibernate-range", 56.0)
        );
        this.fullAiRange = this.baseFullAiRange;
        this.hibernateRange = this.baseHibernateRange;
        this.crammingAlertNeighbors = cfg.getInt("optimizer.cramming-alert-neighbors", 12);
        this.skipNamedMobs = cfg.getBoolean("optimizer.skip-named-mobs", true);
        this.skipTamedMobs = cfg.getBoolean("optimizer.skip-tamed-mobs", true);

        excludedTypes.clear();
        for (String name : cfg.getStringList("optimizer.excluded-types")) {
            try {
                excludedTypes.add(EntityType.valueOf(name.trim().toUpperCase(Locale.ROOT)));
            } catch (IllegalArgumentException ignored) {
                // Ignore unknown entity types across minor versions
            }
        }
    }

    /**
     * Runs on the Main Thread every N ticks (default: 10 ticks).
     */
    public void tick() {
        if (!enabled || !api.isNativeLoaded()) {
            return;
        }

        long start = System.nanoTime();
        int totalMobs = 0;
        int fullMobs = 0;
        int hibernatedCount = 0;

        for (World world : Bukkit.getWorlds()) {
            Collection<Mob> worldMobs = world.getEntitiesByClass(Mob.class);
            if (worldMobs.isEmpty()) {
                continue;
            }

            List<Player> players = world.getPlayers();
            mobScratch.clear();

            for (Mob mob : worldMobs) {
                if (!mob.isValid() || mob.isDead()) {
                    continue;
                }
                if (isProtectedMob(mob)) {
                    aiOwnership.releaseAi(mob, MobAiOwnershipManager.REASON_LOD);
                    continue;
                }
                mobScratch.add(mob);
            }

            int mobCount = mobScratch.size();
            if (mobCount == 0) {
                continue;
            }

            int playerCount = players.size();
            mainThreadArena.ensureEntityCapacity(mobCount);
            mainThreadArena.ensureTargetCapacity(Math.max(1, playerCount));

            for (int i = 0; i < mobCount; i++) {
                Location loc = mobScratch.get(i).getLocation();
                mainThreadArena.setEntityPosition(i, (float) loc.getX(), (float) loc.getY(), (float) loc.getZ(), 0.5f);
            }

            for (int j = 0; j < playerCount; j++) {
                Location loc = players.get(j).getLocation();
                mainThreadArena.setTargetPosition(j, (float) loc.getX(), (float) loc.getY(), (float) loc.getZ());
            }

            // Single zero-copy Rust call evaluates all mobs vs all players in parallel
            int activeInWorld = api.computeAiTargets(
                mainThreadArena, mobCount, playerCount, fullAiRange, hibernateRange
            );
            totalMobs += mobCount;
            // K1 failure semantics: never consume stale outIntB buffer slots if native call failed
            if (activeInWorld < 0) {
                continue;
            }
            fullMobs += activeInWorld;

            for (int i = 0; i < mobCount; i++) {
                Mob mob = mobScratch.get(i);
                int tier = mainThreadArena.outIntB.get(i);

                if (tier == RustNative.LOD_HIBERNATE) {
                    if (aiOwnership.suppressAi(mob, MobAiOwnershipManager.REASON_LOD)) {
                        hibernatedCount++;
                    }
                } else {
                    aiOwnership.releaseAi(mob, MobAiOwnershipManager.REASON_LOD);
                }
            }
        }

        mobScratch.clear();
        this.lastTotalMobs = totalMobs;
        this.lastFullMobs = fullMobs;
        this.lastHibernatedMobs = hibernatedCount;
        this.lastRunNanos = System.nanoTime() - start;
    }

    /**
     * Restores awareness for every mob hibernated by this optimizer (called on disable/reload).
     */
    public int restoreAll() {
        return aiOwnership.restoreAllForReason(MobAiOwnershipManager.REASON_LOD);
    }

    /**
     * Runs Rust Spatial-Hash collision & density analysis on a live world to find mob-farm hotspots.
     */
    public WorldScanResult scanWorld(World world) {
        long start = System.nanoTime();
        Collection<Mob> mobs = world.getEntitiesByClass(Mob.class);
        mobScratch.clear();
        for (Mob mob : mobs) {
            if (mob.isValid() && !mob.isDead()) {
                mobScratch.add(mob);
            }
        }

        int n = mobScratch.size();
        if (n == 0) {
            return new WorldScanResult(world.getName(), 0, 0L, 0, 0, 0.0, 0.0, null);
        }

        mainThreadArena.ensureEntityCapacity(n);
        for (int i = 0; i < n; i++) {
            Mob mob = mobScratch.get(i);
            Location loc = mob.getLocation();
            float r = (float) Math.max(0.35, mob.getBoundingBox().getWidthX() * 0.55);
            mainThreadArena.setEntityPosition(i, (float) loc.getX(), (float) loc.getY(), (float) loc.getZ(), r);
        }

        long rustStart = System.nanoTime();
        long totalColliding = api.computeCollisions(mainThreadArena, n, 2.0f, 0.5f, 0.5f);
        double rustMs = (System.nanoTime() - rustStart) / 1_000_000.0;
        if (totalColliding < 0L) {
            mobScratch.clear();
            double totalMs = (System.nanoTime() - start) / 1_000_000.0;
            return new WorldScanResult(world.getName(), n, 0L, 0, 0, rustMs, totalMs, null);
        }

        int crowdedMobs = 0;
        int worstNeighbors = 0;
        Location worstLocation = null;

        for (int i = 0; i < n; i++) {
            int neighbors = mainThreadArena.outIntA.get(i);
            if (neighbors >= crammingAlertNeighbors) {
                crowdedMobs++;
            }
            if (neighbors > worstNeighbors) {
                worstNeighbors = neighbors;
                worstLocation = mobScratch.get(i).getLocation();
            }
        }

        mobScratch.clear();
        double totalMs = (System.nanoTime() - start) / 1_000_000.0;
        return new WorldScanResult(
            world.getName(), n, totalColliding, crowdedMobs, worstNeighbors, rustMs, totalMs, worstLocation
        );
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

    /**
     * Dynamically adjusts live AI activation & hibernation ranges from a Rust Brain directive
     * while preserving the configured base defaults from {@code config.yml}.
     */
    public void setDynamicRanges(float fullRange, float hibernateRange) {
        float safeFull = Float.isFinite(fullRange) ? Math.max(6.0f, fullRange) : this.baseFullAiRange;
        float safeHib = Float.isFinite(hibernateRange)
            ? Math.max(safeFull + 4.0f, hibernateRange)
            : Math.max(safeFull + 4.0f, this.baseHibernateRange);
        this.fullAiRange = safeFull;
        this.hibernateRange = safeHib;
    }

    /**
     * Restores dynamic AI ranges back to their configured base defaults.
     */
    public void resetDynamicRanges() {
        this.fullAiRange = this.baseFullAiRange;
        this.hibernateRange = this.baseHibernateRange;
    }

    public float getBaseFullAiRange() {
        return baseFullAiRange;
    }

    public float getBaseHibernateRange() {
        return baseHibernateRange;
    }

    public float getActiveFullAiRange() {
        return fullAiRange;
    }

    public float getActiveHibernateRange() {
        return hibernateRange;
    }

    public boolean isEnabled() {
        return enabled;
    }

    public double getLastRunMs() {
        return lastRunNanos / 1_000_000.0;
    }

    public int getLastTotalMobs() {
        return lastTotalMobs;
    }

    public int getLastFullMobs() {
        return lastFullMobs;
    }

    public int getLastHibernatedMobs() {
        return lastHibernatedMobs;
    }

    public int getTrackedHibernatedSize() {
        return aiOwnership.countByAiReason(MobAiOwnershipManager.REASON_LOD);
    }

    public record WorldScanResult(
        String worldName,
        int entityCount,
        long totalCollidingPairs,
        int crowdedEntityCount,
        int maxNeighborsAtHotspot,
        double rustComputeMs,
        double totalSnapshotMs,
        Location hotspotLocation
    ) {
    }
}
