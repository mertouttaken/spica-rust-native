use crate::backup::{create_world_backup, BackupOutcome};
use crate::brain::BrainState;
use crate::config::LauncherConfig;
use crate::preflight::JvmPlan;
use std::io::BufRead;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::System;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

const GRACEFUL_STOP_TIMEOUT_SECS: u64 = 30;
const STABLE_UPTIME_RESET_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorAction {
    None,
    Restart,
    Stop,
}

/// Spawns a background Tokio task that samples host CPU and RAM via `sysinfo 0.32`
/// every 2 seconds and updates shared `BrainState`.
pub fn spawn_host_monitor(
    brain: Arc<Mutex<BrainState>>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut sys = System::new_all();
        sys.refresh_cpu_usage();
        sys.refresh_memory();

        let mut ticker = tokio::time::interval(Duration::from_secs(2));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    sys.refresh_cpu_usage();
                    sys.refresh_memory();
                    let cpu_pct = sys.global_cpu_usage();
                    let ram_used_mb = sys.used_memory() / (1024 * 1024);
                    let ram_total_mb = sys.total_memory() / (1024 * 1024);

                    match brain.lock() {
                        Ok(mut guard) => guard.update_host_metrics(cpu_pct, ram_used_mb, ram_total_mb),
                        Err(poisoned) => poisoned
                            .into_inner()
                            .update_host_metrics(cpu_pct, ram_used_mb, ram_total_mb),
                    }
                }
            }
        }
    })
}

/// Spawns a dedicated OS thread reading interactive console lines without blocking Tokio workers.
fn spawn_stdin_reader() -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    let _ = std::thread::Builder::new()
        .name("spica-stdin".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut locked = stdin.lock();
            let mut line = String::new();
            loop {
                line.clear();
                match locked.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let cleaned = line
                            .trim_start_matches('\u{FEFF}')
                            .trim_end_matches(&['\r', '\n'][..])
                            .to_string();
                        if tx.send(cleaned).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    rx
}

async fn pipe_stream<R: AsyncRead + Unpin>(reader: R, is_stderr: bool) {
    let mut buf_reader = BufReader::new(reader);
    let mut raw = Vec::with_capacity(512);
    loop {
        raw.clear();
        match buf_reader.read_until(b'\n', &mut raw).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&raw);
                let line = text.trim_end_matches(&['\r', '\n'][..]);
                if is_stderr {
                    eprintln!("{}", line);
                } else {
                    println!("{}", line);
                }
            }
        }
    }
}

async fn send_to_child(child_stdin: &mut Option<ChildStdin>, line: &str) {
    if let Some(stdin) = child_stdin.as_mut() {
        let mut payload = String::with_capacity(line.len() + 1);
        payload.push_str(line);
        payload.push('\n');
        if let Err(e) = stdin.write_all(payload.as_bytes()).await {
            eprintln!("[SpicaSupervisor] Purpur stdin yazma hatasi: {}", e);
            return;
        }
        let _ = stdin.flush().await;
    } else {
        eprintln!("[SpicaSupervisor] Purpur stdin aktif degil.");
    }
}

fn print_supervisor_help() {
    println!("================ Spica Supervisor Komutlari ================");
    println!("  :brain    -> Canli Rust Brain durumu, EWMA MSPT ve direktif");
    println!("  :stats    -> Host CPU/RAM, JVM Heap, TPS ve varlik ozeti");
    println!("  :backup   -> Zero-JVM-Overhead Rust dunya yedegi (.tar.gz)");
    println!("  :restart  -> Purpur'a 'stop' gonderir ve temiz yeniden baslatir");
    println!("  :stop     -> Purpur'a 'stop' gonderir ve launcher'dan cikar");
    println!("  :help     -> Bu yardim mesajini gosterir");
    println!("  (Diger tum satirlar dogrudan Purpur konsoluna iletilir)");
    println!("============================================================");
}

fn print_brain_status(brain: &Arc<Mutex<BrainState>>) {
    let s = match brain.lock() {
        Ok(guard) => guard.snapshot(),
        Err(poisoned) => poisoned.into_inner().snapshot(),
    };
    let ipc_label = if s.connected {
        format!("BAGLI ({} v{})", s.server_name, s.plugin_version)
    } else {
        "BEKLENIYOR / BAGLI DEGIL".to_string()
    };

    println!("-------------------- [Spica Rust Brain] --------------------");
    println!("  IPC Durumu     : {}", ipc_label);
    println!(
        "  Brain Seviyesi : {} (kod: {}) | Sebep: {}",
        s.state, s.state_code, s.directive.reason
    );
    println!(
        "  MSPT / EWMA    : {:.1} ms / {:.1} ms | TPS: {:.2}",
        s.mspt, s.ewma_mspt, s.tps
    );
    println!(
        "  Aktif Direktif : ViewDist={} | SimDist={} | AI={:.1} | Hibernate={:.1}",
        s.directive.view_distance,
        s.directive.sim_distance,
        s.directive.full_ai_range,
        s.directive.hibernate_range
    );
    println!(
        "  Item Merge     : {} (yaricap: {:.1})",
        if s.directive.item_merge_enabled {
            "ACIK"
        } else {
            "KAPALI"
        },
        s.directive.item_merge_radius
    );
    println!("------------------------------------------------------------");
}

fn print_stats(brain: &Arc<Mutex<BrainState>>) {
    let s = match brain.lock() {
        Ok(guard) => guard.snapshot(),
        Err(poisoned) => poisoned.into_inner().snapshot(),
    };

    println!("-------------------- [Spica Live Stats] --------------------");
    println!(
        "  Host CPU / RAM : {:.1}% CPU | {} / {} MB RAM",
        s.host_cpu_pct, s.host_ram_used_mb, s.host_ram_total_mb
    );
    println!(
        "  JVM Heap       : {} / {} MB",
        s.heap_used_mb, s.heap_max_mb
    );
    println!(
        "  Purpur Tick    : TPS={:.2} | MSPT={:.1} ms (EWMA {:.1} ms)",
        s.tps, s.mspt, s.ewma_mspt
    );
    println!(
        "  Dunya Yuk      : Oyuncu={} | Entity={} | Chunk={}",
        s.players, s.entities, s.loaded_chunks
    );
    println!(
        "  AI & Farm Gov  : Hibernated={} | Farm Throttled={} | Villagers Throttled={}",
        s.hibernated, s.farm_throttled, s.villagers_throttled
    );
    println!(
        "  Surge Guard    : Elytra Clamps={} | Explosion Items Merged={}",
        s.elytra_clamps, s.explosion_items_merged
    );
    println!(
        "  Mob Tracking   : Hidden Pairs={} | Transitions/s={}",
        s.tracked_pairs_hidden, s.tracking_transitions_per_sec
    );
    println!(
        "  Redstone Gov   : Throttled Chunks={} | Ops Blocked={}",
        s.redstone_chunks_throttled, s.redstone_ops_blocked
    );
    println!(
        "  World Backups  : Completed={} | Last={} ms ({:.2} MB)",
        s.backups_completed, s.last_backup_duration_ms, s.last_backup_size_mb
    );
    println!(
        "  Calisma Suresi : {} sn | Brain: {}",
        s.uptime_secs, s.state
    );
    println!("------------------------------------------------------------");
}

/// Runs the Purpur child process lifecycle loop with piped console, interactive `:` commands,
/// graceful Ctrl+C handling, and exponential crash-recovery backoff.
pub async fn run_supervisor(
    cfg: &LauncherConfig,
    plan: &JvmPlan,
    brain: Arc<Mutex<BrainState>>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<(), String> {
    let mut stdin_rx = spawn_stdin_reader();
    let mut stdin_open = true;
    let mut restart_count: u32 = 0;

    loop {
        println!(
            "[SpicaSupervisor] Purpur baslatiliyor: {} -Xms{}M -Xmx{}M ({})",
            plan.jar_path.display(),
            plan.min_ram_mb,
            plan.max_ram_mb,
            plan.resolved_gc
        );

        let mut child = Command::new(&plan.java_bin)
            .current_dir(&plan.work_dir)
            .args(&plan.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                format!(
                    "HATA: Java sureci baslatilamadi ('{}'): {}. Java 21+ kurulu ve PATH uzerinde oldugundan emin olun.",
                    plan.java_bin, e
                )
            })?;

        let mut child_stdin = child.stdin.take();
        let stdout_handle = child
            .stdout
            .take()
            .map(|out| tokio::spawn(pipe_stream(out, false)));
        let stderr_handle = child
            .stderr
            .take()
            .map(|err| tokio::spawn(pipe_stream(err, true)));

        let started_at = Instant::now();
        let mut requested_action = SupervisorAction::None;
        let mut force_kill_deadline: Option<tokio::time::Instant> = None;
        let mut backup_rx: Option<oneshot::Receiver<Result<BackupOutcome, String>>> = None;
        let mut scheduled_backup_timer = if cfg.backup.enabled {
            let interval = Duration::from_secs(cfg.backup.interval_minutes.max(1) * 60);
            let mut t = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            Some(t)
        } else {
            None
        };

        let exit_result = loop {
            let current_deadline = force_kill_deadline;
            tokio::select! {
                status = child.wait() => {
                    break status;
                }
                backup_res = async {
                    match backup_rx.as_mut() {
                        Some(rx) => rx.await,
                        None => std::future::pending().await,
                    }
                }, if backup_rx.is_some() => {
                    backup_rx = None;
                    // Guarantee save-on is ALWAYS restored after backup finishes or fails
                    send_to_child(&mut child_stdin, "save-on").await;
                    match backup_res {
                        Ok(Ok(outcome)) => {
                            let size_mb = outcome.size_bytes as f64 / (1024.0 * 1024.0);
                            match brain.lock() {
                                Ok(mut guard) => guard.record_backup(outcome.duration_ms, outcome.size_bytes),
                                Err(poisoned) => poisoned
                                    .into_inner()
                                    .record_backup(outcome.duration_ms, outcome.size_bytes),
                            }
                            println!(
                                "[SpicaBackup] Tamamlandi: {} ({:.2} MB | {} dosya | {} ms | dunyalar={:?} | silinen eski={})",
                                outcome.archive_path.display(),
                                size_mb,
                                outcome.files_archived,
                                outcome.duration_ms,
                                outcome.worlds_included,
                                outcome.rotated_deleted
                            );
                        }
                        Ok(Err(err)) => {
                            eprintln!("[SpicaBackup] HATA (save-on geri acildi): {}", err);
                        }
                        Err(_) => {
                            eprintln!("[SpicaBackup] HATA: Yedekleme gorevi beklenmedik sekilde iptal oldu (save-on geri acildi).");
                        }
                    }
                }
                _ = async {
                    match scheduled_backup_timer.as_mut() {
                        Some(timer) => {
                            timer.tick().await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                }, if scheduled_backup_timer.is_some() && backup_rx.is_none() && requested_action == SupervisorAction::None => {
                    println!(
                        "[SpicaBackup] Zamanlanmis dunya yedegi baslatiliyor (save-off -> save-all flush -> Rust tar.gz)..."
                    );
                    send_to_child(&mut child_stdin, "save-off").await;
                    send_to_child(&mut child_stdin, "save-all flush").await;
                    let (tx, rx) = oneshot::channel();
                    let work_dir_clone = plan.work_dir.clone();
                    let backup_cfg_clone = cfg.backup.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(backup_cfg_clone.flush_wait_ms)).await;
                        let res = match tokio::task::spawn_blocking(move || {
                            create_world_backup(&work_dir_clone, &backup_cfg_clone)
                        })
                        .await
                        {
                            Ok(r) => r,
                            Err(e) => Err(format!("Yedekleme thread hatasi: {}", e)),
                        };
                        let _ = tx.send(res);
                    });
                    backup_rx = Some(rx);
                }
                maybe_line = stdin_rx.recv(), if stdin_open => {
                    match maybe_line {
                        None => {
                            stdin_open = false;
                        }
                        Some(line) => {
                            let trimmed = line.trim();
                            if trimmed.starts_with(':') {
                                match trimmed {
                                    ":brain" => print_brain_status(&brain),
                                    ":stats" => print_stats(&brain),
                                    ":help" => print_supervisor_help(),
                                    ":backup" => {
                                        if backup_rx.is_some() {
                                            println!("[SpicaBackup] Zaten devam eden bir yedekleme var.");
                                        } else if requested_action != SupervisorAction::None {
                                            println!("[SpicaBackup] Sunucu kapanma/yeniden baslatma asamasinda, yedekleme baslatilmadi.");
                                        } else {
                                            println!(
                                                "[SpicaBackup] Manuel dunya yedegi baslatiliyor (save-off -> save-all flush -> Rust tar.gz)..."
                                            );
                                            send_to_child(&mut child_stdin, "save-off").await;
                                            send_to_child(&mut child_stdin, "save-all flush").await;
                                            let (tx, rx) = oneshot::channel();
                                            let work_dir_clone = plan.work_dir.clone();
                                            let backup_cfg_clone = cfg.backup.clone();
                                            tokio::spawn(async move {
                                                tokio::time::sleep(Duration::from_millis(backup_cfg_clone.flush_wait_ms)).await;
                                                let res = match tokio::task::spawn_blocking(move || {
                                                    create_world_backup(&work_dir_clone, &backup_cfg_clone)
                                                })
                                                .await
                                                {
                                                    Ok(r) => r,
                                                    Err(e) => Err(format!("Yedekleme thread hatasi: {}", e)),
                                                };
                                                let _ = tx.send(res);
                                            });
                                            backup_rx = Some(rx);
                                        }
                                    }
                                    ":restart" => {
                                        if backup_rx.is_some() {
                                            send_to_child(&mut child_stdin, "save-on").await;
                                            backup_rx = None;
                                        }
                                        requested_action = SupervisorAction::Restart;
                                        println!("[SpicaSupervisor] Purpur guvenli yeniden baslatiliyor ('stop' gonderiliyor)...");
                                        send_to_child(&mut child_stdin, "stop").await;
                                        force_kill_deadline = Some(
                                            tokio::time::Instant::now()
                                                + Duration::from_secs(GRACEFUL_STOP_TIMEOUT_SECS),
                                        );
                                    }
                                    ":stop" => {
                                        if backup_rx.is_some() {
                                            send_to_child(&mut child_stdin, "save-on").await;
                                            backup_rx = None;
                                        }
                                        requested_action = SupervisorAction::Stop;
                                        println!("[SpicaSupervisor] Purpur ve Spica Launcher durduruluyor ('stop' gonderiliyor)...");
                                        send_to_child(&mut child_stdin, "stop").await;
                                        force_kill_deadline = Some(
                                            tokio::time::Instant::now()
                                                + Duration::from_secs(GRACEFUL_STOP_TIMEOUT_SECS),
                                        );
                                    }
                                    _ => {
                                        println!(
                                            "[SpicaSupervisor] Bilinmeyen komut '{}'. Kullanilabilir: :brain, :stats, :backup, :restart, :stop, :help",
                                            trimmed
                                        );
                                    }
                                }
                            } else {
                                send_to_child(&mut child_stdin, &line).await;
                            }
                        }
                    }
                }
                ctrl_c_res = tokio::signal::ctrl_c() => {
                    if ctrl_c_res.is_ok() {
                        if requested_action == SupervisorAction::Stop {
                            eprintln!("[SpicaSupervisor] Ikinci Ctrl+C alindi, Purpur zorla sonlandiriliyor...");
                            let _ = child.kill().await;
                        } else {
                            requested_action = SupervisorAction::Stop;
                            println!("[SpicaSupervisor] Ctrl+C alindi. Purpur guvenli kapatiliyor ('stop' gonderiliyor)...");
                            send_to_child(&mut child_stdin, "stop").await;
                            force_kill_deadline = Some(
                                tokio::time::Instant::now()
                                    + Duration::from_secs(GRACEFUL_STOP_TIMEOUT_SECS),
                            );
                        }
                    }
                }
                _ = async move {
                    if let Some(deadline) = current_deadline {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if current_deadline.is_some() => {
                    eprintln!(
                        "[SpicaSupervisor] Purpur {} sn icinde kapanmadi, surec zorla sonlandiriliyor...",
                        GRACEFUL_STOP_TIMEOUT_SECS
                    );
                    let _ = child.kill().await;
                    force_kill_deadline = None;
                }
            }
        };

        // Close stdin pipe and drain remaining child output cleanly
        drop(child_stdin);
        if let Some(h) = stdout_handle {
            let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
        }
        if let Some(h) = stderr_handle {
            let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
        }

        match brain.lock() {
            Ok(mut guard) => guard.mark_disconnected(),
            Err(poisoned) => poisoned.into_inner().mark_disconnected(),
        }

        match requested_action {
            SupervisorAction::Stop => {
                println!("[SpicaSupervisor] Purpur durduruldu. Spica Launcher kapatiliyor.");
                let _ = shutdown_tx.send(true);
                return Ok(());
            }
            SupervisorAction::Restart => {
                restart_count = 0;
                println!("[SpicaSupervisor] Purpur 2 saniye icinde yeniden baslatilacak...");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            SupervisorAction::None => {}
        }

        match exit_result {
            Ok(status) if status.success() => {
                println!(
                    "[SpicaSupervisor] Purpur temiz sekilde kapandi (cikis kodu: 0). Supervisor sonlaniyor."
                );
                let _ = shutdown_tx.send(true);
                return Ok(());
            }
            Ok(status) => {
                let code_desc = status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "sinyal".to_string());
                eprintln!(
                    "[SpicaSupervisor] UYARI: Purpur beklenmedik sekilde kapandi (cikis kodu: {}).",
                    code_desc
                );
            }
            Err(e) => {
                eprintln!(
                    "[SpicaSupervisor] UYARI: Purpur sureci beklenirken hata olustu: {}",
                    e
                );
            }
        }

        if !cfg.server.auto_restart {
            let _ = shutdown_tx.send(true);
            return Err(
                "Purpur hata ile kapandi ve 'auto_restart = false' oldugu icin yeniden baslatilmadi."
                    .to_string(),
            );
        }

        if started_at.elapsed() >= Duration::from_secs(STABLE_UPTIME_RESET_SECS) {
            restart_count = 0;
        }

        restart_count = restart_count.saturating_add(1);
        if restart_count > cfg.server.max_restarts {
            let _ = shutdown_tx.send(true);
            return Err(format!(
                "HATA: Purpur maksimum yeniden baslatma limitini asti (max_restarts = {}). Supervisor durduruldu.",
                cfg.server.max_restarts
            ));
        }

        let backoff_secs = 5u64.saturating_mul(1u64 << (restart_count - 1).min(5));
        println!(
            "[SpicaSupervisor] Cokme kurtarma ({}/{}): {} saniye sonra yeniden baslatilacak (iptal icin :stop veya Ctrl+C)...",
            restart_count, cfg.server.max_restarts, backoff_secs
        );

        let mut abort_restart = false;
        let sleep = tokio::time::sleep(Duration::from_secs(backoff_secs));
        tokio::pin!(sleep);

        loop {
            tokio::select! {
                _ = &mut sleep => break,
                ctrl_c_res = tokio::signal::ctrl_c() => {
                    if ctrl_c_res.is_ok() {
                        println!("[SpicaSupervisor] Yeniden baslatma beklemesi Ctrl+C ile iptal edildi.");
                        abort_restart = true;
                        break;
                    }
                }
                maybe_line = stdin_rx.recv(), if stdin_open => {
                    match maybe_line {
                        None => {
                            stdin_open = false;
                        }
                        Some(line) => {
                            match line.trim() {
                                ":stop" => {
                                    println!("[SpicaSupervisor] Yeniden baslatma :stop ile iptal edildi.");
                                    abort_restart = true;
                                    break;
                                }
                                ":brain" => print_brain_status(&brain),
                                ":stats" => print_stats(&brain),
                                ":help" => print_supervisor_help(),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        if abort_restart {
            let _ = shutdown_tx.send(true);
            return Ok(());
        }
    }
}
