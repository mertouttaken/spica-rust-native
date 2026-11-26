package net.spicanetwork.rustnative.benchmark;

import com.destroystokyo.paper.event.server.ServerTickEndEvent;
import com.destroystokyo.paper.event.server.ServerTickStartEvent;
import com.sun.management.OperatingSystemMXBean;
import java.io.IOException;
import java.lang.management.ManagementFactory;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.Locale;
import net.spicanetwork.rustnative.nativebridge.RustNative;
import org.bukkit.Bukkit;
import org.bukkit.Location;
import org.bukkit.World;
import org.bukkit.command.Command;
import org.bukkit.command.CommandSender;
import org.bukkit.command.ConsoleCommandSender;
import org.bukkit.entity.Mob;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.plugin.java.JavaPlugin;

/** Opt-in test plugin for an isolated Purpur process. Computes and observes only. */
public final class ChunkTrackingShadow extends JavaPlugin implements Listener {
    private enum Backend { FULL, JAVA, RUST, INCREMENTAL, VERIFY }
    private enum Phase { IDLE, DENSE, SPREAD, TELEPORT }

    private final ChunkTrackingQuery query = new ChunkTrackingQuery();
    private final Location scratch = new Location(null, 0, 0, 0);
    private final OperatingSystemMXBean os = ManagementFactory.getPlatformMXBean(OperatingSystemMXBean.class);
    private final Sample[] samples = new Sample[16_384];
    private final Mob[] verificationMobs = new Mob[ChunkTrackingQuery.MAX_MOBS];
    private Backend backend;
    private ChunkTrackingIncremental incremental;
    private boolean interleaved;
    private Phase phase = Phase.IDLE;
    private int window;
    private int tickWindow;
    private int sampleCount;
    private int tick;
    private long totalNs;
    private long indexNs;
    private long evaluationNs;
    private int players;
    private int mobs;
    private int matches;
    private int comparisons;
    private int upserts;
    private int removals;
    private boolean failed;

    @Override
    public void onEnable() {
        String mode = System.getProperty("spica.shadow.backend").toUpperCase(Locale.ROOT);
        interleaved = mode.equals("INTERLEAVED");
        System.load(Path.of(System.getProperty("spica.shadow.library")).toAbsolutePath().toString());
        RustNative.nativeInitPool(4);
        useBackend(interleaved ? Backend.FULL : Backend.valueOf(mode));
        if (backend == Backend.VERIFY) {
            verifyBoundary();
        }
        Bukkit.getPluginManager().registerEvents(this, this);
        getLogger().info("SHADOW_READY " + mode);
    }

    private static boolean usesIncremental(Backend mode) {
        return mode == Backend.INCREMENTAL || mode == Backend.VERIFY;
    }

    private void useBackend(Backend next) {
        if (usesIncremental(next) && !usesIncremental(backend)) {
            if (incremental == null) {
                incremental = new ChunkTrackingIncremental();
            }
            Bukkit.getPluginManager().registerEvents(incremental, this);
        } else if (usesIncremental(backend) && !usesIncremental(next)) {
            incremental.close();
        }
        backend = next;
    }

    @Override
    public void onDisable() {
        if (incremental != null) {
            incremental.close();
        }
    }

    private void verifyBoundary() {
        query.player(0, 0.0, -0.25);
        query.player(1, -49.0, -0.25);
        query.mob(0, -48.0, -0.25);
        query.mob(1, -48.01, -0.25);
        query.mob(2, 0.0, 48.0);
        if (query.verify(2, 3) != 3) {
            throw new IllegalStateException("Incorrect boundary candidate set");
        }
    }

    @Override
    public boolean onCommand(CommandSender sender, Command command, String label, String[] args) {
        if (!(sender instanceof ConsoleCommandSender) || args.length < 1 || args.length > 2) {
            return false;
        }
        if (args.length == 1 && args[0].equals("dump") && phase == Phase.IDLE) {
            Sample[] snapshot = Arrays.copyOf(samples, sampleCount);
            Thread.ofPlatform().name("spica-shadow-report").start(() -> writeSamples(snapshot));
            return true;
        }
        try {
            Phase nextPhase = Phase.valueOf(args[0].toUpperCase(Locale.ROOT));
            if (args.length == 2) {
                Backend nextBackend = Backend.valueOf(args[1].toUpperCase(Locale.ROOT));
                if (!interleaved || nextBackend == Backend.VERIFY) {
                    return false;
                }
                useBackend(nextBackend);
            }
            phase = nextPhase;
        } catch (IllegalArgumentException invalidPhase) {
            return false;
        }
        window++;
        getLogger().info("SHADOW_PHASE " + phase + " " + System.nanoTime() + " " + os.getProcessCpuTime() + " " + window);
        return true;
    }

    @EventHandler
    public void tickStart(ServerTickStartEvent event) {
        if (failed || Bukkit.getOnlinePlayers().size() != 20) {
            return;
        }
        long start = System.nanoTime();
        tickWindow = window;
        players = 0;
        mobs = 0;
        matches = 0;
        comparisons = 0;
        upserts = 0;
        removals = 0;
        indexNs = 0;
        evaluationNs = 0;
        try {
            for (World world : Bukkit.getWorlds()) {
                scanWorld(world);
            }
            totalNs = System.nanoTime() - start;
            tick = event.getTickNumber();
        } catch (RuntimeException | LinkageError error) {
            failed = true;
            getLogger().severe("SHADOW_FAILED " + error);
            throw error;
        }
    }

    private void scanWorld(World world) {
        var worldPlayers = world.getPlayers();
        if (worldPlayers.isEmpty()) {
            return;
        }
        if (worldPlayers.size() > ChunkTrackingQuery.MAX_PLAYERS) {
            throw new IllegalStateException("World exceeds shadow player capacity");
        }
        int pc = 0;
        for (Player player : worldPlayers) {
            player.getLocation(scratch);
            query.player(pc++, scratch.getX(), scratch.getZ());
        }
        players += pc;
        if (backend == Backend.INCREMENTAL) {
            var state = incremental.world(world);
            matches += queryIncremental(state, pc);
            mobs += state.liveCount;
            comparisons += pc * state.liveCount;
            return;
        }

        var worldMobs = world.getEntitiesByClass(Mob.class);
        if (worldMobs.size() > ChunkTrackingQuery.MAX_MOBS) {
            throw new IllegalStateException("World exceeds shadow mob capacity");
        }
        int mc = 0;
        for (Mob mob : worldMobs) {
            if (mob.isValid() && !mob.isDead()) {
                if (backend == Backend.VERIFY) {
                    verificationMobs[mc] = mob;
                }
                mob.getLocation(scratch);
                query.mob(mc++, scratch.getX(), scratch.getZ());
            }
        }
        mobs += mc;
        comparisons += pc * mc;
        if (backend == Backend.VERIFY) {
            matches += verifyWorld(world, pc, mc);
            return;
        }
        if (mc == 0) {
            return;
        }
        long start = System.nanoTime();
        if (backend == Backend.JAVA || backend == Backend.RUST) {
            query.buildIndex(mc);
        }
        long indexed = System.nanoTime();
        matches += switch (backend) {
            case FULL -> query.fullScan(pc, mc);
            case JAVA -> query.indexedJava(pc, mc);
            case RUST -> query.indexedRust(pc, mc);
            default -> throw new IllegalStateException("Unexpected snapshot backend " + backend);
        };
        evaluationNs += System.nanoTime() - indexed;
        indexNs += indexed - start;
    }

    private int queryIncremental(ChunkTrackingIncremental.WorldIndex state, int pc) {
        long start = System.nanoTime();
        incremental.stage(state);
        long staged = System.nanoTime();
        int result = incremental.query(state, pc, query.playerInput());
        evaluationNs += System.nanoTime() - staged;
        indexNs += staged - start;
        upserts += state.upserts;
        removals += state.removals;
        return result;
    }

    private int verifyWorld(World world, int pc, int mc) {
        try {
            int expected = mc > 0 ? query.verify(pc, mc) : 0;
            var state = incremental.world(world);
            int actual = queryIncremental(state, pc);
            if (state.liveCount != mc || expected != actual) {
                throw new IllegalStateException("Incremental snapshot count mismatch: mobs=" + state.liveCount
                    + "/" + mc + ", candidates=" + actual + "/" + expected);
            }
            for (int m = 0; m < mc; m++) {
                int slot = incremental.slotOf(state, verificationMobs[m]);
                for (int p = 0; p < pc; p++) {
                    if (query.candidate(p, mc, m) != incremental.candidate(state, p, slot)) {
                        throw new IllegalStateException("Incremental candidate differs at player " + p + ", mob " + m);
                    }
                }
            }
            return actual;
        } finally {
            Arrays.fill(verificationMobs, 0, mc, null);
        }
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void tickEnd(ServerTickEndEvent event) {
        long eventNanos = usesIncremental(backend) ? incremental.takeEventNanos() : 0;
        if (phase == Phase.IDLE || window != tickWindow || failed || tick != event.getTickNumber() || players != 20) {
            return;
        }
        if (sampleCount == samples.length) {
            failed = true;
            getLogger().severe("SHADOW_FAILED sample capacity exceeded");
            return;
        }
        samples[sampleCount++] = new Sample(phase, window, backend, tick, event.getTickDuration(), totalNs + eventNanos,
            indexNs, evaluationNs, players, mobs, matches, comparisons, upserts, removals);
    }

    private void writeSamples(Sample[] snapshot) {
        try (var out = Files.newBufferedWriter(Path.of("chunk-tracking.csv"))) {
            out.write("phase,window,backend,tick,tick_ms,total_ns,index_ns,evaluation_ns,players,mobs,matches,full_pairs,upserts,removals\n");
            for (Sample row : snapshot) {
                out.write(row.phase + "," + row.window + "," + row.backend + "," + row.tick + "," + row.tickMs + "," + row.totalNs + ","
                    + row.indexNs + "," + row.evaluationNs + "," + row.players + "," + row.mobs + ","
                    + row.matches + "," + row.comparisons + "," + row.upserts + "," + row.removals + "\n");
            }
        } catch (IOException error) {
            getLogger().severe("SHADOW_FAILED " + error);
            return;
        }
        getLogger().info("SHADOW_WRITTEN " + snapshot.length);
    }

    private record Sample(Phase phase, int window, Backend backend, int tick, double tickMs, long totalNs, long indexNs,
                          long evaluationNs, int players, int mobs, int matches, int comparisons, int upserts, int removals) {}
}
