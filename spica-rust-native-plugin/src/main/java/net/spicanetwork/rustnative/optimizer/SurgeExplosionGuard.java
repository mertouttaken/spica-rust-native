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
import org.bukkit.inventory.ItemStack;
import org.bukkit.plugin.Plugin;
import org.bukkit.util.Vector;

import java.util.ArrayList;
import java.util.Collection;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
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
    private static final int INFINITE_PICKUP_DELAY_THRESHOLD = 32_767;
    private static final int MAX_EXPLOSION_MERGE_PAIRS = 2048;

    private final Plugin plugin;
    private final SpicaRustAPI api;
    private final EntityBatchArena surgeArena = new EntityBatchArena(1024, 64);
    private final List<Player> glidingScratch = new ArrayList<>(64);
    private final List<Item> itemScratch = new ArrayList<>(1024);
    private final Location scratchLoc = new Location(null, 0.0, 0.0, 0.0);

    private final Set<UUID> pendingExplosionWorlds = new HashSet<>();
    private final Map<UUID, Long> actionbarCooldownUntilMs = new HashMap<>();

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

    // Rolling 1-second chunk-load telemetry (Main Thread only)
    private long chunkWindowStartMs = System.currentTimeMillis();
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
     * Clears transient pending queues and cooldown maps on disable or reload.
     */
    public void resetPending() {
        pendingExplosionWorlds.clear();
        actionbarCooldownUntilMs.clear();
        chunkLoadsCurrentSec = 0;
        chunkWindowStartMs = System.currentTimeMillis();
        lastGlidingPlayers = 0;
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void onChunkLoad(ChunkLoadEvent event) {
        if (!enabled) {
            return;
        }
        chunkLoadsCurrentSec += event.isNewChunk() ? 2 : 1;
    }

    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void onEntityExplode(EntityExplodeEvent event) {
        if (!enabled || !explosionEnabled) {
            return;
        }
        if (event.blockList().size() < explosionMinBlocksTrigger) {
            return;
        }
        World world = event.getLocation().getWorld();
        if (world != null) {
            scheduleExplosionBurstMerge(world);
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
        World world = event.getBlock().getWorld();
        if (world != null) {
            scheduleExplosionBurstMerge(world);
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
        rotateChunkLoadWindow(nowMs);

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

        glidingScratch.clear();
        for (Player player : Bukkit.getOnlinePlayers()) {
            if (player.isOnline()
                && player.isValid()
                && player.isGliding()
                && !player.hasPermission(ELYTRA_BYPASS_PERMISSION)) {
                glidingScratch.add(player);
            }
        }

        int count = glidingScratch.size();
        this.lastGlidingPlayers = count;
        if (count == 0) {
            return;
        }

        if (!actionbarCooldownUntilMs.isEmpty()) {
            actionbarCooldownUntilMs.entrySet().removeIf(entry -> entry.getValue() <= nowMs);
        }

        surgeArena.ensureEntityCapacity(count);
        for (int i = 0; i < count; i++) {
            Player player = glidingScratch.get(i);
            player.getLocation(scratchLoc);
            Vector vel = player.getVelocity();

            surgeArena.x.put(i, (float) scratchLoc.getX());
            surgeArena.y.put(i, (float) scratchLoc.getY());
            surgeArena.z.put(i, (float) scratchLoc.getZ());
            surgeArena.setEntityVelocity(i, (float) vel.getX(), (float) vel.getY(), (float) vel.getZ());
            surgeArena.flags.put(i, 1);
        }

        long rustStart = System.nanoTime();
        int evalResult = api.evaluatePlayerSurges(surgeArena, count, effectiveBrainState, baseMaxElytraSpeed);
        this.lastRustComputeNanos = System.nanoTime() - rustStart;
        if (evalResult < 0) {
            return;
        }

        for (int i = 0; i < count; i++) {
            int action = surgeArena.outIntA.get(i);
            if (action != RustNative.SURGE_CLAMP_VELOCITY) {
                continue;
            }

            float cvx = surgeArena.outX.get(i);
            float cvy = surgeArena.outY.get(i);
            float cvz = surgeArena.outZ.get(i);
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

    private void rotateChunkLoadWindow(long nowMs) {
        long elapsedMs = nowMs - chunkWindowStartMs;
        if (elapsedMs >= CHUNK_WINDOW_MS) {
            if (elapsedMs >= CHUNK_WINDOW_MS * 2L) {
                this.lastChunkLoadsPerSec = (int) Math.round((chunkLoadsCurrentSec * 1_000.0) / elapsedMs);
            } else {
                this.lastChunkLoadsPerSec = chunkLoadsCurrentSec;
            }
            this.chunkLoadsCurrentSec = 0;
            this.chunkWindowStartMs = nowMs;
        } else if (chunkLoadsCurrentSec > lastChunkLoadsPerSec) {
            this.lastChunkLoadsPerSec = chunkLoadsCurrentSec;
        }
    }

    private void scheduleExplosionBurstMerge(World world) {
        if (!plugin.isEnabled() || !api.isNativeLoaded()) {
            return;
        }
        UUID worldId = world.getUID();
        if (!pendingExplosionWorlds.add(worldId)) {
            return;
        }
        Bukkit.getScheduler().runTaskLater(plugin, () -> processExplosionBurstMerge(worldId), 1L);
    }

    private void processExplosionBurstMerge(UUID worldId) {
        pendingExplosionWorlds.remove(worldId);

        if (!enabled || !explosionEnabled || !api.isNativeLoaded()) {
            return;
        }
        World world = Bukkit.getWorld(worldId);
        if (world == null) {
            return;
        }

        Collection<Item> groundItems = world.getEntitiesByClass(Item.class);
        if (groundItems.size() < 2) {
            return;
        }

        itemScratch.clear();
        for (Item item : groundItems) {
            if (!item.isValid() || item.isDead()) {
                continue;
            }
            if (item.getPickupDelay() >= INFINITE_PICKUP_DELAY_THRESHOLD) {
                continue;
            }
            ItemStack stack = item.getItemStack();
            int maxStack = stack.getMaxStackSize();
            int amount = stack.getAmount();
            if (maxStack <= 1 || amount <= 0 || amount >= maxStack) {
                continue;
            }
            itemScratch.add(item);
        }

        int itemCount = itemScratch.size();
        if (itemCount < 2) {
            return;
        }

        this.explosionBurstsBuffered++;

        surgeArena.ensureEntityCapacity(itemCount);
        for (int i = 0; i < itemCount; i++) {
            Item item = itemScratch.get(i);
            item.getLocation(scratchLoc);
            ItemStack stack = item.getItemStack();
            int typeBucket = (stack.getType().ordinal() << 1) | (stack.hasItemMeta() ? 1 : 0);
            surgeArena.setItem(
                i,
                (float) scratchLoc.getX(),
                (float) scratchLoc.getY(),
                (float) scratchLoc.getZ(),
                typeBucket,
                stack.getAmount()
            );
        }

        int maxPairs = Math.min(itemCount, MAX_EXPLOSION_MERGE_PAIRS);
        int pairCount = api.planItemMerges(surgeArena, itemCount, 64, explosionMergeRadius, maxPairs);
        if (pairCount <= 0) {
            return;
        }

        long merged = 0L;
        int clampedPairs = Math.min(pairCount, surgeArena.getEntityCapacity());
        for (int p = 0; p < clampedPairs; p++) {
            int targetIdx = surgeArena.outIntA.get(p);
            int sourceIdx = surgeArena.outIntB.get(p);
            if (targetIdx < 0 || targetIdx >= itemCount
                || sourceIdx < 0 || sourceIdx >= itemCount
                || targetIdx == sourceIdx) {
                continue;
            }

            Item targetItem = itemScratch.get(targetIdx);
            Item sourceItem = itemScratch.get(sourceIdx);
            if (!targetItem.isValid() || !sourceItem.isValid()) {
                continue;
            }
            if (targetItem.getPickupDelay() >= INFINITE_PICKUP_DELAY_THRESHOLD
                || sourceItem.getPickupDelay() >= INFINITE_PICKUP_DELAY_THRESHOLD) {
                continue;
            }

            ItemStack targetStack = targetItem.getItemStack();
            ItemStack sourceStack = sourceItem.getItemStack();

            // Critical NBT / enchantment / PDC safety gate before merging stacks
            if (!targetStack.isSimilar(sourceStack)) {
                continue;
            }

            int maxStackSize = targetStack.getMaxStackSize();
            int freeSpace = maxStackSize - targetStack.getAmount();
            int sourceAmount = sourceStack.getAmount();
            if (freeSpace <= 0 || sourceAmount <= 0) {
                continue;
            }

            int transfer = Math.min(freeSpace, sourceAmount);
            targetStack.setAmount(targetStack.getAmount() + transfer);
            targetItem.setItemStack(targetStack);

            int remaining = sourceAmount - transfer;
            if (remaining <= 0) {
                sourceItem.remove();
                merged++;
            } else {
                sourceStack.setAmount(remaining);
                sourceItem.setItemStack(sourceStack);
            }
        }

        this.explosionItemsMergedTotal += merged;
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
