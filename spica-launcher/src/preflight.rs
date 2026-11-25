use crate::config::{LauncherConfig, ServerConfig};
use std::fs;
use std::path::{Path, PathBuf};
use sysinfo::System;

#[derive(Debug, Clone)]
pub struct HostHardware {
    pub total_ram_mb: u64,
    pub used_ram_mb: u64,
    pub logical_cores: usize,
    pub os: &'static str,
    pub arch: &'static str,
}

#[derive(Debug, Clone)]
pub struct JvmPlan {
    pub java_bin: String,
    pub args: Vec<String>,
    pub ipc_token: String,
    pub min_ram_mb: u64,
    pub max_ram_mb: u64,
    pub resolved_gc: String,
    pub resolved_native_threads: usize,
    pub jar_path: PathBuf,
    pub work_dir: PathBuf,
}

/// Detects host physical memory, logical CPU core count, OS, and architecture.
pub fn detect_hardware() -> HostHardware {
    let sys = System::new_all();
    let total_ram_mb = sys.total_memory() / (1024 * 1024);
    let used_ram_mb = sys.used_memory() / (1024 * 1024);
    let logical_cores = sys.cpus().len().max(1);

    HostHardware {
        total_ram_mb,
        used_ram_mb,
        logical_cores,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

/// Computes `(min_ram_mb, max_ram_mb)` leaving safe OS headroom when auto-sized (`0`).
pub fn compute_ram_mb(server: &ServerConfig, hw: &HostHardware) -> (u64, u64) {
    let max_ram = if server.max_ram_mb == 0 {
        if hw.total_ram_mb >= 16384 {
            8192
        } else if hw.total_ram_mb >= 8192 {
            4096
        } else {
            (hw.total_ram_mb / 2).max(2048)
        }
    } else {
        server.max_ram_mb.max(512)
    };

    let min_ram = if server.min_ram_mb == 0 {
        max_ram
    } else {
        server.min_ram_mb.min(max_ram).max(256)
    };

    (min_ram, max_ram)
}

/// Builds the complete Java 21+ Purpur launch plan including GC flags and Brain IPC properties.
pub fn build_jvm_plan(cfg: &LauncherConfig, hw: &HostHardware, ipc_token: &str) -> JvmPlan {
    let (min_ram_mb, max_ram_mb) = compute_ram_mb(&cfg.server, hw);

    let use_zgc = match cfg.server.gc_mode.trim().to_ascii_lowercase().as_str() {
        "zgc" => true,
        "g1gc" => false,
        _ => max_ram_mb >= 6144 && hw.logical_cores >= 4,
    };

    let resolved_gc = if use_zgc {
        "Generational ZGC".to_string()
    } else {
        "Aikar Tuned G1GC".to_string()
    };

    let resolved_native_threads = if cfg.native.worker_threads == 0 {
        (hw.logical_cores / 2).clamp(1, 8)
    } else {
        cfg.native.worker_threads
    };

    let work_dir = PathBuf::from(&cfg.server.work_dir);
    let jar_candidate = Path::new(&cfg.server.jar);
    let jar_path = if jar_candidate.is_absolute() {
        jar_candidate.to_path_buf()
    } else {
        work_dir.join(jar_candidate)
    };

    let mut args: Vec<String> = Vec::with_capacity(40);
    args.push(format!("-Xms{}M", min_ram_mb));
    args.push(format!("-Xmx{}M", max_ram_mb));

    if use_zgc {
        args.push("-XX:+UseZGC".to_string());
        args.push("-XX:+ZGenerational".to_string());
        args.push("-XX:+AlwaysPreTouch".to_string());
    } else {
        let g1_flags = [
            "-XX:+UseG1GC",
            "-XX:+ParallelRefProcEnabled",
            "-XX:MaxGCPauseMillis=150",
            "-XX:+UnlockExperimentalVMOptions",
            "-XX:+DisableExplicitGC",
            "-XX:+AlwaysPreTouch",
            "-XX:G1NewSizePercent=30",
            "-XX:G1MaxNewSizePercent=40",
            "-XX:G1HeapRegionSize=8M",
            "-XX:G1ReservePercent=20",
            "-XX:G1HeapWastePercent=5",
            "-XX:G1MixedGCCountTarget=4",
            "-XX:InitiatingHeapOccupancyPercent=15",
            "-XX:G1MixedGCLiveThresholdPercent=90",
            "-XX:G1RSetUpdatingPauseTimePercent=5",
            "-XX:SurvivorRatio=32",
            "-XX:+PerfDisableSharedMem",
            "-XX:MaxTenuringThreshold=1",
        ];
        for flag in g1_flags {
            args.push(flag.to_string());
        }
    }

    // Spica Brain IPC & Native bridge system properties (Y7: IPC token is passed via child env SPICA_IPC_TOKEN, never CLI args)
    args.push(format!("-Dspica.ipc.host={}", cfg.brain.ipc_host));
    args.push(format!("-Dspica.ipc.port={}", cfg.brain.ipc_port));
    args.push(format!("-Dspica.native.threads={}", resolved_native_threads));
    if !cfg.native.native_dir.trim().is_empty() {
        args.push(format!("-Dspica.native.dir={}", cfg.native.native_dir));
    }

    // Clean piped console output for Purpur/Paper
    args.push("-Dterminal.jline=false".to_string());
    args.push("-Dterminal.ansi=true".to_string());

    // User extra JVM args (placed before -jar)
    for extra in &cfg.server.extra_jvm_args {
        let trimmed = extra.trim();
        if !trimmed.is_empty() {
            args.push(trimmed.to_string());
        }
    }

    args.push("-jar".to_string());
    args.push(cfg.server.jar.clone());
    args.push("nogui".to_string());

    JvmPlan {
        java_bin: cfg.server.java_bin.clone(),
        args,
        ipc_token: ipc_token.to_string(),
        min_ram_mb,
        max_ram_mb,
        resolved_gc,
        resolved_native_threads,
        jar_path,
        work_dir,
    }
}

impl JvmPlan {
    /// Renders a human-readable command line for logs or `--dry-run`.
    pub fn format_command(&self, mask_token: bool) -> String {
        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(self.java_bin.clone());
        for arg in &self.args {
            if mask_token && arg.starts_with("-Dspica.ipc.token=") {
                parts.push("-Dspica.ipc.token=<redacted>".to_string());
            } else if arg.contains(' ') {
                parts.push(format!("\"{}\"", arg));
            } else {
                parts.push(arg.clone());
            }
        }
        parts.join(" ")
    }
}

/// Validates work directory, Purpur JAR presence, and `eula.txt` state.
/// Returns a list of non-fatal warnings on success, or a clear Turkish error message on failure.
pub fn validate_environment(cfg: &LauncherConfig, plan: &JvmPlan) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();

    if !plan.work_dir.exists() {
        return Err(format!(
            "HATA: Sunucu calisma dizini bulunamadi: '{}'. Lutfen spica-launcher.toml icindeki 'work_dir' ayarini kontrol edin.",
            plan.work_dir.display()
        ));
    }

    if !plan.jar_path.is_file() {
        return Err(format!(
            "HATA: Purpur sunucu JAR dosyasi bulunamadi: '{}'.\n\
             Cozum: Purpur 1.21.4 JAR dosyasini bu konuma yerlestirin, spica-launcher.toml icindeki 'jar' alanini guncelleyin veya '--jar <dosya_yolu>' parametresini kullanin.",
            plan.jar_path.display()
        ));
    }

    let eula_path = plan.work_dir.join("eula.txt");
    let eula_accepted = if eula_path.is_file() {
        match fs::read_to_string(&eula_path) {
            Ok(content) => content.lines().any(|line| {
                let t = line.trim().to_ascii_lowercase();
                t == "eula=true"
            }),
            Err(_) => false,
        }
    } else {
        false
    };

    if !eula_accepted {
        if cfg.server.accept_eula {
            let eula_content = "# Minecraft EULA - Spica Launcher tarafindan accept_eula=true ayari ile onaylandi\n\
                                # https://aka.ms/MinecraftEULA\n\
                                eula=true\n";
            fs::write(&eula_path, eula_content).map_err(|e| {
                format!(
                    "HATA: eula.txt dosyasi yazilamadi ({}): {}",
                    eula_path.display(),
                    e
                )
            })?;
            warnings.push(format!(
                "eula.txt dosyasi 'accept_eula = true' ayari nedeniyle otomatik olarak onaylandi ({}).",
                eula_path.display()
            ));
        } else {
            warnings.push(format!(
                "UYARI: '{}' icinde eula=true bulunamadi. Sunucunun acilabilmesi icin eula.txt dosyasini onaylayin veya spica-launcher.toml icinde 'accept_eula = true' yapin.",
                eula_path.display()
            ));
        }
    }

    Ok(warnings)
}

/// Writes a safe, non-destructive `spica-purpur-tuning.txt` guide tailored to detected hardware
/// and SpicaRustNative Brain configuration. Never overwrites existing server configs or worlds.
pub fn write_tuning_guide(
    cfg: &LauncherConfig,
    hw: &HostHardware,
    plan: &JvmPlan,
) -> Result<PathBuf, String> {
    let out_path = plan.work_dir.join("spica-purpur-tuning.txt");

    let chunk_worker_threads = (hw.logical_cores / 2).clamp(2, 8);
    let content = format!(
        "====================================================================\n\
         SpicaRustNative + Purpur 1.21.4 Donanim Uyumlu Performans Rehberi\n\
         (spica-launcher v2.0.0 --tune tarafindan guvenli olarak olusturuldu)\n\
         ====================================================================\n\n\
         Algilanan Donanim:\n\
         - Isletim Sistemi : {} ({})\n\
         - Mantiksal CPU   : {} cekirdek\n\
         - Toplam RAM      : {} MB (Kullanilan: {} MB)\n\
         - Onerilen JVM    : -Xms{}M -Xmx{}M | GC: {}\n\
         - Native Threads  : {}\n\n\
         Not: Bu dosya mevcut dunyalarinizi veya yapilandirma dosyalarinizi\n\
         DEGISTIRMEZ. Asagidaki onerileri ilgili dosyalara guvenle uygulayabilirsiniz.\n\n\
         1) server.properties\n\
            view-distance={}\n\
            simulation-distance={}\n\
            network-compression-threshold=256\n\
            sync-chunk-writes=false\n\
            max-tick-time=60000\n\n\
         2) config/paper-global.yml\n\
            chunk-system:\n\
              io-threads: {}\n\
              worker-threads: {}\n\
            misc:\n\
              use-alternative-luck-formula: true\n\n\
         3) config/paper-world-defaults.yml\n\
            chunks:\n\
              delay-chunk-unloads-by: 10s\n\
              max-auto-save-chunks-per-tick: 8\n\
              prevent-moving-into-unloaded-chunks: true\n\
            entities:\n\
              spawning:\n\
                per-player-mob-spawns: true\n\
                alt-item-despawn-rate:\n\
                  enabled: true\n\
            environment:\n\
              optimize-explosions: true\n\n\
         4) purpur.yml\n\
            settings:\n\
              use-alternate-keepalive: true\n\
              tps-catchup: true\n\
            world-settings:\n\
              default:\n\
                mobs:\n\
                  villager:\n\
                    lobotomize:\n\
                      enabled: true\n\
                gameplay-mechanics:\n\
                  entity-lifespan: 0\n\n\
         5) SpicaRustNative Adaptive Brain Eslesmesi\n\
            - IPC Adresi         : {}:{}\n\
            - Metrik HTTP        : 127.0.0.1:{} (/status, /metrics)\n\
            - Dinamik View Dist  : {} .. {}\n\
            - Dinamik Sim Dist   : {} .. {}\n\
            - AI Full Range      : {:.1} blok\n\
            - Hibernate Range    : {:.1} blok\n",
        hw.os,
        hw.arch,
        hw.logical_cores,
        hw.total_ram_mb,
        hw.used_ram_mb,
        plan.min_ram_mb,
        plan.max_ram_mb,
        plan.resolved_gc,
        plan.resolved_native_threads,
        cfg.brain.max_view_distance,
        cfg.brain.max_sim_distance,
        (hw.logical_cores / 4).clamp(1, 4),
        chunk_worker_threads,
        cfg.brain.ipc_host,
        cfg.brain.ipc_port,
        cfg.brain.metrics_port,
        cfg.brain.min_view_distance,
        cfg.brain.max_view_distance,
        cfg.brain.min_sim_distance,
        cfg.brain.max_sim_distance,
        cfg.brain.base_full_ai_range,
        cfg.brain.base_hibernate_range,
    );

    fs::write(&out_path, content).map_err(|e| {
        format!(
            "HATA: Tuning rehberi yazilamadi ({}): {}",
            out_path.display(),
            e
        )
    })?;

    Ok(out_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_ram_and_zgc_selection_on_high_spec_host() {
        let cfg = LauncherConfig::default();
        let hw = HostHardware {
            total_ram_mb: 32768,
            used_ram_mb: 8192,
            logical_cores: 8,
            os: "linux",
            arch: "x86_64",
        };
        let plan = build_jvm_plan(&cfg, &hw, "0123456789abcdef0123456789abcdef");
        assert_eq!(plan.min_ram_mb, 8192);
        assert_eq!(plan.max_ram_mb, 8192);
        assert!(plan.args.contains(&"-XX:+UseZGC".to_string()));
        assert!(plan.args.contains(&"-XX:+ZGenerational".to_string()));
        assert_eq!(plan.ipc_token, "0123456789abcdef0123456789abcdef");
        assert!(
            !plan.args.iter().any(|a| a.contains("ipc.token")),
            "IPC token must never appear in JVM CLI args"
        );
    }

    #[test]
    fn auto_ram_and_g1gc_selection_on_mid_spec_host() {
        let cfg = LauncherConfig::default();
        let hw = HostHardware {
            total_ram_mb: 8192,
            used_ram_mb: 2048,
            logical_cores: 4,
            os: "linux",
            arch: "x86_64",
        };
        let plan = build_jvm_plan(&cfg, &hw, "deadbeef");
        assert_eq!(plan.max_ram_mb, 4096);
        assert!(plan.args.contains(&"-XX:+UseG1GC".to_string()));
        assert!(plan.args.contains(&"-XX:G1HeapRegionSize=8M".to_string()));
    }
}
