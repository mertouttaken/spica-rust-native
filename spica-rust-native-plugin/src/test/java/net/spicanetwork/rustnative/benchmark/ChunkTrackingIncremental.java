package net.spicanetwork.rustnative.benchmark;

import com.destroystokyo.paper.event.entity.EntityAddToWorldEvent;
import com.destroystokyo.paper.event.entity.EntityRemoveFromWorldEvent;
import it.unimi.dsi.fastutil.ints.Int2ObjectOpenHashMap;
import java.nio.ByteBuffer;
import java.nio.LongBuffer;
import java.util.IdentityHashMap;
import java.util.Map;
import java.util.concurrent.ConcurrentLinkedQueue;
import net.spicanetwork.rustnative.nativebridge.RustNative;
import org.bukkit.World;
import org.bukkit.entity.Mob;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.HandlerList;
import org.bukkit.event.Listener;
import org.bukkit.event.world.WorldUnloadEvent;

/** Maintains the shadow roster from world lifecycle events and shares only changed slots. */
final class ChunkTrackingIncremental implements Listener, AutoCloseable {
    private static final int MAX_MOBS = ChunkTrackingQuery.MAX_MOBS;
    private static final int DELTA_WORDS = 3;

    private final Map<World, WorldIndex> worlds = new IdentityHashMap<>();
    private final ByteBuffer deltaBytes = ChunkTrackingQuery.direct(MAX_MOBS * DELTA_WORDS * Long.BYTES);
    private final ByteBuffer pairBytes = ChunkTrackingQuery.direct(ChunkTrackingQuery.MAX_PLAYERS * MAX_MOBS / Byte.SIZE);
    private final LongBuffer deltas = deltaBytes.asLongBuffer();
    private final LongBuffer pairs = pairBytes.asLongBuffer();
    private final boolean dirtyOnly;
    private final Int2ObjectOpenHashMap<Binding> dirtyBindings;
    private final ConcurrentLinkedQueue<Integer> offThread;
    private final Thread owner = Thread.currentThread();
    private long eventNanos;

    ChunkTrackingIncremental(boolean dirtyOnly) {
        this.dirtyOnly = dirtyOnly;
        this.dirtyBindings = dirtyOnly ? new Int2ObjectOpenHashMap<>() : null;
        this.offThread = dirtyOnly ? new ConcurrentLinkedQueue<>() : null;
    }

    void attachMovementFeed() {
        ChunkTrackingPositionFeed.attach(this::mark);
    }

    WorldIndex world(World world) {
        return worlds.computeIfAbsent(world, loaded -> {
            WorldIndex index = new WorldIndex(loaded, dirtyOnly);
            if (dirtyOnly) {
                for (int slot = 0; slot < index.slotCount; slot++) {
                    bind(index, index.mobs[slot], slot);
                }
            }
            return index;
        });
    }

    int query(WorldIndex world, int playerCount, ByteBuffer players) {
        int result = RustNative.nativeUpdateTrackingIndex(
            world.handle, world.slotCount, world.upserts + world.removals, deltaBytes, playerCount, players, pairBytes
        );
        if (result < 0) {
            throw new IllegalStateException("Native incremental tracking rejected the batch");
        }
        return result;
    }

    void stage(WorldIndex world) {
        if (world.overflowed) {
            throw new IllegalStateException("World exceeds incremental shadow capacity");
        }
        world.upserts = 0;
        world.removals = 0;
        world.mobChecks = 0;
        if (dirtyOnly) {
            stageDirty(world);
            return;
        }
        world.liveCount = 0;
        for (int slot = 0; slot < world.slotCount; slot++) {
            Mob mob = world.mobs[slot];
            boolean published = world.published[slot];
            if (mob == null || !mob.isValid() || mob.isDead()) {
                if (published) {
                    write(world.upserts + world.removals++, ~((long) slot), 0, 0);
                    world.published[slot] = false;
                }
                continue;
            }

            world.liveCount++;
            // Sample authoritative coordinates, including pushes and passenger motion between events.
            double x = mob.getX();
            double z = mob.getZ();
            if (!published || x != world.lastX[slot] || z != world.lastZ[slot]) {
                write(world.upserts++ + world.removals, slot, x, z);
                world.published[slot] = true;
                world.lastX[slot] = x;
                world.lastZ[slot] = z;
            }
        }
        world.mobChecks = world.liveCount;
    }

    private void stageDirty(WorldIndex world) {
        Integer queued;
        while ((queued = offThread.poll()) != null) {
            enqueue(queued.intValue());
        }
        for (int i = 0; i < world.dirtyCount; i++) {
            int slot = world.dirtySlots[i];
            world.queued[slot] = false;
            Mob mob = world.mobs[slot];
            boolean published = world.published[slot];
            if (mob == null || !mob.isValid() || mob.isDead()) {
                if (published) {
                    write(world.upserts + world.removals, ~((long) slot), 0, 0);
                    world.removals++;
                    world.published[slot] = false;
                    world.liveCount--;
                }
                continue;
            }
            world.mobChecks++;
            double x = mob.getX();
            double z = mob.getZ();
            if (!published || x != world.lastX[slot] || z != world.lastZ[slot]) {
                write(world.upserts + world.removals, slot, x, z);
                world.upserts++;
                world.published[slot] = true;
                world.lastX[slot] = x;
                world.lastZ[slot] = z;
                if (!published) {
                    world.liveCount++;
                }
            }
        }
        world.dirtyCount = 0;
    }

    private void bind(WorldIndex world, Mob mob, int slot) {
        dirtyBindings.put(mob.getEntityId(), new Binding(world, slot));
        world.enqueue(slot);
    }

    private void mark(int id) {
        if (Thread.currentThread() != owner) {
            offThread.add(id);
            return;
        }
        long start = System.nanoTime();
        enqueue(id);
        eventNanos += System.nanoTime() - start;
    }

    private void enqueue(int id) {
        Binding binding = dirtyBindings.get(id);
        if (binding != null) {
            binding.world.enqueue(binding.slot);
        }
    }

    private void write(int index, long token, double x, double z) {
        int offset = index * DELTA_WORDS;
        deltas.put(offset, token).put(offset + 1, Double.doubleToRawLongBits(x))
            .put(offset + 2, Double.doubleToRawLongBits(z));
    }

    int slotOf(WorldIndex world, Mob mob) {
        Integer slot = world.slots.get(mob);
        if (slot == null) {
            throw new IllegalStateException("Incremental roster missed a mob from the full snapshot");
        }
        return slot;
    }

    boolean candidate(WorldIndex world, int player, int slot) {
        return ChunkTrackingQuery.isSet(pairs, player * world.slotCount + slot);
    }

    long takeEventNanos() {
        long elapsed = eventNanos;
        eventNanos = 0;
        return elapsed;
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void added(EntityAddToWorldEvent event) {
        if (event.getEntity() instanceof Mob mob) {
            long start = System.nanoTime();
            WorldIndex world = worlds.get(event.getWorld());
            if (world != null) {
                int slot = world.add(mob);
                if (dirtyOnly && slot >= 0) {
                    bind(world, mob, slot);
                }
            }
            eventNanos += System.nanoTime() - start;
        }
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void removed(EntityRemoveFromWorldEvent event) {
        if (event.getEntity() instanceof Mob mob) {
            long start = System.nanoTime();
            WorldIndex world = worlds.get(event.getWorld());
            if (world != null) {
                int slot = world.remove(mob);
                if (dirtyOnly && slot >= 0) {
                    int id = mob.getEntityId();
                    Binding binding = dirtyBindings.get(id);
                    if (binding != null && binding.world == world && binding.slot == slot) {
                        dirtyBindings.remove(id);
                    }
                    world.enqueue(slot);
                }
            }
            eventNanos += System.nanoTime() - start;
        }
    }

    @EventHandler(priority = EventPriority.MONITOR, ignoreCancelled = true)
    public void unloaded(WorldUnloadEvent event) {
        WorldIndex world = worlds.remove(event.getWorld());
        if (world != null) {
            if (dirtyOnly) {
                dirtyBindings.values().removeIf(binding -> binding.world == world);
            }
            world.close();
        }
    }

    @Override
    public void close() {
        if (dirtyOnly) {
            ChunkTrackingPositionFeed.detach();
            dirtyBindings.clear();
            offThread.clear();
        }
        HandlerList.unregisterAll(this);
        worlds.values().forEach(WorldIndex::close);
        worlds.clear();
        eventNanos = 0;
    }

    private record Binding(WorldIndex world, int slot) { }

    static final class WorldIndex implements AutoCloseable {
        private final Mob[] mobs = new Mob[MAX_MOBS];
        private final boolean[] published = new boolean[MAX_MOBS];
        private final double[] lastX = new double[MAX_MOBS];
        private final double[] lastZ = new double[MAX_MOBS];
        private final int[] freeSlots = new int[MAX_MOBS];
        private final Map<Mob, Integer> slots = new IdentityHashMap<>();
        private final int[] dirtySlots;
        private final boolean[] queued;
        private final long handle;
        private int freeCount;
        private int slotCount;
        private boolean overflowed;
        private int dirtyCount;
        int liveCount;
        int upserts;
        int removals;
        int mobChecks;

        WorldIndex(World world, boolean dirtyOnly) {
            dirtySlots = dirtyOnly ? new int[MAX_MOBS] : null;
            queued = dirtyOnly ? new boolean[MAX_MOBS] : null;
            for (Mob mob : world.getEntitiesByClass(Mob.class)) {
                add(mob);
            }
            handle = RustNative.nativeOpenTrackingIndex();
            if (handle == 0) {
                throw new IllegalStateException("Native tracking index could not be opened");
            }
        }

        private int add(Mob mob) {
            if (slots.containsKey(mob)) {
                return -1;
            }
            if (freeCount == 0 && slotCount == MAX_MOBS) {
                overflowed = true;
                return -1;
            }
            int slot = freeCount > 0 ? freeSlots[--freeCount] : slotCount++;
            slots.put(mob, slot);
            mobs[slot] = mob;
            // Slot reuse forces an upsert even when the replacement has identical coordinates.
            lastX[slot] = Double.NaN;
            return slot;
        }

        private int remove(Mob mob) {
            Integer slot = slots.remove(mob);
            if (slot != null) {
                mobs[slot] = null;
                freeSlots[freeCount++] = slot;
                return slot;
            }
            return -1;
        }

        private void enqueue(int slot) {
            if (!queued[slot]) {
                queued[slot] = true;
                dirtySlots[dirtyCount++] = slot;
            }
        }

        @Override
        public void close() {
            RustNative.nativeCloseTrackingIndex(handle);
        }
    }
}
