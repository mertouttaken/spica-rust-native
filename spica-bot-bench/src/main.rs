//! SpicaRustNative Offline Bot Stress Tester CLI (`spica-bot-bench`)
//! Connects N concurrent Minecraft 1.21.4 (Protocol 769) offline-mode bots to a Purpur
//! server and measures exact steady-state clientbound entity packet rates & network bandwidth (O7).

mod bot;
mod protocol;

use crate::bot::{run_bot, BenchMetrics};
use std::io;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

const VERSION: &str = env!("CARGO_PKG_VERSION");

struct CliArgs {
    host: String,
    port: u16,
    bots: usize,
    duration_secs: u64,
    warmup_secs: u64,
    stagger_ms: u64,
    min_ready_pct: u32,
    prefix: String,
    json: bool,
    snapshot_stdin: bool,
    help: bool,
    version: bool,
}

impl Default for CliArgs {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 25565,
            bots: 20,
            duration_secs: 20,
            warmup_secs: 2,
            stagger_ms: 80,
            min_ready_pct: 90,
            prefix: "SpicaBot".to_string(),
            json: false,
            snapshot_stdin: false,
            help: false,
            version: false,
        }
    }
}

fn parse_args() -> Result<CliArgs, String> {
    let mut cfg = CliArgs::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--host" => {
                cfg.host = args
                    .next()
                    .ok_or_else(|| "Missing value for --host".to_string())?;
            }
            "--port" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "Missing value for --port".to_string())?;
                cfg.port = raw
                    .parse::<u16>()
                    .map_err(|_| format!("Invalid --port: {}", raw))?;
            }
            "--bots" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "Missing value for --bots".to_string())?;
                cfg.bots = raw
                    .parse::<usize>()
                    .map_err(|_| format!("Invalid --bots: {}", raw))?
                    .clamp(1, 500);
            }
            "--duration" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "Missing value for --duration".to_string())?;
                cfg.duration_secs = raw
                    .parse::<u64>()
                    .map_err(|_| format!("Invalid --duration: {}", raw))?
                    .clamp(3, 3600);
            }
            "--warmup" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "Missing value for --warmup".to_string())?;
                cfg.warmup_secs = raw
                    .parse::<u64>()
                    .map_err(|_| format!("Invalid --warmup: {}", raw))?
                    .clamp(0, 120);
            }
            "--stagger-ms" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "Missing value for --stagger-ms".to_string())?;
                cfg.stagger_ms = raw
                    .parse::<u64>()
                    .map_err(|_| format!("Invalid --stagger-ms: {}", raw))?
                    .clamp(0, 5000);
            }
            "--min-ready-pct" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "Missing value for --min-ready-pct".to_string())?;
                cfg.min_ready_pct = raw
                    .parse::<u32>()
                    .map_err(|_| format!("Invalid --min-ready-pct: {}", raw))?
                    .clamp(1, 100);
            }
            "--prefix" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "Missing value for --prefix".to_string())?;
                if raw.is_empty() || raw.len() > 11 {
                    return Err(
                        "--prefix must be 1..=11 characters (MC 16-char username limit)".to_string(),
                    );
                }
                cfg.prefix = raw;
            }
            "--json" => cfg.json = true,
            "--snapshot-stdin" => cfg.snapshot_stdin = true,
            "--help" | "-h" => cfg.help = true,
            "--version" | "-V" => cfg.version = true,
            other => {
                return Err(format!("Unknown argument: {} (use --help)", other));
            }
        }
    }
    Ok(cfg)
}

fn print_usage() {
    println!(
        "spica-bot-bench v{} — Minecraft 1.21.4 (Protocol 769) Offline Bot Stress Tester",
        VERSION
    );
    println!();
    println!("Usage:");
    println!("  spica-bot-bench [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --host <addr>         Server hostname or IP (default: 127.0.0.1)");
    println!("  --port <u16>          Server port (default: 25565)");
    println!("  --bots <N>            Number of concurrent bots, 1..500 (default: 20)");
    println!("  --duration <secs>     Steady-state measurement duration in seconds (default: 20)");
    println!("  --warmup <secs>       Post-join warmup before steady-state measurement (default: 2)");
    println!("  --stagger-ms <ms>     Delay between bot connections (default: 80)");
    println!("  --min-ready-pct <P>   Minimum ready bot % required for exit code 0 (default: 90)");
    println!("  --prefix <name>       Bot username prefix, max 11 chars (default: SpicaBot)");
    println!("  --json                Emit machine-readable JSON summary on stdout");
    println!("  --snapshot-stdin      Accept 'snapshot' for counters and 'finish' to end measurement on stdin");
    println!("  -V, --version         Print version");
    println!("  -h, --help            Show this help");
}

fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(&mut out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn start_snapshot_reader(metrics: Arc<BenchMetrics>, finish_requested: Arc<AtomicBool>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("spica-snapshots".into())
        .spawn(move || {
            use std::io::Write;
            for line in io::stdin().lines().map_while(Result::ok) {
                if line == "finish" {
                    finish_requested.store(true, Ordering::Relaxed);
                    break;
                }
                if line != "snapshot" {
                    continue;
                }
                let result = writeln!(
                    io::stderr().lock(),
                    "SPICA_SNAPSHOT {} {} {} {} {}",
                    u8::from(metrics.measuring.load(Ordering::Relaxed)),
                    metrics.active_ready_play.load(Ordering::Relaxed),
                    metrics.total_packets_in.load(Ordering::Relaxed),
                    metrics.total_bytes_in.load(Ordering::Relaxed),
                    metrics.total_entity_packets(),
                );
                if result.is_err() {
                    break;
                }
            }
        })?;
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: {}", e);
            return ExitCode::from(2);
        }
    };

    if args.help {
        print_usage();
        return ExitCode::SUCCESS;
    }
    if args.version {
        println!("spica-bot-bench {}", VERSION);
        return ExitCode::SUCCESS;
    }

    if !args.json {
        println!(
            "=== spica-bot-bench v{} (MC 1.21.4 / Protocol 769) ===",
            VERSION
        );
        println!(
            "Target: {}:{} | Bots: {} | Warmup: {}s | Measure: {}s | Stagger: {}ms | MinReady: {}%",
            args.host,
            args.port,
            args.bots,
            args.warmup_secs,
            args.duration_secs,
            args.stagger_ms,
            args.min_ready_pct
        );
    }

    let metrics = Arc::new(BenchMetrics::default());
    let finish_requested = Arc::new(AtomicBool::new(false));
    if args.snapshot_stdin {
        if let Err(error) = start_snapshot_reader(Arc::clone(&metrics), Arc::clone(&finish_requested)) {
            eprintln!("Unable to start snapshot reader: {error}");
            return ExitCode::FAILURE;
        }
    }
    let (stop_tx, stop_rx) = watch::channel(false);
    let min_required_ready = ((args.bots as u64 * args.min_ready_pct as u64) + 99) / 100;

    // Phase 1: Ramp-up all bots (metrics.measuring = false so join/chunk burst does not skew A/B)
    let mut handles = Vec::with_capacity(args.bots);
    for idx in 1..=args.bots {
        let username = format!("{}{:03}", args.prefix, idx);
        let h = args.host.clone();
        let p = args.port;
        let m = Arc::clone(&metrics);
        let rx = stop_rx.clone();
        handles.push(tokio::spawn(async move {
            run_bot(h, p, username, m, rx).await;
        }));
        if args.stagger_ms > 0 && idx < args.bots {
            tokio::time::sleep(Duration::from_millis(args.stagger_ms)).await;
        }
    }

    // Wait up to 15s for bots to complete initial teleport ACK (ready_play)
    let ready_wait_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < ready_wait_deadline {
        let ready = metrics.ready_play.load(Ordering::Relaxed);
        let failed = metrics.failed_connect.load(Ordering::Relaxed);
        let dropped_pre = metrics.disconnected_before_ready.load(Ordering::Relaxed);
        if ready >= args.bots as u64 || (ready + failed + dropped_pre) >= args.bots as u64 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if args.warmup_secs > 0 {
        tokio::time::sleep(Duration::from_secs(args.warmup_secs)).await;
    }

    // Phase 2: Steady-state measurement window (O7) — sample active ready bot-seconds every 250ms
    let measure_start = Instant::now();
    metrics.measuring.store(true, Ordering::SeqCst);

    let mut active_bot_seconds = 0.0f64;
    let sample_step = Duration::from_millis(250);
    let measure_target = Duration::from_secs(args.duration_secs);
    let mut last_sample = measure_start;
    while measure_start.elapsed() < measure_target && !finish_requested.load(Ordering::Relaxed) {
        let remaining = measure_target.saturating_sub(measure_start.elapsed());
        tokio::time::sleep(remaining.min(sample_step)).await;
        let now = Instant::now();
        let dt = now.duration_since(last_sample).as_secs_f64();
        last_sample = now;
        let active = metrics.active_ready_play.load(Ordering::Relaxed) as f64;
        active_bot_seconds += active * dt;
    }

    metrics.measuring.store(false, Ordering::SeqCst);
    let elapsed_secs = measure_start.elapsed().as_secs_f64().max(0.001);

    let _ = stop_tx.send(true);
    // Shared 3s shutdown deadline across all bot tasks (never stalls 3s * N sequentially)
    let shutdown_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    for h in handles {
        let _ = tokio::time::timeout_at(shutdown_deadline, h).await;
    }

    let connected = metrics.connected_play.load(Ordering::Relaxed);
    let ready = metrics.ready_play.load(Ordering::Relaxed);
    let failed = metrics.failed_connect.load(Ordering::Relaxed);
    let disconnected_pre = metrics.disconnected_before_ready.load(Ordering::Relaxed);
    let disconnected_post = metrics.disconnected_after_ready.load(Ordering::Relaxed);
    let disconnected = disconnected_pre + disconnected_post;
    let surviving_ready = ready.saturating_sub(disconnected_post);
    let spawn = metrics.entity_spawn.load(Ordering::Relaxed);
    let mv = metrics.entity_move.load(Ordering::Relaxed);
    let pos_sync = metrics.entity_pos_sync.load(Ordering::Relaxed);
    let meta = metrics.entity_meta.load(Ordering::Relaxed);
    let remove = metrics.entity_remove.load(Ordering::Relaxed);
    let entities_removed = metrics.entities_removed_total.load(Ordering::Relaxed);
    let total_ent = metrics.total_entity_packets();
    let total_pkts = metrics.total_packets_in.load(Ordering::Relaxed);
    let total_bytes = metrics.total_bytes_in.load(Ordering::Relaxed);
    let keepalives = metrics.keepalives_acked.load(Ordering::Relaxed);
    let p95_join_ms = metrics.p95_join_latency_ms();
    let entity_pkt_per_sec = total_ent as f64 / elapsed_secs;
    let total_pkt_per_sec = total_pkts as f64 / elapsed_secs;
    let kb_per_sec = (total_bytes as f64 / 1024.0) / elapsed_secs;
    let entity_pkt_per_bot_sec = if active_bot_seconds > 0.001 {
        total_ent as f64 / active_bot_seconds
    } else {
        0.0
    };

    if args.json {
        println!(
            "{{\"version\":\"{}\",\"host\":\"{}\",\"port\":{},\"requestedBots\":{},\"connectedPlay\":{},\"readyPlay\":{},\"failedConnect\":{},\"disconnectedEarly\":{},\"disconnectedBeforeReady\":{},\"disconnectedAfterReady\":{},\"p95JoinMs\":{},\"elapsedSecs\":{:.2},\"activeBotSeconds\":{:.2},\"entitySpawn\":{},\"entityMove\":{},\"entityPosSync\":{},\"entityMeta\":{},\"entityRemove\":{},\"entitiesRemovedTotal\":{},\"totalEntityPackets\":{},\"entityPacketsPerSec\":{:.1},\"entityPacketsPerBotSec\":{:.2},\"totalPacketsIn\":{},\"totalPacketsPerSec\":{:.1},\"totalBytesIn\":{},\"bandwidthKbPerSec\":{:.1},\"keepAlivesAcked\":{}}}",
            json_escape(VERSION),
            json_escape(&args.host),
            args.port,
            args.bots,
            connected,
            ready,
            failed,
            disconnected,
            disconnected_pre,
            disconnected_post,
            p95_join_ms,
            elapsed_secs,
            active_bot_seconds,
            spawn,
            mv,
            pos_sync,
            meta,
            remove,
            entities_removed,
            total_ent,
            entity_pkt_per_sec,
            entity_pkt_per_bot_sec,
            total_pkts,
            total_pkt_per_sec,
            total_bytes,
            kb_per_sec,
            keepalives
        );
    } else {
        println!("--- Steady-State Bot Stress Test Summary ({:.2}s) ---", elapsed_secs);
        println!(
            "  Bots Ready      : {}/{} (surviving={}, connected={}, failed={}, dropped_pre={}, dropped_post={}, p95_join={}ms, keepalives={})",
            ready, args.bots, surviving_ready, connected, failed, disconnected_pre, disconnected_post, p95_join_ms, keepalives
        );
        println!(
            "  Entity Packets  : {} total ({:.1} pkt/s | {:.2} pkt/bot-s) [spawn={}, move={}, posSync(0x20)={}, meta={}, remove={} ({} entities)]",
            total_ent, entity_pkt_per_sec, entity_pkt_per_bot_sec, spawn, mv, pos_sync, meta, remove, entities_removed
        );
        println!(
            "  All Packets In  : {} total ({:.1} pkt/s) | Bandwidth: {:.1} KB/s ({} bytes | {:.1} active bot-s)",
            total_pkts, total_pkt_per_sec, kb_per_sec, total_bytes, active_bot_seconds
        );
    }

    // O7: Fail run unless surviving ready bots AND sustained active bot-seconds meet threshold
    let finish_requested = finish_requested.load(Ordering::Relaxed);
    let target_secs = if finish_requested {
        elapsed_secs
    } else {
        args.duration_secs as f64
    };
    let min_active_bot_secs = (min_required_ready as f64) * target_secs * 0.90;
    if ready < min_required_ready
        || surviving_ready < min_required_ready
        || active_bot_seconds < min_active_bot_secs
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
