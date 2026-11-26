use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    pub server: ServerConfig,
    pub brain: BrainConfig,
    pub native: NativeConfig,
    pub backup: BackupConfig,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            brain: BrainConfig::default(),
            native: NativeConfig::default(),
            backup: BackupConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub jar: String,
    pub java_bin: String,
    pub work_dir: String,
    pub min_ram_mb: u64,
    pub max_ram_mb: u64,
    pub gc_mode: String,
    pub auto_restart: bool,
    pub max_restarts: u32,
    pub accept_eula: bool,
    pub extra_jvm_args: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            jar: "purpur-1.21.4.jar".to_string(),
            java_bin: "java".to_string(),
            work_dir: ".".to_string(),
            min_ram_mb: 0,
            max_ram_mb: 0,
            gc_mode: "auto".to_string(),
            auto_restart: true,
            max_restarts: 5,
            accept_eula: false,
            extra_jvm_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrainConfig {
    pub ipc_host: String,
    pub ipc_port: u16,
    pub allow_non_loopback_ipc: bool,
    pub metrics_port: u16,
    pub enable_metrics_http: bool,
    pub base_full_ai_range: f64,
    pub base_hibernate_range: f64,
    pub min_view_distance: i32,
    pub max_view_distance: i32,
    pub min_sim_distance: i32,
    pub max_sim_distance: i32,
}

impl Default for BrainConfig {
    fn default() -> Self {
        Self {
            ipc_host: "127.0.0.1".to_string(),
            ipc_port: 25595,
            allow_non_loopback_ipc: false,
            metrics_port: 9120,
            enable_metrics_http: true,
            base_full_ai_range: 32.0,
            base_hibernate_range: 56.0,
            min_view_distance: 5,
            max_view_distance: 10,
            min_sim_distance: 4,
            max_sim_distance: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeConfig {
    pub worker_threads: usize,
    pub native_dir: String,
}

impl Default for NativeConfig {
    fn default() -> Self {
        Self {
            worker_threads: 0,
            native_dir: "plugins/SpicaRustNative/native".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupConfig {
    pub enabled: bool,
    pub interval_minutes: u64,
    pub keep_last: usize,
    pub backup_dir: String,
    pub worlds: Vec<String>,
    pub require_all_worlds: bool,
    pub compression_level: u32,
    pub flush_wait_ms: u64,
    pub snapshot_command: Vec<String>,
    pub incremental_autosave: bool,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 60,
            keep_last: 5,
            backup_dir: "backups".to_string(),
            worlds: vec![
                "world".to_string(),
                "world_nether".to_string(),
                "world_the_end".to_string(),
            ],
            require_all_worlds: true,
            compression_level: 3,
            flush_wait_ms: 15_000,
            snapshot_command: Vec::new(),
            incremental_autosave: true,
        }
    }
}

impl LauncherConfig {
    /// Loads `spica-launcher.toml` from `path`, or creates a default config if missing.
    /// Returns `(config, created_new)`.
    pub fn load_or_create(path: &Path) -> Result<(Self, bool), String> {
        if path.exists() {
            let raw = fs::read_to_string(path)
                .map_err(|e| format!("Yapilandirma okunamadi ({}): {}", path.display(), e))?;
            let mut cfg: LauncherConfig = toml::from_str(&raw)
                .map_err(|e| format!("TOML ayristirma hatasi ({}): {}", path.display(), e))?;
            cfg.sanitize();
            Ok((cfg, false))
        } else {
            let cfg = LauncherConfig::default();
            Self::write_config(path, &cfg)?;
            Ok((cfg, true))
        }
    }

    /// Writes a default configuration file to `path`.
    pub fn write_default(path: &Path) -> Result<(), String> {
        let cfg = LauncherConfig::default();
        Self::write_config(path, &cfg)
    }

    fn write_config(path: &Path, cfg: &LauncherConfig) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    format!(
                        "Yapilandirma dizini olusturulamadi ({}): {}",
                        parent.display(),
                        e
                    )
                })?;
            }
        }
        let body =
            toml::to_string_pretty(cfg).map_err(|e| format!("TOML serilestirme hatasi: {}", e))?;
        let header = "# SpicaRustNative - Rust Startup Orchestrator & Adaptive Brain Daemon\n\
                      # Purpur 1.21.4 Core + Rust Brain (spica-launcher v2.6.0)\n\
                      # min_ram_mb / max_ram_mb = 0 -> otomatik donanim boyutlandirmasi\n\
                      # gc_mode = \"auto\" | \"zgc\" | \"g1gc\"\n\n";
        let full = format!("{}{}", header, body);
        fs::write(path, full)
            .map_err(|e| format!("Yapilandirma yazilamadi ({}): {}", path.display(), e))
    }

    /// Ensures numeric bounds and ranges stay valid even if manually edited.
    pub fn sanitize(&mut self) {
        if self.server.jar.trim().is_empty() {
            self.server.jar = "purpur-1.21.4.jar".to_string();
        }
        if self.server.java_bin.trim().is_empty() {
            self.server.java_bin = "java".to_string();
        }
        if self.server.work_dir.trim().is_empty() {
            self.server.work_dir = ".".to_string();
        }
        if self.brain.ipc_host.trim().is_empty() {
            self.brain.ipc_host = "127.0.0.1".to_string();
        }

        self.brain.min_view_distance = self.brain.min_view_distance.clamp(2, 32);
        self.brain.max_view_distance = self
            .brain
            .max_view_distance
            .clamp(self.brain.min_view_distance, 32);

        self.brain.min_sim_distance = self.brain.min_sim_distance.clamp(2, 32);
        self.brain.max_sim_distance = self
            .brain
            .max_sim_distance
            .clamp(self.brain.min_sim_distance, 32);

        if !self.brain.base_full_ai_range.is_finite() || self.brain.base_full_ai_range < 8.0 {
            self.brain.base_full_ai_range = 32.0;
        }
        if !self.brain.base_hibernate_range.is_finite()
            || self.brain.base_hibernate_range <= self.brain.base_full_ai_range
        {
            self.brain.base_hibernate_range = self.brain.base_full_ai_range + 16.0;
        }

        self.backup.interval_minutes = self.backup.interval_minutes.max(1);
        self.backup.keep_last = self.backup.keep_last.max(1);
        self.backup.compression_level = self.backup.compression_level.clamp(1, 9);
        self.backup.flush_wait_ms = self.backup.flush_wait_ms.clamp(500, 300_000);
        if self.backup.backup_dir.trim().is_empty() {
            self.backup.backup_dir = "backups".to_string();
        }
        if self.backup.worlds.is_empty() {
            self.backup.worlds = vec!["world".to_string()];
        }
    }
}

/// Generates a 64-character (256-bit) lowercase hexadecimal IPC authentication token using
/// the operating system CSPRNG (`getrandom`: `BCryptGenRandom` on Windows, `getrandom(2)` on Linux).
pub fn generate_auth_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes)
        .expect("OS CSPRNG (getrandom) basarisiz oldu; guvenli IPC token uretilemiyor");
    let mut out = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{:02x}", b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrip() {
        let cfg = LauncherConfig::default();
        let encoded = toml::to_string_pretty(&cfg).expect("serialize default config");
        let decoded: LauncherConfig = toml::from_str(&encoded).expect("deserialize default config");
        assert_eq!(decoded.server.jar, "purpur-1.21.4.jar");
        assert_eq!(decoded.brain.ipc_port, 25595);
        assert_eq!(decoded.brain.metrics_port, 9120);
        assert_eq!(decoded.brain.min_view_distance, 5);
        assert_eq!(decoded.brain.max_view_distance, 10);
        assert_eq!(decoded.native.worker_threads, 0);
        assert!(decoded.backup.require_all_worlds);
    }

    #[test]
    fn auth_token_is_64_hex_csprng_and_unique() {
        let t1 = generate_auth_token();
        let t2 = generate_auth_token();
        assert_eq!(t1.len(), 64);
        assert_eq!(t2.len(), 64);
        assert!(t1.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(t2.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t1, t2);
    }
}
