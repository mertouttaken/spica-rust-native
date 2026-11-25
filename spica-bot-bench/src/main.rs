//! SpicaRustNative Offline Bot Stress Tester CLI (`spica-bot-bench`)
//! Connects N concurrent Minecraft 1.21.4 (Protocol 769) offline-mode bots to a Purpur
//! server and measures exact clientbound entity packet rates & network bandwidth.

mod bot;
mod protocol;

use crate::bot::{run_bot, BenchMetrics};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

const VERSION: &str = env!("CARGO_PKG_VERSION");

struct CliArgs {
    host: String,
    port: u16,
    bots: usize,
    duration_secs: u64,
    stagger_ms: u64,
    prefix: String,
    json: bool,
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
            stagger_ms: 80,
            prefix: "SpicaBot".to_string(),
            json: false,
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
            "--stagger-ms" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "Missing value for --stagger-ms".to_string())?;
                cfg.stagger_ms = raw
                    .parse::<u64>()
                    .map_err(|_| format!("Invalid --stagger-ms: {}", raw))?
                    .clamp(0, 5000);
            }
            "--prefix" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "Missing value for --prefix".to_string())?;
                if raw.is_empty() || raw.len() > 11 {
                    return Err("--prefix must be 1..=11 characters (MC 16-char username limit)".to_string());
                }
                cfg.prefix = raw;
            }
            "--json" => cfg.json = true,
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
    println!("spica-bot-bench v{} — Minecraft 1.21.4 (Protocol 769) Offline Bot Stress Tester", VERSION);
    println!();
    println!("Usage:");
    println!("  spica-bot-bench [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --host <addr>       Server hostname or IP (default: 127.0.0.1)");
    println!("  --port <u16>        Server port (default: 25565)");
    println!("  --bots <N>          Number of concurrent bots, 1..500 (default: 20)");
    println!("  --duration <secs>   Total test duration in seconds (default: 20)");
    println!("  --stagger-ms <ms>   Delay between bot connections (default: 80)");
    println!("  --prefix <name>     Bot username prefix, max 11 chars (default: SpicaBot)");
    println!("  --json              Emit machine-readable JSON summary on stdout");
    println!("  -V, --version       Print version");
    println!("  -h, --help          Show this help");
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
            "Target: {}:{} | Bots: {} | Duration: {}s | Stagger: {}ms",
            args.host, args.port, args.bots, args.duration_secs, args.stagger_ms
        );
    }

    let metrics = Arc::new(BenchMetrics::default());
    let (stop_tx, stop_rx) = watch::channel(false);
    let started = Instant::now();

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

    let elapsed_so_far = started.elapsed();
    let total_target = Duration::from_secs(args.duration_secs);
    if elapsed_so_far < total_target {
        tokio::time::sleep(total_target - elapsed_so_far).await;
    }

    let _ = stop_tx.send(true);
    for h in handles {
        let _ = tokio::time::timeout(Duration::from_secs(3), h).await;
    }

    let elapsed_secs = started.elapsed().as_secs_f64().max(0.001);
    let connected = metrics.connected_play.load(Ordering::Relaxed);
    let failed = metrics.failed_connect.load(Ordering::Relaxed);
    let disconnected = metrics.disconnected_early.load(Ordering::Relaxed);
    let spawn = metrics.entity_spawn.load(Ordering::Relaxed);
    let mv = metrics.entity_move.load(Ordering::Relaxed);
    let meta = metrics.entity_meta.load(Ordering::Relaxed);
    let remove = metrics.entity_remove.load(Ordering::Relaxed);
    let total_ent = metrics.total_entity_packets();
    let total_pkts = metrics.total_packets_in.load(Ordering::Relaxed);
    let total_bytes = metrics.total_bytes_in.load(Ordering::Relaxed);
    let keepalives = metrics.keepalives_acked.load(Ordering::Relaxed);
    let entity_pkt_per_sec = total_ent as f64 / elapsed_secs;
    let total_pkt_per_sec = total_pkts as f64 / elapsed_secs;
    let kb_per_sec = (total_bytes as f64 / 1024.0) / elapsed_secs;

    if args.json {
        println!(
            "{{\"version\":\"{}\",\"host\":\"{}\",\"port\":{},\"requestedBots\":{},\"connectedPlay\":{},\"failedConnect\":{},\"disconnectedEarly\":{},\"elapsedSecs\":{:.2},\"entitySpawn\":{},\"entityMove\":{},\"entityMeta\":{},\"entityRemove\":{},\"totalEntityPackets\":{},\"entityPacketsPerSec\":{:.1},\"totalPacketsIn\":{},\"totalPacketsPerSec\":{:.1},\"totalBytesIn\":{},\"bandwidthKbPerSec\":{:.1},\"keepAlivesAcked\":{}}}",
            VERSION,
            args.host,
            args.port,
            args.bots,
            connected,
            failed,
            disconnected,
            elapsed_secs,
            spawn,
            mv,
            meta,
            remove,
            total_ent,
            entity_pkt_per_sec,
            total_pkts,
            total_pkt_per_sec,
            total_bytes,
            kb_per_sec,
            keepalives
        );
    } else {
        println!("--- Bot Stress Test Summary ({:.2}s) ---", elapsed_secs);
        println!(
            "  Bots Connected  : {}/{} (failed={}, disconnected_early={}, keepalives={})",
            connected, args.bots, failed, disconnected, keepalives
        );
        println!(
            "  Entity Packets  : {} total ({:.1} pkt/s) [spawn={}, move={}, meta={}, remove={}]",
            total_ent, entity_pkt_per_sec, spawn, mv, meta, remove
        );
        println!(
            "  All Packets In  : {} total ({:.1} pkt/s) | Bandwidth: {:.1} KB/s ({} bytes)",
            total_pkts, total_pkt_per_sec, kb_per_sec, total_bytes
        );
    }

    if connected == 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
