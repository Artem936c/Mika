//! Saving a backup and restoring from one: the commands behind the block
//! "transfer to another PC" of the settings and of the first-run screen
//! (decision 0034).
//!
//! Nothing here is a tool: only the user's hand starts these commands. No
//! path comes from the webview either. A file is named by the Windows dialog
//! this module opens, and the restore works on the file that was picked and
//! previewed: its SHA-256 is compared, and the file is locked against
//! writing while it is read. A password is used once and dropped; between the preview and the restore
//! only the key made from it stays in memory, for fifteen minutes at most.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use mika_core::backup::{
    self, Access, BackupError, Category, ExportInfo, ExportReport, Header, Limits, Preview,
    SecretRef, Stores, SwitchedOff,
};
use mika_core::config::Config;
use mika_core::tools::RiskLevel;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tauri::AppHandle;

use crate::app::Ctx;
use crate::file_dialog::{pick_open, pick_save};
use crate::strings::tr;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// One export or restore at a time.
static BUSY: AtomicBool = AtomicBool::new(false);
/// The file picked for a restore.
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
/// The last file saved, for "show in folder".
static LAST_EXPORT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// How long a picked file, and the key that opens it, are kept.
const PENDING_TTL: Duration = Duration::from_secs(15 * 60);
/// The copy of the data of this PC made before a restore.
const SAFETY_NAME: &str = "before-restore.mikabackup";
const FILE_PATTERN: &str = "*.mikabackup";

/// Held while an export or a restore runs.
struct Busy;

impl Busy {
    fn take() -> Result<Busy, String> {
        BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Busy)
            .map_err(|_| tr("error.backup_busy"))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// What a file looked like when it was picked. Checked again before every
/// read: a file swapped after the preview is not restored.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    prefix_sha256: [u8; 32],
}

struct Pending {
    path: PathBuf,
    stamp: Stamp,
    header: Header,
    /// What the preview read: the right to open the body, and the SHA-256
    /// of the whole file. `access` is the only place a key made from the
    /// password lives. Set by the preview.
    seen: Option<(Access, [u8; 32])>,
    opened: Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum PendingState {
    Fresh,
    Expired,
    Changed,
}

fn pending_state(age: Duration, picked: &Stamp, now: &Stamp) -> PendingState {
    if age > PENDING_TTL {
        PendingState::Expired
    } else if picked != now {
        PendingState::Changed
    } else {
        PendingState::Fresh
    }
}

/// Forgets the picked file, and with it the key, when its time is up, even
/// if no command comes to notice.
fn expire_later(opened: Instant) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(PENDING_TTL.saturating_sub(opened.elapsed())).await;
        let mut slot = PENDING.lock();
        if slot.as_ref().is_some_and(|p| p.opened == opened) {
            *slot = None;
        }
    });
}

/// Text for the user. Nothing but the answer of the system about a file or
/// the database is added to it.
fn error_text(e: &BackupError, writing: bool) -> String {
    match e {
        BackupError::PasswordShort(min) => {
            tr("error.backup_password_short").replace("{min}", &min.to_string())
        }
        BackupError::PasswordLong(max) => {
            tr("error.backup_password_long").replace("{max}", &max.to_string())
        }
        BackupError::PasswordNeeded => tr("error.backup_password_needed"),
        BackupError::WrongPassword => tr("error.backup_password_wrong"),
        BackupError::NotBackup => tr("error.backup_not_backup"),
        BackupError::Newer(format) => {
            tr("error.backup_newer").replace("{format}", &format.to_string())
        }
        BackupError::Unsupported => tr("error.backup_unsupported"),
        BackupError::Damaged => tr("error.backup_damaged"),
        BackupError::TooBig => tr("error.backup_too_big"),
        BackupError::NothingSelected => tr("error.backup_nothing"),
        BackupError::Io(e) if writing => tr("error.backup_write").replace("{why}", &e.to_string()),
        BackupError::Io(e) => tr("error.backup_read").replace("{why}", &e.to_string()),
        BackupError::Db(e) => tr("error.backup_db").replace("{why}", &e.to_string()),
        BackupError::Secrets(_) => tr("error.backup_keys"),
    }
}

/// `mika-backup-2026-11-14.mikabackup`.
pub fn default_name(date: chrono::NaiveDate) -> String {
    format!(
        "mika-backup-{}.{}",
        date.format("%Y-%m-%d"),
        backup::EXTENSION
    )
}

/// The ticked categories. `None` for an empty list or an id nobody knows.
fn parse_categories(ids: &[String]) -> Option<BTreeSet<Category>> {
    let selected = ids
        .iter()
        .map(|id| Category::parse(id))
        .collect::<Option<BTreeSet<Category>>>()?;
    (!selected.is_empty()).then_some(selected)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `<path>.part`, in the same folder: a rename then never crosses a disk.
fn part_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

fn safety_path() -> PathBuf {
    mika_core::paths::backups_dir().join(SAFETY_NAME)
}

/// Writes a whole backup under the given name and makes sure it is on disk.
/// With a password the file is ciphertext from its first chunk.
fn write_part(
    stores: &Stores<'_>,
    info: &ExportInfo<'_>,
    part: &Path,
    password: Option<&str>,
) -> Result<ExportReport, BackupError> {
    let mut out = BufWriter::new(File::create(part)?);
    let report = backup::export(stores, info, password, &mut out)?;
    out.flush()?;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    Ok(report)
}

/// Writes a backup next to its final name and renames it at the end. On any
/// error the unfinished file is removed and an older file of that name stays.
fn write_file(
    stores: &Stores<'_>,
    info: &ExportInfo<'_>,
    path: &Path,
    password: Option<&str>,
) -> Result<ExportReport, BackupError> {
    let part = part_path(path);
    let written = write_part(stores, info, &part, password).and_then(|report| {
        std::fs::rename(&part, path)?;
        Ok(report)
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    written
}

/// What an export of this PC needs besides the stores: the settings as they
/// are now, the time and the name of the PC.
struct Here {
    config: Config,
    created_at: String,
    device: String,
}

impl Here {
    fn now(ctx: &Ctx) -> Here {
        Here {
            config: ctx.config.lock().clone(),
            created_at: mika_core::storage::now(),
            device: std::env::var("COMPUTERNAME").unwrap_or_default(),
        }
    }

    fn stores<'a>(&'a self, ctx: &'a Ctx) -> Stores<'a> {
        Stores {
            config: &self.config,
            db: &ctx.db,
            secrets: ctx.secrets.as_ref(),
        }
    }

    fn info(&self) -> ExportInfo<'_> {
        ExportInfo {
            app_version: env!("CARGO_PKG_VERSION"),
            created_at: &self.created_at,
            device: &self.device,
        }
    }
}

/// Opens a file for reading so that nobody can write to it, rename it or
/// put another file in its place while it is open. What was checked is then
/// what is read.
fn open_locked(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
}

/// SHA-256 of a whole file, whatever the place it was read to.
fn digest_of(file: &mut File) -> std::io::Result<[u8; 32]> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().into())
}

/// Opens a backup file, reads its header and notes what the file looks like.
/// The reader then stands at the body. The file is locked while the reader
/// lives.
fn open_backup(path: &Path) -> Result<(BufReader<File>, Header, Stamp), BackupError> {
    read_backup(open_locked(path)?)
}

fn read_backup(mut file: File) -> Result<(BufReader<File>, Header, Stamp), BackupError> {
    let meta = file.metadata()?;
    if meta.len() > Limits::default().file {
        return Err(BackupError::TooBig);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let header = backup::read_header(&mut reader)?;
    let stamp = Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        prefix_sha256: header.prefix_sha256,
    };
    Ok((reader, header, stamp))
}

/// Opens the picked file again and makes sure it still is the file that was
/// picked. Returns the reader at the body and the SHA-256 of the whole file;
/// the file stays locked while the reader lives, so the bytes that were
/// hashed are the bytes that are read. `Err` says whether its time is up or
/// the file is another one now.
fn reopen(
    path: &Path,
    picked: &Stamp,
    opened: Instant,
) -> Result<(BufReader<File>, [u8; 32]), PendingState> {
    if opened.elapsed() > PENDING_TTL {
        return Err(PendingState::Expired);
    }
    // A file that does not open any more is not the one that was shown.
    let changed = |_| PendingState::Changed;
    let mut file = open_locked(path).map_err(changed)?;
    // Not worth hashing a file of another size.
    if file.metadata().map_err(changed)?.len() != picked.len {
        return Err(PendingState::Changed);
    }
    let digest = digest_of(&mut file).map_err(changed)?;
    let (reader, _, now) = read_backup(file).map_err(|_| PendingState::Changed)?;
    match pending_state(opened.elapsed(), picked, &now) {
        PendingState::Fresh => Ok((reader, digest)),
        other => Err(other),
    }
}

/// Whether the file read now is, byte for byte, the file the preview read.
/// The header alone would not tell: the body of a copy without a password
/// is covered by nothing else.
fn same_file(previewed: &[u8; 32], now: &[u8; 32]) -> bool {
    previewed == now
}

fn stale_text(state: &PendingState) -> String {
    match state {
        PendingState::Changed => tr("error.backup_changed"),
        PendingState::Fresh | PendingState::Expired => tr("error.backup_expired"),
    }
}

fn owner_of(window: &tauri::WebviewWindow) -> isize {
    window.hwnd().map(|h| h.0 as isize).unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ExportView {
    path: String,
    bytes: u64,
    encrypted: bool,
    keys: usize,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BackupFileView {
    path: String,
    name: String,
    bytes: u64,
    created_at: String,
    device: String,
    app_version: String,
    encrypted: bool,
    keys: u32,
    /// The file was made by a newer version of the app.
    newer_app: bool,
}

fn file_view(path: &Path, header: &Header, bytes: u64) -> BackupFileView {
    BackupFileView {
        path: path.display().to_string(),
        name: file_name(path),
        bytes,
        created_at: header.created_at.clone(),
        device: header.device.clone(),
        app_version: header.app_version.clone(),
        encrypted: header.encrypted,
        keys: header.secrets,
        newer_app: crate::update::compare(&header.app_version, env!("CARGO_PKG_VERSION"))
            == std::cmp::Ordering::Greater,
    }
}

#[derive(serde::Serialize)]
pub struct PreviewView {
    #[serde(flatten)]
    preview: Preview,
    file: BackupFileView,
    /// Where the data of this PC is kept before it is replaced.
    safety_copy: String,
}

#[derive(serde::Serialize)]
struct RestoredView {
    id: Category,
    count: u64,
}

#[derive(serde::Serialize)]
pub struct RestoreView {
    restored: Vec<RestoredView>,
    keys_written: usize,
    keys_failed: Vec<SecretRef>,
    switched_off: Vec<SwitchedOff>,
    docs_folders: Vec<String>,
    /// Scenarios by event left out: the saved command they name is neither
    /// restored nor on this PC.
    scenarios_without_command: u64,
    restart_needed: bool,
    /// Why the restored settings were not saved, when they were not.
    config_error: Option<String>,
    safety_copy: String,
}

/// Saves a backup where the user says. `None` when the dialog was closed.
#[tauri::command]
pub async fn backup_export(
    window: tauri::WebviewWindow,
    ctx: CtxState<'_>,
    protect: bool,
    password: String,
    repeat: String,
) -> Result<Option<ExportView>, String> {
    // Everything about the password is settled before a dialog opens.
    let password = if protect {
        backup::check_password(&password).map_err(|e| error_text(&e, true))?;
        if password != repeat {
            return Err(tr("error.backup_password_repeat"));
        }
        Some(password)
    } else {
        None
    };
    let busy = Busy::take()?;
    let owner = owner_of(&window);
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = busy;
        export_blocking(&ctx, owner, password.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn export_blocking(
    ctx: &Ctx,
    owner: isize,
    password: Option<&str>,
) -> Result<Option<ExportView>, String> {
    let today = chrono::Local::now().date_naive();
    let Some(path) = pick_save(
        owner,
        &tr("backup.dialog_save"),
        (&tr("backup.dialog_filter"), FILE_PATTERN),
        &default_name(today),
        backup::EXTENSION,
    )?
    else {
        return Ok(None);
    };
    let here = Here::now(ctx);
    let report = write_file(&here.stores(ctx), &here.info(), &path, password)
        .map_err(|e| error_text(&e, true))?;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    ctx.audit.record(
        "backup_export",
        &file_name(&path),
        RiskLevel::High,
        "allow_once",
        "button",
    );
    log::info!(
        "backup saved: {bytes} bytes, encrypted: {}, keys: {}",
        report.encrypted,
        report.secrets
    );
    let view = ExportView {
        path: path.display().to_string(),
        bytes,
        encrypted: report.encrypted,
        keys: report.secrets,
    };
    *LAST_EXPORT.lock() = Some(path);
    Ok(Some(view))
}

/// Shows the last saved backup in its folder. The path is the one this
/// module wrote, never one from the webview.
#[tauri::command]
pub fn backup_reveal() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let Some(path) = LAST_EXPORT.lock().clone() else {
        return Ok(());
    };
    if let Err(e) = std::fs::metadata(&path) {
        return Err(tr("error.backup_read").replace("{why}", &e.to_string()));
    }
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Lets the user pick a backup file and says what it is, before any
/// password. `None` when the dialog was closed.
#[tauri::command]
pub async fn backup_import_pick(
    window: tauri::WebviewWindow,
) -> Result<Option<BackupFileView>, String> {
    let owner = owner_of(&window);
    tauri::async_runtime::spawn_blocking(move || {
        // A new pick forgets the old file and its key.
        *PENDING.lock() = None;
        let Some(path) = pick_open(
            owner,
            &tr("backup.dialog_open"),
            (&tr("backup.dialog_filter"), FILE_PATTERN),
        )?
        else {
            return Ok(None);
        };
        let (_, header, stamp) = open_backup(&path).map_err(|e| error_text(&e, false))?;
        let view = file_view(&path, &header, stamp.len);
        *PENDING.lock() = Some(Pending {
            path,
            stamp,
            header,
            seen: None,
            opened: Instant::now(),
        });
        Ok(Some(view))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Reads the picked file whole and says what a restore would do. Changes
/// nothing. A wrong password leaves the file picked, so it can be typed again.
#[tauri::command]
pub async fn backup_import_preview(
    ctx: CtxState<'_>,
    password: Option<String>,
) -> Result<PreviewView, String> {
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || preview_blocking(&ctx, password.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

fn preview_blocking(ctx: &Ctx, password: Option<&str>) -> Result<PreviewView, String> {
    // The slow work runs without the lock: a cancel is not kept waiting.
    let (path, stamp, header, opened) = {
        let slot = PENDING.lock();
        let p = slot.as_ref().ok_or_else(|| tr("error.backup_expired"))?;
        (p.path.clone(), p.stamp.clone(), p.header.clone(), p.opened)
    };
    let (mut body, digest) = reopen(&path, &stamp, opened).map_err(|state| {
        *PENDING.lock() = None;
        stale_text(&state)
    })?;
    let access = backup::unlock(&header, password).map_err(|e| error_text(&e, false))?;
    let here = Here::now(ctx);
    let preview = backup::preview(
        &mut body,
        &header,
        &access,
        &here.stores(ctx),
        &Limits::default(),
    )
    .map_err(|e| error_text(&e, false))?;
    {
        // Still the same pick? A cancel or a new pick in between wins.
        let mut slot = PENDING.lock();
        match slot.as_mut() {
            Some(p) if p.opened == opened => p.seen = Some((access, digest)),
            _ => return Err(tr("error.backup_expired")),
        }
    }
    expire_later(opened);
    Ok(PreviewView {
        preview,
        file: file_view(&path, &header, stamp.len),
        safety_copy: safety_path().display().to_string(),
    })
}

/// Replaces the ticked categories by those of the picked file.
#[tauri::command]
pub async fn backup_import_apply(
    app: AppHandle,
    ctx: CtxState<'_>,
    categories: Vec<String>,
) -> Result<RestoreView, String> {
    let busy = Busy::take()?;
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = busy;
        apply_blocking(&app, &ctx, &categories)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Why a restore did not happen, and whether the picked file stays picked.
struct Refusal {
    text: String,
    keep: bool,
}

/// Whether a file whose restore was refused is kept for another try. Never
/// past its time: the key that opens it would outlive the fifteen minutes.
fn stays_picked(keep: bool, age: Duration) -> bool {
    keep && age <= PENDING_TTL
}

fn apply_blocking(
    app: &AppHandle,
    ctx: &Arc<Ctx>,
    categories: &[String],
) -> Result<RestoreView, String> {
    let selected = parse_categories(categories).ok_or_else(|| tr("error.backup_nothing"))?;
    let pending = PENDING
        .lock()
        .take()
        .ok_or_else(|| tr("error.backup_expired"))?;
    match restore_pending(app, ctx, &pending, &selected) {
        Ok(view) => Ok(view),
        Err(refusal) => {
            // Nothing was changed: the user may fix the cause and press
            // again, while the time of the pick lasts. The timer that
            // forgets the key found the slot empty if it woke meanwhile, so
            // it is set again.
            if stays_picked(refusal.keep, pending.opened.elapsed()) {
                let opened = pending.opened;
                let mut slot = PENDING.lock();
                if slot.is_none() {
                    *slot = Some(pending);
                    drop(slot);
                    expire_later(opened);
                }
            }
            Err(refusal.text)
        }
    }
}

fn restore_pending(
    app: &AppHandle,
    ctx: &Arc<Ctx>,
    pending: &Pending,
    selected: &BTreeSet<Category>,
) -> Result<RestoreView, Refusal> {
    let gone = |state: PendingState| Refusal {
        text: stale_text(&state),
        keep: false,
    };
    // Only a file that went through the preview is restored.
    let (access, previewed) = pending
        .seen
        .as_ref()
        .ok_or_else(|| gone(PendingState::Expired))?;
    let (mut body, digest) = reopen(&pending.path, &pending.stamp, pending.opened).map_err(gone)?;
    // The rows that go in are the rows that were on the screen.
    if !same_file(previewed, &digest) {
        return Err(gone(PendingState::Changed));
    }

    // The data of this PC first, by the same code as an ordinary backup,
    // without keys. If it cannot be kept, nothing is restored.
    let here = Here::now(ctx);
    let safety = safety_path();
    let part = part_path(&safety);
    let kept = std::fs::create_dir_all(mika_core::paths::backups_dir())
        .map_err(BackupError::from)
        .and_then(|()| write_part(&here.stores(ctx), &here.info(), &part, None));
    if let Err(e) = kept {
        let _ = std::fs::remove_file(&part);
        let why = match &e {
            BackupError::Io(io) => io.to_string(),
            other => error_text(other, true),
        };
        return Err(Refusal {
            text: tr("error.backup_safety").replace("{why}", &why),
            keep: true,
        });
    }

    ctx.session.stop();
    let report = backup::restore(
        &mut body,
        &pending.header,
        access,
        &here.stores(ctx),
        selected,
        &Limits::default(),
    )
    .map_err(|e| {
        let _ = std::fs::remove_file(&part);
        Refusal {
            text: error_text(&e, false),
            keep: true,
        }
    })?;
    // Only now the older copy is replaced: when the file being restored is
    // that copy itself, it was still whole while it was read. The reader
    // goes first: it keeps its file from being replaced.
    drop(body);
    if let Err(e) = std::fs::rename(&part, &safety) {
        log::warn!("the copy of the previous data stays under its temporary name: {e}");
    }

    // The scenario table was written past its store: the watcher reads the
    // scenarios again, and finds every one of them switched off.
    if selected.contains(&Category::Scenarios) {
        ctx.tool_env().shared.triggers.reloaded();
    }
    let config_error = report
        .config
        .clone()
        .and_then(|new| install_config(ctx, app, new).err());
    ctx.audit.record(
        "backup_restore",
        &file_name(&pending.path),
        RiskLevel::High,
        "allow_once",
        "button",
    );
    log::info!(
        "backup restored: {} categories, keys written: {}, keys refused: {}",
        report.restored.len(),
        report.keys_written,
        report.keys_failed.len()
    );
    Ok(RestoreView {
        restored: report
            .restored
            .iter()
            .map(|(id, count)| RestoredView {
                id: *id,
                count: *count,
            })
            .collect(),
        keys_written: report.keys_written,
        keys_failed: report.keys_failed,
        switched_off: report.switched_off,
        docs_folders: report.docs_folders,
        scenarios_without_command: report.scenarios_without_command,
        restart_needed: report.restart_needed,
        config_error,
        safety_copy: safety.display().to_string(),
    })
}

/// Puts restored settings in force: in memory, on disk and in everything
/// that mirrors them, so what landed switched off is off at once. The
/// window, the avatar placement, the tray title and the conversation the
/// agent holds in memory follow at the restart.
///
/// What reads the settings in memory each time follows by itself: the
/// scenarios (`[events]`), the evening summary, the voice switches of the
/// Telegram bridge. Saving them re-registers the hotkeys, so dictation and
/// the screen question, which land off, lose their keys at once. The cost
/// tracker and the budget pause follow through `reload_provider`.
fn install_config(ctx: &Arc<Ctx>, app: &AppHandle, new: Config) -> Result<(), String> {
    *ctx.config.lock() = new.clone();
    let saved = ctx.save_config();
    ctx.web_enabled
        .store(new.tools.web_enabled, Ordering::Relaxed);
    ctx.screen_enabled
        .store(new.privacy.screen_enabled, Ordering::Relaxed);
    if !new.privacy.screen_enabled || !new.screen_ask.enabled {
        // A picture taken for a question does not outlive its permission.
        ctx.screen_ask_cancel();
    }
    *ctx.news_feed.write() = new.tools.news_feed.clone();
    *ctx.ffmpeg_path.write() = new.tools.ffmpeg_path.clone();
    *ctx.home_city.write() = new.island.city.clone();
    let shared = &ctx.tool_env().shared;
    shared
        .app_time_on
        .store(new.tracking.app_time, Ordering::Relaxed);
    shared
        .clipboard_on
        .store(new.tracking.clipboard, Ordering::Relaxed);
    if !new.tracking.clipboard {
        shared.clips.clear();
    }
    if !new.notifications.enabled {
        shared.watches.stop_all();
    }
    // The deep search reads its settings from this live copy.
    *shared.research.write() = new.research.clone();
    ctx.cancel_listening();
    ctx.set_input_device(&new.voice.input_device);
    ctx.restart_wake();
    ctx.restart_telegram();
    // The automatic voice follows the speech language of the restored settings.
    crate::lang::pick_voice(ctx);
    ctx.speaker.set(new.voice.speak_replies);
    ctx.speaker.set_brief(new.voice.brief_long);
    ctx.apply_voice_engine();
    ctx.reload_provider();
    ctx.refresh_prompt();
    ctx.rebuild_registry();
    // Every server of restored settings is disabled: this stops the old processes.
    let mcp = ctx.clone();
    tauri::async_runtime::spawn(async move { mcp.reconnect_mcp().await });
    crate::bridge::settings_changed(app);
    saved
}

/// Forgets the picked file and the key that opens it.
#[tauri::command]
pub fn backup_import_cancel() {
    *PENDING.lock() = None;
}

/// Starts the app again once this process has ended and the name of the
/// single instance is free. The exit may take minutes when a screen recording
/// or a background job is being stopped and saved, so the script waits for
/// the process itself, not for a fixed time (`platform::run_after_exit`).
#[tauri::command]
pub fn backup_restart(app: AppHandle, ctx: CtxState<'_>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    ctx.session.stop();
    // The place of the ordinary window is kept in memory while it moves.
    let _ = ctx.save_config();
    crate::platform::run_after_exit(&format!("start \"\" \"{}\"", exe.display()))
        .map_err(|e| e.to_string())?;
    log::info!("restarting after a restore");
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::secrets::{MemorySecretStore, Secret, SecretError, SecretStore};
    use mika_core::storage::Db;

    const INFO: ExportInfo<'static> = ExportInfo {
        app_version: "0.3.3",
        created_at: "2026-11-14T15:30:00+05:00",
        device: "TEST-PC",
    };

    fn stamp(len: u64, modified_s: u64, hash: u8) -> Stamp {
        Stamp {
            len,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(modified_s)),
            prefix_sha256: [hash; 32],
        }
    }

    #[test]
    fn default_name_carries_the_date_and_the_extension() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        assert_eq!(default_name(date), "mika-backup-2026-11-14.mikabackup");
        let date = chrono::NaiveDate::from_ymd_opt(2027, 1, 5).unwrap();
        assert_eq!(default_name(date), "mika-backup-2027-01-05.mikabackup");
        assert!(SAFETY_NAME.ends_with(&format!(".{}", backup::EXTENSION)));
        assert_eq!(FILE_PATTERN, format!("*.{}", backup::EXTENSION));
    }

    #[test]
    fn every_error_has_a_text_of_its_own() {
        let all = || {
            vec![
                BackupError::PasswordShort(10),
                BackupError::PasswordLong(256),
                BackupError::PasswordNeeded,
                BackupError::WrongPassword,
                BackupError::NotBackup,
                BackupError::Newer(2),
                BackupError::Unsupported,
                BackupError::Damaged,
                BackupError::TooBig,
                BackupError::NothingSelected,
                BackupError::Io(std::io::Error::other("disk is full")),
                BackupError::Db(mika_core::storage::rusqlite::Error::InvalidQuery.into()),
                BackupError::Secrets(SecretError::Backend("the store is locked".into())),
            ]
        };
        for writing in [true, false] {
            let texts: Vec<String> = all().iter().map(|e| error_text(e, writing)).collect();
            for (text, e) in texts.iter().zip(all()) {
                assert!(!text.starts_with("error."), "{e:?}: {text}");
                assert!(!text.contains('{'), "{e:?}: {text}");
                assert!(text.chars().any(|c| ('а'..='я').contains(&c)), "{e:?}");
            }
            let mut unique = texts.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), texts.len());
        }
        assert!(error_text(&BackupError::PasswordShort(10), true).contains("10"));
        assert!(error_text(&BackupError::PasswordLong(256), true).contains("256"));
        assert!(error_text(&BackupError::Newer(2), false).contains('2'));
        // Reading and writing are told apart; the system's answer is passed on.
        let io = || BackupError::Io(std::io::Error::other("disk is full"));
        assert_ne!(error_text(&io(), true), error_text(&io(), false));
        assert!(error_text(&io(), true).contains("disk is full"));
        // What the secret store said is not passed on.
        let keys = BackupError::Secrets(SecretError::Backend("the store is locked".into()));
        assert!(!error_text(&keys, true).contains("locked"));
    }

    #[test]
    fn every_text_this_module_asks_for_is_in_the_locale_file() {
        for key in [
            "backup.dialog_save",
            "backup.dialog_open",
            "backup.dialog_filter",
            "error.backup_busy",
            "error.backup_password_short",
            "error.backup_password_long",
            "error.backup_password_repeat",
            "error.backup_password_needed",
            "error.backup_password_wrong",
            "error.backup_not_backup",
            "error.backup_newer",
            "error.backup_unsupported",
            "error.backup_damaged",
            "error.backup_too_big",
            "error.backup_read",
            "error.backup_write",
            "error.backup_keys",
            "error.backup_db",
            "error.backup_safety",
            "error.backup_expired",
            "error.backup_changed",
            "error.backup_nothing",
            "error.backup_dialog",
        ] {
            assert_ne!(tr(key), key, "{key} is missing in ru.json");
        }
        assert!(tr("error.backup_safety").contains("{why}"));
        assert_ne!(
            stale_text(&PendingState::Changed),
            stale_text(&PendingState::Expired)
        );
    }

    #[test]
    fn a_picked_file_is_used_only_while_fresh_and_unchanged() {
        let picked = stamp(1000, 50, 7);
        let soon = Duration::from_secs(60);
        assert_eq!(
            pending_state(soon, &picked, &picked.clone()),
            PendingState::Fresh
        );
        assert_eq!(
            pending_state(PENDING_TTL, &picked, &picked.clone()),
            PendingState::Fresh
        );
        assert_eq!(
            pending_state(
                PENDING_TTL + Duration::from_secs(1),
                &picked,
                &picked.clone()
            ),
            PendingState::Expired
        );
        // Another length, another time, another header.
        for now in [stamp(1001, 50, 7), stamp(1000, 51, 7), stamp(1000, 50, 8)] {
            assert_eq!(pending_state(soon, &picked, &now), PendingState::Changed);
        }
        let mut no_time = picked.clone();
        no_time.modified = None;
        assert_eq!(
            pending_state(soon, &picked, &no_time),
            PendingState::Changed
        );
        // Too old wins over changed: the file is not even looked at.
        assert_eq!(
            pending_state(PENDING_TTL * 2, &picked, &stamp(1, 1, 1)),
            PendingState::Expired
        );
        assert_eq!(PENDING_TTL, Duration::from_secs(900));
    }

    #[test]
    fn a_refused_restore_does_not_keep_the_key_past_its_time() {
        let soon = Duration::from_secs(60);
        assert!(stays_picked(true, soon));
        assert!(stays_picked(true, PENDING_TTL));
        // The restore was refused after the fifteen minutes ran out.
        assert!(!stays_picked(true, PENDING_TTL + Duration::from_secs(1)));
        // The file changed or was never previewed: gone at once.
        assert!(!stays_picked(false, soon));
    }

    #[test]
    fn category_ids_are_parsed_strictly() {
        let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_categories(&ids(&["notes", "settings", "notes"])),
            Some(BTreeSet::from([Category::Settings, Category::Notes]))
        );
        assert_eq!(
            parse_categories(&ids(&["rules_allow", "secrets", "tasks"])),
            Some(BTreeSet::from([
                Category::Tasks,
                Category::RulesAllow,
                Category::Secrets
            ]))
        );
        assert_eq!(parse_categories(&[]), None);
        assert_eq!(parse_categories(&ids(&["notes", "audit"])), None);
        assert_eq!(parse_categories(&ids(&["Notes"])), None);
        assert_eq!(parse_categories(&ids(&[""])), None);
        assert_eq!(parse_categories(&ids(&["notes "])), None);
    }

    /// The stores of a PC that exists only in memory.
    struct TestPc {
        config: Config,
        db: Db,
        secrets: MemorySecretStore,
    }

    impl TestPc {
        fn new() -> TestPc {
            let pc = TestPc {
                config: Config::default(),
                db: Db::open_in_memory().unwrap(),
                secrets: MemorySecretStore::default(),
            };
            pc.db
                .with(|c| {
                    c.execute(
                        "INSERT INTO notes (created_at, title, body) VALUES ('d', 'Заметка', 'текст')",
                        [],
                    )
                })
                .unwrap();
            pc.secrets
                .set("speech.api_key", &Secret::new("sk-test-VALUE"))
                .unwrap();
            pc
        }

        fn stores(&self) -> Stores<'_> {
            Stores {
                config: &self.config,
                db: &self.db,
                secrets: &self.secrets,
            }
        }
    }

    #[test]
    fn a_backup_is_written_next_to_its_name_and_renamed_at_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy.mikabackup");
        let pc = TestPc::new();
        assert_eq!(part_path(&path), dir.path().join("copy.mikabackup.part"));

        let report = write_file(&pc.stores(), &INFO, &path, None).unwrap();
        assert!(!report.encrypted);
        assert!(path.is_file());
        assert!(!part_path(&path).exists());

        // The file reads back as what was picked.
        let (_, header, stamp) = open_backup(&path).unwrap();
        assert_eq!(header.device, "TEST-PC");
        assert_eq!(stamp.len, std::fs::metadata(&path).unwrap().len());
        assert_eq!(stamp.prefix_sha256, header.prefix_sha256);
        let view = file_view(&path, &header, stamp.len);
        assert_eq!(view.name, "copy.mikabackup");
        assert_eq!(view.device, "TEST-PC");
        assert!(!view.encrypted && view.keys == 0 && !view.newer_app);
        assert_eq!(
            serde_json::to_value(&view).unwrap()["created_at"],
            "2026-11-14T15:30:00+05:00"
        );

        // A refused export leaves the older file whole and no unfinished one.
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            write_file(&pc.stores(), &INFO, &path, Some("short")),
            Err(BackupError::PasswordShort(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!part_path(&path).exists());
        // A folder that is not there: nothing appears.
        let nowhere = dir.path().join("missing").join("copy.mikabackup");
        assert!(matches!(
            write_file(&pc.stores(), &INFO, &nowhere, None),
            Err(BackupError::Io(_))
        ));
        assert!(!dir.path().join("missing").exists());
    }

    #[test]
    fn a_file_swapped_after_the_pick_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy.mikabackup");
        let pc = TestPc::new();
        write_file(&pc.stores(), &INFO, &path, None).unwrap();
        let (_, _, picked) = open_backup(&path).unwrap();
        let now = Instant::now();
        assert!(reopen(&path, &picked, now).is_ok());
        // Too old: not even opened.
        let long_ago = now.checked_sub(PENDING_TTL + Duration::from_secs(1));
        if let Some(long_ago) = long_ago {
            assert_eq!(
                reopen(&path, &picked, long_ago).err(),
                Some(PendingState::Expired)
            );
        }

        // Another backup under the same name: another header at least.
        let other = ExportInfo {
            device: "OTHER-PC!",
            ..INFO
        };
        write_file(&pc.stores(), &other, &path, None).unwrap();
        assert_eq!(
            reopen(&path, &picked, now).err(),
            Some(PendingState::Changed)
        );
        // Gone, or not a backup any more.
        std::fs::write(&path, b"something else").unwrap();
        assert_eq!(
            reopen(&path, &picked, now).err(),
            Some(PendingState::Changed)
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            reopen(&path, &picked, now).err(),
            Some(PendingState::Changed)
        );
    }

    #[test]
    fn a_body_changed_after_the_preview_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy.mikabackup");
        let pc = TestPc::new();
        write_file(&pc.stores(), &INFO, &path, None).unwrap();
        let (_, _, picked) = open_backup(&path).unwrap();
        let now = Instant::now();
        let (_, previewed) = reopen(&path, &picked, now).unwrap();
        let (_, again) = reopen(&path, &picked, now).unwrap();
        assert!(same_file(&previewed, &again));

        // The body of a copy without a password is rewritten to the same
        // length and the time of the file is put back: the header, the
        // length and the time say nothing.
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(modified).unwrap();
        drop(file);
        let (_, _, stamp_now) = open_backup(&path).unwrap();
        assert_eq!(stamp_now, picked);

        let (_, changed) = reopen(&path, &picked, now).unwrap();
        assert!(!same_file(&previewed, &changed));
    }

    #[test]
    fn a_backup_being_read_cannot_be_written_or_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy.mikabackup");
        let pc = TestPc::new();
        write_file(&pc.stores(), &INFO, &path, None).unwrap();
        let (_, _, picked) = open_backup(&path).unwrap();
        let (reader, _) = reopen(&path, &picked, Instant::now()).unwrap();

        // While the reader lives: no write, no other file under the name,
        // no removal. Reading by others is fine.
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::write(&path, b"other").is_err());
        let other = dir.path().join("other.mikabackup");
        std::fs::write(&other, b"other").unwrap();
        assert!(std::fs::rename(&other, &path).is_err());
        assert!(std::fs::remove_file(&path).is_err());
        assert!(std::fs::read(&path).is_ok());

        drop(reader);
        assert!(std::fs::rename(&other, &path).is_ok());
    }

    #[test]
    fn an_encrypted_file_on_disk_shows_no_key_and_restores_through_its_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy.mikabackup");
        let pc = TestPc::new();
        let report = write_file(&pc.stores(), &INFO, &path, Some("correct horse battery")).unwrap();
        assert!(report.encrypted);
        assert_eq!(report.secrets, 1);
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(13).any(|w| w == b"sk-test-VALUE"));

        let (mut body, header, stamp) = open_backup(&path).unwrap();
        let view = file_view(&path, &header, stamp.len);
        assert!(view.encrypted);
        assert_eq!(view.keys, 1);
        let access = backup::unlock(&header, Some("correct horse battery")).unwrap();
        let new = TestPc {
            config: Config::default(),
            db: Db::open_in_memory().unwrap(),
            secrets: MemorySecretStore::default(),
        };
        let selected = parse_categories(&["notes".to_string(), "secrets".to_string()]).unwrap();
        let done = backup::restore(
            &mut body,
            &header,
            &access,
            &new.stores(),
            &selected,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(done.keys_written, 1);
        assert_eq!(
            new.secrets.get("speech.api_key").unwrap(),
            Some(Secret::new("sk-test-VALUE"))
        );
        // The view of the result has the shape the interface reads.
        let view = RestoreView {
            restored: done
                .restored
                .iter()
                .map(|(id, count)| RestoredView {
                    id: *id,
                    count: *count,
                })
                .collect(),
            keys_written: done.keys_written,
            keys_failed: vec![SecretRef::fixed(backup::SecretKind::Telegram)],
            switched_off: vec![
                SwitchedOff::Microphone,
                SwitchedOff::Web,
                SwitchedOff::Mcp { count: 2 },
            ],
            docs_folders: Vec::new(),
            scenarios_without_command: 2,
            restart_needed: done.restart_needed,
            config_error: None,
            safety_copy: "C:\\x".into(),
        };
        assert_eq!(
            serde_json::to_value(&view).unwrap(),
            serde_json::json!({
                "restored": [{ "id": "notes", "count": 1 }, { "id": "secrets", "count": 1 }],
                "keys_written": 1,
                "keys_failed": [{ "kind": "telegram", "name": null }],
                "switched_off": [{ "id": "microphone" }, { "id": "web" }, { "id": "mcp", "count": 2 }],
                "docs_folders": [],
                "scenarios_without_command": 2,
                "restart_needed": true,
                "config_error": null,
                "safety_copy": "C:\\x",
            })
        );
    }

    #[test]
    fn the_preview_view_is_flat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy.mikabackup");
        let pc = TestPc::new();
        write_file(&pc.stores(), &INFO, &path, None).unwrap();
        let (mut body, header, stamp) = open_backup(&path).unwrap();
        let access = backup::unlock(&header, None).unwrap();
        let preview = backup::preview(
            &mut body,
            &header,
            &access,
            &pc.stores(),
            &Limits::default(),
        )
        .unwrap();
        let view = PreviewView {
            preview,
            file: file_view(&path, &header, stamp.len),
            safety_copy: "C:\\backups\\before-restore.mikabackup".into(),
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(
            json["categories"].as_array().unwrap().len(),
            Category::ALL.len()
        );
        assert_eq!(json["categories"][3]["id"], "notes");
        assert_eq!(json["categories"][3]["in_file"], 1);
        assert_eq!(json["file"]["name"], "copy.mikabackup");
        assert_eq!(
            json["safety_copy"],
            "C:\\backups\\before-restore.mikabackup"
        );
        assert!(json["allow_rules"].is_array() && json["secrets"].is_array());
        assert!(json["tasks"].is_array());
        assert_eq!(json["tasks_total"], 0);
        assert!(json["scenarios"].is_array());
        assert_eq!(json["scenarios_total"], 0);
        assert_eq!(json["categories"][8]["id"], "scenarios");
        assert!(json.get("preview").is_none());
    }

    #[test]
    fn only_one_export_or_restore_runs_at_a_time() {
        let first = Busy::take().unwrap();
        assert_eq!(Busy::take().err(), Some(tr("error.backup_busy")));
        drop(first);
        assert!(Busy::take().is_ok());
    }
}
