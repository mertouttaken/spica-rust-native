mod backup;
mod brain;
mod config;
mod ipc;
mod metrics_http;
mod preflight;
mod supervisor;

use crate::brain::BrainState;
use crate::config::{generate_auth_token, LauncherConfig};
use crate::preflight::{build_jvm_plan, detect_hardware, validate_environment, write_tuning_guide};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug)]
struct CliOptions {
    init: bool,
    dry_run: bool,
    tune: bool,
    help: bool,
    version: bool,
    config_path: PathBuf,
    jar_override: Option<String>,
}

fn parse_cli_args() -> Result<CliOptions, String> {
    let mut opts = CliOptions {
        init: false,
        dry_run: false,
        tune: false,
        help: false,
        version: false,
        config_path: PathBuf::from("spica-launcher.toml"),
        jar_override: None,
    };

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--init" => opts.init = true,
            "--dry-run" => opts.dry_run = true,
            "--tune" => opts.tune = true,
            "--help" | "-h" => opts.help = true,
            "--version" | "-V" => opts.version = true,
            "--config" => {
                let val = args
                    .next()
                    .ok_or_else(|| "HATA: '--config' parametresi icin dosya yolu gerekli.".to_string())?;
                opts.config_path = PathBuf::from(val);
            }
            "--jar" => {
                let val = args
                    .next()
                    .ok_or_else(|| "HATA: '--jar' parametresi icin JAR yolu gerekli.".to_string())?;
                opts.jar_override = Some(val);
            }
            other => {
                return Err(format!(
                    "HATA: Bilinmeyen parametre '{}'. Yardim icin '--help' kullanin.",
                    other
                ));
            }
        }
    }

    Ok(opts)
}

fn print_usage() {
    println!("SpicaRustNative Launcher v{} (Purpur 1.21.4 Core + Rust Brain)", VERSION);
    println!();
    println!("Kullanim:");
    println!("  spica-launcher [SECENEKLER]");
    println!();
    println!("Secenekler:");
    println!("  --init            Varsayilan spica-launcher.toml dosyasini olusturur ve cikar");
    println!("  --dry-run         Donanim ve uretilen JVM komutunu gosterir, Java baslatmaz");
    println!("  --tune            Donanima uygun guvenli spica-purpur-tuning.txt rehberi uretir");
    println!("  --config <yol>    Yapilandirma TOML dosya yolu (varsayilan: spica-launcher.toml)");
    println!("  --jar <yol>       Yapilandirmadaki Purpur JAR yolunu gecici olarak degistirir");
    println!("  -V, --version     Surum bilgisini yazdirir");
    println!("  -h, --help        Bu yardim mesajini yazdirir");
    println!();
    println!("Calisma Sirasinda Dahili Konsol Komutlari:");
    println!("  :brain            Canli Rust Brain durumu, EWMA MSPT ve aktif direktif");
    println!("  :stats            Host CPU/RAM, JVM Heap, TPS ve varlik ozeti");
    println!("  :backup           Zero-JVM-Overhead Rust dunya yedegi alir (save-off -> tar.gz -> save-on)");
    println!("  :restart          Purpur'u temiz durdurur ve yeniden baslatir");
    println!("  :stop             Purpur'u durdurur ve launcher'dan cikar");
    println!("  :help             Dahili komut listesini gosterir");
}

fn print_banner() {
    println!("====================================================================");
    println!(
        "  SpicaRustNative Launcher v{} | Purpur 1.21.4 Core + Rust Brain",
        VERSION
    );
    println!("====================================================================");
}

#[tokio::main]
async fn main() -> ExitCode {
    let opts = match parse_cli_args() {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("{}", msg);
            return ExitCode::from(2);
        }
    };

    if opts.help {
        print_usage();
        return ExitCode::SUCCESS;
    }

    if opts.version {
        println!("spica-launcher {}", VERSION);
        return ExitCode::SUCCESS;
    }

    if opts.init {
        if opts.config_path.exists() {
            println!(
                "[SpicaLauncher] Yapilandirma dosyasi zaten mevcut: {}",
                opts.config_path.display()
            );
            return ExitCode::SUCCESS;
        }
        match LauncherConfig::write_default(&opts.config_path) {
            Ok(()) => {
                println!(
                    "[SpicaLauncher] Varsayilan yapilandirma olusturuldu: {}",
                    opts.config_path.display()
                );
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("{}", e);
                return ExitCode::FAILURE;
            }
        }
    }

    print_banner();

    let (mut cfg, created_new) = match LauncherConfig::load_or_create(&opts.config_path) {
        Ok(res) => res,
        Err(e) => {
            eprintln!("{}", e);
            return ExitCode::FAILURE;
        }
    };

    if created_new {
        println!(
            "[SpicaLauncher] Varsayilan '{}' olusturuldu.",
            opts.config_path.display()
        );
    }

    if let Some(jar_path) = opts.jar_override {
        cfg.server.jar = jar_path;
        cfg.sanitize();
    }

    let hw = detect_hardware();
    let ipc_token = generate_auth_token();
    let plan = build_jvm_plan(&cfg, &hw, &ipc_token);

    println!(
        "[Preflight] Donanim : {} ({}) | {} mantiksal cekirdek | {} MB toplam RAM ({} MB kullanimda)",
        hw.os, hw.arch, hw.logical_cores, hw.total_ram_mb, hw.used_ram_mb
    );
    println!(
        "[Preflight] JVM Plan: -Xms{}M -Xmx{}M | GC={} | Native Threads={}",
        plan.min_ram_mb, plan.max_ram_mb, plan.resolved_gc, plan.resolved_native_threads
    );

    if opts.dry_run {
        println!("[Dry-Run] Calisma Dizini : {}", plan.work_dir.display());
        println!("[Dry-Run] Hedef JAR      : {}", plan.jar_path.display());
        println!(
            "[Dry-Run] Brain IPC      : {}:{}",
            cfg.brain.ipc_host, cfg.brain.ipc_port
        );
        if cfg.brain.enable_metrics_http {
            println!(
                "[Dry-Run] Dashboard HTTP : http://{}:{} (Dashboard | /status | /metrics)",
                "127.0.0.1", cfg.brain.metrics_port
            );
        }
        println!("[Dry-Run] Uretilen Komut :");
        println!("  {}", plan.format_command(true));
        return ExitCode::SUCCESS;
    }

    if opts.tune {
        match write_tuning_guide(&cfg, &hw, &plan) {
            Ok(out_path) => {
                println!(
                    "[Preflight] Guvenli performans rehberi olusturuldu: {}",
                    out_path.display()
                );
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("{}", e);
                return ExitCode::FAILURE;
            }
        }
    }

    match validate_environment(&cfg, &plan) {
        Ok(warnings) => {
            for w in warnings {
                println!("[Preflight] {}", w);
            }
        }
        Err(err) => {
            eprintln!("{}", err);
            return ExitCode::FAILURE;
        }
    }

    let brain = Arc::new(Mutex::new(BrainState::new(
        cfg.brain.clone(),
        hw.used_ram_mb,
        hw.total_ram_mb,
    )));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let ipc_handle = match ipc::start_ipc_server(
        &cfg.brain.ipc_host,
        cfg.brain.ipc_port,
        cfg.brain.allow_non_loopback_ipc,
        ipc_token,
        Arc::clone(&brain),
        shutdown_rx.clone(),
    )
    .await
    {
        Ok(h) => {
            println!(
                "[SpicaBrain] IPC TCP sunucusu dinleniyor: {}:{}",
                cfg.brain.ipc_host, cfg.brain.ipc_port
            );
            h
        }
        Err(e) => {
            eprintln!("{}", e);
            return ExitCode::FAILURE;
        }
    };

    let metrics_handle = if cfg.brain.enable_metrics_http {
        match metrics_http::start_metrics_server(
            cfg.brain.metrics_port,
            Arc::clone(&brain),
            shutdown_rx.clone(),
        )
        .await
        {
            Ok(h) => {
                println!(
                    "[SpicaMetrics] Canli Dashboard aktif: http://{}:{} (Dashboard | /status | /metrics)",
                    "127.0.0.1", cfg.brain.metrics_port
                );
                Some(h)
            }
            Err(e) => {
                eprintln!("{}", e);
                let _ = shutdown_tx.send(true);
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    let host_mon_handle = supervisor::spawn_host_monitor(Arc::clone(&brain), shutdown_rx);

    println!("[SpicaSupervisor] Hazir! Dahili komutlar: :brain | :stats | :backup | :restart | :stop | :help");

    let run_result = supervisor::run_supervisor(&cfg, &plan, brain, shutdown_tx.clone()).await;

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(1), ipc_handle).await;
    if let Some(mh) = metrics_handle {
        let _ = tokio::time::timeout(Duration::from_secs(1), mh).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), host_mon_handle).await;

    match run_result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", e);
            ExitCode::FAILURE
        }
    }
}
