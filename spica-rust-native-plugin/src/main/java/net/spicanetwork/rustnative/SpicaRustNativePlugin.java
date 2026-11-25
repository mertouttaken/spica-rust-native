package net.spicanetwork.rustnative;

import net.spicanetwork.rustnative.api.SpicaRustAPI;
import net.spicanetwork.rustnative.brain.BrainActuator;
import net.spicanetwork.rustnative.brain.BrainIpcClient;
import net.spicanetwork.rustnative.command.RustBridgeCommand;
import net.spicanetwork.rustnative.nativebridge.NativeLoader;
import net.spicanetwork.rustnative.nativebridge.RustNative;
import net.spicanetwork.rustnative.optimizer.EntityLodOptimizer;
import org.bukkit.Bukkit;
import org.bukkit.command.PluginCommand;
import org.bukkit.plugin.ServicePriority;
import org.bukkit.plugin.java.JavaPlugin;
import org.bukkit.scheduler.BukkitTask;

/**
 * Main entry point for SpicaRustNative v2.0.0 (Purpur 1.21.4 Execution Core + Rust Brain).
 */
public final class SpicaRustNativePlugin extends JavaPlugin {

    private SpicaRustAPI api;
    private EntityLodOptimizer optimizer;
    private BrainIpcClient brainIpcClient;
    private BrainActuator brainActuator;
    private BukkitTask optimizerTask;
    private BukkitTask brainTask;

    @Override
    public void onEnable() {
        saveDefaultConfig();

        try {
            NativeLoader.load(getDataFolder().toPath(), getLogger());
            int requestedThreads = getConfig().getInt("native.worker-threads", 0);
            int activeThreads = RustNative.nativeInitPool(requestedThreads);
            getLogger().info("[SpicaRustNative] Rust motoru hazir: " + RustNative.nativeGetInfo()
                + " (aktif worker=" + activeThreads + ")");
        } catch (Throwable t) {
            getLogger().severe("[SpicaRustNative] Rust native kutuphanesi yuklenemedi: " + t.getMessage());
            getLogger().severe("[SpicaRustNative] Plugin devre disi birakiliyor.");
            Bukkit.getPluginManager().disablePlugin(this);
            return;
        }

        this.api = new SpicaRustAPI();
        this.optimizer = new EntityLodOptimizer(api);
        this.optimizer.reloadConfig(getConfig());

        this.brainIpcClient = createAndStartIpcClient();
        this.brainActuator = new BrainActuator(api, optimizer, brainIpcClient, getLogger());
        this.brainActuator.reloadConfig(getConfig());

        Bukkit.getServicesManager().register(SpicaRustAPI.class, api, this, ServicePriority.Normal);

        PluginCommand cmd = getCommand("rustbridge");
        if (cmd != null) {
            RustBridgeCommand executor = new RustBridgeCommand(this);
            cmd.setExecutor(executor);
            cmd.setTabCompleter(executor);
        }

        scheduleOptimizer();
        scheduleBrain();
    }

    @Override
    public void onDisable() {
        if (brainTask != null) {
            brainTask.cancel();
            brainTask = null;
        }
        if (optimizerTask != null) {
            optimizerTask.cancel();
            optimizerTask = null;
        }
        if (brainIpcClient != null) {
            brainIpcClient.shutdown();
            brainIpcClient = null;
        }
        if (brainActuator != null) {
            brainActuator.restoreDefaults();
        }
        if (optimizer != null) {
            optimizer.resetDynamicRanges();
            int restored = optimizer.restoreAll();
            if (restored > 0) {
                getLogger().info("[SpicaRustNative] " + restored + " uyutulmus mob guvenle uyandirildi.");
            }
        }
        if (api != null) {
            Bukkit.getServicesManager().unregister(SpicaRustAPI.class, api);
        }
    }

    public void reloadBridgeConfig() {
        reloadConfig();

        if (brainTask != null) {
            brainTask.cancel();
            brainTask = null;
        }
        if (brainIpcClient != null) {
            brainIpcClient.shutdown();
            brainIpcClient = null;
        }
        if (brainActuator != null) {
            brainActuator.restoreDefaults();
        }
        if (optimizer != null) {
            optimizer.restoreAll();
            optimizer.resetDynamicRanges();
            optimizer.reloadConfig(getConfig());
        }

        this.brainIpcClient = createAndStartIpcClient();
        if (brainActuator != null) {
            brainActuator.setIpcClient(this.brainIpcClient);
            brainActuator.reloadConfig(getConfig());
        }

        scheduleOptimizer();
        scheduleBrain();
    }

    private BrainIpcClient createAndStartIpcClient() {
        String fallbackHost = getConfig().getString("brain.ipc-fallback-host", "127.0.0.1");
        int fallbackPort = getConfig().getInt("brain.ipc-fallback-port", 0);
        BrainIpcClient client = new BrainIpcClient(fallbackHost, fallbackPort, getLogger());
        if (getConfig().getBoolean("brain.enabled", true)) {
            client.start();
        }
        return client;
    }

    private void scheduleOptimizer() {
        if (optimizerTask != null) {
            optimizerTask.cancel();
            optimizerTask = null;
        }
        if (!getConfig().getBoolean("optimizer.enabled", true)) {
            return;
        }
        long interval = Math.max(1L, getConfig().getLong("optimizer.interval-ticks", 10L));
        this.optimizerTask = Bukkit.getScheduler().runTaskTimer(this, () -> optimizer.tick(), interval, interval);
    }

    private void scheduleBrain() {
        if (brainTask != null) {
            brainTask.cancel();
            brainTask = null;
        }
        if (!getConfig().getBoolean("brain.enabled", true)) {
            return;
        }
        long interval = Math.max(5L, getConfig().getLong("brain.tick-interval", 20L));
        this.brainTask = Bukkit.getScheduler().runTaskTimer(this, () -> brainActuator.tick(), interval, interval);
    }

    public SpicaRustAPI getApi() {
        return api;
    }

    public EntityLodOptimizer getOptimizer() {
        return optimizer;
    }

    public BrainActuator getBrainActuator() {
        return brainActuator;
    }

    public BrainIpcClient getBrainIpcClient() {
        return brainIpcClient;
    }
}
