use crate::backup::{create_snapshot_backup, BackupCancelGate, BackupOutcome};
use crate::brain::BrainState;
use crate::config::{generate_auth_token, BackupConfig, LauncherConfig};
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
const MIN_FLUSH_ACK_TIMEOUT_MS: u64 = 15_000;
const BACKUP_CANCEL_DRAIN_SECS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorAction {
    None,
    Restart,
    Stop,
}

#[derive(Debug, PartialEq, Eq)]
enum CheckpointEvent {
    Result { id: String, saved: bool },
    WriteFailure,
}

enum BackupPhase {
    Idle,
    WaitingCheckpoint {
        id: String,
        deadline: tokio::time::Instant,
        write_failed: bool,
    },
    Archiving {
        cancel_gate: Arc<BackupCancelGate>,
        rx: oneshot::Receiver<Result<BackupOutcome, String>>,
    },
}

impl BackupPhase {
    fn is_active(&self) -> bool {
        !matches!(self, Self::Idle)
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

/// Validates a Purpur/Paper timestamp and optional thread name before the log level.
fn is_valid_server_log_prefix(prefix: &str, allow_worker: bool) -> bool {
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
    // Classic format includes a thread. Save errors can come from region I/O workers.
    prefix[9..]
        .strip_prefix("] [")
        .and_then(|name| name.strip_suffix('/'))
        .is_some_and(|name| {
            name == "Server thread"
                || (allow_worker && !name.is_empty() && !name.contains(['[', ']']))
        })
}

/// Only a structured checkpoint result for the requested ID can start a backup.
fn detect_checkpoint_event(line: &str) -> Option<CheckpointEvent> {
    let clean = strip_ansi(line);
    let clean = clean.trim_end();
    if let Some((prefix, msg)) = clean
        .split_once("ERROR]: ")
        .or_else(|| clean.split_once("WARN]: "))
    {
        // Paper's console can prefix the message with the short logger name.
        let msg = msg
            .strip_prefix('[')
            .and_then(|rest| rest.split_once("] "))
            .map_or(msg, |(_, message)| message);
        // Purpur 1.21.4 can log these save failures and still report save-all success.
        const SAVE_FAILURE_PREFIXES: &[&str] = &[
            "Failed to save chunk (",
            "Failed to save chunk data (",
            "Failed to save entity data (",
            "Failed to save poi data (",
            "Failed to write chunk data for task: ",
            "Exception when flushing regions in world ",
            "Could not save data to ",
            "Failed to save level ",
            "Failed to save player data for ",
        ];
        if is_valid_server_log_prefix(prefix, true)
            && SAVE_FAILURE_PREFIXES
                .iter()
                .any(|start| msg.starts_with(start))
        {
            return Some(CheckpointEvent::WriteFailure);
        }
    }
    let (prefix, msg) = clean.split_once("INFO]: ")?;
    if !is_valid_server_log_prefix(prefix, false) {
        return None;
    }
    let (id, status) = msg
        .strip_prefix("[SpicaRustNative] SpicaBackup checkpoint ")?
        .split_once(' ')?;
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let saved = match status {
        "saved" => true,
        "failed" => false,
        _ => return None,
    };
    Some(CheckpointEvent::Result {
        id: id.to_string(),
        saved,
    })
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
    save_tx: Option<mpsc::UnboundedSender<CheckpointEvent>>,
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
                        if let Some(ev) = detect_checkpoint_event(line) {
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

/// Cancel any worker and allow it to release the snapshot before child shutdown.
async fn abort_active_backup(backup_phase: &mut BackupPhase) {
    if let BackupPhase::Archiving { cancel_gate, rx } =
        std::mem::replace(backup_phase, BackupPhase::Idle)
    {
        let _ = cancel_gate.request_cancel();
        if tokio::time::timeout(Duration::from_secs(BACKUP_CANCEL_DRAIN_SECS), rx)
            .await
            .is_err()
        {
            eprintln!("[SpicaBackup] Snapshot iscisi zamaninda kapanmadi.");
        }
    }
}

async fn initiate_backup_transaction(
    backup_phase: &mut BackupPhase,
    child_stdin: &mut Option<ChildStdin>,
    config: &BackupConfig,
) {
    if config.snapshot_command.is_empty() {
        eprintln!("[SpicaBackup] HATA: Canli yedek icin backup.snapshot_command gerekli.");
        return;
    }
    let id = generate_auth_token();
    if let Err(err) = send_to_child(
        child_stdin,
        &format!("spicarustnative:spicabackupcheckpoint {id}"),
    )
    .await
    {
        eprintln!("[SpicaBackup] HATA: Kayit noktasi istegi gonderilemedi: {err}");
        return;
    }

    *backup_phase = BackupPhase::WaitingCheckpoint {
        id,
        deadline: tokio::time::Instant::now()
            + Duration::from_millis(config.flush_wait_ms.max(MIN_FLUSH_ACK_TIMEOUT_MS)),
        write_failed: false,
    };
}

fn print_supervisor_help() {
    println!("================ Spica Supervisor Komutlari ================");
    println!("  :brain    -> Canli Rust Brain durumu, EWMA MSPT ve direktif");
    println!("  :stats    -> Host CPU/RAM, JVM Heap, TPS ve varlik ozeti");
    println!("  :backup   -> Dunya snapshot'ini .tar.gz olarak yedekler");
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
/// checkpoint-verified snapshot backups, graceful Ctrl+C handling, and crash recovery.
pub async fn run_supervisor(
    cfg: &LauncherConfig,
    plan: &JvmPlan,
    brain: Arc<Mutex<BrainState>>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<(), String> {
    let mut stdin_rx = spawn_stdin_reader();
    let mut stdin_open = true;
    let mut restart_count: u32 = 0;
    if cfg.backup.enabled && cfg.backup.snapshot_command.is_empty() {
        eprintln!("[SpicaBackup] Zamanlanmis canli yedek icin backup.snapshot_command gerekli.");
    }
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

        let (save_ev_tx, mut save_ev_rx) = mpsc::unbounded_channel::<CheckpointEvent>();
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
        let mut scheduled_backup_timer = if cfg.backup.enabled
            && !cfg.backup.snapshot_command.is_empty()
        {
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
                BackupPhase::WaitingCheckpoint { deadline, .. } => Some(*deadline),
                _ => None,
            };
            let is_archiving = matches!(backup_phase, BackupPhase::Archiving { .. });
            let backup_idle = !backup_phase.is_active();

            tokio::select! {
                status = child.wait() => {
                    break status;
                }
                Some(event) = save_ev_rx.recv() => {
                    if let BackupPhase::WaitingCheckpoint { id, deadline, write_failed } = &mut backup_phase {
                        let (result_id, saved) = match event {
                            CheckpointEvent::Result { id, saved } => (id, saved),
                            CheckpointEvent::WriteFailure => {
                                *write_failed = true;
                                continue;
                            }
                        };
                        if *id != result_id || tokio::time::Instant::now() >= *deadline
                            || requested_action != SupervisorAction::None
                        {
                            continue;
                        }
                        if !saved || *write_failed {
                            let reason = if *write_failed { "Purpur yazma hatasi bildirdi." }
                                else { "Purpur kayit noktasi basarisiz." };
                            backup_phase = BackupPhase::Idle;
                            eprintln!("[SpicaBackup] HATA: {reason}");
                            continue;
                        }
                        let cancel_gate = Arc::new(BackupCancelGate::new());
                        let worker_gate = Arc::clone(&cancel_gate);
                        let (tx, rx) = oneshot::channel();
                        let work_dir = plan.work_dir.clone();
                        let config = cfg.backup.clone();
                        tokio::spawn(async move {
                            let result = create_snapshot_backup(
                                &work_dir, &config, &result_id, worker_gate,
                            ).await;
                            let _ = tx.send(result);
                        });
                        backup_phase = BackupPhase::Archiving { cancel_gate, rx };
                    }
                }
                _ = async move {
                    if let Some(dl) = phase_deadline {
                        tokio::time::sleep_until(dl).await;
                    }
                }, if phase_deadline.is_some() => {
                    backup_phase = BackupPhase::Idle;
                    eprintln!("[SpicaBackup] HATA: Purpur kayit noktasi onayi zaman asimina ugradi.");
                }
                backup_res = async {
                    match &mut backup_phase {
                        BackupPhase::Archiving { rx, .. } => rx.await,
                        _ => std::future::pending().await,
                    }
                }, if is_archiving => {
                    backup_phase = BackupPhase::Idle;
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
                        Ok(Err(err)) => eprintln!("[SpicaBackup] HATA: {err}"),
                        Err(_) => eprintln!("[SpicaBackup] HATA: Yedek islemi beklenmedik sekilde kapandi."),
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
                        &cfg.backup,
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
                                                &cfg.backup,
                                            )
                                            .await;
                                        }
                                    }
                                    ":restart" => {
                                        abort_active_backup(&mut backup_phase).await;
                                        requested_action = SupervisorAction::Restart;
                                        println!("[SpicaSupervisor] Purpur guvenli yeniden baslatiliyor ('stop' gonderiliyor)...");
                                        let _ = send_to_child(&mut child_stdin, "stop").await;
                                        force_kill_deadline = Some(
                                            tokio::time::Instant::now()
                                                + Duration::from_secs(GRACEFUL_STOP_TIMEOUT_SECS),
                                        );
                                    }
                                    ":stop" => {
                                        abort_active_backup(&mut backup_phase).await;
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
                                    abort_active_backup(&mut backup_phase).await;
                                    if base_cmd == "stop" {
                                        requested_action = SupervisorAction::Stop;
                                        force_kill_deadline = Some(
                                            tokio::time::Instant::now()
                                                + Duration::from_secs(GRACEFUL_STOP_TIMEOUT_SECS),
                                        );
                                    }
                                    let _ = send_to_child(&mut child_stdin, &line).await;
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
                            abort_active_backup(&mut backup_phase).await;
                            let _ = child.kill().await;
                        } else {
                            abort_active_backup(&mut backup_phase).await;
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
                    abort_active_backup(&mut backup_phase).await;
                    let _ = child.kill().await;
                    force_kill_deadline = None;
                }
            }
        };

        // Ensure any in-flight backup for this exited child generation is cancelled & drained before cleanup/restart
        abort_active_backup(&mut backup_phase).await;

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
    fn checkpoint_event_requires_plugin_prefix_and_id() {
        let id = "a".repeat(64);
        let valid = format!("[15:20:01 INFO]: [SpicaRustNative] SpicaBackup checkpoint {id} saved");
        assert_eq!(
            detect_checkpoint_event(&valid),
            Some(CheckpointEvent::Result {
                id: id.clone(),
                saved: true
            })
        );
        let failed = format!(
            "[15:20:01] [Server thread/INFO]: [SpicaRustNative] SpicaBackup checkpoint {id} failed"
        );
        assert_eq!(
            detect_checkpoint_event(&failed),
            Some(CheckpointEvent::Result {
                id: id.clone(),
                saved: false
            })
        );
        assert_eq!(
            detect_checkpoint_event("[15:20:01 INFO]: Saved the game"),
            None
        );
        assert_eq!(
            detect_checkpoint_event(&valid.replace("INFO]: ", "INFO]: <Player> ")),
            None
        );
        assert_eq!(
            detect_checkpoint_event(&valid.replace(&id, "invalid")),
            None
        );
    }

    #[test]
    fn checkpoint_detects_save_errors_from_server_and_io_threads() {
        for line in [
            "[15:20:01 ERROR]: Failed to save chunk data (11,-8) in world 'world'",
            "[15:20:01 ERROR]: [MoonriseRegionFileIO] Failed to write chunk data for task: test",
            "[15:20:01] [Region I/O Worker #1/ERROR]: Failed to write chunk data for task: test",
            "[15:20:01 WARN]: Failed to save player data for SpicaBot001",
        ] {
            assert_eq!(
                detect_checkpoint_event(line),
                Some(CheckpointEvent::WriteFailure)
            );
        }
        for line in [
            "[15:20:01 INFO]: <Player> [15:20:01 ERROR]: Failed to save level world",
            "[15:20:01 ERROR]: Unrelated startup failure",
            "[garbled] [Region I/O Worker #1/ERROR]: Failed to write chunk data for task: test",
        ] {
            assert_eq!(detect_checkpoint_event(line), None);
        }
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
