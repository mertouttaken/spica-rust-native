use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

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
    pub compression_level: u32,
    pub flush_wait_ms: u64,
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
            compression_level: 3,
            flush_wait_ms: 2500,
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
        let body = toml::to_string_pretty(cfg)
            .map_err(|e| format!("TOML serilestirme hatasi: {}", e))?;
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
        self.backup.flush_wait_ms = self.backup.flush_wait_ms.clamp(500, 15_000);
        if self.backup.backup_dir.trim().is_empty() {
            self.backup.backup_dir = "backups".to_string();
        }
        if self.backup.worlds.is_empty() {
            self.backup.worlds = vec!["world".to_string()];
        }
    }
}

/// SplitMix64 mixing function for non-cryptographic-dependency high-entropy token generation.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Generates a 32-character lowercase hexadecimal IPC authentication token
/// using SystemTime nanoseconds, process ID, stack address entropy, and an atomic counter.
pub fn generate_auth_token() -> String {
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let low_time = now_nanos as u64;
    let high_time = (now_nanos >> 64) as u64;
    let pid = u64::from(std::process::id());
    let seq = TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);

    let stack_probe: u64 = 0x517c_c1b7_2722_0a95;
    let addr_entropy = (&stack_probe as *const u64) as usize as u64;

    let seed1 = low_time
        ^ pid.rotate_left(17)
        ^ seq.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ addr_entropy;
    let seed2 = high_time
        ^ low_time.rotate_right(23)
        ^ pid.wrapping_mul(0xbf58_476d_1ce4_e5b9)
        ^ seq.rotate_left(31)
        ^ 0xa076_1d64_78bd_642f;

    let part1 = splitmix64(seed1);
    let part2 = splitmix64(seed2 ^ part1.rotate_left(13));
    format!("{:016x}{:016x}", part1, part2)
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
    }

    #[test]
    fn auth_token_is_32_hex_and_unique() {
        let t1 = generate_auth_token();
        let t2 = generate_auth_token();
        assert_eq!(t1.len(), 32);
        assert_eq!(t2.len(), 32);
        assert!(t1.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(t2.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t1, t2);
    }
}
