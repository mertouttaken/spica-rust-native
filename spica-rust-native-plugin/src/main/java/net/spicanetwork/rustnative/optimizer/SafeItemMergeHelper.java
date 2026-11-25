package net.spicanetwork.rustnative.optimizer;

import net.spicanetwork.rustnative.api.SpicaRustAPI;
import net.spicanetwork.rustnative.nativebridge.EntityBatchArena;
import org.bukkit.Bukkit;
import org.bukkit.Location;
import org.bukkit.entity.Item;
import org.bukkit.event.entity.ItemMergeEvent;
import org.bukkit.inventory.ItemStack;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;

/**
 * Unified, event-compatible, ownership-preserving ground item merge executor shared by
 * {@link SurgeExplosionGuard} and {@link net.spicanetwork.rustnative.brain.BrainActuator} (Y5).
 *
 * Safety guarantees:
 * - Preserves display/hologram items ({@code pickupDelay >= 32767}), unlimited-lifetime items,
 *   and NPC/metadata-tagged drops.
 * - Requires identical {@link Item#getOwner()} and {@link Item#getThrower()} before merging so
 *   player-owned loot/drop protection plugins are never bypassed.
 * - Groups candidates by actual {@link ItemStack#getMaxStackSize()} (16, 64, or custom 1.21.4 up to 99)
 *   and only fires {@link ItemMergeEvent} for full vanilla absorption ({@code target + source <= maxStack})
 *   so stacker/audit plugins never observe non-vanilla partial removals.
 * - Preserves vanilla despawn age ({@code Math.min(target.ticksLived, source.ticksLived)}) so fresh
 *   items absorbed into older stacks never despawn prematurely.
 */
public final class SafeItemMergeHelper {

    public static final int INFINITE_PICKUP_DELAY_THRESHOLD = 32_767;

    private SafeItemMergeHelper() {
    }

    public static boolean isEligibleCandidate(Item item) {
        if (item == null || !item.isValid() || item.isDead()) {
            return false;
        }
        if (item.getPickupDelay() >= INFINITE_PICKUP_DELAY_THRESHOLD || item.isUnlimitedLifetime()) {
            return false;
        }
        if (item.hasMetadata("NPC") || item.customName() != null) {
            return false;
        }
        ItemStack stack = item.getItemStack();
        int maxStack = stack.getMaxStackSize();
        int amount = stack.getAmount();
        return maxStack > 1 && amount > 0 && amount < maxStack;
    }

    /**
     * Plans merge pairs in Rust (grouped by {@code maxStackSize}) and safely applies them on the
     * Main Thread with full {@link ItemMergeEvent}, owner/thrower, distance, despawn-age, and NBT checks.
     *
     * @return number of fully absorbed source item entities removed
     */
    public static long planAndApplySafeMerges(
        SpicaRustAPI api,
        EntityBatchArena arena,
        List<Item> candidates,
        float mergeRadius,
        int maxPairs
    ) {
        int totalCount = candidates.size();
        if (totalCount < 2 || maxPairs <= 0) {
            return 0L;
        }

        // Fast path: check whether all candidates share the same maxStackSize (common case: 64)
        int firstMax = candidates.get(0).getItemStack().getMaxStackSize();
        boolean uniformMax = true;
        for (int i = 1; i < totalCount; i++) {
            if (candidates.get(i).getItemStack().getMaxStackSize() != firstMax) {
                uniformMax = false;
                break;
            }
        }

        if (uniformMax) {
            return planGroup(api, arena, candidates, firstMax, mergeRadius, maxPairs);
        }

        // Partition by exact maxStackSize (e.g. 16 for ender pearls/eggs, 64 standard, up to 99 custom 1.21.4)
        Map<Integer, List<Item>> groups = new HashMap<>(4);
        for (int i = 0; i < totalCount; i++) {
            Item item = candidates.get(i);
            int maxStack = item.getItemStack().getMaxStackSize();
            if (maxStack > 1) {
                groups.computeIfAbsent(maxStack, k -> new ArrayList<>()).add(item);
            }
        }

        long mergedTotal = 0L;
        int remainingPairs = maxPairs;
        for (Map.Entry<Integer, List<Item>> entry : groups.entrySet()) {
            if (remainingPairs <= 0) {
                break;
            }
            List<Item> group = entry.getValue();
            if (group.size() < 2) {
                continue;
            }
            long merged = planGroup(api, arena, group, entry.getKey(), mergeRadius, remainingPairs);
            mergedTotal += merged;
            remainingPairs -= (int) Math.min(remainingPairs, merged);
        }
        return mergedTotal;
    }

    private static long planGroup(
        SpicaRustAPI api,
        EntityBatchArena arena,
        List<Item> candidates,
        int groupMaxStackSize,
        float mergeRadius,
        int maxPairs
    ) {
        int itemCount = candidates.size();
        if (itemCount < 2 || groupMaxStackSize <= 1 || maxPairs <= 0) {
            return 0L;
        }

        float safeRadius = Float.isFinite(mergeRadius) && mergeRadius > 0.25f
            ? Math.min(mergeRadius, 16.0f)
            : 3.5f;
        double maxDistSq = (double) safeRadius * (double) safeRadius + 0.05;

        Location scratchLoc = new Location(null, 0.0, 0.0, 0.0);
        arena.ensureEntityCapacity(itemCount);
        for (int i = 0; i < itemCount; i++) {
            Item item = candidates.get(i);
            item.getLocation(scratchLoc);
            ItemStack stack = item.getItemStack();
            int typeBucket = (stack.getType().ordinal() << 1) | (stack.hasItemMeta() ? 1 : 0);
            arena.setItem(
                i,
                (float) scratchLoc.getX(),
                (float) scratchLoc.getY(),
                (float) scratchLoc.getZ(),
                typeBucket,
                stack.getAmount()
            );
        }

        int clampedMaxPairs = Math.min(itemCount, maxPairs);
        int pairCount = api.planItemMerges(arena, itemCount, groupMaxStackSize, safeRadius, clampedMaxPairs);
        if (pairCount <= 0) {
            return 0L;
        }

        long mergedEntities = 0L;
        int clampedPairs = Math.min(pairCount, arena.getEntityCapacity());
        Location locA = new Location(null, 0.0, 0.0, 0.0);
        Location locB = new Location(null, 0.0, 0.0, 0.0);

        for (int p = 0; p < clampedPairs; p++) {
            int targetIdx = arena.outIntA.get(p);
            int sourceIdx = arena.outIntB.get(p);
            if (targetIdx < 0 || targetIdx >= itemCount
                || sourceIdx < 0 || sourceIdx >= itemCount
                || targetIdx == sourceIdx) {
                continue;
            }

            Item targetItem = candidates.get(targetIdx);
            Item sourceItem = candidates.get(sourceIdx);
            if (!isEligibleCandidate(targetItem) || !isEligibleCandidate(sourceItem)) {
                continue;
            }
            if (targetItem.getWorld() != sourceItem.getWorld()) {
                continue;
            }
            if (!Objects.equals(targetItem.getOwner(), sourceItem.getOwner())
                || !Objects.equals(targetItem.getThrower(), sourceItem.getThrower())
                || targetItem.canPlayerPickup() != sourceItem.canPlayerPickup()
                || targetItem.canMobPickup() != sourceItem.canMobPickup()) {
                continue;
            }

            targetItem.getLocation(locA);
            sourceItem.getLocation(locB);
            if (locA.distanceSquared(locB) > maxDistSq) {
                continue;
            }

            ItemStack targetStack = targetItem.getItemStack();
            ItemStack sourceStack = sourceItem.getItemStack();
            if (!targetStack.isSimilar(sourceStack)) {
                continue;
            }

            int maxStackSize = targetStack.getMaxStackSize();
            int combined = targetStack.getAmount() + sourceStack.getAmount();
            // Vanilla ItemMergeEvent contract: only fire & merge when source is fully absorbed into target
            if (sourceStack.getAmount() <= 0 || combined > maxStackSize) {
                continue;
            }

            ItemMergeEvent mergeEvent = new ItemMergeEvent(sourceItem, targetItem);
            Bukkit.getPluginManager().callEvent(mergeEvent);
            if (mergeEvent.isCancelled()) {
                continue;
            }

            // Full post-event re-validation in case a listener mutated ownership, world, location, flags, or counts (R10)
            if (!isEligibleCandidate(targetItem) || !isEligibleCandidate(sourceItem)) {
                continue;
            }
            if (targetItem.getWorld() != sourceItem.getWorld()) {
                continue;
            }
            if (!Objects.equals(targetItem.getOwner(), sourceItem.getOwner())
                || !Objects.equals(targetItem.getThrower(), sourceItem.getThrower())
                || targetItem.canPlayerPickup() != sourceItem.canPlayerPickup()
                || targetItem.canMobPickup() != sourceItem.canMobPickup()) {
                continue;
            }
            targetItem.getLocation(locA);
            sourceItem.getLocation(locB);
            if (locA.distanceSquared(locB) > maxDistSq) {
                continue;
            }
            targetStack = targetItem.getItemStack();
            sourceStack = sourceItem.getItemStack();
            if (!targetStack.isSimilar(sourceStack)) {
                continue;
            }

            maxStackSize = targetStack.getMaxStackSize();
            int postCombined = targetStack.getAmount() + sourceStack.getAmount();
            if (sourceStack.getAmount() <= 0 || postCombined > maxStackSize) {
                continue;
            }

            targetStack.setAmount(postCombined);
            targetItem.setItemStack(targetStack);
            // R9: Match Purpur 1.21.4 ItemEntity.merge bytecode (Math.max pickupDelay, younger ticksLived)
            targetItem.setPickupDelay(Math.max(targetItem.getPickupDelay(), sourceItem.getPickupDelay()));
            int clampedSourceTicks = Math.max(1, sourceItem.getTicksLived());
            if (clampedSourceTicks < targetItem.getTicksLived()) {
                targetItem.setTicksLived(clampedSourceTicks);
            }
            sourceItem.remove();
            mergedEntities++;
        }

        return mergedEntities;
    }
}
