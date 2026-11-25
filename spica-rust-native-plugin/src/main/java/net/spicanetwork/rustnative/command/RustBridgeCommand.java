package net.spicanetwork.rustnative.command;

import net.kyori.adventure.text.Component;
import net.kyori.adventure.text.format.NamedTextColor;
import net.spicanetwork.rustnative.SpicaRustNativePlugin;
import net.spicanetwork.rustnative.benchmark.BridgeBenchmark;
import net.spicanetwork.rustnative.brain.BrainActuator;
import net.spicanetwork.rustnative.brain.BrainDirective;
import net.spicanetwork.rustnative.nativebridge.NativeLoader;
import net.spicanetwork.rustnative.nativebridge.RustNative;
import net.spicanetwork.rustnative.optimizer.EntityLodOptimizer;
import net.spicanetwork.rustnative.optimizer.FarmVillagerGovernor;
import net.spicanetwork.rustnative.optimizer.MobTrackingOptimizer;
import net.spicanetwork.rustnative.optimizer.RedstoneHopperGovernor;
import net.spicanetwork.rustnative.optimizer.SurgeExplosionGuard;
import org.bukkit.Bukkit;
import org.bukkit.Location;
import org.bukkit.World;
import org.bukkit.command.Command;
import org.bukkit.command.CommandExecutor;
import org.bukkit.command.CommandSender;
import org.bukkit.command.TabCompleter;
import org.bukkit.entity.Player;

import java.util.List;
import java.util.Locale;

public final class RustBridgeCommand implements CommandExecutor, TabCompleter {

    private final SpicaRustNativePlugin plugin;

    public RustBridgeCommand(SpicaRustNativePlugin plugin) {
        this.plugin = plugin;
    }

    @Override
    public boolean onCommand(CommandSender sender, Command command, String label, String[] args) {
        if (args.length == 0) {
            sendHelp(sender);
            return true;
        }

        switch (args[0].toLowerCase(Locale.ROOT)) {
            case "status" -> handleStatus(sender);
            case "brain" -> handleBrain(sender);
            case "farms" -> handleFarms(sender);
            case "surge" -> handleSurge(sender);
            case "tracking" -> handleTracking(sender);
            case "redstone" -> handleRedstone(sender);
            case "kernels" -> handleKernels(sender);
            case "chunks" -> handleChunks(sender, args);
            case "bench" -> handleBench(sender, args);
            case "scan" -> handleScan(sender, args);
            case "reload" -> handleReload(sender);
            default -> sendHelp(sender);
        }
        return true;
    }

    private void handleStatus(CommandSender sender) {
        EntityLodOptimizer opt = plugin.getOptimizer();
        FarmVillagerGovernor farms = plugin.getFarmGovernor();
        SurgeExplosionGuard surge = plugin.getSurgeGuard();
        MobTrackingOptimizer tracking = plugin.getMobTrackingOptimizer();
        RedstoneHopperGovernor redstone = plugin.getRedstoneGovernor();
        BrainActuator brain = plugin.getBrainActuator();
        BrainDirective dir = brain != null ? brain.getLastDirective() : null;

        sender.sendMessage(Component.text("=== SpicaRustNative v2.5.0 Durum ===", NamedTextColor.GOLD));
        sender.sendMessage(Component.text("Platform: " + NativeLoader.detectPlatformKey(), NamedTextColor.GRAY));
        sender.sendMessage(Component.text("Native Engine: " + plugin.getApi().getNativeInfo(), NamedTextColor.AQUA));
        sender.sendMessage(Component.text("Kutuphane: " + NativeLoader.getLoadedPath(), NamedTextColor.DARK_GRAY));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Optimizer: %s | Son tarama: %.3f ms | Toplam Mob: %d (Aktif: %d, Uyutulan: %d)",
                opt.isEnabled() ? "AKTIF" : "KAPALI",
                opt.getLastRunMs(),
                opt.getLastTotalMobs(),
                opt.getLastFullMobs(),
                opt.getLastHibernatedMobs()
            ),
            NamedTextColor.GREEN
        ));
        if (farms != null) {
            sender.sendMessage(Component.text(
                String.format(
                    Locale.US,
                    "Farm Governor: %s | Rust: %.3f ms | Farm: %d | Carpisma Kapali: %d | Koylu: %d",
                    farms.isEnabled() ? "AKTIF" : "KAPALI",
                    farms.getLastRustComputeMs(),
                    farms.getLastFarmThrottled(),
                    farms.getLastCollisionDisabled(),
                    farms.getLastVillagersThrottled()
                ),
                NamedTextColor.YELLOW
            ));
        }
        if (surge != null) {
            sender.sendMessage(Component.text(
                String.format(
                    Locale.US,
                    "Surge Guard: %s | Chunk/s: %d | Elytra Clamp: %d | Patlama Tampon: %d (Esya: %d)",
                    surge.isEnabled() ? "AKTIF" : "KAPALI",
                    surge.getLastChunkLoadsPerSec(),
                    surge.getElytraClampsTotal(),
                    surge.getExplosionBurstsBuffered(),
                    surge.getExplosionItemsMergedTotal()
                ),
                NamedTextColor.LIGHT_PURPLE
            ));
        }
        if (tracking != null) {
            sender.sendMessage(Component.text(
                String.format(
                    Locale.US,
                    "Mob Tracking: %s | Rust: %.3f ms | Gizli Cift: %d | Gorunur: %d | Gecis/sn: %d",
                    tracking.isEnabled() ? "AKTIF" : "KAPALI",
                    tracking.getLastRustMs(),
                    tracking.getLastTotalHiddenPairs(),
                    tracking.getLastTotalVisiblePairs(),
                    tracking.getRollingTransitionsPerSec()
                ),
                NamedTextColor.AQUA
            ));
        }
        if (redstone != null) {
            sender.sendMessage(Component.text(
                String.format(
                    Locale.US,
                    "Redstone Governor: %s | Rust: %.3f ms | Aktif Chunk: %d | Kisitli: %d | Engellenen Op: %d",
                    redstone.isEnabled() ? "AKTIF" : "KAPALI",
                    redstone.getLastRustMs(),
                    redstone.getActiveChunkCount(),
                    redstone.getThrottledChunkCount(),
                    redstone.getTotalOpsBlocked()
                ),
                redstone.getThrottledChunkCount() > 0 ? NamedTextColor.RED : NamedTextColor.GOLD
            ));
        }
        if (brain != null) {
            String mode = brain.isLauncherConnected() ? "LAUNCHER IPC" : "IN-PROCESS RUST";
            String state = dir != null ? dir.state() : "BEKLENIYOR";
            NamedTextColor stateColor = dir != null ? colorForState(dir.stateCode()) : NamedTextColor.GRAY;
            sender.sendMessage(Component.text(
                String.format(
                    Locale.US,
                    "Rust Brain: %s | Mod: %s | Durum: %s | Birlesen Esya: %d",
                    brain.isBrainEnabled() ? "AKTIF" : "KAPALI",
                    mode,
                    state,
                    brain.getTotalItemsMerged()
                ),
                stateColor
            ));
        }
    }

    private void handleFarms(CommandSender sender) {
        FarmVillagerGovernor farms = plugin.getFarmGovernor();
        if (farms == null) {
            sender.sendMessage(Component.text("Farm & Villager Governor modulu baslatilmadi.", NamedTextColor.RED));
            return;
        }

        int stateCode = farms.getLastBrainStateCode();
        String stateName = BrainDirective.stateNameForCode(stateCode);

        sender.sendMessage(Component.text("=== SpicaRustNative Farm & Villager Governor ===", NamedTextColor.GOLD));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Durum: %s | Koylu Optimizasyonu: %s | Rust Hesaplama: %.3f ms (Toplam: %.3f ms)",
                farms.isEnabled() ? "AKTIF" : "KAPALI",
                farms.isVillagerOptimizationEnabled() ? "AKTIF" : "KAPALI",
                farms.getLastRustComputeMs(),
                farms.getLastRunMs()
            ),
            farms.isEnabled() ? NamedTextColor.GREEN : NamedTextColor.RED
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Taranan Mob: %d | Throttled Farm Mob: %d | Carpisma Kapali: %d | Throttled Koylu: %d",
                farms.getLastTotalMobs(),
                farms.getLastFarmThrottled(),
                farms.getLastCollisionDisabled(),
                farms.getLastVillagersThrottled()
            ),
            NamedTextColor.AQUA
        ));

        if (!farms.isVillagerOptimizationEnabled() || farms.getSecondsUntilNextRestockPulse() < 0) {
            sender.sendMessage(Component.text(
                "Restock Pulse: PASIF (Koylu optimizasyonu veya periyodik pulse devre disi)",
                NamedTextColor.GRAY
            ));
        } else if (farms.isInRestockPulse()) {
            sender.sendMessage(Component.text(
                String.format(
                    Locale.US,
                    "Restock Pulse: AKTIF (Koyluler stok yenileme icin uyanik! Kalan: %ds / Periyot: %ds)",
                    farms.getRestockPulseRemainingSeconds(),
                    farms.getRestockPulseIntervalSeconds()
                ),
                NamedTextColor.GREEN
            ));
        } else {
            sender.sendMessage(Component.text(
                String.format(
                    Locale.US,
                    "Restock Pulse: BEKLEMEDE (Sonraki pulse: %ds sonra | Sure: %ds / Periyot: %ds)",
                    farms.getSecondsUntilNextRestockPulse(),
                    farms.getRestockPulseDurationSeconds(),
                    farms.getRestockPulseIntervalSeconds()
                ),
                NamedTextColor.YELLOW
            ));
        }

        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Farm Esikleri: Yaricap=%.1f | Hafif(Carpisma)=%d | Agir(AI+Carpisma)=%d | Chunk Limiti=%d",
                farms.getCrowdRadius(),
                farms.getMildCrowdThreshold(),
                farms.getSevereCrowdThreshold(),
                farms.getChunkMobLimit()
            ),
            NamedTextColor.LIGHT_PURPLE
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Koylu Esikleri: Uyanma=%.1f blok | Kalabalik=%d | Etkilesim Koruma=%ds | Brain=%s",
                farms.getVillagerWakeRange(),
                farms.getVillagerCrowdThreshold(),
                farms.getInteractGraceSeconds(),
                stateName
            ),
            colorForState(stateCode)
        ));
    }

    private void handleBrain(CommandSender sender) {
        BrainActuator brain = plugin.getBrainActuator();
        EntityLodOptimizer opt = plugin.getOptimizer();
        if (brain == null) {
            sender.sendMessage(Component.text("Rust Brain modulu baslatilmadi.", NamedTextColor.RED));
            return;
        }

        BrainDirective dir = brain.getLastDirective();
        String mode = brain.isLauncherConnected()
            ? "LAUNCHER IPC (" + brain.getIpcEndpoint() + ")"
            : "IN-PROCESS RUST";

        sender.sendMessage(Component.text("=== SpicaRustNative Rust Brain ===", NamedTextColor.GOLD));
        sender.sendMessage(Component.text(
            "Brain Modu: " + mode + " | Aktif Kaynak: " + brain.getLastBrainSource(),
            brain.isLauncherConnected() ? NamedTextColor.AQUA : NamedTextColor.YELLOW
        ));

        if (dir == null) {
            sender.sendMessage(Component.text(
                "Henuz Brain direktifi uretilmedi (ilk tick bekleniyor).",
                NamedTextColor.GRAY
            ));
            return;
        }

        NamedTextColor stateColor = colorForState(dir.stateCode());
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Durum: %s (kod=%d) | MSPT: %.2f ms | EWMA MSPT: %.2f ms | TPS: %.2f",
                dir.state(), dir.stateCode(), brain.getLastMspt(), dir.ewmaMspt(), brain.getLastTps()
            ),
            stateColor
        ));

        int liveView = !Bukkit.getWorlds().isEmpty() ? Bukkit.getWorlds().get(0).getViewDistance() : dir.viewDistance();
        int liveSim = !Bukkit.getWorlds().isEmpty() ? Bukkit.getWorlds().get(0).getSimulationDistance() : dir.simDistance();
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "View/Sim Distance: Canli=%d/%d | Direktif=%d/%d | Aktuasyon: %s",
                liveView, liveSim, dir.viewDistance(), dir.simDistance(),
                brain.isActuationEnabled() ? "AKTIF" : "SADECE IZLEME"
            ),
            NamedTextColor.GREEN
        ));

        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Dinamik AI Menzili: Full=%.1f blok (baz=%.1f) | Hibernate=%.1f blok (baz=%.1f)",
                opt.getActiveFullAiRange(), opt.getBaseFullAiRange(),
                opt.getActiveHibernateRange(), opt.getBaseHibernateRange()
            ),
            NamedTextColor.AQUA
        ));

        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Rust Esya Birlestirme: %s (yaricap=%.1f) | Toplam Birlesen: %d | Heap: %d/%d MB",
                dir.itemMergeEnabled() ? "AKTIF" : "PASIF",
                dir.itemMergeRadius(),
                brain.getTotalItemsMerged(),
                brain.getLastHeapUsedMb(),
                brain.getLastHeapMaxMb()
            ),
            NamedTextColor.LIGHT_PURPLE
        ));

        sender.sendMessage(Component.text("Neden: " + dir.reason(), NamedTextColor.GRAY));
    }

    private void handleChunks(CommandSender sender, String[] args) {
        if (!plugin.getApi().isNativeLoaded()) {
            sender.sendMessage(Component.text("Rust native kutuphanesi yuklu degil!", NamedTextColor.RED));
            return;
        }

        World targetWorld = resolveTargetWorld(sender, args);
        if (targetWorld == null) {
            sender.sendMessage(Component.text("Hedef dunya bulunamadi.", NamedTextColor.RED));
            return;
        }

        BrainActuator.ChunkHeatmapReport report = plugin.getBrainActuator().analyzeHotChunks(targetWorld, 8);
        sender.sendMessage(Component.text(
            "=== Rust O(N) Chunk Heatmap (" + report.worldName() + ") ===",
            NamedTextColor.GOLD
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Taranan Entity: %d | Rust Hesaplama: %.3f ms (Toplam: %.3f ms)",
                report.totalEntities(), report.rustComputeMs(), report.totalMs()
            ),
            NamedTextColor.AQUA
        ));

        if (report.hotChunks().isEmpty()) {
            sender.sendMessage(Component.text("Yogun chunk bulunamadi.", NamedTextColor.GRAY));
            return;
        }

        int rank = 1;
        for (BrainActuator.HotChunkEntry entry : report.hotChunks()) {
            NamedTextColor rowColor = entry.crowdedCount() >= 10
                ? NamedTextColor.RED
                : (entry.entityCount() >= 30 ? NamedTextColor.YELLOW : NamedTextColor.GREEN);
            sender.sendMessage(Component.text(
                String.format(
                    Locale.US,
                    "#%d Chunk(%d, %d) | Blok(%d, %d) | Entity: %d | Sikisik: %d",
                    rank++,
                    entry.chunkX(), entry.chunkZ(),
                    entry.blockX(), entry.blockZ(),
                    entry.entityCount(),
                    entry.crowdedCount()
                ),
                rowColor
            ));
        }
    }

    private void handleBench(CommandSender sender, String[] args) {
        if (!plugin.getApi().isNativeLoaded()) {
            sender.sendMessage(Component.text("Rust native kutuphanesi yuklu degil!", NamedTextColor.RED));
            return;
        }

        int entities = args.length >= 2 ? parseInt(args[1], 3000) : 3000;
        int players = args.length >= 3 ? parseInt(args[2], 100) : 100;
        int iters = args.length >= 4 ? parseInt(args[3], 50) : 50;

        sender.sendMessage(Component.text(
            String.format("Async benchmark baslatildi: %d entity, %d oyuncu, %d iterasyon...", entities, players, iters),
            NamedTextColor.YELLOW
        ));

        Bukkit.getScheduler().runTaskAsynchronously(plugin, () -> {
            BridgeBenchmark.BenchmarkReport r = BridgeBenchmark.run(plugin.getApi(), entities, players, iters);
            Bukkit.getScheduler().runTask(plugin, () -> {
                sender.sendMessage(Component.text("=== Java vs Rust Native MSPT Benchmark ===", NamedTextColor.GOLD));
                sender.sendMessage(Component.text(
                    String.format(Locale.US, "Veri Seti: %d entity | %d oyuncu | %d iterasyon", r.entities(), r.players(), r.iterations()),
                    NamedTextColor.GRAY
                ));
                sender.sendMessage(Component.text(
                    String.format(
                        Locale.US,
                        "[Carpisma/Yogunluk] Java: %.3f ms | Rust (SpatialHash+Rayon): %.3f ms (%.1fx hizli | cift: %d)",
                        r.javaCollisionMs(), r.rustCollisionMs(), r.collisionSpeedup(), r.rustPairs()
                    ),
                    NamedTextColor.GREEN
                ));
                sender.sendMessage(Component.text(
                    String.format(
                        Locale.US,
                        "[AI Hedef + LOD]    Java: %.3f ms | Rust (ZeroCopy+Rayon):    %.3f ms (%.1fx hizli)",
                        r.javaAiMs(), r.rustAiMs(), r.aiSpeedup()
                    ),
                    NamedTextColor.AQUA
                ));
                sender.sendMessage(Component.text(
                    String.format(Locale.US, "[Boids/Suru AI]     Rust (SpatialHash+Rayon): %.3f ms/tick", r.rustBoidsMs()),
                    NamedTextColor.LIGHT_PURPLE
                ));
            });
        });
    }

    private void handleScan(CommandSender sender, String[] args) {
        if (!plugin.getApi().isNativeLoaded()) {
            sender.sendMessage(Component.text("Rust native kutuphanesi yuklu degil!", NamedTextColor.RED));
            return;
        }

        World targetWorld = resolveTargetWorld(sender, args);
        if (targetWorld == null) {
            sender.sendMessage(Component.text("Hedef dunya bulunamadi.", NamedTextColor.RED));
            return;
        }

        EntityLodOptimizer.WorldScanResult res = plugin.getOptimizer().scanWorld(targetWorld);
        sender.sendMessage(Component.text("=== Rust Canli Dunya Tarama (" + res.worldName() + ") ===", NamedTextColor.GOLD));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Mob: %d | Carpisan Cift: %d | Sikisik Mob: %d | Rust: %.3f ms (Toplam: %.3f ms)",
                res.entityCount(), res.totalCollidingPairs(), res.crowdedEntityCount(), res.rustComputeMs(), res.totalSnapshotMs()
            ),
            NamedTextColor.GREEN
        ));

        if (res.hotspotLocation() != null && res.maxNeighborsAtHotspot() > 0) {
            Location loc = res.hotspotLocation();
            sender.sendMessage(Component.text(
                String.format(
                    Locale.US,
                    "En Yogun Nokta: X=%.1f Y=%.1f Z=%.1f (Komsu sayisi: %d)",
                    loc.getX(), loc.getY(), loc.getZ(), res.maxNeighborsAtHotspot()
                ),
                NamedTextColor.YELLOW
            ));
        } else {
            sender.sendMessage(Component.text("Yogun mob kumesi tespit edilmedi.", NamedTextColor.GRAY));
        }
    }

    private void handleSurge(CommandSender sender) {
        SurgeExplosionGuard surge = plugin.getSurgeGuard();
        if (surge == null) {
            sender.sendMessage(Component.text("Surge & Explosion Guard modulu baslatilmadi.", NamedTextColor.RED));
            return;
        }

        int effState = surge.getLastEffectiveBrainStateCode();
        String effStateName = BrainDirective.stateNameForCode(effState);

        sender.sendMessage(Component.text("=== SpicaRustNative Elytra & Explosion Surge Guard ===", NamedTextColor.GOLD));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Durum: %s | Elytra Guard: %s | Patlama Tamponu: %s | Rust: %.3f ms",
                surge.isEnabled() ? "AKTIF" : "KAPALI",
                surge.isElytraEnabled() ? "AKTIF" : "KAPALI",
                surge.isExplosionEnabled() ? "AKTIF" : "KAPALI",
                surge.getLastRustComputeMs()
            ),
            surge.isEnabled() ? NamedTextColor.GREEN : NamedTextColor.RED
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Chunk Yukleme Hizi: %d /sn (Esik: %d /sn | Surge Eskalasyonu: %s)",
                surge.getLastChunkLoadsPerSec(),
                surge.getChunkLoadSurgeThreshold(),
                surge.isChunkSurgeEscalated() ? "AKTIF (+1 Brain)" : "NORMAL"
            ),
            surge.isChunkSurgeEscalated() ? NamedTextColor.RED : NamedTextColor.AQUA
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Aktif Elytra Hiz Limiti: %.2f blok/tick (Baz: %.2f | Efektif Brain: %s)",
                surge.getActiveSpeedCap(),
                surge.getBaseMaxElytraSpeed(),
                effStateName
            ),
            colorForState(effState)
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Suzulen Oyuncu: %d | Toplam Elytra Clamp: %d",
                surge.getLastGlidingPlayers(),
                surge.getElytraClampsTotal()
            ),
            NamedTextColor.YELLOW
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Patlama Tamponlanan Dalga: %d | Patlamada Birlesen Esya: %d (Min Blok: %d | Yaricap: %.1f)",
                surge.getExplosionBurstsBuffered(),
                surge.getExplosionItemsMergedTotal(),
                surge.getExplosionMinBlocksTrigger(),
                surge.getExplosionMergeRadius()
            ),
            NamedTextColor.LIGHT_PURPLE
        ));
    }

    private void handleTracking(CommandSender sender) {
        MobTrackingOptimizer tracking = plugin.getMobTrackingOptimizer();
        if (tracking == null) {
            sender.sendMessage(Component.text("Mob Tracking Optimizer modulu baslatilmadi.", NamedTextColor.RED));
            return;
        }

        int stateCode = tracking.getLastBrainStateCode();
        String stateName = BrainDirective.stateNameForCode(stateCode);

        sender.sendMessage(Component.text("=== SpicaRustNative Mob Visibility & Tracking Optimizer ===", NamedTextColor.GOLD));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Durum: %s | Rust Hesaplama: %.3f ms (Toplam: %.3f ms) | Periyot: %d tick",
                tracking.isEnabled() ? "AKTIF" : "KAPALI",
                tracking.getLastRustMs(),
                tracking.getLastRunMs(),
                tracking.getIntervalTicks()
            ),
            tracking.isEnabled() ? NamedTextColor.GREEN : NamedTextColor.RED
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Gizli Cift: %d | Gorunur Cift: %d | Butce Kesimi (Top-K): %d",
                tracking.getLastTotalHiddenPairs(),
                tracking.getLastTotalVisiblePairs(),
                tracking.getLastCulledByBudget()
            ),
            NamedTextColor.AQUA
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Gecis Hizi: %d /sn | Son Tick Delta: %d | Toplam Gecis: %d (Limit: %d/tick)",
                tracking.getRollingTransitionsPerSec(),
                tracking.getLastDeltasApplied(),
                tracking.getTotalTransitionsApplied(),
                tracking.getMaxDeltasPerTick()
            ),
            NamedTextColor.YELLOW
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Efektif Menziller (Brain=%s): MaxTakip=%.1f (baz=%.1f) | YakinGuven=%.1f | Dikey=%.1f (baz=%.1f)",
                stateName,
                tracking.getEffectiveMaxTrackRange(),
                tracking.getMaxTrackRange(),
                tracking.getSafetyNearRange(),
                tracking.getEffectiveMaxVerticalDelta(),
                tracking.getMaxVerticalDelta()
            ),
            colorForState(stateCode)
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Arka FOV Kesimi: %.1f blok (dot<=%.2f) | Oyuncu Basina Cap: %d mob (baz=%d)",
                tracking.getEffectiveRearCullRange(),
                tracking.getRearCullDotThreshold(),
                tracking.getEffectiveMaxMobsPerPlayer(),
                tracking.getMaxMobsPerPlayer()
            ),
            NamedTextColor.LIGHT_PURPLE
        ));
    }

    private void handleRedstone(CommandSender sender) {
        RedstoneHopperGovernor redstone = plugin.getRedstoneGovernor();
        if (redstone == null) {
            sender.sendMessage(Component.text("Redstone & Hopper Governor modulu baslatilmadi.", NamedTextColor.RED));
            return;
        }

        int stateCode = redstone.getLastBrainStateCode();
        String stateName = BrainDirective.stateNameForCode(stateCode);

        sender.sendMessage(Component.text("=== SpicaRustNative Redstone & Hopper Governor ===", NamedTextColor.GOLD));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Durum: %s | Rust Hesaplama: %.3f ms (Toplam: %.3f ms) | Oyuncu Koruma: %s | Lockdown Uyari: %s",
                redstone.isEnabled() ? "AKTIF" : "KAPALI",
                redstone.getLastRustMs(),
                redstone.getLastTotalMs(),
                redstone.isProtectPlayerTriggers() ? "AKTIF" : "KAPALI",
                redstone.isAlertAdminsOnLockdown() ? "AKTIF" : "KAPALI"
            ),
            redstone.isEnabled() ? NamedTextColor.GREEN : NamedTextColor.RED
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Aktif Chunk: %d | Kisitlanan Chunk: %d | Engellenen: Redstone=%d | Hopper=%d (Toplam=%d)",
                redstone.getActiveChunkCount(),
                redstone.getThrottledChunkCount(),
                redstone.getRedstoneBlockedTotal(),
                redstone.getHopperBlockedTotal(),
                redstone.getTotalOpsBlocked()
            ),
            NamedTextColor.AQUA
        ));
        sender.sendMessage(Component.text(
            String.format(
                Locale.US,
                "Efektif Limitler (Brain=%s): Redstone=%d/sn (baz=%d) | Hopper=%d/sn (baz=%d)",
                stateName,
                redstone.getEffectiveRedstoneLimit(),
                redstone.getRedstoneLimitPerSec(),
                redstone.getEffectiveHopperLimit(),
                redstone.getHopperLimitPerSec()
            ),
            colorForState(stateCode)
        ));

        List<RedstoneHopperGovernor.HotRedstoneChunk> hotChunks = redstone.getHotChunks();
        if (hotChunks.isEmpty()) {
            sender.sendMessage(Component.text(
                "En Sicak Top-8 Chunk: Aktif yogun Redstone/Hopper chunk'i tespit edilmedi.",
                NamedTextColor.GRAY
            ));
            return;
        }

        sender.sendMessage(Component.text("--- En Sicak Top-" + hotChunks.size() + " Redstone/Hopper Chunk ---", NamedTextColor.YELLOW));
        int rank = 1;
        for (RedstoneHopperGovernor.HotRedstoneChunk hot : hotChunks) {
            NamedTextColor rowColor = switch (hot.action()) {
                case RustNative.REDSTONE_ACTION_LOCKDOWN -> NamedTextColor.RED;
                case RustNative.REDSTONE_ACTION_THROTTLE_REDSTONE,
                     RustNative.REDSTONE_ACTION_THROTTLE_HOPPERS -> NamedTextColor.GOLD;
                default -> NamedTextColor.GREEN;
            };
            sender.sendMessage(Component.text(
                String.format(
                    Locale.US,
                    "#%d %s Chunk(%d, %d) | Blok(%d, %d) | RS: %d/s | Hop: %d/s | Heat: %.1f | %s (%d%%)",
                    rank++,
                    hot.worldName(),
                    hot.chunkX(),
                    hot.chunkZ(),
                    hot.blockX(),
                    hot.blockZ(),
                    hot.redstonePerSec(),
                    hot.hopperPerSec(),
                    hot.heat(),
                    hot.actionName(),
                    hot.throttlePercent()
                ),
                rowColor
            ));
        }
    }

    private void handleKernels(CommandSender sender) {
        if (!plugin.getApi().isNativeLoaded()) {
            sender.sendMessage(Component.text("Rust native kutuphanesi yuklu degil!", NamedTextColor.RED));
            return;
        }

        sender.sendMessage(Component.text(
            "Async Rust kernel benchmark baslatildi (128x128 Flow-Field + 256x256 FBM Noise + 100x2000 Mob Tracking)...",
            NamedTextColor.YELLOW
        ));

        Bukkit.getScheduler().runTaskAsynchronously(plugin, () -> {
            try {
                BridgeBenchmark.KernelBenchmarkReport r = BridgeBenchmark.runKernelsBenchmark(plugin.getApi());
                Bukkit.getScheduler().runTask(plugin, () -> {
                    sender.sendMessage(Component.text("=== Rust Native Kernels Benchmark (v2.5.0) ===", NamedTextColor.GOLD));
                    sender.sendMessage(Component.text(
                        String.format(
                            Locale.US,
                            "[Flow-Field %dx%d] Rust (Dijkstra+Wall): %.3f ms/calisma (%d iter | erisilebilir: %d hucre)",
                            r.flowWidth(),
                            r.flowDepth(),
                            r.flowFieldMs(),
                            r.flowIterations(),
                            r.flowReachableCells()
                        ),
                        NamedTextColor.GREEN
                    ));
                    sender.sendMessage(Component.text(
                        String.format(
                            Locale.US,
                            "  Duvar arkasi ornek yon @(32,64): (%.3f, %.3f)",
                            r.sampleDirX(),
                            r.sampleDirZ()
                        ),
                        NamedTextColor.GRAY
                    ));
                    sender.sendMessage(Component.text(
                        String.format(
                            Locale.US,
                            "[WorldGen FBM %dx%d] Rust (%d-oktav, %d blok): %.3f ms/calisma (%d iter | aralik: [%.3f, %.3f])",
                            r.noiseWidth(),
                            r.noiseDepth(),
                            r.noiseOctaves(),
                            r.noiseWidth() * r.noiseDepth(),
                            r.noiseMs(),
                            r.noiseIterations(),
                            r.noiseMinHeight(),
                            r.noiseMaxHeight()
                        ),
                        NamedTextColor.AQUA
                    ));
                    sender.sendMessage(Component.text(
                        String.format(
                            Locale.US,
                            "[Mob Tracking %dp x %dm] Rust (%d cift): %.3f ms/calisma (%d iter | gizli: %d | gorunur: %d | butce: %d | delta: %d)",
                            r.trackingPlayers(),
                            r.trackingMobs(),
                            r.trackingPlayers() * r.trackingMobs(),
                            r.trackingMs(),
                            r.trackingIterations(),
                            r.trackingHiddenPairs(),
                            r.trackingVisiblePairs(),
                            r.trackingBudgetCulled(),
                            r.trackingDeltas()
                        ),
                        NamedTextColor.LIGHT_PURPLE
                    ));
                });
            } catch (Throwable t) {
                Bukkit.getScheduler().runTask(plugin, () -> sender.sendMessage(Component.text(
                    "Rust kernel benchmark hatasi: " + t.getMessage(),
                    NamedTextColor.RED
                )));
            }
        });
    }

    private void handleReload(CommandSender sender) {
        plugin.reloadBridgeConfig();
        sender.sendMessage(Component.text("SpicaRustNative yapilandirmasi yenilendi.", NamedTextColor.GREEN));
    }

    private void sendHelp(CommandSender sender) {
        sender.sendMessage(Component.text("=== SpicaRustNative v2.5.0 Komutlari ===", NamedTextColor.GOLD));
        sender.sendMessage(Component.text("/rustbridge status - Native motor, Brain, Farm, Surge, Mob Tracking, Redstone ve optimizer durumu", NamedTextColor.GRAY));
        sender.sendMessage(Component.text("/rustbridge brain - Rust Brain (IPC/In-Process) canli direktif ve metrikler", NamedTextColor.GRAY));
        sender.sendMessage(Component.text("/rustbridge farms - Mob farm & koylu (trading hall) governor durumu", NamedTextColor.GRAY));
        sender.sendMessage(Component.text("/rustbridge surge - Elytra/Chunk Surge Guard ve Patlama tampon metrikleri", NamedTextColor.GRAY));
        sender.sendMessage(Component.text("/rustbridge tracking - Rust Mob Gorunurluk & Paket Takip (hideEntity/showEntity) durumu", NamedTextColor.GRAY));
        sender.sendMessage(Component.text("/rustbridge redstone - Rust Redstone & Hopper Lag-Machine Governor durumu ve en sicak Top 8 chunk", NamedTextColor.GRAY));
        sender.sendMessage(Component.text("/rustbridge kernels - Async 128x128 Flow-Field + 256x256 FBM Noise + 200k Mob Tracking testi", NamedTextColor.GRAY));
        sender.sendMessage(Component.text("/rustbridge chunks [dunya] - Rust O(N) en yogun Top 8 chunk haritasi", NamedTextColor.GRAY));
        sender.sendMessage(Component.text("/rustbridge bench [entity] [oyuncu] [iter] - Java vs Rust MSPT testi", NamedTextColor.GRAY));
        sender.sendMessage(Component.text("/rustbridge scan [dunya] - Canli mob sikismasi / farm taramasi", NamedTextColor.GRAY));
        sender.sendMessage(Component.text("/rustbridge reload - config.yml yenile", NamedTextColor.GRAY));
    }

    private World resolveTargetWorld(CommandSender sender, String[] args) {
        if (args.length >= 2) {
            return Bukkit.getWorld(args[1]);
        }
        if (sender instanceof Player player) {
            return player.getWorld();
        }
        if (!Bukkit.getWorlds().isEmpty()) {
            return Bukkit.getWorlds().get(0);
        }
        return null;
    }

    private static NamedTextColor colorForState(int stateCode) {
        return switch (stateCode) {
            case BrainDirective.STATE_GREEN -> NamedTextColor.GREEN;
            case BrainDirective.STATE_YELLOW -> NamedTextColor.YELLOW;
            case BrainDirective.STATE_ORANGE -> NamedTextColor.GOLD;
            case BrainDirective.STATE_RED -> NamedTextColor.RED;
            case BrainDirective.STATE_EMERGENCY -> NamedTextColor.DARK_RED;
            default -> NamedTextColor.GRAY;
        };
    }

    private int parseInt(String raw, int fallback) {
        try {
            return Integer.parseInt(raw);
        } catch (NumberFormatException e) {
            return fallback;
        }
    }

    @Override
    public List<String> onTabComplete(CommandSender sender, Command command, String alias, String[] args) {
        if (args.length == 1) {
            return List.of("status", "brain", "farms", "surge", "tracking", "redstone", "kernels", "chunks", "bench", "scan", "reload");
        }
        if (args.length == 2 && (args[0].equalsIgnoreCase("scan") || args[0].equalsIgnoreCase("chunks"))) {
            return Bukkit.getWorlds().stream().map(World::getName).toList();
        }
        return List.of();
    }
}
