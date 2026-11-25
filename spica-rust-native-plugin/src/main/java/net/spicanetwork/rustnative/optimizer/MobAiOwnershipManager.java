package net.spicanetwork.rustnative.optimizer;

import org.bukkit.Bukkit;
import org.bukkit.NamespacedKey;
import org.bukkit.World;
import org.bukkit.entity.Entity;
import org.bukkit.entity.EntityType;
import org.bukkit.entity.Mob;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.entity.CreatureSpawnEvent;
import org.bukkit.event.entity.EntityDeathEvent;
import org.bukkit.event.entity.EntityRemoveEvent;
import org.bukkit.event.world.EntitiesLoadEvent;
import org.bukkit.event.world.EntitiesUnloadEvent;
import org.bukkit.persistence.PersistentDataContainer;
import org.bukkit.persistence.PersistentDataType;
import org.bukkit.plugin.Plugin;

import java.util.HashMap;
import java.util.HashSet;
import java.util.Iterator;
import java.util.Map;
import java.util.Set;
import java.util.UUID;

/**
 * Unified Mob AI ({@link Mob#setAware(boolean)}) and Collision ({@link Mob#setCollidable(boolean)})
 * ownership manager with persistent crash/unload recovery (K5).
 *
 * Guarantees:
 * - Never claims ownership of or overwrites mobs whose {@code aware} or {@code collidable} state
 *   was already {@code false} due to another plugin or map design.
 * - Tags every Spica-managed suppression in the mob's {@link PersistentDataContainer} so that even
 *   after a hard crash ({@code kill -9} / power loss), mobs persisted with {@code Bukkit.Aware=0b}
 *   are safely recovered on next server start / chunk load without touching third-party statues/NPCs.
 * - Restores Spica-managed mobs on {@link EntitiesUnloadEvent} (at {@link EventPriority#LOWEST})
 *   BEFORE Purpur serializes chunk entity NBT to disk, preventing {@code Bukkit.Aware=0b} leakage
 *   and preventing unbounded UUID set growth.
 */
public final class MobAiOwnershipManager implements Listener {

    public static final int REASON_LOD = 1;
    public static final int REASON_FARM = 2;
    /** Third-party plugins may attach this Bukkit metadata key to permanently opt a mob out of Spica AI/collision suppression (R7). */
    public static final String OPT_OUT_METADATA = "spica_ignore";

    private static final byte TAG_ACTIVE = 1;

    private final NamespacedKey aiTagKey;
    private final NamespacedKey collisionTagKey;

    private final Map<UUID, Integer> aiReasons = new HashMap<>();
    private final Set<UUID> collisionOwned = new HashSet<>();

    public MobAiOwnershipManager(Plugin plugin) {
        this.aiTagKey = new NamespacedKey(plugin, "spica_ai_suppressed");
        this.collisionTagKey = new NamespacedKey(plugin, "spica_col_suppressed");
    }

    /**
     * Suppresses AI awareness on {@code mob} for the given {@code reason} bit.
     * Refuses to claim ownership if {@code mob.isAware()} is already {@code false} without Spica's
     * ownership record or PDC tag (protecting NPCs / third-party plugins).
     */
    public boolean suppressAi(Mob mob, int reason) {
        if (mob == null || !mob.isValid() || mob.isDead()) {
            return false;
        }
        // R6: BeehiveBlockEntity.addOccupant serializes Bee NBT BEFORE EntityRemoveEvent(ENTER_BLOCK);
        // never suppress Bee AI so Bukkit.Aware=0b can never be baked into beehive occupant NBT.
        // R7: Respect third-party opt-out metadata ("spica_ignore") and drop any prior Spica ownership cleanly.
        if (mob.hasMetadata(OPT_OUT_METADATA)) {
            disownOptedOutMob(mob);
            return false;
        }
        if (mob.getType() == EntityType.BEE) {
            return false;
        }
        UUID id = mob.getUniqueId();
        Integer existing = aiReasons.get(id);
        if (existing != null) {
            aiReasons.put(id, existing | reason);
            if (mob.isAware()) {
                mob.setAware(false);
            }
            setTag(mob, aiTagKey);
            return true;
        }

        // Not currently tracked in memory: only claim if currently aware OR tagged by a previous crash
        boolean taggedFromPriorRun = hasTag(mob, aiTagKey);
        if (!mob.isAware() && !taggedFromPriorRun) {
            return false;
        }

        if (mob.isAware()) {
            mob.setAware(false);
        }
        setTag(mob, aiTagKey);
        aiReasons.put(id, reason);
        return true;
    }

    /**
     * Releases the given {@code reason} bit for {@code mob}. Restores {@code setAware(true)} and
     * clears the PDC tag ONLY when no other Spica reason holds AI suppression on this mob.
     */
    public boolean releaseAi(Mob mob, int reason) {
        if (mob == null) {
            return false;
        }
        if (mob.hasMetadata(OPT_OUT_METADATA)) {
            disownOptedOutMob(mob);
            return false;
        }
        UUID id = mob.getUniqueId();
        Integer existing = aiReasons.get(id);
        if (existing == null) {
            if (hasTag(mob, aiTagKey)) {
                clearTag(mob, aiTagKey);
                if (!mob.isAware()) {
                    mob.setAware(true);
                    return true;
                }
            }
            return false;
        }

        int next = existing & ~reason;
        if (next != 0) {
            aiReasons.put(id, next);
            return false;
        }

        aiReasons.remove(id);
        clearTag(mob, aiTagKey);
        if (!mob.isAware()) {
            mob.setAware(true);
            return true;
        }
        return false;
    }

    /**
     * Suppresses entity-entity collision pushing on {@code mob}, refusing to touch mobs already
     * marked non-collidable by another plugin.
     */
    public boolean suppressCollision(Mob mob) {
        if (mob == null || !mob.isValid() || mob.isDead()) {
            return false;
        }
        if (mob.hasMetadata(OPT_OUT_METADATA)) {
            disownOptedOutMob(mob);
            return false;
        }
        if (mob.getType() == EntityType.BEE) {
            return false;
        }
        UUID id = mob.getUniqueId();
        if (collisionOwned.contains(id)) {
            if (mob.isCollidable()) {
                mob.setCollidable(false);
            }
            setTag(mob, collisionTagKey);
            return true;
        }

        boolean taggedFromPriorRun = hasTag(mob, collisionTagKey);
        if (!mob.isCollidable() && !taggedFromPriorRun) {
            return false;
        }

        if (mob.isCollidable()) {
            mob.setCollidable(false);
        }
        setTag(mob, collisionTagKey);
        collisionOwned.add(id);
        return true;
    }

    /**
     * Restores collision on {@code mob} ONLY if Spica owns collision suppression for it.
     */
    public boolean releaseCollision(Mob mob) {
        if (mob == null) {
            return false;
        }
        if (mob.hasMetadata(OPT_OUT_METADATA)) {
            disownOptedOutMob(mob);
            return false;
        }
        UUID id = mob.getUniqueId();
        boolean owned = collisionOwned.remove(id);
        boolean tagged = hasTag(mob, collisionTagKey);
        if (!owned && !tagged) {
            return false;
        }
        clearTag(mob, collisionTagKey);
        if (!mob.isCollidable()) {
            mob.setCollidable(true);
            return true;
        }
        return false;
    }

    /**
     * Releases all Spica-owned AI and collision suppressions for {@code mob} upon player interaction.
     * Never alters mobs that Spica did not suppress.
     */
    public boolean releaseAllForInteract(Mob mob) {
        if (mob == null) {
            return false;
        }
        if (mob.hasMetadata(OPT_OUT_METADATA)) {
            disownOptedOutMob(mob);
            return false;
        }
        UUID id = mob.getUniqueId();
        boolean changed = false;

        boolean ownedAi = aiReasons.remove(id) != null || hasTag(mob, aiTagKey);
        if (ownedAi) {
            clearTag(mob, aiTagKey);
            if (!mob.isAware()) {
                mob.setAware(true);
            }
            changed = true;
        }

        boolean ownedCol = collisionOwned.remove(id) || hasTag(mob, collisionTagKey);
        if (ownedCol) {
            clearTag(mob, collisionTagKey);
            if (!mob.isCollidable()) {
                mob.setCollidable(true);
            }
            changed = true;
        }

        return changed;
    }

    public boolean isAiSuppressedByReason(UUID id, int reason) {
        Integer mask = aiReasons.get(id);
        return mask != null && (mask & reason) != 0;
    }

    public boolean isCollisionSuppressed(UUID id) {
        return collisionOwned.contains(id);
    }

    public int countByAiReason(int reason) {
        int count = 0;
        for (int mask : aiReasons.values()) {
            if ((mask & reason) != 0) {
                count++;
            }
        }
        return count;
    }

    public int getCollisionTrackedCount() {
        return collisionOwned.size();
    }

    /**
     * Releases a single reason across all tracked loaded mobs (e.g. when one optimizer is reloaded
     * or disabled independently).
     */
    public int restoreAllForReason(int reason) {
        int restored = 0;
        Iterator<Map.Entry<UUID, Integer>> it = aiReasons.entrySet().iterator();
        while (it.hasNext()) {
            Map.Entry<UUID, Integer> entry = it.next();
            int next = entry.getValue() & ~reason;
            if (next != 0) {
                entry.setValue(next);
                continue;
            }
            UUID id = entry.getKey();
            it.remove();
            Entity entity = Bukkit.getEntity(id);
            if (entity instanceof Mob mob) {
                clearTag(mob, aiTagKey);
                if (!mob.hasMetadata(OPT_OUT_METADATA) && !mob.isAware()) {
                    mob.setAware(true);
                    restored++;
                }
            }
        }
        return restored;
    }

    /**
     * Restores collision for all Spica-owned collision-suppressed mobs.
     */
    public int restoreAllCollisions() {
        int restored = 0;
        for (UUID id : collisionOwned) {
            Entity entity = Bukkit.getEntity(id);
            if (entity instanceof Mob mob) {
                clearTag(mob, collisionTagKey);
                if (!mob.hasMetadata(OPT_OUT_METADATA) && !mob.isCollidable()) {
                    mob.setCollidable(true);
                    restored++;
                }
            }
        }
        collisionOwned.clear();
        return restored;
    }

    /**
     * Full clean restore on plugin disable: restores every tracked mob AND sweeps loaded worlds
     * for any Spica PDC-tagged mobs so nothing is left suppressed.
     */
    public int restoreEverything() {
        int restored = 0;
        Set<UUID> allIds = new HashSet<>(aiReasons.size() + collisionOwned.size());
        allIds.addAll(aiReasons.keySet());
        allIds.addAll(collisionOwned);

        for (UUID id : allIds) {
            Entity entity = Bukkit.getEntity(id);
            if (entity instanceof Mob mob) {
                boolean ownAi = aiReasons.containsKey(id) || hasTag(mob, aiTagKey);
                boolean ownCol = collisionOwned.contains(id) || hasTag(mob, collisionTagKey);
                if ((ownAi || ownCol) && restoreTaggedMob(mob, ownAi, ownCol)) {
                    restored++;
                }
            }
        }
        aiReasons.clear();
        collisionOwned.clear();
        restored += recoverAllLoadedWorlds();
        return restored;
    }

    /**
     * Startup/crash recovery sweep over all already-loaded worlds: recovers any mob still holding
     * a Spica PDC tag from a previous abrupt crash/kill.
     */
    public int recoverAllLoadedWorlds() {
        int recovered = 0;
        for (World world : Bukkit.getWorlds()) {
            for (Mob mob : world.getEntitiesByClass(Mob.class)) {
                UUID id = mob.getUniqueId();
                boolean needAi = !aiReasons.containsKey(id) && hasTag(mob, aiTagKey);
                boolean needCol = !collisionOwned.contains(id) && hasTag(mob, collisionTagKey);
                if (needAi || needCol) {
                    if (restoreTaggedMob(mob, needAi, needCol)) {
                        recovered++;
                    }
                }
            }
        }
        return recovered;
    }

    /**
     * Runs BEFORE Purpur writes unloading chunk entities to disk so Spica's temporary
     * {@code setAware(false)} never leaks permanently into chunk NBT ({@code Bukkit.Aware=0b}).
     */
    @EventHandler(priority = EventPriority.LOWEST)
    public void onEntitiesUnload(EntitiesUnloadEvent event) {
        for (Entity entity : event.getEntities()) {
            if (!(entity instanceof Mob mob)) {
                continue;
            }
            UUID id = mob.getUniqueId();
            boolean ownedAi = aiReasons.remove(id) != null || hasTag(mob, aiTagKey);
            boolean ownedCol = collisionOwned.remove(id) || hasTag(mob, collisionTagKey);
            if (ownedAi || ownedCol) {
                restoreTaggedMob(mob, ownedAi, ownedCol);
            }
        }
    }

    /**
     * Crash-recovery hook on chunk load: if the server previously crashed before unload/disable
     * could clean up Spica PDC tags, immediately wake those mobs on load.
     */
    @EventHandler(priority = EventPriority.MONITOR)
    public void onEntitiesLoad(EntitiesLoadEvent event) {
        for (Entity entity : event.getEntities()) {
            if (!(entity instanceof Mob mob)) {
                continue;
            }
            UUID id = mob.getUniqueId();
            boolean orphanedAi = !aiReasons.containsKey(id) && hasTag(mob, aiTagKey);
            boolean orphanedCol = !collisionOwned.contains(id) && hasTag(mob, collisionTagKey);
            if (orphanedAi || orphanedCol) {
                restoreTaggedMob(mob, orphanedAi, orphanedCol);
            }
        }
    }

    /**
     * R6: Recovers any mob emerging from a block (e.g. legacy beehive occupant spawned via
     * {@link CreatureSpawnEvent.SpawnReason#BEEHIVE}) that still carries an orphaned Spica PDC tag.
     */
    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onCreatureSpawn(CreatureSpawnEvent event) {
        if (!(event.getEntity() instanceof Mob mob)) {
            return;
        }
        UUID id = mob.getUniqueId();
        boolean orphanedAi = !aiReasons.containsKey(id) && hasTag(mob, aiTagKey);
        boolean orphanedCol = !collisionOwned.contains(id) && hasTag(mob, collisionTagKey);
        if (orphanedAi || orphanedCol) {
            restoreTaggedMob(mob, orphanedAi, orphanedCol);
        }
    }

    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onEntityDeath(EntityDeathEvent event) {
        UUID id = event.getEntity().getUniqueId();
        aiReasons.remove(id);
        collisionOwned.remove(id);
    }

    @EventHandler(priority = EventPriority.LOWEST)
    public void onEntityRemove(EntityRemoveEvent event) {
        Entity entity = event.getEntity();
        UUID id = entity.getUniqueId();
        boolean ownedAi = aiReasons.remove(id) != null;
        boolean ownedCol = collisionOwned.remove(id);
        if (entity instanceof Mob mob) {
            EntityRemoveEvent.Cause cause = event.getCause();
            // Restore before serialization/removal for non-terminal causes (e.g. ENTER_BLOCK for bees,
            // PLAYER_QUIT for ridden vehicles, UNLOAD, PLUGIN) so Bukkit.Aware=0b never persists.
            boolean shouldRestore = cause != EntityRemoveEvent.Cause.DEATH
                && cause != EntityRemoveEvent.Cause.OUT_OF_WORLD
                && cause != EntityRemoveEvent.Cause.DESPAWN;
            if (shouldRestore) {
                boolean needAi = ownedAi || hasTag(mob, aiTagKey);
                boolean needCol = ownedCol || hasTag(mob, collisionTagKey);
                if (needAi || needCol) {
                    restoreTaggedMob(mob, needAi, needCol);
                }
            }
        }
    }

    private void disownOptedOutMob(Mob mob) {
        UUID id = mob.getUniqueId();
        aiReasons.remove(id);
        collisionOwned.remove(id);
        clearTag(mob, aiTagKey);
        clearTag(mob, collisionTagKey);
    }

    private boolean restoreTaggedMob(Mob mob, boolean restoreAi, boolean restoreCol) {
        if (mob.hasMetadata(OPT_OUT_METADATA)) {
            disownOptedOutMob(mob);
            return false;
        }
        boolean changed = false;
        if (restoreAi) {
            clearTag(mob, aiTagKey);
            if (!mob.isAware()) {
                mob.setAware(true);
                changed = true;
            }
        }
        if (restoreCol) {
            clearTag(mob, collisionTagKey);
            if (!mob.isCollidable()) {
                mob.setCollidable(true);
                changed = true;
            }
        }
        return changed;
    }

    private static boolean hasTag(Mob mob, NamespacedKey key) {
        PersistentDataContainer pdc = mob.getPersistentDataContainer();
        return pdc.has(key, PersistentDataType.BYTE);
    }

    private static void setTag(Mob mob, NamespacedKey key) {
        PersistentDataContainer pdc = mob.getPersistentDataContainer();
        if (!pdc.has(key, PersistentDataType.BYTE)) {
            pdc.set(key, PersistentDataType.BYTE, TAG_ACTIVE);
        }
    }

    private static void clearTag(Mob mob, NamespacedKey key) {
        PersistentDataContainer pdc = mob.getPersistentDataContainer();
        if (pdc.has(key, PersistentDataType.BYTE)) {
            pdc.remove(key);
        }
    }
}
