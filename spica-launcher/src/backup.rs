use crate::config::BackupConfig;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

const BACKUP_PREFIX: &str = "spica-backup-";
const BACKUP_SUFFIX: &str = ".tar.gz";
pub const BACKUP_CANCELLED_ERR: &str =
    "Yedekleme iptal edildi (sunucu durduruluyor veya yeniden baslatiliyor).";

const STATE_RUNNING: u8 = 0;
const STATE_PUBLISHING: u8 = 1;
const STATE_CANCELLED: u8 = 2;

static TMP_FILE_SEQ: AtomicU64 = AtomicU64::new(1);

/// Atomic publish/cancellation gate shared between the supervisor and the blocking archiver.
///
/// Guarantees a TOCTOU-free transition:
/// - Once `request_cancel()` moves `RUNNING -> CANCELLED`, the worker can NEVER publish
///   (rename `.tmp` -> `.tar.gz`) or rotate old backups.
/// - Once `try_begin_publish()` moves `RUNNING -> PUBLISHING`, the verified archive has
///   committed to publish and completes immediately before the worker returns.
#[derive(Debug)]
pub struct BackupCancelGate {
    state: AtomicU8,
}

impl BackupCancelGate {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(STATE_RUNNING),
        }
    }

    /// Requests cancellation. Returns `true` if cancellation won before publish started,
    /// or `false` if publish was already committed.
    pub fn request_cancel(&self) -> bool {
        self.state
            .compare_exchange(
                STATE_RUNNING,
                STATE_CANCELLED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::SeqCst) == STATE_CANCELLED
    }

    /// Atomically transitions `RUNNING -> PUBLISHING`. Returns `false` if already cancelled.
    pub fn try_begin_publish(&self) -> bool {
        self.state
            .compare_exchange(
                STATE_RUNNING,
                STATE_PUBLISHING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }
}

impl Default for BackupCancelGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary returned after a successful zero-JVM-overhead Rust world backup.
#[derive(Debug, Clone)]
pub struct BackupOutcome {
    pub archive_path: PathBuf,
    pub size_bytes: u64,
    pub files_archived: usize,
    pub duration_ms: u64,
    pub worlds_included: Vec<String>,
    pub rotated_deleted: usize,
}

/// Reader wrapper that aborts mid-file when `cancel_gate` is cancelled so `:stop`/`:restart`/crash
/// terminates archiving AND integrity verification promptly without waiting for large `.mca` files.
struct CancellableFileReader<'a> {
    inner: File,
    cancel_gate: &'a BackupCancelGate,
}

impl<'a> Read for CancellableFileReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel_gate.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                BACKUP_CANCELLED_ERR,
            ));
        }
        self.inner.read(buf)
    }
}

/// Converts Unix epoch seconds into a sortable UTC timestamp (`YYYYMMDD-HHMMSS`)
/// without external date/time dependencies (Howard Hinnant civil-from-days algorithm).
fn format_utc_timestamp(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let secs_of_day = epoch_secs % 86_400;
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }

    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        y, m, d, hour, minute, second
    )
}

fn resolve_backup_dir(work_dir: &Path, backup_dir: &str) -> PathBuf {
    let p = Path::new(backup_dir);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        work_dir.join(p)
    }
}

fn is_session_lock(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.eq_ignore_ascii_case("session.lock"))
        .unwrap_or(false)
}

fn verify_tar_gz_integrity(
    archive_path: &Path,
    expected_entries: usize,
    cancel_gate: &BackupCancelGate,
) -> Result<(), String> {
    if cancel_gate.is_cancelled() {
        return Err(BACKUP_CANCELLED_ERR.to_string());
    }
    let file = File::open(archive_path)
        .map_err(|e| format!("Yedek butunluk kontrolunde acilamadi: {}", e))?;
    let cancellable = CancellableFileReader {
        inner: file,
        cancel_gate,
    };
    let gz = GzDecoder::new(cancellable);
    let mut archive = tar::Archive::new(gz);
    let mut count = 0usize;
    let entries = archive.entries().map_err(|e| {
        if cancel_gate.is_cancelled() || e.kind() == io::ErrorKind::Interrupted {
            BACKUP_CANCELLED_ERR.to_string()
        } else {
            format!("Yedek TAR akisi bozuk: {}", e)
        }
    })?;
    for entry_res in entries {
        if cancel_gate.is_cancelled() {
            return Err(BACKUP_CANCELLED_ERR.to_string());
        }
        let _entry = entry_res.map_err(|e| {
            if cancel_gate.is_cancelled() || e.kind() == io::ErrorKind::Interrupted {
                BACKUP_CANCELLED_ERR.to_string()
            } else {
                format!("Yedek icinde bozuk kayit: {}", e)
            }
        })?;
        count += 1;
    }
    if count != expected_entries {
        return Err(format!(
            "Yedek butunluk uyusmazligi: beklenen={}, bulunan={}",
            expected_entries, count
        ));
    }
    // Drain the remaining GZIP stream to EOF so flate2 verifies the CRC32 & ISIZE trailer
    let mut gz = archive.into_inner();
    io::copy(&mut gz, &mut io::sink()).map_err(|e| {
        if cancel_gate.is_cancelled() || e.kind() == io::ErrorKind::Interrupted {
            BACKUP_CANCELLED_ERR.to_string()
        } else {
            format!("Yedek GZIP CRC32/kuyruk dogrulamasi basarisiz: {}", e)
        }
    })?;
    Ok(())
}

/// Non-cancellable convenience wrapper used by tests and offline utilities.
#[allow(dead_code)]
pub fn create_world_backup(work_dir: &Path, cfg: &BackupConfig) -> Result<BackupOutcome, String> {
    let never_cancel = BackupCancelGate::new();
    create_world_backup_cancellable(work_dir, cfg, &never_cancel)
}

/// Archives configured Minecraft world directories under `work_dir` into a collision-proof,
/// strictly sortable, fsynced, integrity-verified `.tar.gz` file inside `cfg.backup_dir`,
/// skipping `session.lock` files, supporting immediate TOCTOU-safe cancellation via `cancel_gate`,
/// and rotating older backups ONLY after the new archive passes full verification and publishes.
pub fn create_world_backup_cancellable(
    work_dir: &Path,
    cfg: &BackupConfig,
    cancel_gate: &BackupCancelGate,
) -> Result<BackupOutcome, String> {
    if cancel_gate.is_cancelled() {
        return Err(BACKUP_CANCELLED_ERR.to_string());
    }

    let started = Instant::now();
    let backup_dir = resolve_backup_dir(work_dir, &cfg.backup_dir);
    fs::create_dir_all(&backup_dir).map_err(|e| {
        format!(
            "Yedek dizini olusturulamadi ({}): {}",
            backup_dir.display(),
            e
        )
    })?;

    let canon_backup_dir = fs::canonicalize(&backup_dir).unwrap_or_else(|_| backup_dir.clone());

    let mut existing_worlds: Vec<(String, PathBuf)> = Vec::new();
    let mut missing_worlds: Vec<String> = Vec::new();
    for world_name in &cfg.worlds {
        let trimmed = world_name.trim();
        if trimmed.is_empty() {
            continue;
        }
        let world_path = work_dir.join(trimmed);
        if world_path.is_dir() {
            let canon_world = fs::canonicalize(&world_path).unwrap_or_else(|_| world_path.clone());
            if canon_backup_dir.starts_with(&canon_world) {
                return Err(format!(
                    "Guvenlik hatasi: backup_dir ('{}') kaynak dunya dizini ('{}') icinde olamaz.",
                    backup_dir.display(),
                    world_path.display()
                ));
            }
            existing_worlds.push((trimmed.to_string(), world_path));
        } else {
            missing_worlds.push(trimmed.to_string());
        }
    }

    if cfg.require_all_worlds && !missing_worlds.is_empty() {
        return Err(format!(
            "Yedekleme iptal edildi: require_all_worlds=true ancak eksik dunya klasorleri var: {:?}",
            missing_worlds
        ));
    }

    if existing_worlds.is_empty() {
        return Err(format!(
            "Yedeklenecek dunya klasoru bulunamadi (work_dir={}, arananlar={:?}).",
            work_dir.display(),
            cfg.worlds
        ));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let epoch_secs = now.as_secs();
    let millis = now.subsec_millis();
    let seq = TMP_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let stamp = format_utc_timestamp(epoch_secs);

    // Fixed-width UTC + millis + monotonic seq guarantees collision-free names AND strictly
    // monotonic lexicographical descending sort order during same-second rotation.
    let final_path = backup_dir.join(format!(
        "{}{}-{:03}-{:06}{}",
        BACKUP_PREFIX, stamp, millis, seq, BACKUP_SUFFIX
    ));

    let tmp_path = backup_dir.join(format!(
        ".spica-tmp-{}-{}-{}.tar.gz.tmp",
        std::process::id(),
        epoch_secs,
        seq
    ));

    let compression_level = cfg.compression_level.clamp(1, 9);
    let mut files_archived = 0usize;
    let mut worlds_included = Vec::with_capacity(existing_worlds.len());

    let write_res: Result<(), String> = (|| {
        let tmp_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| {
                format!(
                    "Gecici yedek dosyasi olusturulamadi ({}): {}",
                    tmp_path.display(),
                    e
                )
            })?;
        let buffered = BufWriter::with_capacity(256 * 1024, tmp_file);
        let encoder = GzEncoder::new(buffered, Compression::new(compression_level));
        let mut tar_builder = tar::Builder::new(encoder);

        for (world_name, world_path) in &existing_worlds {
            if cancel_gate.is_cancelled() {
                return Err(BACKUP_CANCELLED_ERR.to_string());
            }
            worlds_included.push(world_name.clone());
            for entry_res in WalkDir::new(world_path).follow_links(false) {
                if cancel_gate.is_cancelled() {
                    return Err(BACKUP_CANCELLED_ERR.to_string());
                }
                let entry = entry_res.map_err(|e| {
                    format!(
                        "Dunya dizini taranirken okuma hatasi ({}): {}",
                        world_path.display(),
                        e
                    )
                })?;
                let entry_path = entry.path();
                if is_session_lock(entry_path) {
                    continue;
                }
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry_path.strip_prefix(world_path).map_err(|e| {
                    format!(
                        "Goreli dosya yolu hesaplanamadi ({}): {}",
                        entry_path.display(),
                        e
                    )
                })?;
                let archive_rel = Path::new(world_name).join(rel);

                let src_file = File::open(entry_path).map_err(|e| {
                    format!(
                        "Dosya acilamadi ({}): {}",
                        entry_path.display(),
                        e
                    )
                })?;
                let meta = src_file.metadata().map_err(|e| {
                    format!(
                        "Dosya metadatasi okunamadi ({}): {}",
                        entry_path.display(),
                        e
                    )
                })?;
                let mut header = tar::Header::new_gnu();
                header.set_metadata(&meta);
                header.set_cksum();

                let mut reader = CancellableFileReader {
                    inner: src_file,
                    cancel_gate,
                };
                if let Err(err) = tar_builder.append_data(&mut header, &archive_rel, &mut reader) {
                    if cancel_gate.is_cancelled() || err.kind() == io::ErrorKind::Interrupted {
                        return Err(BACKUP_CANCELLED_ERR.to_string());
                    }
                    return Err(format!(
                        "Dosya arsivlenemedi ({}): {}",
                        entry_path.display(),
                        err
                    ));
                }
                files_archived += 1;
            }
        }

        if cancel_gate.is_cancelled() {
            return Err(BACKUP_CANCELLED_ERR.to_string());
        }

        let encoder = tar_builder
            .into_inner()
            .map_err(|e| format!("TAR akisi sonlandirilamadi: {}", e))?;
        let mut buffered = encoder
            .finish()
            .map_err(|e| format!("Gzip sikistirma sonlandirilamadi: {}", e))?;
        buffered
            .flush()
            .map_err(|e| format!("Yedek tamponu diske yazilamadi: {}", e))?;
        let raw_file = buffered
            .into_inner()
            .map_err(|e| format!("Yedek dosya tutamaci alinamadi: {}", e))?;
        raw_file
            .sync_all()
            .map_err(|e| format!("Yedek dosyasi diske esitlenemedi (fsync): {}", e))?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if cancel_gate.is_cancelled() {
        let _ = fs::remove_file(&tmp_path);
        return Err(BACKUP_CANCELLED_ERR.to_string());
    }

    // Verify full archive readability & exact entry count BEFORE publishing and BEFORE rotating
    if let Err(verify_err) = verify_tar_gz_integrity(&tmp_path, files_archived, cancel_gate) {
        let _ = fs::remove_file(&tmp_path);
        return Err(verify_err);
    }

    // Atomic TOCTOU-free publish gate: if supervisor already cancelled, abort and delete .tmp
    if !cancel_gate.try_begin_publish() {
        let _ = fs::remove_file(&tmp_path);
        return Err(BACKUP_CANCELLED_ERR.to_string());
    }

    fs::rename(&tmp_path, &final_path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        format!(
            "Gecici yedek nihai ada tasinamadi ({} -> {}): {}",
            tmp_path.display(),
            final_path.display(),
            e
        )
    })?;

    let size_bytes = fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
    let rotated_deleted = rotate_old_backups(&backup_dir, cfg.keep_last);
    let duration_ms = started.elapsed().as_millis() as u64;

    Ok(BackupOutcome {
        archive_path: final_path,
        size_bytes,
        files_archived,
        duration_ms,
        worlds_included,
        rotated_deleted,
    })
}

/// Keeps at most `keep_last` newest `spica-backup-*.tar.gz` archives in `backup_dir`
/// and deletes older ones. Never touches unrelated files.
pub fn rotate_old_backups(backup_dir: &Path, keep_last: usize) -> usize {
    let keep = keep_last.max(1);
    let Ok(entries) = fs::read_dir(backup_dir) else {
        return 0;
    };

    let mut archives: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with(BACKUP_PREFIX) && name.ends_with(BACKUP_SUFFIX) {
            archives.push((name.to_string(), path));
        }
    }

    // Lexicographical descending order places newest YYYYMMDD-HHMMSS-mmm-SSSSSS archives first
    archives.sort_by(|a, b| b.0.cmp(&a.0));

    let mut deleted = 0usize;
    for (_, old_path) in archives.into_iter().skip(keep) {
        if fs::remove_file(old_path).is_ok() {
            deleted += 1;
        }
    }
    deleted
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(1);

    fn make_temp_dir(label: &str) -> PathBuf {
        let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "spica-backup-test-{}-{}-{}",
            label,
            std::process::id(),
            seq
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn test_backup_creates_tar_gz_and_skips_session_lock() {
        let work_dir = make_temp_dir("archive");
        let world_dir = work_dir.join("world");
        let region_dir = world_dir.join("region");
        fs::create_dir_all(&region_dir).expect("create world/region");

        fs::write(world_dir.join("level.dat"), b"fake-level-nbt-payload").expect("write level.dat");
        fs::write(region_dir.join("r.0.0.mca"), vec![0x5Au8; 4096]).expect("write region");
        fs::write(world_dir.join("session.lock"), b"LOCKED").expect("write session.lock");

        let cfg = BackupConfig {
            enabled: true,
            interval_minutes: 60,
            keep_last: 3,
            backup_dir: "backups".to_string(),
            worlds: vec!["world".to_string(), "world_nether".to_string()],
            require_all_worlds: false,
            compression_level: 3,
            flush_wait_ms: 500,
        };

        let outcome = create_world_backup(&work_dir, &cfg).expect("backup should succeed");
        assert!(outcome.archive_path.exists());
        assert_eq!(outcome.files_archived, 2);
        assert_eq!(outcome.worlds_included, vec!["world".to_string()]);
        assert!(outcome.size_bytes > 0);

        let file = File::open(&outcome.archive_path).expect("open archive");
        let gz = GzDecoder::new(file);
        let mut archive = tar::Archive::new(gz);
        let mut names = Vec::new();
        for entry in archive.entries().expect("read entries") {
            let e = entry.expect("valid entry");
            let p = e.path().expect("entry path").to_string_lossy().replace('\\', "/");
            names.push(p);
        }
        assert!(names.contains(&"world/level.dat".to_string()));
        assert!(names.contains(&"world/region/r.0.0.mca".to_string()));
        assert!(!names.iter().any(|n| n.contains("session.lock")));

        let _ = fs::remove_dir_all(&work_dir);
    }

    #[test]
    fn test_backup_rotation_keeps_last_n() {
        let backup_dir = make_temp_dir("rotate");
        let names = [
            "spica-backup-20260101-010000-001-000001.tar.gz",
            "spica-backup-20260101-020000-001-000002.tar.gz",
            "spica-backup-20260101-030000-001-000003.tar.gz",
            "spica-backup-20260101-040000-001-000004.tar.gz",
            "spica-backup-20260101-050000-001-000005.tar.gz",
        ];
        for n in &names {
            fs::write(backup_dir.join(n), b"dummy").expect("write dummy backup");
        }
        fs::write(backup_dir.join("notes.txt"), b"keep-me").expect("write notes");

        let deleted = rotate_old_backups(&backup_dir, 2);
        assert_eq!(deleted, 3);
        assert!(backup_dir
            .join("spica-backup-20260101-050000-001-000005.tar.gz")
            .exists());
        assert!(backup_dir
            .join("spica-backup-20260101-040000-001-000004.tar.gz")
            .exists());
        assert!(!backup_dir
            .join("spica-backup-20260101-030000-001-000003.tar.gz")
            .exists());
        assert!(backup_dir.join("notes.txt").exists());

        let _ = fs::remove_dir_all(&backup_dir);
    }

    #[test]
    fn test_same_second_backup_rotation_keeps_newest() {
        let work_dir = make_temp_dir("same-second");
        let world_dir = work_dir.join("world");
        fs::create_dir_all(&world_dir).expect("create world");
        fs::write(world_dir.join("level.dat"), b"v1").expect("write v1");

        let cfg = BackupConfig {
            enabled: true,
            interval_minutes: 60,
            keep_last: 1,
            backup_dir: "backups".to_string(),
            worlds: vec!["world".to_string()],
            require_all_worlds: false,
            compression_level: 1,
            flush_wait_ms: 500,
        };

        let first = create_world_backup(&work_dir, &cfg).expect("first backup");
        fs::write(world_dir.join("level.dat"), b"v2").expect("write v2");
        let second = create_world_backup(&work_dir, &cfg).expect("second backup");

        assert_ne!(first.archive_path, second.archive_path);
        assert!(
            !first.archive_path.exists(),
            "Older same-second backup must be rotated out"
        );
        assert!(
            second.archive_path.exists(),
            "Newer same-second backup must be preserved"
        );

        let _ = fs::remove_dir_all(&work_dir);
    }

    #[test]
    fn test_backup_cancellation_cleans_tmp_and_preserves_old_backups() {
        let work_dir = make_temp_dir("cancel");
        let world_dir = work_dir.join("world");
        let backup_dir = work_dir.join("backups");
        fs::create_dir_all(&world_dir).expect("create world");
        fs::create_dir_all(&backup_dir).expect("create backups");
        fs::write(world_dir.join("level.dat"), b"data").expect("write level");
        let existing = backup_dir.join("spica-backup-20260101-010000-000-000001.tar.gz");
        fs::write(&existing, b"good-old-backup").expect("write old backup");

        let cfg = BackupConfig {
            enabled: true,
            interval_minutes: 60,
            keep_last: 1,
            backup_dir: "backups".to_string(),
            worlds: vec!["world".to_string()],
            require_all_worlds: false,
            compression_level: 3,
            flush_wait_ms: 500,
        };

        let gate = BackupCancelGate::new();
        assert!(gate.request_cancel());
        assert!(!gate.try_begin_publish());
        let res = create_world_backup_cancellable(&work_dir, &cfg, &gate);
        assert!(res.is_err());
        assert!(
            existing.exists(),
            "Cancelled backup must never rotate/delete old backups"
        );
        let has_tmp = fs::read_dir(&backup_dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(!has_tmp, "Cancelled backup must leave zero .tmp files behind");

        let _ = fs::remove_dir_all(&work_dir);
    }

    #[test]
    fn test_backup_rejects_backup_dir_inside_world_dir_and_missing_required_worlds() {
        let work_dir = make_temp_dir("nested");
        let world_dir = work_dir.join("world");
        fs::create_dir_all(&world_dir).expect("create world");
        fs::write(world_dir.join("level.dat"), b"data").expect("write level");

        let nested_cfg = BackupConfig {
            enabled: true,
            interval_minutes: 60,
            keep_last: 3,
            backup_dir: "world/backups".to_string(),
            worlds: vec!["world".to_string()],
            require_all_worlds: false,
            compression_level: 3,
            flush_wait_ms: 500,
        };
        let res = create_world_backup(&work_dir, &nested_cfg);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("backup_dir"));

        let strict_cfg = BackupConfig {
            enabled: true,
            interval_minutes: 60,
            keep_last: 3,
            backup_dir: "backups".to_string(),
            worlds: vec!["world".to_string(), "world_nether".to_string()],
            require_all_worlds: true,
            compression_level: 3,
            flush_wait_ms: 500,
        };
        let res_strict = create_world_backup(&work_dir, &strict_cfg);
        assert!(res_strict.is_err());
        assert!(res_strict.unwrap_err().contains("require_all_worlds"));

        let _ = fs::remove_dir_all(&work_dir);
    }
}
