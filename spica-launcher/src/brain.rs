use crate::config::BrainConfig;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Instant;

pub const EWMA_ALPHA: f64 = 0.25;
const HYSTERESIS_RECOVERY_TICKS: u8 = 2;
pub const HISTORY_CAPACITY: usize = 120;

/// Severity state of the adaptive Rust Brain Governor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BrainLevel {
    Green = 0,
    Yellow = 1,
    Orange = 2,
    Red = 3,
    Emergency = 4,
}

impl BrainLevel {
    pub fn code(self) -> u8 {
        match self {
            BrainLevel::Green => 0,
            BrainLevel::Yellow => 1,
            BrainLevel::Orange => 2,
            BrainLevel::Red => 3,
            BrainLevel::Emergency => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BrainLevel::Green => "GREEN",
            BrainLevel::Yellow => "YELLOW",
            BrainLevel::Orange => "ORANGE",
            BrainLevel::Red => "RED",
            BrainLevel::Emergency => "EMERGENCY",
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            BrainLevel::Green => "healthy",
            BrainLevel::Yellow => "moderate_load",
            BrainLevel::Orange => "high_load",
            BrainLevel::Red => "critical_load",
            BrainLevel::Emergency => "emergency_overload",
        }
    }

    pub fn step_down(self) -> Self {
        match self {
            BrainLevel::Green => BrainLevel::Green,
            BrainLevel::Yellow => BrainLevel::Green,
            BrainLevel::Orange => BrainLevel::Yellow,
            BrainLevel::Red => BrainLevel::Orange,
            BrainLevel::Emergency => BrainLevel::Red,
        }
    }

    fn from_metrics(ewma_mspt: f64, tps: f64) -> Self {
        let mut level = if ewma_mspt >= 55.0 {
            BrainLevel::Emergency
        } else if ewma_mspt >= 46.0 {
            BrainLevel::Red
        } else if ewma_mspt >= 38.0 {
            BrainLevel::Orange
        } else if ewma_mspt >= 28.0 {
            BrainLevel::Yellow
        } else {
            BrainLevel::Green
        };

        // Secondary TPS safety guard if TPS collapses despite transient MSPT readings
        if tps > 0.0 {
            if tps < 14.0 && level < BrainLevel::Emergency {
                level = BrainLevel::Emergency;
            } else if tps < 16.5 && level < BrainLevel::Red {
                level = BrainLevel::Red;
            } else if tps < 18.0 && level < BrainLevel::Orange {
                level = BrainLevel::Orange;
            }
        }

        level
    }
}

/// Telemetry payload sent from Purpur (`BrainIpcClient`) over NDJSON TCP IPC.
#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryPayload {
    pub mspt: f64,
    pub tps: f64,
    #[serde(default)]
    pub entities: u32,
    #[serde(default)]
    pub hibernated: u32,
    #[serde(rename = "farmThrottled", default)]
    pub farm_throttled: u32,
    #[serde(rename = "villagersThrottled", default)]
    pub villagers_throttled: u32,
    #[serde(default)]
    pub players: u32,
    #[serde(rename = "loadedChunks", default)]
    pub loaded_chunks: u32,
    #[serde(rename = "heapUsedMb", default)]
    pub heap_used_mb: u64,
    #[serde(rename = "heapMaxMb", default)]
    pub heap_max_mb: u64,
}

/// Directive payload sent back to Purpur (`BrainIpcClient`) matching the exact IPC JSON contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainDirective {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub state: String,
    #[serde(rename = "stateCode")]
    pub state_code: u8,
    #[serde(rename = "ewmaMspt")]
    pub ewma_mspt: f64,
    #[serde(rename = "viewDistance")]
    pub view_distance: i32,
    #[serde(rename = "simDistance")]
    pub sim_distance: i32,
    #[serde(rename = "fullAiRange")]
    pub full_ai_range: f64,
    #[serde(rename = "hibernateRange")]
    pub hibernate_range: f64,
    #[serde(rename = "itemMergeEnabled")]
    pub item_merge_enabled: bool,
    #[serde(rename = "itemMergeRadius")]
    pub item_merge_radius: f64,
    pub reason: String,
}

/// Immutable JSON-serializable snapshot of BrainState + Host metrics (used by `/status`, `/metrics`, `:brain`, `:stats`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainSnapshot {
    pub mspt: f64,
    pub ewma_mspt: f64,
    pub tps: f64,
    pub entities: u32,
    pub hibernated: u32,
    pub farm_throttled: u32,
    pub villagers_throttled: u32,
    pub players: u32,
    pub loaded_chunks: u32,
    pub heap_used_mb: u64,
    pub heap_max_mb: u64,
    pub host_cpu_pct: f32,
    pub host_ram_used_mb: u64,
    pub host_ram_total_mb: u64,
    pub state: String,
    pub state_code: u8,
    pub directive: BrainDirective,
    pub connected: bool,
    pub server_name: String,
    pub plugin_version: String,
    pub uptime_secs: u64,
}

/// Single historical telemetry sample kept in the 120-second ring buffer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySample {
    pub uptime_secs: u64,
    pub mspt: f64,
    pub ewma_mspt: f64,
    pub tps: f64,
    pub entities: u32,
    pub hibernated: u32,
    pub farm_throttled: u32,
    pub villagers_throttled: u32,
    pub players: u32,
    pub heap_used_mb: u64,
    pub host_cpu_pct: f32,
    pub state_code: u8,
}

/// Shared adaptive Brain state protected by `Arc<Mutex<BrainState>>`.
#[derive(Debug)]
pub struct BrainState {
    pub mspt: f64,
    pub ewma_mspt: f64,
    pub tps: f64,
    pub entities: u32,
    pub hibernated: u32,
    pub farm_throttled: u32,
    pub villagers_throttled: u32,
    pub players: u32,
    pub loaded_chunks: u32,
    pub heap_used_mb: u64,
    pub heap_max_mb: u64,
    pub host_cpu_pct: f32,
    pub host_ram_used_mb: u64,
    pub host_ram_total_mb: u64,
    pub state: String,
    pub state_code: u8,
    pub directive: BrainDirective,
    pub connected: bool,
    pub server_name: String,
    pub plugin_version: String,
    pub uptime_secs: u64,

    // Internal governor state
    started_at: Instant,
    level: BrainLevel,
    recovery_streak: u8,
    samples: u64,
    config: BrainConfig,
    next_conn_id: u64,
    active_conn_id: u64,
    history: VecDeque<TelemetrySample>,
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn lerp_distance(min_val: i32, max_val: i32, factor: f64) -> i32 {
    let min_v = min_val.min(max_val);
    let max_v = max_val.max(min_val);
    let interpolated = (min_v as f64 + (max_v - min_v) as f64 * factor).round() as i32;
    interpolated.clamp(min_v, max_v)
}

fn build_directive(level: BrainLevel, ewma_mspt: f64, cfg: &BrainConfig) -> BrainDirective {
    let min_vd = cfg.min_view_distance;
    let max_vd = cfg.max_view_distance.max(min_vd);
    let min_sd = cfg.min_sim_distance;
    let max_sd = cfg.max_sim_distance.max(min_sd);
    let base_ai = cfg.base_full_ai_range;
    let base_hib = cfg.base_hibernate_range;

    let (view_distance, sim_distance, full_ai_range, hibernate_range, item_merge_enabled, item_merge_radius) =
        match level {
            BrainLevel::Green => (max_vd, max_sd, round1(base_ai), round1(base_hib), false, 2.5),
            BrainLevel::Yellow => (
                lerp_distance(min_vd, max_vd, 0.80),
                lerp_distance(min_sd, max_sd, 0.75),
                round1((base_ai * 0.875).max(12.0)),
                round1((base_hib * 0.85).max(24.0)),
                false,
                3.0,
            ),
            BrainLevel::Orange => (
                lerp_distance(min_vd, max_vd, 0.55),
                lerp_distance(min_sd, max_sd, 0.50),
                round1((base_ai * 0.75).max(12.0)),
                round1((base_hib * 0.72).max(22.0)),
                true,
                3.5,
            ),
            BrainLevel::Red => (
                lerp_distance(min_vd, max_vd, 0.25),
                lerp_distance(min_sd, max_sd, 0.25),
                round1((base_ai * 0.60).max(10.0)),
                round1((base_hib * 0.58).max(20.0)),
                true,
                4.5,
            ),
            BrainLevel::Emergency => (
                min_vd,
                min_sd,
                round1((base_ai * 0.45).max(10.0)),
                round1((base_hib * 0.45).max(18.0)),
                true,
                6.0,
            ),
        };

    BrainDirective {
        msg_type: "directive".to_string(),
        state: level.as_str().to_string(),
        state_code: level.code(),
        ewma_mspt: round1(ewma_mspt),
        view_distance,
        sim_distance,
        full_ai_range,
        hibernate_range,
        item_merge_enabled,
        item_merge_radius,
        reason: level.reason().to_string(),
    }
}

impl BrainState {
    pub fn new(config: BrainConfig, host_ram_used_mb: u64, host_ram_total_mb: u64) -> Self {
        let initial_level = BrainLevel::Green;
        let directive = build_directive(initial_level, 0.0, &config);
        Self {
            mspt: 0.0,
            ewma_mspt: 0.0,
            tps: 20.0,
            entities: 0,
            hibernated: 0,
            farm_throttled: 0,
            villagers_throttled: 0,
            players: 0,
            loaded_chunks: 0,
            heap_used_mb: 0,
            heap_max_mb: 0,
            host_cpu_pct: 0.0,
            host_ram_used_mb,
            host_ram_total_mb,
            state: initial_level.as_str().to_string(),
            state_code: initial_level.code(),
            directive,
            connected: false,
            server_name: String::new(),
            plugin_version: String::new(),
            uptime_secs: 0,
            started_at: Instant::now(),
            level: initial_level,
            recovery_streak: 0,
            samples: 0,
            config,
            next_conn_id: 0,
            active_conn_id: 0,
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
        }
    }

    /// Registers an authenticated Purpur IPC session and returns its connection ID.
    pub fn register_connection(&mut self, server_name: String, plugin_version: String) -> u64 {
        self.next_conn_id = self.next_conn_id.wrapping_add(1).max(1);
        self.active_conn_id = self.next_conn_id;
        self.connected = true;
        self.server_name = server_name;
        self.plugin_version = plugin_version;
        self.uptime_secs = self.started_at.elapsed().as_secs();
        self.active_conn_id
    }

    /// Clears IPC connected flag only if `conn_id` is still the active connection.
    pub fn clear_connection(&mut self, conn_id: u64) {
        if self.active_conn_id == conn_id {
            self.connected = false;
        }
        self.uptime_secs = self.started_at.elapsed().as_secs();
    }

    /// Marks IPC disconnected unconditionally (e.g. when Purpur child process exits).
    pub fn mark_disconnected(&mut self) {
        self.connected = false;
        self.uptime_secs = self.started_at.elapsed().as_secs();
    }

    /// Updates host CPU & RAM metrics sampled by `sysinfo`.
    pub fn update_host_metrics(&mut self, cpu_pct: f32, ram_used_mb: u64, ram_total_mb: u64) {
        self.host_cpu_pct = if cpu_pct.is_finite() && cpu_pct >= 0.0 {
            cpu_pct
        } else {
            0.0
        };
        self.host_ram_used_mb = ram_used_mb;
        self.host_ram_total_mb = ram_total_mb;
        self.uptime_secs = self.started_at.elapsed().as_secs();
    }

    /// Evaluates incoming Purpur telemetry using EWMA (alpha = 0.25) and 2-tick recovery hysteresis,
    /// updates internal state, and returns the resulting `BrainDirective`.
    pub fn ingest_telemetry(&mut self, telemetry: &TelemetryPayload) -> BrainDirective {
        let clean_mspt = if telemetry.mspt.is_finite() && telemetry.mspt >= 0.0 {
            telemetry.mspt
        } else {
            0.0
        };
        let clean_tps = if telemetry.tps.is_finite() && telemetry.tps >= 0.0 {
            telemetry.tps.min(20.0)
        } else {
            20.0
        };

        if self.samples == 0 {
            self.ewma_mspt = clean_mspt;
        } else {
            self.ewma_mspt = EWMA_ALPHA * clean_mspt + (1.0 - EWMA_ALPHA) * self.ewma_mspt;
        }
        self.samples = self.samples.saturating_add(1);

        self.mspt = round1(clean_mspt);
        self.tps = (clean_tps * 100.0).round() / 100.0;
        self.entities = telemetry.entities;
        self.hibernated = telemetry.hibernated;
        self.farm_throttled = telemetry.farm_throttled;
        self.villagers_throttled = telemetry.villagers_throttled;
        self.players = telemetry.players;
        self.loaded_chunks = telemetry.loaded_chunks;
        self.heap_used_mb = telemetry.heap_used_mb;
        self.heap_max_mb = telemetry.heap_max_mb;
        self.uptime_secs = self.started_at.elapsed().as_secs();

        let target = BrainLevel::from_metrics(self.ewma_mspt, clean_tps);
        if target > self.level {
            // Escalate immediately to protect TPS under sudden load spikes
            self.level = target;
            self.recovery_streak = 0;
        } else if target < self.level {
            // Hysteresis: require 2 consecutive recovery samples before stepping down 1 level
            self.recovery_streak = self.recovery_streak.saturating_add(1);
            if self.recovery_streak >= HYSTERESIS_RECOVERY_TICKS {
                self.level = self.level.step_down();
                self.recovery_streak = 0;
            }
        } else {
            self.recovery_streak = 0;
        }

        self.state = self.level.as_str().to_string();
        self.state_code = self.level.code();
        self.directive = build_directive(self.level, self.ewma_mspt, &self.config);

        if self.history.len() >= HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(TelemetrySample {
            uptime_secs: self.uptime_secs,
            mspt: self.mspt,
            ewma_mspt: round1(self.ewma_mspt),
            tps: self.tps,
            entities: self.entities,
            hibernated: self.hibernated,
            farm_throttled: self.farm_throttled,
            villagers_throttled: self.villagers_throttled,
            players: self.players,
            heap_used_mb: self.heap_used_mb,
            host_cpu_pct: (self.host_cpu_pct * 10.0).round() / 10.0,
            state_code: self.state_code,
        });

        self.directive.clone()
    }

    /// Returns a chronological snapshot of up to the last 120 telemetry samples.
    pub fn history_snapshot(&self) -> Vec<TelemetrySample> {
        self.history.iter().cloned().collect()
    }

    /// Produces a fresh serializable snapshot of all Brain & Host metrics.
    pub fn snapshot(&self) -> BrainSnapshot {
        BrainSnapshot {
            mspt: self.mspt,
            ewma_mspt: round1(self.ewma_mspt),
            tps: self.tps,
            entities: self.entities,
            hibernated: self.hibernated,
            farm_throttled: self.farm_throttled,
            villagers_throttled: self.villagers_throttled,
            players: self.players,
            loaded_chunks: self.loaded_chunks,
            heap_used_mb: self.heap_used_mb,
            heap_max_mb: self.heap_max_mb,
            host_cpu_pct: (self.host_cpu_pct * 10.0).round() / 10.0,
            host_ram_used_mb: self.host_ram_used_mb,
            host_ram_total_mb: self.host_ram_total_mb,
            state: self.state.clone(),
            state_code: self.state_code,
            directive: self.directive.clone(),
            connected: self.connected,
            server_name: self.server_name.clone(),
            plugin_version: self.plugin_version.clone(),
            uptime_secs: self.started_at.elapsed().as_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(mspt: f64, tps: f64) -> TelemetryPayload {
        TelemetryPayload {
            mspt,
            tps,
            entities: 420,
            hibernated: 120,
            farm_throttled: 0,
            villagers_throttled: 0,
            players: 18,
            loaded_chunks: 1600,
            heap_used_mb: 2400,
            heap_max_mb: 8192,
        }
    }

    #[test]
    fn test_ewma_calculation() {
        let mut brain = BrainState::new(BrainConfig::default(), 4096, 16384);

        let d1 = brain.ingest_telemetry(&sample(20.0, 20.0));
        assert!((brain.ewma_mspt - 20.0).abs() < 1e-6);
        assert_eq!(d1.state, "GREEN");
        assert_eq!(d1.state_code, 0);

        // Second sample: 0.25 * 40.0 + 0.75 * 20.0 = 25.0
        let d2 = brain.ingest_telemetry(&sample(40.0, 20.0));
        assert!((brain.ewma_mspt - 25.0).abs() < 1e-6);
        assert_eq!(d2.ewma_mspt, 25.0);
        assert_eq!(d2.state, "GREEN");
    }

    #[test]
    fn test_immediate_escalation() {
        let mut brain = BrainState::new(BrainConfig::default(), 4096, 16384);

        // Start healthy
        let d1 = brain.ingest_telemetry(&sample(16.0, 20.0));
        assert_eq!(d1.state_code, 0);
        assert_eq!(d1.view_distance, 10);
        assert_eq!(d1.sim_distance, 8);
        assert!(!d1.item_merge_enabled);

        // Heavy lag spike pushes EWMA above 55ms -> immediate EMERGENCY
        let _ = brain.ingest_telemetry(&sample(180.0, 12.0));
        assert_eq!(brain.state, "EMERGENCY");
        assert_eq!(brain.state_code, 4);
        assert_eq!(brain.directive.view_distance, 5);
        assert_eq!(brain.directive.sim_distance, 4);
        assert!(brain.directive.item_merge_enabled);
        assert_eq!(brain.directive.reason, "emergency_overload");
    }

    #[test]
    fn test_hysteresis_requires_two_consecutive_recovery_ticks() {
        let mut brain = BrainState::new(BrainConfig::default(), 4096, 16384);

        // First sample directly in ORANGE range (ewma = 42.0)
        let d0 = brain.ingest_telemetry(&sample(42.0, 19.5));
        assert_eq!(d0.state, "ORANGE");
        assert_eq!(d0.state_code, 2);

        // Bring EWMA into YELLOW/GREEN territory:
        // Tick 1 recovery: 0.25*10 + 0.75*42 = 34.0 (target = YELLOW < ORANGE) -> streak = 1, stays ORANGE
        let d1 = brain.ingest_telemetry(&sample(10.0, 20.0));
        assert_eq!(d1.state, "ORANGE");
        assert_eq!(d1.state_code, 2);

        // Tick 2 recovery: 0.25*10 + 0.75*34 = 28.0 (target = YELLOW < ORANGE) -> streak = 2, steps down to YELLOW!
        let d2 = brain.ingest_telemetry(&sample(10.0, 20.0));
        assert_eq!(d2.state, "YELLOW");
        assert_eq!(d2.state_code, 1);

        // Tick 3 recovery: 0.25*10 + 0.75*28 = 23.5 (target = GREEN < YELLOW) -> streak = 1, stays YELLOW
        let d3 = brain.ingest_telemetry(&sample(10.0, 20.0));
        assert_eq!(d3.state, "YELLOW");
        assert_eq!(d3.state_code, 1);

        // Brief load bump resets streak (target = YELLOW == YELLOW): 0.25*44 + 0.75*23.5 = 28.625
        let d4 = brain.ingest_telemetry(&sample(44.0, 20.0));
        assert_eq!(d4.state, "YELLOW");

        // Now needs 2 fresh healthy ticks to reach GREEN:
        // 0.25*10 + 0.75*28.625 = 23.96875 -> streak = 1, stays YELLOW
        let d5 = brain.ingest_telemetry(&sample(10.0, 20.0));
        assert_eq!(d5.state, "YELLOW");

        // 0.25*10 + 0.75*23.96875 = 20.476 -> streak = 2, reaches GREEN!
        let d6 = brain.ingest_telemetry(&sample(10.0, 20.0));
        assert_eq!(d6.state, "GREEN");
        assert_eq!(d6.state_code, 0);
        assert_eq!(d6.view_distance, 10);
        assert_eq!(d6.sim_distance, 8);
    }

    #[test]
    fn test_directive_json_contract_exact_keys() {
        let mut brain = BrainState::new(BrainConfig::default(), 4096, 16384);
        let directive = brain.ingest_telemetry(&sample(18.1, 20.0));
        let json_val = serde_json::to_value(&directive).expect("serialize directive");

        assert_eq!(json_val["type"], "directive");
        assert_eq!(json_val["state"], "GREEN");
        assert_eq!(json_val["stateCode"], 0);
        assert_eq!(json_val["ewmaMspt"], 18.1);
        assert_eq!(json_val["viewDistance"], 10);
        assert_eq!(json_val["simDistance"], 8);
        assert_eq!(json_val["fullAiRange"], 32.0);
        assert_eq!(json_val["hibernateRange"], 56.0);
        assert_eq!(json_val["itemMergeEnabled"], false);
        assert_eq!(json_val["itemMergeRadius"], 2.5);
        assert_eq!(json_val["reason"], "healthy");
    }

    #[test]
    fn test_farm_and_villager_telemetry_roundtrip_and_defaults() {
        let mut brain = BrainState::new(BrainConfig::default(), 4096, 16384);

        // 1. Telemetry containing farmThrottled & villagersThrottled
        let json_with_farm = r#"{
            "type": "telemetry",
            "mspt": 22.5,
            "tps": 20.0,
            "entities": 640,
            "hibernated": 150,
            "farmThrottled": 88,
            "villagersThrottled": 24,
            "players": 20,
            "loadedChunks": 1800,
            "heapUsedMb": 3072,
            "heapMaxMb": 8192
        }"#;
        let payload: TelemetryPayload =
            serde_json::from_str(json_with_farm).expect("deserialize telemetry with farm fields");
        assert_eq!(payload.farm_throttled, 88);
        assert_eq!(payload.villagers_throttled, 24);

        let _ = brain.ingest_telemetry(&payload);
        let snap = brain.snapshot();
        assert_eq!(snap.farm_throttled, 88);
        assert_eq!(snap.villagers_throttled, 24);

        let snap_json = serde_json::to_value(&snap).expect("serialize snapshot");
        assert_eq!(snap_json["farmThrottled"], 88);
        assert_eq!(snap_json["villagersThrottled"], 24);

        // 2. Legacy telemetry omitting farmThrottled & villagersThrottled defaults both to 0
        let legacy_json = r#"{
            "type": "telemetry",
            "mspt": 19.0,
            "tps": 20.0,
            "entities": 300,
            "hibernated": 40,
            "players": 10,
            "loadedChunks": 900,
            "heapUsedMb": 1500,
            "heapMaxMb": 8192
        }"#;
        let legacy_payload: TelemetryPayload =
            serde_json::from_str(legacy_json).expect("deserialize legacy telemetry");
        assert_eq!(legacy_payload.farm_throttled, 0);
        assert_eq!(legacy_payload.villagers_throttled, 0);

        let _ = brain.ingest_telemetry(&legacy_payload);
        let legacy_snap_json =
            serde_json::to_value(&brain.snapshot()).expect("serialize legacy snapshot");
        assert_eq!(legacy_snap_json["farmThrottled"], 0);
        assert_eq!(legacy_snap_json["villagersThrottled"], 0);
    }

    #[test]
    fn test_history_ring_buffer_caps_at_120_and_preserves_newest() {
        let mut brain = BrainState::new(BrainConfig::default(), 4096, 16384);
        brain.update_host_metrics(28.4, 5120, 16384);

        for tick in 1u32..=150 {
            let payload = TelemetryPayload {
                mspt: 15.0 + (tick % 30) as f64,
                tps: 20.0,
                entities: tick,
                hibernated: tick / 2,
                farm_throttled: tick / 3,
                villagers_throttled: tick / 5,
                players: 10,
                loaded_chunks: 1000,
                heap_used_mb: 2048 + tick as u64,
                heap_max_mb: 8192,
            };
            let _ = brain.ingest_telemetry(&payload);
        }

        let history = brain.history_snapshot();
        assert_eq!(history.len(), 120);
        assert_eq!(history.first().expect("oldest retained sample").entities, 31);
        assert_eq!(history.last().expect("newest retained sample").entities, 150);
        assert_eq!(history.last().expect("newest retained sample").heap_used_mb, 2198);
        assert!((history.last().expect("newest retained sample").host_cpu_pct - 28.4).abs() < 0.05);

        let sample_json =
            serde_json::to_value(history.last().expect("last sample")).expect("serialize sample");
        assert!(sample_json.get("uptimeSecs").is_some());
        assert!(sample_json.get("ewmaMspt").is_some());
        assert!(sample_json.get("farmThrottled").is_some());
        assert!(sample_json.get("villagersThrottled").is_some());
        assert!(sample_json.get("heapUsedMb").is_some());
        assert!(sample_json.get("hostCpuPct").is_some());
        assert!(sample_json.get("stateCode").is_some());
    }
}
