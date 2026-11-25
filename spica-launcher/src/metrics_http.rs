use crate::brain::{BrainSnapshot, BrainState, TelemetrySample};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_REQ_BYTES: usize = 4096;
const DASHBOARD_HTML: &str = include_str!("dashboard.html");

#[derive(Serialize)]
struct HistoryResponse<'a> {
    snapshot: &'a BrainSnapshot,
    history: &'a [TelemetrySample],
}

/// Binds the lightweight HTTP/1.1 metrics & status server on `127.0.0.1:<port>`.
pub async fn start_metrics_server(
    port: u16,
    brain: Arc<Mutex<BrainState>>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<JoinHandle<()>, String> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr).await.map_err(|e| {
        format!(
            "HATA: Metrik HTTP sunucusu '{}' adresine baglanamadi: {}",
            addr, e
        )
    })?;

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                accept_res = listener.accept() => {
                    match accept_res {
                        Ok((stream, _peer)) => {
                            let brain_clone = Arc::clone(&brain);
                            tokio::spawn(async move {
                                handle_http_connection(stream, brain_clone).await;
                            });
                        }
                        Err(err) => {
                            eprintln!("[SpicaMetrics HTTP] Baglanti kabul hatasi: {}", err);
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            }
        }
    });

    Ok(handle)
}

async fn handle_http_connection(mut stream: TcpStream, brain: Arc<Mutex<BrainState>>) {
    let mut req_buf = [0u8; MAX_HTTP_REQ_BYTES];
    let bytes_read = match tokio::time::timeout(HTTP_READ_TIMEOUT, stream.read(&mut req_buf)).await
    {
        Ok(Ok(n)) if n > 0 => n,
        _ => return,
    };

    let req_text = String::from_utf8_lossy(&req_buf[..bytes_read]);
    let request_line = req_text.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_target = parts.next().unwrap_or("/");
    let path = raw_target.split('?').next().unwrap_or("/");

    if method != "GET" && method != "HEAD" {
        let _ = write_http_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "Only GET is supported.\n",
            false,
        )
        .await;
        return;
    }

    let head_only = method == "HEAD";

    match path {
        "/" | "/dashboard" => {
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                DASHBOARD_HTML,
                head_only,
            )
            .await;
        }
        "/history" => {
            let (snapshot, history) = match brain.lock() {
                Ok(guard) => (guard.snapshot(), guard.history_snapshot()),
                Err(poisoned) => {
                    let guard = poisoned.into_inner();
                    (guard.snapshot(), guard.history_snapshot())
                }
            };
            let body = render_history_json(&snapshot, &history);
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "application/json",
                &body,
                head_only,
            )
            .await;
        }
        "/status" => {
            let snapshot = match brain.lock() {
                Ok(guard) => guard.snapshot(),
                Err(poisoned) => poisoned.into_inner().snapshot(),
            };
            let body = serde_json::to_string_pretty(&snapshot)
                .unwrap_or_else(|_| "{\"error\":\"serialization_failed\"}".to_string());
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                &body,
                head_only,
            )
            .await;
        }
        "/metrics" => {
            let snapshot = match brain.lock() {
                Ok(guard) => guard.snapshot(),
                Err(poisoned) => poisoned.into_inner().snapshot(),
            };
            let body = render_prometheus_metrics(&snapshot);
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "text/plain; version=0.0.4; charset=utf-8",
                &body,
                head_only,
            )
            .await;
        }
        _ => {
            let _ = write_http_response(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                "Spica Launcher Metrics HTTP\nEndpoints:\n  GET /\n  GET /dashboard\n  GET /history\n  GET /status\n  GET /metrics\n",
                head_only,
            )
            .await;
        }
    }
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
    head_only: bool,
) -> tokio::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {}\r\n\
         Content-Type: {}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n",
        status,
        content_type,
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    if !head_only {
        stream.write_all(body.as_bytes()).await?;
    }
    stream.flush().await
}

/// Serializes the combined live `BrainSnapshot` and 120-second `TelemetrySample` history for `/history`.
pub fn render_history_json(snapshot: &BrainSnapshot, history: &[TelemetrySample]) -> String {
    let payload = HistoryResponse { snapshot, history };
    serde_json::to_string(&payload)
        .unwrap_or_else(|_| "{\"error\":\"serialization_failed\"}".to_string())
}

/// Formats `BrainSnapshot` into standard Prometheus exposition format.
pub fn render_prometheus_metrics(s: &BrainSnapshot) -> String {
    let connected_val = if s.connected { 1 } else { 0 };
    format!(
        "# HELP spica_mspt Latest raw Purpur tick duration in milliseconds.\n\
         # TYPE spica_mspt gauge\n\
         spica_mspt {:.2}\n\
         # HELP spica_ewma_mspt Exponentially weighted moving average MSPT computed by Rust Brain.\n\
         # TYPE spica_ewma_mspt gauge\n\
         spica_ewma_mspt {:.2}\n\
         # HELP spica_tps Latest server ticks per second.\n\
         # TYPE spica_tps gauge\n\
         spica_tps {:.2}\n\
         # HELP spica_entities Total active entities reported by Purpur.\n\
         # TYPE spica_entities gauge\n\
         spica_entities {}\n\
         # HELP spica_hibernated_mobs Total hibernated mobs managed by SpicaRustNative.\n\
         # TYPE spica_hibernated_mobs gauge\n\
         spica_hibernated_mobs {}\n\
         # HELP spica_farm_throttled_mobs Total dense farm mobs currently throttled by Rust Farm Governor.\n\
         # TYPE spica_farm_throttled_mobs gauge\n\
         spica_farm_throttled_mobs {}\n\
         # HELP spica_villagers_throttled Total trading-hall/crowded villagers currently throttled by Rust Farm Governor.\n\
         # TYPE spica_villagers_throttled gauge\n\
         spica_villagers_throttled {}\n\
         # HELP spica_players Online player count.\n\
         # TYPE spica_players gauge\n\
         spica_players {}\n\
         # HELP spica_brain_state Adaptive Brain governor severity (0=GREEN, 1=YELLOW, 2=ORANGE, 3=RED, 4=EMERGENCY).\n\
         # TYPE spica_brain_state gauge\n\
         spica_brain_state {}\n\
         # HELP spica_host_cpu_percent Host global CPU usage percentage.\n\
         # TYPE spica_host_cpu_percent gauge\n\
         spica_host_cpu_percent {:.2}\n\
         # HELP spica_heap_used_mb JVM heap memory currently used in megabytes.\n\
         # TYPE spica_heap_used_mb gauge\n\
         spica_heap_used_mb {}\n\
         # HELP spica_heap_max_mb JVM maximum heap memory in megabytes.\n\
         # TYPE spica_heap_max_mb gauge\n\
         spica_heap_max_mb {}\n\
         # HELP spica_loaded_chunks Total loaded chunks reported by Purpur.\n\
         # TYPE spica_loaded_chunks gauge\n\
         spica_loaded_chunks {}\n\
         # HELP spica_host_ram_used_mb Host physical RAM used in megabytes.\n\
         # TYPE spica_host_ram_used_mb gauge\n\
         spica_host_ram_used_mb {}\n\
         # HELP spica_host_ram_total_mb Host total physical RAM in megabytes.\n\
         # TYPE spica_host_ram_total_mb gauge\n\
         spica_host_ram_total_mb {}\n\
         # HELP spica_ipc_connected Whether Purpur BrainIpcClient is currently connected (1 or 0).\n\
         # TYPE spica_ipc_connected gauge\n\
         spica_ipc_connected {}\n\
         # HELP spica_uptime_seconds Launcher daemon uptime in seconds.\n\
         # TYPE spica_uptime_seconds gauge\n\
         spica_uptime_seconds {}\n\
         # HELP spica_elytra_clamps_total Cumulative Elytra speed clamps enforced by Surge Guard.\n\
         # TYPE spica_elytra_clamps_total counter\n\
         spica_elytra_clamps_total {}\n\
         # HELP spica_explosion_items_merged_total Cumulative dropped items pre-merged by Explosion Guard.\n\
         # TYPE spica_explosion_items_merged_total counter\n\
         spica_explosion_items_merged_total {}\n\
         # HELP spica_tracked_pairs_hidden Total player-mob tracking pairs currently culled by Rust Mob Tracking Optimizer.\n\
         # TYPE spica_tracked_pairs_hidden gauge\n\
         spica_tracked_pairs_hidden {}\n\
         # HELP spica_tracking_transitions_per_sec Mob visibility hide/show delta transitions per second.\n\
         # TYPE spica_tracking_transitions_per_sec gauge\n\
         spica_tracking_transitions_per_sec {}\n",
        s.mspt,
        s.ewma_mspt,
        s.tps,
        s.entities,
        s.hibernated,
        s.farm_throttled,
        s.villagers_throttled,
        s.players,
        s.state_code,
        s.host_cpu_pct,
        s.heap_used_mb,
        s.heap_max_mb,
        s.loaded_chunks,
        s.host_ram_used_mb,
        s.host_ram_total_mb,
        connected_val,
        s.uptime_secs,
        s.elytra_clamps,
        s.explosion_items_merged,
        s.tracked_pairs_hidden,
        s.tracking_transitions_per_sec,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::{BrainState, TelemetryPayload};
    use crate::config::BrainConfig;

    #[test]
    fn prometheus_output_contains_all_required_metrics() {
        let mut brain = BrainState::new(BrainConfig::default(), 4096, 16384);
        brain.update_host_metrics(37.5, 6000, 16384);
        brain.ingest_telemetry(&TelemetryPayload {
            mspt: 21.4,
            tps: 20.0,
            entities: 310,
            hibernated: 85,
            farm_throttled: 42,
            villagers_throttled: 18,
            elytra_clamps: 7,
            explosion_items_merged: 315,
            tracked_pairs_hidden: 1240,
            tracking_transitions_per_sec: 64,
            players: 12,
            loaded_chunks: 1200,
            heap_used_mb: 2048,
            heap_max_mb: 8192,
        });
        let text = render_prometheus_metrics(&brain.snapshot());
        for metric in [
            "spica_mspt ",
            "spica_ewma_mspt ",
            "spica_tps ",
            "spica_entities ",
            "spica_hibernated_mobs ",
            "spica_farm_throttled_mobs 42",
            "spica_villagers_throttled 18",
            "spica_players ",
            "spica_brain_state ",
            "spica_host_cpu_percent ",
            "spica_heap_used_mb ",
            "# TYPE spica_elytra_clamps_total counter",
            "spica_elytra_clamps_total 7",
            "# TYPE spica_explosion_items_merged_total counter",
            "spica_explosion_items_merged_total 315",
            "# TYPE spica_tracked_pairs_hidden gauge",
            "spica_tracked_pairs_hidden 1240",
            "# TYPE spica_tracking_transitions_per_sec gauge",
            "spica_tracking_transitions_per_sec 64",
        ] {
            assert!(text.contains(metric), "Missing metric: {}", metric);
        }
    }

    #[test]
    fn embedded_dashboard_contains_brand_and_history_endpoint() {
        assert!(
            DASHBOARD_HTML.contains("SpicaRustNative"),
            "Dashboard HTML must contain SpicaRustNative branding"
        );
        assert!(
            DASHBOARD_HTML.contains("/history"),
            "Dashboard HTML must poll /history endpoint"
        );
    }

    #[test]
    fn render_history_json_contains_snapshot_and_history_keys() {
        let mut brain = BrainState::new(BrainConfig::default(), 4096, 16384);
        brain.update_host_metrics(22.5, 5000, 16384);
        brain.ingest_telemetry(&TelemetryPayload {
            mspt: 19.2,
            tps: 20.0,
            entities: 250,
            hibernated: 60,
            farm_throttled: 12,
            villagers_throttled: 6,
            elytra_clamps: 3,
            explosion_items_merged: 95,
            tracked_pairs_hidden: 860,
            tracking_transitions_per_sec: 42,
            players: 8,
            loaded_chunks: 900,
            heap_used_mb: 1800,
            heap_max_mb: 8192,
        });

        let snap = brain.snapshot();
        let history = brain.history_snapshot();
        let json_str = render_history_json(&snap, &history);
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("render_history_json must produce valid JSON");

        assert!(parsed.get("snapshot").is_some(), "Missing snapshot key");
        assert!(parsed.get("history").is_some(), "Missing history key");
        assert_eq!(parsed["snapshot"]["entities"], 250);
        assert_eq!(parsed["snapshot"]["farmThrottled"], 12);
        assert_eq!(parsed["snapshot"]["elytraClamps"], 3);
        assert_eq!(parsed["snapshot"]["explosionItemsMerged"], 95);
        assert_eq!(parsed["snapshot"]["trackedPairsHidden"], 860);
        assert_eq!(parsed["snapshot"]["trackingTransitionsPerSec"], 42);
        let hist_arr = parsed["history"]
            .as_array()
            .expect("history must be a JSON array");
        assert_eq!(hist_arr.len(), 1);
        assert_eq!(hist_arr[0]["entities"], 250);
        assert_eq!(hist_arr[0]["farmThrottled"], 12);
        assert_eq!(hist_arr[0]["villagersThrottled"], 6);
    }
}
