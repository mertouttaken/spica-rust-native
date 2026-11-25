package net.spicanetwork.rustnative.command;

import net.kyori.adventure.text.TranslatableComponent;
import org.bukkit.Bukkit;
import org.bukkit.World;
import org.bukkit.command.Command;
import org.bukkit.command.CommandExecutor;
import org.bukkit.command.CommandSender;
import org.bukkit.command.ConsoleCommandSender;
import org.bukkit.plugin.java.JavaPlugin;
import org.bukkit.scheduler.BukkitTask;

import java.util.Iterator;
import java.util.UUID;
import java.util.regex.Pattern;

/** Prepares a checkpoint over several ticks, then correlates the full vanilla flush. */
public final class BackupCheckpointCommand implements CommandExecutor {
    private static final Pattern CHECKPOINT_ID = Pattern.compile("[a-f0-9]{64}");
    private static final int PREPARE_TICKS = 20;
    private static final long DRAIN_TICKS = 10L;
    private final JavaPlugin plugin;
    private BukkitTask step;
    private String pending;

    public BackupCheckpointCommand(JavaPlugin plugin) {
        this.plugin = plugin;
    }

    @Override
    public boolean onCommand(CommandSender sender, Command command, String label, String[] args) {
        if (!(sender instanceof ConsoleCommandSender) || args.length != 1
            || !CHECKPOINT_ID.matcher(args[0]).matches()) {
            return false;
        }
        String id = args[0];
        if (pending != null) {
            if (!pending.equals(id)) {
                report(id, false);
            }
            return true;
        }
        pending = id;
        if (!plugin.getConfig().getBoolean("backup.prepare-checkpoint", true)) {
            flush(sender);
            return true;
        }

        Iterator<UUID> worlds = Bukkit.getWorlds().stream()
            .filter(World::isAutoSave)
            .map(World::getUID)
            .iterator();
        prepareNextWorld(sender, worlds, PREPARE_TICKS, false);
        return true;
    }

    private void prepareNextWorld(CommandSender sender, Iterator<UUID> worlds, int remaining, boolean queued) {
        if (remaining == 0 || !worlds.hasNext()) {
            if (queued) {
                step = Bukkit.getScheduler().runTaskLater(plugin, () -> flush(sender), DRAIN_TICKS);
            } else {
                flush(sender);
            }
            return;
        }

        step = Bukkit.getScheduler().runTaskLater(plugin, () -> {
            World world = Bukkit.getWorld(worlds.next());
            boolean prepared = false;
            if (world != null && world.isAutoSave()) {
                try {
                    world.save(false);
                    prepared = true;
                } catch (RuntimeException ex) {
                    plugin.getLogger().warning("SpicaBackup could not prepare " + world.getName() + ": " + ex);
                    finish(false);
                    return;
                }
            }
            if (pending != null) {
                prepareNextWorld(sender, worlds, remaining - 1, queued || prepared);
            }
        }, 1L);
    }

    private void flush(CommandSender sender) {
        boolean[] saved = {false};
        CommandSender feedback = Bukkit.createCommandSender(component -> {
            if (component instanceof TranslatableComponent translated
                && translated.key().equals("commands.save.success")) {
                saved[0] = true;
            }
            sender.sendMessage(component);
        });
        try {
            // Paper drains the region I/O and saves changes made after preparation here.
            Bukkit.dispatchCommand(feedback, "minecraft:save-all flush");
        } catch (RuntimeException ex) {
            saved[0] = false;
            plugin.getLogger().warning("SpicaBackup checkpoint failed: " + ex.getMessage());
        }
        finish(saved[0]);
    }

    public void cancel() {
        finish(false);
    }

    private void finish(boolean saved) {
        if (step != null) {
            step.cancel();
            step = null;
        }
        if (pending != null) {
            String id = pending;
            pending = null;
            report(id, saved);
        }
    }

    private void report(String id, boolean saved) {
        plugin.getLogger().info("SpicaBackup checkpoint " + id + (saved ? " saved" : " failed"));
    }
}
