package net.spicanetwork.rustnative.command;

import net.kyori.adventure.text.TranslatableComponent;
import org.bukkit.Bukkit;
import org.bukkit.command.Command;
import org.bukkit.command.CommandExecutor;
import org.bukkit.command.CommandSender;
import org.bukkit.command.ConsoleCommandSender;

import java.util.logging.Logger;
import java.util.regex.Pattern;

/** Associates the vanilla flush result with one launcher request. */
public final class BackupCheckpointCommand implements CommandExecutor {
    private static final Pattern CHECKPOINT_ID = Pattern.compile("[a-f0-9]{64}");
    private final Logger logger;

    public BackupCheckpointCommand(Logger logger) {
        this.logger = logger;
    }

    @Override
    public boolean onCommand(CommandSender sender, Command command, String label, String[] args) {
        if (!(sender instanceof ConsoleCommandSender) || args.length != 1
            || !CHECKPOINT_ID.matcher(args[0]).matches()) {
            return false;
        }

        boolean[] saved = {false};
        CommandSender feedback = Bukkit.createCommandSender(component -> {
            if (component instanceof TranslatableComponent translated
                && translated.key().equals("commands.save.success")) {
                saved[0] = true;
            }
            sender.sendMessage(component);
        });
        try {
            Bukkit.dispatchCommand(feedback, "minecraft:save-all flush");
        } catch (RuntimeException ex) {
            saved[0] = false;
            logger.warning("SpicaBackup checkpoint failed: " + ex.getMessage());
        }
        logger.info("SpicaBackup checkpoint " + args[0] + (saved[0] ? " saved" : " failed"));
        return true;
    }
}
