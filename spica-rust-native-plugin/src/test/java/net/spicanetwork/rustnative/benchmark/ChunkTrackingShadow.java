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
    private enum Backend { FULL, JAVA, RUST, VERIFY }
    private enum Phase { IDLE, DENSE, SPREAD, TELEPORT }

    private final ChunkTrackingQuery query = new ChunkTrackingQuery();
    private final Location scratch = new Location(null, 0, 0, 0);
    private final OperatingSystemMXBean os = ManagementFactory.getPlatformMXBean(OperatingSystemMXBean.class);
    private final Sample[] samples = new Sample[16_384];
    private Backend backend;
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
    private boolean failed;

    @Override
    public void onEnable() {
        String mode = System.getProperty("spica.shadow.backend").toUpperCase(Locale.ROOT);
        interleaved = mode.equals("INTERLEAVED");
        backend = interleaved ? Backend.FULL : Backend.valueOf(mode);
        System.load(Path.of(System.getProperty("spica.shadow.library")).toAbsolutePath().toString());
        RustNative.nativeInitPool(4);
        if (backend == Backend.VERIFY) {
            verifyBoundary();
        }
        Bukkit.getPluginManager().registerEvents(this, this);
        getLogger().info("SHADOW_READY " + mode);
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
                backend = nextBackend;
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
        var worldMobs = world.getEntitiesByClass(Mob.class);
        if (worldPlayers.size() > ChunkTrackingQuery.MAX_PLAYERS || worldMobs.size() > ChunkTrackingQuery.MAX_MOBS) {
            throw new IllegalStateException("World exceeds shadow snapshot capacity");
        }
        int pc = 0;
        for (Player player : worldPlayers) {
            player.getLocation(scratch);
            query.player(pc++, scratch.getX(), scratch.getZ());
        }
        int mc = 0;
        for (Mob mob : worldMobs) {
            if (mob.isValid() && !mob.isDead()) {
                mob.getLocation(scratch);
                query.mob(mc++, scratch.getX(), scratch.getZ());
            }
        }
        players += pc;
        mobs += mc;
        comparisons += pc * mc;
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
            case VERIFY -> query.verify(pc, mc);
        };
        evaluationNs += System.nanoTime() - indexed;
        indexNs += indexed - start;
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void tickEnd(ServerTickEndEvent event) {
        if (phase == Phase.IDLE || window != tickWindow || failed || tick != event.getTickNumber() || players != 20) {
            return;
        }
        if (sampleCount == samples.length) {
            failed = true;
            getLogger().severe("SHADOW_FAILED sample capacity exceeded");
            return;
        }
        samples[sampleCount++] = new Sample(phase, window, backend, tick, event.getTickDuration(), totalNs, indexNs,
            evaluationNs, players, mobs, matches, comparisons);
    }

    private void writeSamples(Sample[] snapshot) {
        try (var out = Files.newBufferedWriter(Path.of("chunk-tracking.csv"))) {
            out.write("phase,window,backend,tick,tick_ms,total_ns,index_ns,evaluation_ns,players,mobs,matches,full_pairs\n");
            for (Sample row : snapshot) {
                out.write(row.phase + "," + row.window + "," + row.backend + "," + row.tick + "," + row.tickMs + "," + row.totalNs + ","
                    + row.indexNs + "," + row.evaluationNs + "," + row.players + "," + row.mobs + ","
                    + row.matches + "," + row.comparisons + "\n");
            }
        } catch (IOException error) {
            getLogger().severe("SHADOW_FAILED " + error);
            return;
        }
        getLogger().info("SHADOW_WRITTEN " + snapshot.length);
    }

    private record Sample(Phase phase, int window, Backend backend, int tick, double tickMs, long totalNs, long indexNs,
                          long evaluationNs, int players, int mobs, int matches, int comparisons) {}
}
