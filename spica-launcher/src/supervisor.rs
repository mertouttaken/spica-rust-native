use crate::backup::{create_world_backup_cancellable, BackupCancelGate, BackupOutcome};
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
const STDIN_WRITE_TIMEOUT_SECS: u64 = 3;
const SAVE_OFF_ACK_TIMEOUT_SECS: u64 = 10;
const SAVE_ON_ACK_TIMEOUT_SECS: u64 = 10;
const MIN_FLUSH_ACK_TIMEOUT_MS: u64 = 15_000;
const BACKUP_CANCEL_DRAIN_SECS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorAction {
    None,
    Restart,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PurpurSaveEvent {
    SaveOffDisabledNow,
    SaveOffAlreadyOff,
    SaveAllFlushed,
    SaveOnAck,
}

enum BackupPhase {
    Idle,
    WaitingSaveOff {
        generation: u64,
        deadline: tokio::time::Instant,
        flush_wait_ms: u64,
    },
    WaitingFlush {
        generation: u64,
        deadline: tokio::time::Instant,
        restore_save_on: bool,
    },
    Archiving {
        generation: u64,
        restore_save_on: bool,
        cancel_gate: Arc<BackupCancelGate>,
        rx: oneshot::Receiver<Result<BackupOutcome, String>>,
    },
    WaitingSaveOn {
        generation: u64,
        deadline: tokio::time::Instant,
    },
}

impl BackupPhase {
    fn is_active(&self) -> bool {
        !matches!(self, BackupPhase::Idle)
    }

    fn blocks_save_commands(&self) -> bool {
        matches!(
            self,
            BackupPhase::WaitingSaveOff { .. }
                | BackupPhase::WaitingFlush { .. }
                | BackupPhase::Archiving { .. }
        )
    }
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

/// Strips ANSI CSI escape sequences (e.g. `\x1b[32m`) from Purpur log lines before exact matching.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && matches!(chars.peek(), Some('[')) {
            let _ = chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Strictly validates that the prefix before `"INFO]: "` is a real Purpur/Paper server
/// timestamp/thread log header (e.g. `"[15:20:01 "` or `"[15:20:01] [Server thread/"`).
fn is_valid_server_info_prefix(prefix: &str) -> bool {
    let b = prefix.as_bytes();
    // Standard Purpur/Paper format: "[HH:MM:SS " (10 bytes before "INFO]: ")
    let valid_hms = b.len() >= 10
        && b[0] == b'['
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3] == b':'
        && b[4].is_ascii_digit()
        && b[5].is_ascii_digit()
        && b[6] == b':'
        && b[7].is_ascii_digit()
        && b[8].is_ascii_digit();
    if !valid_hms {
        return false;
    }
    if b.len() == 10 && b[9] == b' ' {
        return true;
    }
    // Classic Spigot/Vanilla format: "[HH:MM:SS] [Server thread/"
    &prefix[9..] == "] [Server thread/"
}

/// Strictly parses Purpur/Paper server stdout save-lifecycle log lines.
///
/// Requires a strict `[HH:MM:SS INFO]: <message>` (or `[HH:MM:SS] [Server thread/INFO]: <message>`)
/// server log header so neither player chat nor WARN/ERROR/plugin stdout can spoof flush completion.
fn detect_save_event(line: &str) -> Option<PurpurSaveEvent> {
    let clean = strip_ansi(line);
    let trimmed = clean.trim_end();
    let (prefix, msg) = trimmed.split_once("INFO]: ")?;
    if !is_valid_server_info_prefix(prefix) {
        return None;
    }
    match msg.trim() {
        "Saved the game" | "Saved the world" => Some(PurpurSaveEvent::SaveAllFlushed),
        "Automatic saving is now disabled" => Some(PurpurSaveEvent::SaveOffDisabledNow),
        "Saving is already turned off" => Some(PurpurSaveEvent::SaveOffAlreadyOff),
        "Automatic saving is now enabled" | "Saving is already turned on" => {
            Some(PurpurSaveEvent::SaveOnAck)
        }
        _ => None,
    }
}

/// Extracts the normalized base console command token, stripping leading `/` and optional
/// Bukkit/Minecraft namespaces (e.g. `/minecraft:stop` -> `"stop"`, `bukkit:reload confirm` -> `"reload"`).
fn normalize_console_command(trimmed: &str) -> String {
    let without_slash = trimmed.trim_start_matches('/');
    let first_token = without_slash
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match first_token.rsplit_once(':') {
        Some((_, cmd)) => cmd.to_string(),
        None => first_token,
    }
}

async fn pipe_stream<R: AsyncRead + Unpin>(
    reader: R,
    is_stderr: bool,
    save_tx: Option<mpsc::UnboundedSender<PurpurSaveEvent>>,
) {
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
                    if let Some(tx) = save_tx.as_ref() {
                        if let Some(ev) = detect_save_event(line) {
                            let _ = tx.send(ev);
                        }
                    }
                    println!("{}", line);
                }
            }
        }
    }
}

async fn send_to_child(child_stdin: &mut Option<ChildStdin>, line: &str) -> Result<(), String> {
    let Some(stdin) = child_stdin.as_mut() else {
        return Err("Purpur stdin aktif degil.".to_string());
    };
    let mut payload = String::with_capacity(line.len() + 1);
    payload.push_str(line);
    payload.push('\n');

    let write_res = tokio::time::timeout(Duration::from_secs(STDIN_WRITE_TIMEOUT_SECS), async {
        stdin.write_all(payload.as_bytes()).await?;
        stdin.flush().await
    })
    .await;

    match write_res {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            let msg = format!("Purpur stdin yazma hatasi: {}", e);
            eprintln!("[SpicaSupervisor] {}", msg);
            Err(msg)
        }
        Err(_) => {
            let msg = format!(
                "Purpur stdin {} sn icinde yanit vermedi (pipe tikanik).",
                STDIN_WRITE_TIMEOUT_SECS
            );
            eprintln!("[SpicaSupervisor] {}", msg);
            Err(msg)
        }
    }
}

/// Cancels any active backup transaction, waits for the blocking archiver to stop and clean up
/// its temporary file, and restores `save-on` only when `allow_restore_save_on` is true AND this
/// backup transaction actually disabled automatic saving (R4: preserves prior admin `save-off`).
async fn abort_active_backup(
    backup_phase: &mut BackupPhase,
    child_stdin: &mut Option<ChildStdin>,
    allow_restore_save_on: bool,
) {
    let prev = std::mem::replace(backup_phase, BackupPhase::Idle);
    match prev {
        BackupPhase::Idle => {}
        BackupPhase::WaitingSaveOff { .. } => {
            println!("[SpicaBackup] Bekleyen yedek save-off onayi iptal edildi.");
            if allow_restore_save_on {
                if let Err(err) = send_to_child(child_stdin, "save-on").await {
                    eprintln!("[SpicaBackup] UYARI: iptal sonrasi save-on gonderilemedi: {}", err);
                }
            }
        }
        BackupPhase::WaitingFlush {
            restore_save_on, ..
        } => {
            println!("[SpicaBackup] Bekleyen yedek flush islemi iptal edildi.");
            if allow_restore_save_on && restore_save_on {
                if let Err(err) = send_to_child(child_stdin, "save-on").await {
                    eprintln!("[SpicaBackup] UYARI: iptal sonrasi save-on gonderilemedi: {}", err);
                }
            } else if allow_restore_save_on {
                println!("[SpicaBackup] Not: Yedek oncesi save-off durumu korundu.");
            }
        }
        BackupPhase::Archiving {
            restore_save_on,
            cancel_gate,
            rx,
            ..
        } => {
            println!("[SpicaBackup] Aktif yedekleme iptal ediliyor, gecici dosya temizleniyor...");
            let _ = cancel_gate.request_cancel();
            match tokio::time::timeout(Duration::from_secs(BACKUP_CANCEL_DRAIN_SECS), rx).await {
                Ok(_) => {}
                Err(_) => {
                    eprintln!(
                        "[SpicaBackup] UYARI: Arsivleme iscisi {} sn icinde sonlanmadi (publish kilidi iptal modunda).",
                        BACKUP_CANCEL_DRAIN_SECS
                    );
                }
            }
            if allow_restore_save_on && restore_save_on {
                if let Err(err) = send_to_child(child_stdin, "save-on").await {
                    eprintln!("[SpicaBackup] UYARI: iptal sonrasi save-on gonderilemedi: {}", err);
                }
            } else if allow_restore_save_on {
                println!("[SpicaBackup] Not: Yedek oncesi save-off durumu korundu.");
            }
        }
        BackupPhase::WaitingSaveOn { .. } => {
            if allow_restore_save_on {
                let _ = send_to_child(child_stdin, "save-on").await;
            }
        }
    }
}

async fn initiate_backup_transaction(
    backup_phase: &mut BackupPhase,
    child_stdin: &mut Option<ChildStdin>,
    save_ev_rx: &mut mpsc::UnboundedReceiver<PurpurSaveEvent>,
    child_generation: u64,
    flush_wait_ms: u64,
    label: &str,
) {
    // R3: Drain any stale stdout save events queued before this transaction starts
    while save_ev_rx.try_recv().is_ok() {}

    println!(
        "[SpicaBackup] {} dunya yedegi baslatiliyor (save-off onayi -> save-all flush -> Purpur onayi -> tar.gz -> save-on)...",
        label
    );
    if let Err(err) = send_to_child(child_stdin, "save-off").await {
        eprintln!("[SpicaBackup] HATA: save-off gonderilemedi: {}", err);
        return;
    }

    *backup_phase = BackupPhase::WaitingSaveOff {
        generation: child_generation,
        deadline: tokio::time::Instant::now() + Duration::from_secs(SAVE_OFF_ACK_TIMEOUT_SECS),
        flush_wait_ms,
    };
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
/// 4-stage flush-verified cancellable world backups, graceful Ctrl+C handling, and crash recovery.
pub async fn run_supervisor(
    cfg: &LauncherConfig,
    plan: &JvmPlan,
    brain: Arc<Mutex<BrainState>>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<(), String> {
    let mut stdin_rx = spawn_stdin_reader();
    let mut stdin_open = true;
    let mut restart_count: u32 = 0;
    let mut child_generation: u64 = 0;

    loop {
        child_generation = child_generation.wrapping_add(1);
        println!(
            "[SpicaSupervisor] Purpur baslatiliyor (nesil #{}): {} -Xms{}M -Xmx{}M ({})",
            child_generation,
            plan.jar_path.display(),
            plan.min_ram_mb,
            plan.max_ram_mb,
            plan.resolved_gc
        );

        let mut child = Command::new(&plan.java_bin)
            .current_dir(&plan.work_dir)
            .args(&plan.args)
            .env("SPICA_IPC_TOKEN", &plan.ipc_token)
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

        let (save_ev_tx, mut save_ev_rx) = mpsc::unbounded_channel::<PurpurSaveEvent>();
        let mut child_stdin = child.stdin.take();
        let stdout_handle = child
            .stdout
            .take()
            .map(|out| tokio::spawn(pipe_stream(out, false, Some(save_ev_tx))));
        let stderr_handle = child
            .stderr
            .take()
            .map(|err| tokio::spawn(pipe_stream(err, true, None)));

        let started_at = Instant::now();
        let mut requested_action = SupervisorAction::None;
        let mut force_kill_deadline: Option<tokio::time::Instant> = None;
        let mut backup_phase = BackupPhase::Idle;
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
            let phase_deadline = match &backup_phase {
                BackupPhase::WaitingSaveOff { deadline, .. }
                | BackupPhase::WaitingFlush { deadline, .. }
                | BackupPhase::WaitingSaveOn { deadline, .. } => Some(*deadline),
                _ => None,
            };
            let is_archiving = matches!(backup_phase, BackupPhase::Archiving { .. });
            let backup_idle = !backup_phase.is_active();

            tokio::select! {
                status = child.wait() => {
                    break status;
                }
                Some(save_ev) = save_ev_rx.recv() => {
                    match (save_ev, &backup_phase) {
                        (
                            PurpurSaveEvent::SaveOffDisabledNow | PurpurSaveEvent::SaveOffAlreadyOff,
                            BackupPhase::WaitingSaveOff { generation, flush_wait_ms, .. },
                        ) if *generation == child_generation && requested_action == SupervisorAction::None => {
                            let fw = *flush_wait_ms;
                            let should_restore_save_on =
                                matches!(save_ev, PurpurSaveEvent::SaveOffDisabledNow);
                            if !should_restore_save_on {
                                println!(
                                    "[SpicaBackup] Not: Purpur otomatik kayit zaten kapaliydi; yedek bitiminde save-off durumu korunacak."
                                );
                            }
                            // R3: Drain any stale prior SaveAllFlushed events right before sending save-all flush
                            while save_ev_rx.try_recv().is_ok() {}
                            if let Err(err) = send_to_child(&mut child_stdin, "save-all flush").await {
                                eprintln!("[SpicaBackup] HATA: save-all flush gonderilemedi: {}", err);
                                if should_restore_save_on {
                                    match send_to_child(&mut child_stdin, "save-on").await {
                                        Ok(()) => {
                                            backup_phase = BackupPhase::WaitingSaveOn {
                                                generation: child_generation,
                                                deadline: tokio::time::Instant::now()
                                                    + Duration::from_secs(SAVE_ON_ACK_TIMEOUT_SECS),
                                            };
                                        }
                                        Err(save_on_err) => {
                                            backup_phase = BackupPhase::Idle;
                                            eprintln!(
                                                "[SpicaBackup] KRITIK UYARI: flush hatasi sonrasi 'save-on' gonderilemedi: {}",
                                                save_on_err
                                            );
                                        }
                                    }
                                } else {
                                    backup_phase = BackupPhase::Idle;
                                }
                            } else {
                                let timeout_ms = fw.max(MIN_FLUSH_ACK_TIMEOUT_MS);
                                backup_phase = BackupPhase::WaitingFlush {
                                    generation: child_generation,
                                    deadline: tokio::time::Instant::now() + Duration::from_millis(timeout_ms),
                                    restore_save_on: should_restore_save_on,
                                };
                            }
                        }
                        (
                            PurpurSaveEvent::SaveAllFlushed,
                            BackupPhase::WaitingFlush {
                                generation,
                                restore_save_on,
                                ..
                            },
                        ) if *generation == child_generation && requested_action == SupervisorAction::None => {
                            let should_restore_save_on = *restore_save_on;
                            println!("[SpicaBackup] Purpur 'Saved the game' onayi alindi. Rust tar.gz arsivleme basliyor...");
                            let cancel_gate = Arc::new(BackupCancelGate::new());
                            let cancel_worker = Arc::clone(&cancel_gate);
                            let (tx, rx) = oneshot::channel();
                            let work_dir_clone = plan.work_dir.clone();
                            let backup_cfg_clone = cfg.backup.clone();
                            tokio::spawn(async move {
                                let res = match tokio::task::spawn_blocking(move || {
                                    create_world_backup_cancellable(
                                        &work_dir_clone,
                                        &backup_cfg_clone,
                                        &cancel_worker,
                                    )
                                })
                                .await
                                {
                                    Ok(r) => r,
                                    Err(e) => Err(format!("Yedekleme thread hatasi: {}", e)),
                                };
                                let _ = tx.send(res);
                            });
                            backup_phase = BackupPhase::Archiving {
                                generation: child_generation,
                                restore_save_on: should_restore_save_on,
                                cancel_gate,
                                rx,
                            };
                        }
                        (PurpurSaveEvent::SaveOnAck, BackupPhase::WaitingSaveOn { generation, .. })
                            if *generation == child_generation =>
                        {
                            backup_phase = BackupPhase::Idle;
                            println!("[SpicaBackup] Purpur otomatik kayit (save-on) onaylandi.");
                        }
                        _ => {}
                    }
                }
                _ = async move {
                    if let Some(dl) = phase_deadline {
                        tokio::time::sleep_until(dl).await;
                    }
                }, if phase_deadline.is_some() => {
                    let timed_out = std::mem::replace(&mut backup_phase, BackupPhase::Idle);
                    match timed_out {
                        BackupPhase::WaitingSaveOff { .. } => {
                            eprintln!(
                                "[SpicaBackup] HATA: Purpur 'save-off' onayi {} sn icinde gelmedi; yedekleme iptal edildi (save-on gonderiliyor).",
                                SAVE_OFF_ACK_TIMEOUT_SECS
                            );
                            match send_to_child(&mut child_stdin, "save-on").await {
                                Ok(()) => {
                                    backup_phase = BackupPhase::WaitingSaveOn {
                                        generation: child_generation,
                                        deadline: tokio::time::Instant::now()
                                            + Duration::from_secs(SAVE_ON_ACK_TIMEOUT_SECS),
                                    };
                                }
                                Err(err) => {
                                    eprintln!(
                                        "[SpicaBackup] KRITIK UYARI: save-off zaman asimi sonrasi 'save-on' gonderilemedi: {}",
                                        err
                                    );
                                }
                            }
                        }
                        BackupPhase::WaitingFlush { restore_save_on, .. } => {
                            if restore_save_on {
                                eprintln!(
                                    "[SpicaBackup] HATA: Purpur 'Saved the game' onayi zaman asimina ugradi; tutarsiz yedek olusturulmamasi icin iptal edildi (save-on geri aciliyor)."
                                );
                                match send_to_child(&mut child_stdin, "save-on").await {
                                    Ok(()) => {
                                        backup_phase = BackupPhase::WaitingSaveOn {
                                            generation: child_generation,
                                            deadline: tokio::time::Instant::now()
                                                + Duration::from_secs(SAVE_ON_ACK_TIMEOUT_SECS),
                                        };
                                    }
                                    Err(err) => {
                                        eprintln!(
                                            "[SpicaBackup] KRITIK UYARI: flush zaman asimi sonrasi 'save-on' gonderilemedi: {}",
                                            err
                                        );
                                    }
                                }
                            } else {
                                eprintln!(
                                    "[SpicaBackup] HATA: Purpur 'Saved the game' onayi zaman asimina ugradi; yedekleme iptal edildi (onceki save-off korundu)."
                                );
                            }
                        }
                        BackupPhase::WaitingSaveOn { .. } => {
                            eprintln!(
                                "[SpicaBackup] UYARI: Purpur 'save-on' onayi {} sn icinde gorulmedi; manuel 'save-on' kontrol edin.",
                                SAVE_ON_ACK_TIMEOUT_SECS
                            );
                        }
                        _ => {}
                    }
                }
                backup_res = async {
                    match &mut backup_phase {
                        BackupPhase::Archiving { rx, .. } => rx.await,
                        _ => std::future::pending().await,
                    }
                }, if is_archiving => {
                    let (completed_gen, should_restore_save_on) =
                        match std::mem::replace(&mut backup_phase, BackupPhase::Idle) {
                            BackupPhase::Archiving {
                                generation,
                                restore_save_on,
                                ..
                            } => (generation, restore_save_on),
                            _ => (0, false),
                        };
                    // R4: Restore save-on ONLY if Spica actually turned it off (preserve prior admin save-off)
                    if should_restore_save_on {
                        match send_to_child(&mut child_stdin, "save-on").await {
                            Ok(()) => {
                                if completed_gen == child_generation
                                    && requested_action == SupervisorAction::None
                                {
                                    backup_phase = BackupPhase::WaitingSaveOn {
                                        generation: child_generation,
                                        deadline: tokio::time::Instant::now()
                                            + Duration::from_secs(SAVE_ON_ACK_TIMEOUT_SECS),
                                    };
                                }
                            }
                            Err(err) => {
                                eprintln!(
                                    "[SpicaBackup] KRITIK UYARI: Yedek sonrasi 'save-on' gonderilemedi: {}",
                                    err
                                );
                            }
                        }
                    } else {
                        println!(
                            "[SpicaBackup] Yedek oncesi otomatik kayit kapali oldugu icin 'save-off' durumu korundu."
                        );
                    }

                    if completed_gen == child_generation && requested_action == SupervisorAction::None {
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
                                eprintln!(
                                    "[SpicaBackup] HATA ({}): {}",
                                    if should_restore_save_on { "save-on geri acildi" } else { "onceki save-off korundu" },
                                    err
                                );
                            }
                            Err(_) => {
                                eprintln!(
                                    "[SpicaBackup] HATA: Yedekleme gorevi beklenmedik sekilde iptal oldu ({}).",
                                    if should_restore_save_on { "save-on geri acildi" } else { "onceki save-off korundu" }
                                );
                            }
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
                }, if scheduled_backup_timer.is_some() && backup_idle && requested_action == SupervisorAction::None => {
                    initiate_backup_transaction(
                        &mut backup_phase,
                        &mut child_stdin,
                        &mut save_ev_rx,
                        child_generation,
                        cfg.backup.flush_wait_ms,
                        "Zamanlanmis",
                    )
                    .await;
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
                                        if backup_phase.is_active() {
                                            println!("[SpicaBackup] Zaten devam eden bir yedekleme var.");
                                        } else if requested_action != SupervisorAction::None {
                                            println!("[SpicaBackup] Sunucu kapanma/yeniden baslatma asamasinda, yedekleme baslatilmadi.");
                                        } else {
                                            initiate_backup_transaction(
                                                &mut backup_phase,
                                                &mut child_stdin,
                                                &mut save_ev_rx,
                                                child_generation,
                                                cfg.backup.flush_wait_ms,
                                                "Manuel",
                                            )
                                            .await;
                                        }
                                    }
                                    ":restart" => {
                                        abort_active_backup(&mut backup_phase, &mut child_stdin, true).await;
                                        requested_action = SupervisorAction::Restart;
                                        println!("[SpicaSupervisor] Purpur guvenli yeniden baslatiliyor ('stop' gonderiliyor)...");
                                        let _ = send_to_child(&mut child_stdin, "stop").await;
                                        force_kill_deadline = Some(
                                            tokio::time::Instant::now()
                                                + Duration::from_secs(GRACEFUL_STOP_TIMEOUT_SECS),
                                        );
                                    }
                                    ":stop" => {
                                        abort_active_backup(&mut backup_phase, &mut child_stdin, true).await;
                                        requested_action = SupervisorAction::Stop;
                                        println!("[SpicaSupervisor] Purpur ve Spica Launcher durduruluyor ('stop' gonderiliyor)...");
                                        let _ = send_to_child(&mut child_stdin, "stop").await;
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
                                let base_cmd = normalize_console_command(trimmed);
                                if base_cmd == "stop" || base_cmd == "restart" {
                                    // Abort active backup cleanly before forwarding raw stop/restart to Purpur
                                    abort_active_backup(&mut backup_phase, &mut child_stdin, true).await;
                                    if base_cmd == "stop" {
                                        requested_action = SupervisorAction::Stop;
                                        force_kill_deadline = Some(
                                            tokio::time::Instant::now()
                                                + Duration::from_secs(GRACEFUL_STOP_TIMEOUT_SECS),
                                        );
                                    }
                                    let _ = send_to_child(&mut child_stdin, &line).await;
                                } else if backup_phase.blocks_save_commands()
                                    && matches!(base_cmd.as_str(), "save-on" | "save-off" | "save-all" | "reload")
                                {
                                    println!(
                                        "[SpicaBackup] Aktif yedekleme devam ederken '{}' komutu snapshot tutarliligi icin engellendi.",
                                        trimmed
                                    );
                                } else {
                                    let _ = send_to_child(&mut child_stdin, &line).await;
                                }
                            }
                        }
                    }
                }
                ctrl_c_res = tokio::signal::ctrl_c() => {
                    if ctrl_c_res.is_ok() {
                        if requested_action == SupervisorAction::Stop {
                            eprintln!("[SpicaSupervisor] Ikinci Ctrl+C alindi, Purpur zorla sonlandiriliyor...");
                            abort_active_backup(&mut backup_phase, &mut child_stdin, false).await;
                            let _ = child.kill().await;
                        } else {
                            abort_active_backup(&mut backup_phase, &mut child_stdin, true).await;
                            requested_action = SupervisorAction::Stop;
                            println!("[SpicaSupervisor] Ctrl+C alindi. Purpur guvenli kapatiliyor ('stop' gonderiliyor)...");
                            let _ = send_to_child(&mut child_stdin, "stop").await;
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
                    abort_active_backup(&mut backup_phase, &mut child_stdin, false).await;
                    let _ = child.kill().await;
                    force_kill_deadline = None;
                }
            }
        };

        // Ensure any in-flight backup for this exited child generation is cancelled & drained before cleanup/restart
        abort_active_backup(&mut backup_phase, &mut child_stdin, false).await;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_save_event_matches_purpur_and_rejects_player_chat_spoof() {
        assert_eq!(
            detect_save_event("[15:20:01 INFO]: Saved the game"),
            Some(PurpurSaveEvent::SaveAllFlushed)
        );
        assert_eq!(
            detect_save_event("[15:20:01] [Server thread/INFO]: Saved the game"),
            Some(PurpurSaveEvent::SaveAllFlushed)
        );
        assert_eq!(
            detect_save_event("\x1b[32m[15:20:01 INFO]: Automatic saving is now disabled\x1b[0m"),
            Some(PurpurSaveEvent::SaveOffDisabledNow)
        );
        assert_eq!(
            detect_save_event("[15:20:01 INFO]: Saving is already turned off"),
            Some(PurpurSaveEvent::SaveOffAlreadyOff)
        );
        assert_eq!(
            detect_save_event("[15:20:02 INFO]: Automatic saving is now enabled"),
            Some(PurpurSaveEvent::SaveOnAck)
        );
        // Player chat and WARN/plugin stdout spoofing must NEVER match
        assert_eq!(
            detect_save_event("[15:20:01 INFO]: <Griefer> Saved the game"),
            None
        );
        assert_eq!(
            detect_save_event("[15:20:01 WARN]: fake INFO]: Saved the game"),
            None
        );
        assert_eq!(
            detect_save_event("raw plugin stdout INFO]: Saved the game"),
            None
        );
        assert_eq!(
            detect_save_event("[15:20:01 INFO]: [Not Secure] <Bob> Automatic saving is now disabled"),
            None
        );
    }

    #[test]
    fn test_normalize_console_command() {
        assert_eq!(normalize_console_command("stop"), "stop");
        assert_eq!(normalize_console_command("/stop"), "stop");
        assert_eq!(normalize_console_command("/minecraft:stop"), "stop");
        assert_eq!(normalize_console_command("bukkit:reload confirm"), "reload");
        assert_eq!(normalize_console_command("/minecraft:save-on"), "save-on");
        assert_eq!(normalize_console_command("save-all flush"), "save-all");
    }
}
