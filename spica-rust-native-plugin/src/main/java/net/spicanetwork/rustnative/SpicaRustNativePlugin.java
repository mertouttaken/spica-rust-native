package net.spicanetwork.rustnative;

import net.spicanetwork.rustnative.api.SpicaRustAPI;
import net.spicanetwork.rustnative.brain.BrainActuator;
import net.spicanetwork.rustnative.brain.BrainDirective;
import net.spicanetwork.rustnative.brain.BrainIpcClient;
import net.spicanetwork.rustnative.command.RustBridgeCommand;
import net.spicanetwork.rustnative.nativebridge.NativeLoader;
import net.spicanetwork.rustnative.nativebridge.RustNative;
import net.spicanetwork.rustnative.optimizer.EntityLodOptimizer;
import net.spicanetwork.rustnative.optimizer.FarmVillagerGovernor;
import net.spicanetwork.rustnative.optimizer.MobTrackingOptimizer;
import net.spicanetwork.rustnative.optimizer.RedstoneHopperGovernor;
import net.spicanetwork.rustnative.optimizer.SurgeExplosionGuard;
import org.bukkit.Bukkit;
import org.bukkit.command.PluginCommand;
import org.bukkit.plugin.ServicePriority;
import org.bukkit.plugin.java.JavaPlugin;
import org.bukkit.scheduler.BukkitTask;

/**
 * Main entry point for SpicaRustNative v2.5.0 (Purpur 1.21.4 Execution Core + Rust Brain + Farm/Surge/Mob-Tracking/Redstone Optimizers).
 */
public final class SpicaRustNativePlugin extends JavaPlugin {

    private SpicaRustAPI api;
    private EntityLodOptimizer optimizer;
    private FarmVillagerGovernor farmGovernor;
    private SurgeExplosionGuard surgeGuard;
    private MobTrackingOptimizer mobTrackingOptimizer;
    private RedstoneHopperGovernor redstoneGovernor;
    private BrainIpcClient brainIpcClient;
    private BrainActuator brainActuator;
    private BukkitTask optimizerTask;
    private BukkitTask farmGovernorTask;
    private BukkitTask surgeGuardTask;
    private BukkitTask mobTrackingTask;
    private BukkitTask redstoneGovernorTask;
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

        this.farmGovernor = new FarmVillagerGovernor(api);
        this.farmGovernor.reloadConfig(getConfig());

        // Coordinate AI suppression state cleanly between EntityLodOptimizer and FarmVillagerGovernor
        this.optimizer.setExternalAiManagedPredicate(farmGovernor::isMobManagedByFarmGovernor);
        this.farmGovernor.setExternalAiManagedPredicate(optimizer::isHibernatedByUs);
        Bukkit.getPluginManager().registerEvents(farmGovernor, this);

        this.surgeGuard = new SurgeExplosionGuard(this, api);
        this.surgeGuard.reloadConfig(getConfig());
        Bukkit.getPluginManager().registerEvents(surgeGuard, this);

        this.mobTrackingOptimizer = new MobTrackingOptimizer(this, api);
        this.mobTrackingOptimizer.reloadConfig(getConfig());
        Bukkit.getPluginManager().registerEvents(mobTrackingOptimizer, this);

        this.redstoneGovernor = new RedstoneHopperGovernor(api, getLogger());
        this.redstoneGovernor.reloadConfig(getConfig());
        Bukkit.getPluginManager().registerEvents(redstoneGovernor, this);

        this.brainIpcClient = createAndStartIpcClient();
        this.brainActuator = new BrainActuator(
            api, optimizer, farmGovernor, surgeGuard, mobTrackingOptimizer, redstoneGovernor, brainIpcClient, getLogger()
        );
        this.brainActuator.reloadConfig(getConfig());

        Bukkit.getServicesManager().register(SpicaRustAPI.class, api, this, ServicePriority.Normal);

        PluginCommand cmd = getCommand("rustbridge");
        if (cmd != null) {
            RustBridgeCommand executor = new RustBridgeCommand(this);
            cmd.setExecutor(executor);
            cmd.setTabCompleter(executor);
        }

        scheduleOptimizer();
        scheduleFarmGovernor();
        scheduleSurgeGuard();
        scheduleMobTracking();
        scheduleRedstoneGovernor();
        scheduleBrain();
    }

    @Override
    public void onDisable() {
        if (brainTask != null) {
            brainTask.cancel();
            brainTask = null;
        }
        if (redstoneGovernorTask != null) {
            redstoneGovernorTask.cancel();
            redstoneGovernorTask = null;
        }
        if (mobTrackingTask != null) {
            mobTrackingTask.cancel();
            mobTrackingTask = null;
        }
        if (surgeGuardTask != null) {
            surgeGuardTask.cancel();
            surgeGuardTask = null;
        }
        if (farmGovernorTask != null) {
            farmGovernorTask.cancel();
            farmGovernorTask = null;
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
        if (redstoneGovernor != null) {
            int restoredRedstone = redstoneGovernor.restoreAll();
            if (restoredRedstone > 0) {
                getLogger().info("[SpicaRustNative] " + restoredRedstone + " kisitlanmis Redstone/Hopper chunk'i guvenle serbest birakildi.");
            }
        }
        if (mobTrackingOptimizer != null) {
            int restoredTracked = mobTrackingOptimizer.restoreAll();
            if (restoredTracked > 0) {
                getLogger().info("[SpicaRustNative] " + restoredTracked + " gizlenmis mob gorunurluk cifti guvenle geri yuklendi.");
            }
        }
        if (surgeGuard != null) {
            surgeGuard.resetPending();
        }
        if (farmGovernor != null) {
            int restoredFarm = farmGovernor.restoreAll();
            if (restoredFarm > 0) {
                getLogger().info("[SpicaRustNative] " + restoredFarm + " farm/koylu mobu guvenle geri yuklendi.");
            }
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
        if (redstoneGovernorTask != null) {
            redstoneGovernorTask.cancel();
            redstoneGovernorTask = null;
        }
        if (mobTrackingTask != null) {
            mobTrackingTask.cancel();
            mobTrackingTask = null;
        }
        if (surgeGuardTask != null) {
            surgeGuardTask.cancel();
            surgeGuardTask = null;
        }
        if (farmGovernorTask != null) {
            farmGovernorTask.cancel();
            farmGovernorTask = null;
        }
        if (brainIpcClient != null) {
            brainIpcClient.shutdown();
            brainIpcClient = null;
        }
        if (brainActuator != null) {
            brainActuator.restoreDefaults();
        }
        if (redstoneGovernor != null) {
            redstoneGovernor.restoreAll();
            redstoneGovernor.reloadConfig(getConfig());
        }
        if (mobTrackingOptimizer != null) {
            mobTrackingOptimizer.restoreAll();
            mobTrackingOptimizer.reloadConfig(getConfig());
        }
        if (surgeGuard != null) {
            surgeGuard.resetPending();
            surgeGuard.reloadConfig(getConfig());
        }
        if (farmGovernor != null) {
            farmGovernor.restoreAll();
            farmGovernor.reloadConfig(getConfig());
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
        scheduleFarmGovernor();
        scheduleSurgeGuard();
        scheduleMobTracking();
        scheduleRedstoneGovernor();
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

    private void scheduleFarmGovernor() {
        if (farmGovernorTask != null) {
            farmGovernorTask.cancel();
            farmGovernorTask = null;
        }
        if (!getConfig().getBoolean("farm-governor.enabled", true)) {
            return;
        }
        long interval = Math.max(5L, getConfig().getLong("farm-governor.interval-ticks", 20L));
        this.farmGovernorTask = Bukkit.getScheduler().runTaskTimer(
            this,
            () -> {
                int stateCode = brainActuator != null
                    ? brainActuator.getCurrentBrainStateCode()
                    : BrainDirective.STATE_GREEN;
                farmGovernor.tick(stateCode);
            },
            interval,
            interval
        );
    }

    private void scheduleSurgeGuard() {
        if (surgeGuardTask != null) {
            surgeGuardTask.cancel();
            surgeGuardTask = null;
        }
        if (!getConfig().getBoolean("surge-guard.enabled", true)) {
            return;
        }
        long interval = Math.max(1L, getConfig().getLong("surge-guard.elytra.interval-ticks", 10L));
        this.surgeGuardTask = Bukkit.getScheduler().runTaskTimer(
            this,
            () -> {
                int stateCode = brainActuator != null
                    ? brainActuator.getCurrentBrainStateCode()
                    : BrainDirective.STATE_GREEN;
                surgeGuard.tick(stateCode);
            },
            interval,
            interval
        );
    }

    private void scheduleMobTracking() {
        if (mobTrackingTask != null) {
            mobTrackingTask.cancel();
            mobTrackingTask = null;
        }
        if (!getConfig().getBoolean("mob-tracking.enabled", true)) {
            return;
        }
        long interval = Math.max(1L, getConfig().getLong("mob-tracking.interval-ticks", 6L));
        this.mobTrackingTask = Bukkit.getScheduler().runTaskTimer(
            this,
            () -> {
                int stateCode = brainActuator != null
                    ? brainActuator.getCurrentBrainStateCode()
                    : BrainDirective.STATE_GREEN;
                mobTrackingOptimizer.tick(stateCode);
            },
            interval,
            interval
        );
    }

    private void scheduleRedstoneGovernor() {
        if (redstoneGovernorTask != null) {
            redstoneGovernorTask.cancel();
            redstoneGovernorTask = null;
        }
        if (!getConfig().getBoolean("redstone-governor.enabled", true)) {
            return;
        }
        this.redstoneGovernorTask = Bukkit.getScheduler().runTaskTimer(
            this,
            () -> {
                int stateCode = brainActuator != null
                    ? brainActuator.getCurrentBrainStateCode()
                    : BrainDirective.STATE_GREEN;
                redstoneGovernor.tick(stateCode);
            },
            20L,
            20L
        );
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

    public FarmVillagerGovernor getFarmGovernor() {
        return farmGovernor;
    }

    public SurgeExplosionGuard getSurgeGuard() {
        return surgeGuard;
    }

    public MobTrackingOptimizer getMobTrackingOptimizer() {
        return mobTrackingOptimizer;
    }

    public RedstoneHopperGovernor getRedstoneGovernor() {
        return redstoneGovernor;
    }

    public BrainActuator getBrainActuator() {
        return brainActuator;
    }

    public BrainIpcClient getBrainIpcClient() {
        return brainIpcClient;
    }
}
