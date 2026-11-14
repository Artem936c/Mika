//! `av_folder`: one operation over the video and sound files of a folder,
//! one confirm card, one background job (decision 0032).
//!
//! Every file is assessed and processed by the single tool of the operation,
//! so the rules of decision 0027 hold unchanged. The list of files is made
//! when the card is shown and the job works on exactly that list: a folder
//! that changed while the card waited is refused, and a file that changed
//! while the job waited is passed by.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mika_core::bgjobs::{started_output, JobCtx, JobSpec, JobWork};
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::paths::PathAccess;
use mika_core::tools::{
    arg_str, arg_str_opt, spec, with_progress_handle, Assessment, Progress, RiskLevel, Tool,
    ToolError, ToolOutput,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::background::{Stamp, CHANGED_FILE};
use super::convert::{check_compress_args, convert_format};
use super::locate::path_key;
use super::output::ext_of;
use super::time::show_time;
use super::understand::remember_asked;
use super::{
    bad, size_text, AudioProcessTool, AvConvertTool, AvEnv, AvTranscribeTool, VideoCompressTool,
    MAX_TRANSCRIBE_S,
};
use crate::files::{path_arg, path_assessment};
use crate::util::{arg_bool, schema, truncate};

/// Most files one call takes.
pub(crate) const MAX_FILES: usize = 100;
/// Longest total length for convert, compress and audio; recognition takes
/// `MAX_TRANSCRIBE_S` in total.
pub(crate) const MAX_TOTAL_S: f64 = 12.0 * 3600.0;
/// How deep nested folders are looked into with `recursive`.
pub(crate) const MAX_DEPTH: usize = 6;
/// Directory entries looked at.
pub(crate) const MAX_ENTRIES: usize = 5000;
/// Recognition of more than this many seconds in total is a high risk: it costs.
const PAID_LONG_S: f64 = 600.0;
/// Names shown on the card, and reasons of files that do not fit.
const SHOWN_NAMES: usize = 5;
const SHOWN_UNFIT: usize = 3;
/// Characters of one reason in the outcome.
const REASON_CHARS: usize = 200;

const VIDEO: &[&str] = &[
    "mp4", "mkv", "mov", "webm", "avi", "wmv", "flv", "m4v", "mpg", "mpeg", "ts", "mts", "3gp",
];
const AUDIO: &[&str] = &[
    "mp3", "m4a", "wav", "flac", "ogg", "opus", "aac", "wma", "aiff", "ac3", "mka",
];

const NEW_FILES: &str = "исходные файлы не меняются, результаты — новые файлы";
const PAID: &str = "звук уйдёт провайдеру распознавания, это платно";
const IN_BACKGROUND: &str = "работа пойдёт в фоне";
/// The scope of a single call that only checks and writes nothing.
const CHECK_SCOPE: &str = "check:";
/// What `run` says when the folder is not what the card showed.
const FOLDER_CHANGED: &str = "папка изменилась, пока ждало подтверждение: файлы уже не те, что были на карточке. Ничего не запущено; вызови инструмент заново, чтобы спросить ещё раз";
/// What `run` says to a call whose card was never shown, or long ago.
const NOT_SHOWN: &str = "для этого набора файлов нет свежего подтверждения. Ничего не запущено; вызови инструмент заново";
/// How long the list of a card is kept for the run that follows: longer than
/// a confirmation waits.
const SHOWN_KEPT: Duration = Duration::from_secs(180);
/// Cards remembered at once.
const SHOWN_MAX: usize = 32;

/// What a card stood for: the files as they were, the place of the results
/// and the risk.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    files: Vec<Stamp>,
    out_dir: Option<PathBuf>,
    level: RiskLevel,
}

/// The cards shown and not run yet, by the arguments of their calls.
static SHOWN: Mutex<Vec<(String, Instant, Shown)>> = Mutex::new(Vec::new());

fn remember_shown(key: String, shown: Shown) {
    let mut list = SHOWN.lock();
    list.retain(|(k, at, _)| *k != key && at.elapsed() < SHOWN_KEPT);
    if list.len() >= SHOWN_MAX {
        list.remove(0);
    }
    list.push((key, Instant::now(), shown));
}

/// The card of the call with these arguments, once.
fn take_shown(key: &str) -> Option<Shown> {
    let mut list = SHOWN.lock();
    list.retain(|(_, at, _)| at.elapsed() < SHOWN_KEPT);
    let at = list.iter().position(|(k, _, _)| k == key)?;
    Some(list.remove(at).2)
}

/// What is done with every file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Convert,
    Compress,
    Audio,
    Transcribe,
}

impl Op {
    fn parse(raw: &str) -> Result<Self, ToolError> {
        match raw.trim().to_lowercase().as_str() {
            "convert" => Ok(Op::Convert),
            "compress" => Ok(Op::Compress),
            "audio" => Ok(Op::Audio),
            "transcribe" => Ok(Op::Transcribe),
            other => Err(bad(format!(
                "operation — convert, compress, audio или transcribe, а не «{other}»"
            ))),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Op::Convert => "convert",
            Op::Compress => "compress",
            Op::Audio => "audio",
            Op::Transcribe => "transcribe",
        }
    }

    /// Arguments of the folder call that go on to the single tool.
    fn params(self) -> &'static [&'static str] {
        match self {
            Op::Convert => &["format"],
            Op::Compress => &["target_mb", "quality", "height"],
            Op::Audio => &["normalize", "trim_silence", "denoise", "volume"],
            Op::Transcribe => &["srt"],
        }
    }

    /// Longest total length of one call.
    fn limit_s(self) -> f64 {
        match self {
            Op::Transcribe => MAX_TRANSCRIBE_S,
            _ => MAX_TOTAL_S,
        }
    }
}

/// The single tools the folder works through.
#[derive(Clone)]
struct Single {
    convert: Arc<AvConvertTool>,
    compress: Arc<VideoCompressTool>,
    audio: Arc<AudioProcessTool>,
    transcribe: Arc<AvTranscribeTool>,
}

impl Single {
    fn assess(&self, op: Op, args: &Value) -> Result<Assessment, ToolError> {
        match op {
            Op::Convert => self.convert.assess(args),
            Op::Compress => self.compress.assess(args),
            Op::Audio => self.audio.assess(args),
            Op::Transcribe => self.transcribe.assess(args),
        }
    }

    async fn exec(
        &self,
        op: Op,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        match op {
            Op::Convert => self.convert.exec(args, cancel).await,
            Op::Compress => self.compress.exec(args, cancel).await,
            Op::Audio => self.audio.exec(args, cancel).await,
            Op::Transcribe => self.transcribe.exec(args, cancel).await,
        }
    }
}

pub struct AvFolderTool {
    av: AvEnv,
    single: Single,
}

impl AvFolderTool {
    pub fn new(av: AvEnv) -> Self {
        Self {
            single: Single {
                convert: Arc::new(AvConvertTool(av.clone())),
                compress: Arc::new(VideoCompressTool(av.clone())),
                audio: Arc::new(AudioProcessTool(av.clone())),
                transcribe: Arc::new(AvTranscribeTool(av.clone())),
            },
            av,
        }
    }
}

/// One file of the job.
#[derive(Debug, Clone)]
struct Item {
    /// Its name as the card shows it: the path inside the folder.
    shown: String,
    /// Arguments of the single tool, without the place of the result.
    args: Value,
    /// The file as the card saw it.
    stamp: Stamp,
    /// Its length on the card; recognition takes no more of it.
    seconds: f64,
}

/// What a call will do, worked out the same way for the card and for the run.
struct FolderPlan {
    op: Op,
    folder: PathBuf,
    /// The subfolder for the results, if one was asked for.
    out_dir: Option<PathBuf>,
    items: Vec<Item>,
    assessment: Assessment,
}

impl FolderPlan {
    /// What the card of this plan stands for.
    fn shown(&self) -> Shown {
        Shown {
            files: self.items.iter().map(|i| i.stamp.clone()).collect(),
            out_dir: self.out_dir.clone(),
            level: self.assessment.level,
        }
    }
}

/// `1 файл`, `2 файла`, `5 файлов`: the word for `n`.
fn plural<'a>(n: usize, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    match (n % 10, n % 100) {
        (_, 11..=14) => many,
        (1, _) => one,
        (2..=4, _) => few,
        _ => many,
    }
}

fn files_word(n: usize) -> &'static str {
    plural(n, "файл", "файла", "файлов")
}

/// `в 1 файле`, `в 5 файлах`.
fn in_files_word(n: usize) -> &'static str {
    plural(n, "файле", "файлах", "файлах")
}

/// The name of the subfolder for the results, checked.
fn subfolder(args: &Value) -> Result<Option<String>, ToolError> {
    let Some(name) = arg_str_opt(args, "subfolder")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let wrong = name == "."
        || name == ".."
        || name.chars().count() > 60
        || name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
        || name.chars().any(char::is_control);
    if wrong {
        return Err(bad(
            "subfolder — одно имя папки до 60 знаков, без пути и знаков \\ / : * ? \" < > |",
        ));
    }
    Ok(Some(name.to_string()))
}

/// Extensions to take: the asked ones, or every video and sound extension
/// (only video for compression).
fn extensions(args: &Value, op: Op) -> Result<BTreeSet<String>, ToolError> {
    let asked: BTreeSet<String> = arg_str_opt(args, "extensions")
        .unwrap_or("")
        .split([',', ';', ' '])
        .map(|e| e.trim().trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    if asked
        .iter()
        .any(|e| e.len() > 8 || !e.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        return Err(bad("extensions — расширения через запятую: mp4, mov"));
    }
    if !asked.is_empty() {
        return Ok(asked);
    }
    let all = match op {
        Op::Compress => VIDEO.to_vec(),
        _ => [VIDEO, AUDIO].concat(),
    };
    Ok(all.into_iter().map(str::to_string).collect())
}

/// The walk over the folder.
struct Walk<'a> {
    exts: &'a BTreeSet<String>,
    /// The folder of the results: its files are not sources.
    skip: Option<String>,
    recursive: bool,
    looked: usize,
    found: Vec<PathBuf>,
}

impl Walk<'_> {
    fn dir(&mut self, dir: &Path, depth: usize) -> Result<(), ToolError> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if depth == 0 => {
                return Err(bad(format!("папку не удалось прочитать: {e}")));
            }
            // A nested folder that cannot be read is passed by.
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            self.looked += 1;
            if self.looked > MAX_ENTRIES {
                return Err(bad("в папке слишком много файлов: назови папку поуже"));
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_lowercase();
            // Half-written files and the pieces of a recording are Mika's own.
            if name.contains(".mika-part") {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let results = self.skip.as_deref() == Some(path_key(&path).as_str());
                if self.recursive && depth < MAX_DEPTH && !results {
                    self.dir(&path, depth + 1)?;
                }
            } else if kind.is_file() && self.exts.contains(&ext_of(&path)) {
                self.found.push(path);
                if self.found.len() > MAX_FILES {
                    return Err(bad(format!(
                        "подходящих файлов больше {MAX_FILES}: сузь отбор по extensions или раздели папку"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// `a.mp4 — причина; b.mp4 — причина`: the first `max` of `list`.
fn reasons(list: &[(String, String)], max: usize) -> String {
    list.iter()
        .take(max)
        .map(|(name, why)| {
            format!(
                "{name} — {}",
                truncate(why.trim().trim_end_matches('.'), REASON_CHARS)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// `2 ч 05 мин`.
fn hours_text(seconds: f64) -> String {
    let minutes = (seconds / 60.0).round() as u64;
    format!("{} ч {:02} мин", minutes / 60, minutes % 60)
}

/// Checks the arguments of the operation that need no file.
fn check_params(op: Op, args: &Value) -> Result<(), ToolError> {
    match op {
        Op::Convert => convert_format(args).map(|_| ()),
        Op::Compress => check_compress_args(args),
        Op::Audio | Op::Transcribe => Ok(()),
    }
}

/// How the videos are compressed, for the summary: ` до 50 МБ каждое, до 720p`.
fn compress_words(args: &Value) -> String {
    let given = |key: &str| args.get(key).filter(|v| !v.is_null());
    let mut how = Vec::new();
    if let Some(mb) = given("target_mb") {
        let mb = mb
            .as_f64()
            .map(super::args::number)
            .or_else(|| mb.as_str().map(|s| s.trim().to_string()))
            .unwrap_or_default();
        how.push(format!("до {mb} МБ каждое"));
    } else if arg_str_opt(args, "quality").map(str::trim) == Some("strong") {
        how.push("сильно".to_string());
    }
    if let Some(h) = given("height") {
        let h = h
            .as_u64()
            .map(|h| h.to_string())
            .or_else(|| {
                h.as_str()
                    .map(|s| s.trim().trim_end_matches('p').to_string())
            })
            .unwrap_or_default();
        how.push(format!("до {h}p"));
    }
    if how.is_empty() {
        String::new()
    } else {
        format!(" {}", how.join(", "))
    }
}

/// The first line of the card: the operation with the count, the total size
/// and the total length. `paid` is recognition at a high risk: its summary
/// names the minutes that will be paid for.
fn summary(
    op: Op,
    args: &Value,
    folder: &Path,
    n: usize,
    bytes: u64,
    seconds: f64,
    paid: bool,
) -> String {
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string());
    let total = format!("({}, {})", size_text(bytes), show_time(seconds));
    match op {
        Op::Convert => format!(
            "Перевести {n} {} из папки «{name}» в {} {total}",
            files_word(n),
            convert_format(args).unwrap_or_default().to_uppercase()
        ),
        Op::Compress => format!(
            "Сжать {n} видео из папки «{name}»{} {total}",
            compress_words(args)
        ),
        Op::Audio => format!(
            "Обработать звук в {n} {} из папки «{name}» {total}",
            in_files_word(n)
        ),
        Op::Transcribe if paid => format!(
            "Распознать {} мин звука из папки «{name}» — это платно",
            (seconds / 60.0).ceil()
        ),
        Op::Transcribe => format!(
            "Распознать речь в {n} {} из папки «{name}» {total}",
            in_files_word(n)
        ),
    }
}

impl AvFolderTool {
    /// The files, the risk and the card of a call.
    fn prepare(&self, args: &Value) -> Result<FolderPlan, ToolError> {
        let paths = self.av.ffmpeg()?;
        super::local_only(arg_str(args, "folder")?)?;
        let folder = path_arg(args, "folder", &self.av.user_profile)?;
        if !folder.is_dir() {
            return Err(bad(format!("нет папки {}", folder.display())));
        }
        let op = Op::parse(arg_str(args, "operation")?)?;
        check_params(op, args)?;
        let recursive = arg_bool(args, "recursive");
        let out_dir = subfolder(args)?.map(|name| folder.join(name));
        let exts = extensions(args, op)?;

        let mut walk = Walk {
            exts: &exts,
            skip: out_dir.as_deref().map(path_key),
            recursive,
            looked: 0,
            found: Vec::new(),
        };
        walk.dir(&folder, 0)?;
        let mut files = walk.found;
        files.sort_by_key(|p| p.to_string_lossy().to_lowercase());

        // The results go into the subfolder only when it is there already:
        // it is made after the card is confirmed.
        let ready_dir = out_dir.as_deref().filter(|d| d.is_dir());
        let mut level = RiskLevel::Medium;
        // The reason of what raised the level above a plain write.
        let mut stronger: Option<String> = None;
        let mut items = Vec::new();
        let mut unfit: Vec<(String, String)> = Vec::new();
        let (mut bytes, mut seconds) = (0u64, 0.0f64);
        for file in files {
            let shown = file
                .strip_prefix(&folder)
                .unwrap_or(&file)
                .display()
                .to_string();
            let mut one = json!({ "path": file.display().to_string() });
            for key in op.params() {
                if let Some(v) = args.get(*key).filter(|v| !v.is_null()) {
                    one[*key] = v.clone();
                }
            }
            let mut asked = one.clone();
            if let Some(dir) = ready_dir {
                asked["output"] = json!(dir.display().to_string());
            }
            match self.single.assess(op, &asked) {
                Ok(a) if a.scope.starts_with(CHECK_SCOPE) => {
                    unfit.push((shown, "уже меньше нужного размера".into()));
                }
                Ok(a) => {
                    if a.level > level {
                        level = a.level;
                        stronger = Some(a.reason);
                    }
                    let mut length = 0.0;
                    if let Ok(info) = self.av.probe_now(&paths, &file) {
                        bytes += info.size;
                        length = info.duration();
                        seconds += length;
                    }
                    items.push(Item {
                        shown,
                        args: one,
                        stamp: Stamp::of(&file),
                        seconds: length,
                    });
                }
                Err(e @ ToolError::NotReady { .. }) => return Err(e),
                Err(ToolError::BadArguments(why)) => unfit.push((shown, why)),
                Err(e) => unfit.push((shown, e.to_string())),
            }
        }
        if items.is_empty() {
            let mut text = "в папке нет подходящих файлов".to_string();
            if !unfit.is_empty() {
                text.push_str(&format!(": {}", reasons(&unfit, SHOWN_UNFIT)));
            }
            return Err(bad(text));
        }
        if seconds > op.limit_s() {
            return Err(bad(format!(
                "в сумме {}: больше {} ч за один раз не беру, сузь отбор",
                hours_text(seconds),
                (op.limit_s() / 3600.0).round()
            )));
        }
        if let Some(dir) = out_dir.as_deref().filter(|d| !d.is_dir()) {
            let write = path_assessment(
                dir,
                &self.av.user_profile,
                PathAccess::Write,
                String::new(),
                RiskLevel::Medium,
            );
            if write.level > level {
                level = write.level;
                stronger = Some(write.reason);
            }
        }
        if op == Op::Transcribe && seconds > PAID_LONG_S {
            level = RiskLevel::High;
        }

        // The upload and its price are named at every level.
        let reason = match (op, stronger.filter(|s| s != PAID)) {
            (Op::Transcribe, None) => {
                format!("{PAID}; результаты — новые файлы; {IN_BACKGROUND}")
            }
            (Op::Transcribe, Some(why)) => format!("{why}; {PAID}; {IN_BACKGROUND}"),
            (_, None) => format!("{NEW_FILES}; {IN_BACKGROUND}"),
            (_, Some(why)) => format!("{why}; {IN_BACKGROUND}"),
        };
        let n = items.len();
        let high_paid = op == Op::Transcribe && level == RiskLevel::High;
        let summary = summary(op, args, &folder, n, bytes, seconds, high_paid);

        let mut list = items
            .iter()
            .take(SHOWN_NAMES)
            .map(|i| i.shown.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if n > SHOWN_NAMES {
            list.push_str(&format!(" и ещё {}", n - SHOWN_NAMES));
        }
        if high_paid {
            list = format!("{n} {}: {list}", files_word(n));
        }
        let place = match &out_dir {
            Some(dir) => format!("{} → {}", folder.display(), dir.display()),
            None => format!("{} → рядом с исходными", folder.display()),
        };
        let mut target = vec![place, list];
        if !unfit.is_empty() {
            target.push(format!(
                "Не подходят {}: {}",
                unfit.len(),
                reasons(&unfit, SHOWN_UNFIT)
            ));
        }

        let mut from = folder.display().to_string().to_lowercase();
        if recursive {
            // A rule for nested folders nobody looked at would be too wide:
            // the policy refuses «всегда» for a scope with `*`.
            from.push_str("\\**");
        } else if op == Op::Transcribe {
            // The same for a paid upload: a permanent rule would send the
            // sound of whatever lies in the folder later to the provider,
            // for money, with no card that names it.
            from.push_str("\\*");
        }
        let to = match &out_dir {
            Some(dir) => dir.display().to_string().to_lowercase(),
            None => from.clone(),
        };
        let assessment = Assessment::new(level, summary, target.join("\n"), reason)
            .with_scope(format!("{}: {from} -> {to}", op.name()));
        Ok(FolderPlan {
            op,
            folder,
            out_dir,
            items,
            assessment,
        })
    }
}

/// `Готово 11 из 12: результаты в C:\…. Не получилось: a.mp4 — причина.`
fn outcome(done: usize, n: usize, results: &Path, failed: &[(String, String)]) -> String {
    let mut text = format!("Готово {done} из {n}: результаты в {}.", results.display());
    if !failed.is_empty() {
        text.push_str(&format!(" Не получилось: {}", reasons(failed, SHOWN_NAMES)));
        if failed.len() > SHOWN_NAMES {
            text.push_str(&format!(" и ещё {}", failed.len() - SHOWN_NAMES));
        }
        text.push('.');
    }
    text
}

/// One card with every result; a card holds at most `MAX_FILES` files.
fn card(files: &[Value]) -> Value {
    json!({ "kind": "av_files", "files": files[..files.len().min(MAX_FILES)] })
}

/// The work of the job: every item in turn, a failing one passed by.
async fn run_items(
    single: Single,
    op: Op,
    items: Vec<Item>,
    results: PathBuf,
    out_dir: Option<PathBuf>,
    ctx: JobCtx,
) -> Result<ToolOutput, ToolError> {
    let n = items.len();
    let mut files: Vec<Value> = Vec::new();
    let mut done = 0;
    let mut failed: Vec<(String, String)> = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        ctx.handle
            .set_note(&format!("{} из {n}: {}", i + 1, item.shown));
        // The job may have waited for hours: only the file the card saw is
        // worked on, and uploaded.
        if !item.stamp.holds() {
            failed.push((item.shown, CHANGED_FILE.to_string()));
            ctx.handle.set_share((((i + 1) * 100) / n) as u8);
            continue;
        }
        if op == Op::Transcribe {
            remember_asked(&item.stamp.path, item.seconds);
        }
        let mut args = item.args;
        if let Some(dir) = &out_dir {
            args["output"] = json!(dir.display().to_string());
        }
        // The file's own progress is its share of the whole folder.
        let handle = ctx.handle.clone();
        let progress =
            Progress::to(move |share| handle.set_share(((i * 100 + share as usize) / n) as u8));
        let result =
            with_progress_handle(progress, single.exec(op, args, ctx.cancel.child_token())).await;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        match result {
            Ok(out) => {
                let made = out
                    .data
                    .as_ref()
                    .and_then(|d| d["files"].as_array())
                    .filter(|list| !list.is_empty());
                match made {
                    Some(list) => {
                        done += 1;
                        files.extend(list.iter().cloned());
                    }
                    // The tool found nothing to write: its words say why.
                    None => failed.push((
                        item.shown,
                        out.text.lines().next().unwrap_or_default().to_string(),
                    )),
                }
            }
            Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
            Err(ToolError::BadArguments(why)) => failed.push((item.shown, why)),
            Err(e) => failed.push((item.shown, e.to_string())),
        }
        ctx.handle.set_share((((i + 1) * 100) / n) as u8);
        if done > 0 {
            ctx.handle
                .set_partial(&outcome(done, n, &results, &failed), Some(card(&files)));
        }
    }
    if done == 0 {
        return Err(ToolError::Failed(format!(
            "ни один файл не обработан: {}",
            reasons(&failed, SHOWN_UNFIT)
        )));
    }
    Ok(ToolOutput::new(outcome(done, n, &results, &failed)).with_data(card(&files)))
}

#[async_trait]
impl Tool for AvFolderTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "av_folder",
            "Сделать одно и то же со всеми видео или звуковыми файлами папки: перевести в другой формат (convert), сжать (compress), обработать звук (audio) или расшифровать речь (transcribe). Одна карточка на всю папку, работа идёт в фоне. Исходные файлы не меняются, результаты — новые файлы рядом или в подпапке.",
            schema(
                &[
                    ("folder", "string", "Полный путь к папке"),
                    (
                        "operation",
                        "string",
                        "one of: convert, compress, audio, transcribe",
                    ),
                    (
                        "extensions",
                        "string",
                        "Какие файлы брать, через запятую: mp4, mov. Без него — все видео и звук, для compress только видео",
                    ),
                    ("recursive", "boolean", "true — вместе с вложенными папками"),
                    (
                        "subfolder",
                        "string",
                        "Имя подпапки для результатов внутри папки; без него — рядом с исходными",
                    ),
                    (
                        "format",
                        "string",
                        "one of: mp4, mkv, mov, webm, mp3, m4a, wav, flac, ogg, opus",
                    ),
                    (
                        "target_mb",
                        "number",
                        "Для compress: нужный размер каждого файла в МБ, от 1 до 4000",
                    ),
                    ("quality", "string", "one of: normal, strong"),
                    (
                        "height",
                        "integer",
                        "Для compress: короткая сторона кадра: 2160, 1440, 1080, 720, 480 или 360",
                    ),
                    ("normalize", "boolean", "Для audio: выровнять громкость"),
                    (
                        "trim_silence",
                        "boolean",
                        "Для audio: убрать тишину в начале и в конце, только у звуковых файлов",
                    ),
                    ("denoise", "string", "one of: off, normal, strong"),
                    (
                        "volume",
                        "string",
                        "Для audio: изменить громкость: +6dB, -3dB или 150%",
                    ),
                    (
                        "srt",
                        "boolean",
                        "Для transcribe: true — ещё и субтитры .srt",
                    ),
                ],
                &["folder", "operation"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let plan = self.prepare(args)?;
        remember_shown(args.to_string(), plan.shown());
        Ok(plan.assessment)
    }

    async fn run(
        &self,
        args: Value,
        approval: Approval,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let plan = self.prepare(&args)?;
        // The approval was given for the list the card showed. A file that
        // came, went or changed since would be worked on, and for
        // recognition uploaded and paid for, without anyone having seen it.
        match take_shown(&args.to_string()) {
            Some(shown) if shown == plan.shown() => {}
            Some(_) => return Err(ToolError::Failed(FOLDER_CHANGED.into())),
            None => return Err(ToolError::Failed(NOT_SHOWN.into())),
        }
        let made_dir = match &plan.out_dir {
            Some(dir) if !dir.is_dir() => {
                std::fs::create_dir(dir).map_err(|e| {
                    ToolError::Failed(format!("не удалось создать папку {}: {e}", dir.display()))
                })?;
                true
            }
            _ => false,
        };
        let results = plan.out_dir.clone().unwrap_or_else(|| plan.folder.clone());
        let (single, op, items, out_dir) = (
            self.single.clone(),
            plan.op,
            plan.items,
            plan.out_dir.clone(),
        );
        let work: JobWork = Box::new(move |ctx: JobCtx| {
            Box::pin(async move {
                // One approval covers the list the card showed; it ends with the job.
                let _approval = approval;
                run_items(single, op, items, results, out_dir, ctx).await
            })
        });
        let spec = JobSpec::queued("av_folder", plan.assessment.summary);
        match self.av.jobs.submit(spec, work) {
            Ok(submitted) => Ok(started_output(&submitted)),
            Err(e) => {
                if let (true, Some(dir)) = (made_dir, &plan.out_dir) {
                    // Nothing will be written: the empty subfolder goes away.
                    let _ = std::fs::remove_dir(dir);
                }
                Err(ToolError::Failed(e.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mika_core::bgjobs::{JobState, JobView, STARTED_PREFIX};
    use mika_core::policy::{forever_block, WILDCARD_FOREVER};

    use super::super::locate;
    use super::super::testkit::{av, probe_json, Av};
    use super::*;
    use crate::sys::RunOutput;

    const WAIT: Duration = Duration::from_secs(20);
    const MB: u64 = 1024 * 1024;

    /// A folder inside the test's own one.
    fn folder(t: &Av, name: &str) -> PathBuf {
        let dir = t.dir.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A file that ffprobe has already looked at and found `probe` in.
    fn clip(t: &Av, dir: &Path, name: &str, probe: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"media").unwrap();
        t.answer(probe);
        let env = t.av();
        env.probe_now(&env.ffmpeg().unwrap(), &path).unwrap();
        path
    }

    /// An MKV with H.264 and AAC: its conversion to MP4 only copies.
    fn mkv(t: &Av, dir: &Path, name: &str, seconds: f64) -> PathBuf {
        let probe = probe_json(
            "matroska,webm",
            seconds,
            10 * MB,
            Some(("h264", 1280, 720)),
            Some("aac"),
        );
        clip(t, dir, name, &probe)
    }

    fn mp3(t: &Av, dir: &Path, name: &str, seconds: f64) -> PathBuf {
        clip(
            t,
            dir,
            name,
            &probe_json("mp3", seconds, 2 * MB, None, Some("mp3")),
        )
    }

    fn call(dir: &Path, operation: &str, extra: Value) -> Value {
        let mut args = json!({ "folder": dir.display().to_string(), "operation": operation });
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        args
    }

    fn names(plan: &FolderPlan) -> Vec<&str> {
        plan.items.iter().map(|i| i.shown.as_str()).collect()
    }

    async fn start(tool: &AvFolderTool, args: Value) -> u64 {
        tool.assess(&args).unwrap();
        let out = tool
            .run(args, Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        assert!(out.text.starts_with(STARTED_PREFIX), "{}", out.text);
        let card = out.data.unwrap();
        assert_eq!(card["kind"], "bg_job");
        assert_eq!(card["job"]["kind"], "av_folder");
        card["job"]["id"].as_u64().unwrap()
    }

    async fn ended(t: &Av, id: u64) -> JobView {
        t.av().jobs.wait(id, WAIT).await.expect("the job ends")
    }

    #[test]
    fn words_follow_the_count() {
        for (n, word, place) in [
            (1, "файл", "файле"),
            (2, "файла", "файлах"),
            (5, "файлов", "файлах"),
            (11, "файлов", "файлах"),
            (21, "файл", "файле"),
            (22, "файла", "файлах"),
            (100, "файлов", "файлах"),
        ] {
            assert_eq!((files_word(n), in_files_word(n)), (word, place), "{n}");
        }
        assert_eq!(hours_text(13.0 * 3600.0 + 5.0 * 60.0), "13 ч 05 мин");
    }

    #[test]
    fn files_are_taken_by_extension_per_operation() {
        let t = av();
        let dir = folder(&t, "Отпуск");
        mkv(&t, &dir, "б.mkv", 60.0);
        mkv(&t, &dir, "А.MKV", 60.0);
        mp3(&t, &dir, "песня.mp3", 60.0);
        std::fs::write(dir.join("заметки.txt"), b"x").unwrap();
        std::fs::write(dir.join("фото.jpg"), b"x").unwrap();
        let tool = AvFolderTool::new(t.av());

        // Video and sound by default, sorted without regard to case.
        let plan = tool
            .prepare(&call(&dir, "convert", json!({ "format": "mp4" })))
            .unwrap();
        assert_eq!(names(&plan), ["А.MKV", "б.mkv"], "mp3 has no video for mp4");
        let plan = tool
            .prepare(&call(&dir, "convert", json!({ "format": "wav" })))
            .unwrap();
        assert_eq!(names(&plan), ["А.MKV", "б.mkv", "песня.mp3"]);
        // Compression looks at video only.
        let plan = tool
            .prepare(&call(&dir, "compress", json!({ "quality": "normal" })))
            .unwrap();
        assert_eq!(names(&plan), ["А.MKV", "б.mkv"]);
        assert!(
            !plan.assessment.target.contains("песня"),
            "not even as unfit"
        );
        // The filter of the call wins.
        let plan = tool
            .prepare(&call(
                &dir,
                "transcribe",
                json!({ "extensions": ".MP3; wav" }),
            ))
            .unwrap();
        assert_eq!(names(&plan), ["песня.mp3"]);
        assert!(matches!(
            tool.prepare(&call(&dir, "audio", json!({ "normalize": true, "extensions": "*.mp3" }))),
            Err(ToolError::BadArguments(m)) if m.starts_with("extensions")
        ));
        assert!(matches!(
            tool.prepare(&call(&dir, "transcribe", json!({ "extensions": "avi" }))),
            Err(ToolError::BadArguments(m)) if m == "в папке нет подходящих файлов"
        ));
    }

    #[test]
    fn arguments_are_checked_before_any_file() {
        let t = av();
        let dir = folder(&t, "v");
        let tool = AvFolderTool::new(t.av());
        let bad_call = |args: Value| match tool.prepare(&args) {
            Err(ToolError::BadArguments(m)) => m,
            other => panic!("{:?}", other.map(|p| p.assessment)),
        };
        assert!(bad_call(json!({ "operation": "convert" })).contains("folder"));
        assert!(bad_call(json!({ "folder": dir.display().to_string() })).contains("operation"));
        assert!(bad_call(call(&dir, "склеить", json!({}))).starts_with("operation"));
        assert!(bad_call(call(&dir, "convert", json!({}))).contains("format"));
        assert!(
            bad_call(call(&dir, "convert", json!({ "format": "exe" }))).contains("не поддержан")
        );
        assert!(
            bad_call(call(&dir, "compress", json!({}))).contains("target_mb, quality или height")
        );
        assert_eq!(
            bad_call(call(&dir.join("нет"), "audio", json!({}))),
            format!("нет папки {}", dir.join("нет").display())
        );
        assert_eq!(
            bad_call(json!({ "folder": "\\\\.\\pipe\\x", "operation": "audio" })),
            "только файлы на этом компьютере"
        );
        let long = "я".repeat(61);
        for name in ["a\\b", "..", ".", "a/b", "c:", "что?", long.as_str()] {
            assert!(
                bad_call(call(&dir, "audio", json!({ "subfolder": name })))
                    .starts_with("subfolder"),
                "{name}"
            );
        }
        // A file is not a folder.
        let file = mkv(&t, &dir, "a.mkv", 10.0);
        assert!(bad_call(call(&file, "audio", json!({}))).starts_with("нет папки"));
    }

    #[test]
    fn nested_folders_are_taken_only_when_asked() {
        let t = av();
        let dir = folder(&t, "v");
        let deep = folder(&t, "v\\2024\\лето");
        mkv(&t, &dir, "a.mkv", 60.0);
        mkv(&t, &deep, "b.mkv", 60.0);
        let tool = AvFolderTool::new(t.av());
        let flat = call(&dir, "convert", json!({ "format": "mp4" }));
        let plan = tool.prepare(&flat).unwrap();
        assert_eq!(names(&plan), ["a.mkv"]);
        let folder_low = dir.display().to_string().to_lowercase();
        assert_eq!(
            plan.assessment.scope,
            format!("convert: {folder_low} -> {folder_low}")
        );
        assert_eq!(
            forever_block(plan.assessment.level, &plan.assessment.scope),
            None
        );

        let nested = call(
            &dir,
            "convert",
            json!({ "format": "mp4", "recursive": true }),
        );
        let plan = tool.prepare(&nested).unwrap();
        assert_eq!(names(&plan), ["2024\\лето\\b.mkv", "a.mkv"]);
        assert_eq!(
            plan.assessment.scope,
            format!("convert: {folder_low}\\** -> {folder_low}\\**")
        );
        assert_eq!(
            forever_block(plan.assessment.level, &plan.assessment.scope),
            Some(WILDCARD_FOREVER),
            "«всегда» would cover folders nobody saw"
        );
        let with_sub = call(
            &dir,
            "convert",
            json!({ "format": "mp4", "recursive": true, "subfolder": "Готово" }),
        );
        assert_eq!(
            tool.prepare(&with_sub).unwrap().assessment.scope,
            format!("convert: {folder_low}\\** -> {folder_low}\\готово")
        );
    }

    #[test]
    fn the_subfolder_and_half_written_files_are_not_sources() {
        let t = av();
        let dir = folder(&t, "v");
        let ready = folder(&t, "v\\Готово");
        let pieces = folder(&t, "v\\Запись экрана.mika-part");
        mkv(&t, &dir, "a.mkv", 60.0);
        std::fs::write(ready.join("old.mkv"), b"x").unwrap();
        std::fs::write(dir.join("b.mika-part.mkv"), b"x").unwrap();
        std::fs::write(pieces.join("seg-0.mkv"), b"x").unwrap();
        let tool = AvFolderTool::new(t.av());
        let args = call(
            &dir,
            "convert",
            json!({ "format": "mp4", "recursive": true, "subfolder": "готово" }),
        );
        let plan = tool.prepare(&args).unwrap();
        assert_eq!(names(&plan), ["a.mkv"]);
        assert_eq!(
            plan.assessment.target.lines().next().unwrap(),
            format!("{} → {}", dir.display(), dir.join("готово").display())
        );
        // Without the subfolder as the place of results, its files are sources.
        t.answer(&probe_json(
            "matroska,webm",
            5.0,
            MB,
            Some(("h264", 640, 360)),
            Some("aac"),
        ));
        let all = call(
            &dir,
            "convert",
            json!({ "format": "mp4", "recursive": true }),
        );
        assert_eq!(
            names(&tool.prepare(&all).unwrap()),
            ["a.mkv", "Готово\\old.mkv"]
        );
    }

    #[test]
    fn too_many_files_and_too_much_length_are_refused() {
        let t = av();
        let many = folder(&t, "many");
        for i in 0..=MAX_FILES {
            std::fs::write(many.join(format!("{i:03}.mp4")), b"x").unwrap();
        }
        let tool = AvFolderTool::new(t.av());
        match tool.assess(&call(&many, "compress", json!({ "quality": "normal" }))) {
            Err(ToolError::BadArguments(m)) => assert_eq!(
                m,
                "подходящих файлов больше 100: сузь отбор по extensions или раздели папку"
            ),
            other => panic!("{other:?}"),
        }
        assert!(
            t.f.runs.lock().is_empty(),
            "refused before any file is opened"
        );

        let long = folder(&t, "long");
        for i in 0..4 {
            mkv(&t, &long, &format!("{i}.mkv"), 3.5 * 3600.0);
        }
        match tool.assess(&call(&long, "convert", json!({ "format": "mp4" }))) {
            Err(ToolError::BadArguments(m)) => assert_eq!(
                m,
                "в сумме 14 ч 00 мин: больше 12 ч за один раз не беру, сузь отбор"
            ),
            other => panic!("{other:?}"),
        }
        // Recognition has its own, lower limit.
        let talks = folder(&t, "talks");
        for i in 0..3 {
            mp3(&t, &talks, &format!("{i}.mp3"), 4000.0);
        }
        match tool.assess(&call(&talks, "transcribe", json!({}))) {
            Err(ToolError::BadArguments(m)) => {
                assert!(m.contains("больше 3 ч за один раз не беру"), "{m}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_card_names_the_files_and_those_that_do_not_fit() {
        let t = av();
        let dir = folder(&t, "Отпуск");
        for i in 1..=7 {
            mkv(&t, &dir, &format!("день{i}.mkv"), 100.0);
        }
        // Already MP4 with streams that fit: nothing to convert.
        clip(
            &t,
            &dir,
            "готовый.mp4",
            &probe_json("mov,mp4", 50.0, MB, Some(("h264", 640, 360)), Some("aac")),
        );
        let tool = AvFolderTool::new(t.av());
        let a = tool
            .assess(&call(&dir, "convert", json!({ "format": "MP4" })))
            .unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert_eq!(
            a.summary,
            "Перевести 7 файлов из папки «Отпуск» в MP4 (70,0 МБ, 11:40)"
        );
        assert_eq!(
            a.reason,
            "исходные файлы не меняются, результаты — новые файлы; работа пойдёт в фоне"
        );
        let lines: Vec<&str> = a.target.lines().collect();
        assert_eq!(
            lines,
            [
                format!("{} → рядом с исходными", dir.display()).as_str(),
                "день1.mkv, день2.mkv, день3.mkv, день4.mkv, день5.mkv и ещё 2",
                "Не подходят 1: готовый.mp4 — файл уже в этом формате",
            ]
        );

        // Compression: a file already under the size is named, not processed.
        let small = folder(&t, "Мелочь");
        clip(
            &t,
            &small,
            "big.mp4",
            &probe_json(
                "mov,mp4",
                600.0,
                300 * MB,
                Some(("h264", 1920, 1080)),
                Some("aac"),
            ),
        );
        clip(
            &t,
            &small,
            "small.mp4",
            &probe_json(
                "mov,mp4",
                60.0,
                5 * MB,
                Some(("h264", 1280, 720)),
                Some("aac"),
            ),
        );
        let a = tool
            .assess(&call(
                &small,
                "compress",
                json!({ "target_mb": 50, "height": 720 }),
            ))
            .unwrap();
        assert_eq!(
            a.summary,
            "Сжать 1 видео из папки «Мелочь» до 50 МБ каждое, до 720p (300 МБ, 10:00)"
        );
        assert_eq!(
            a.target.lines().last().unwrap(),
            "Не подходят 1: small.mp4 — уже меньше нужного размера"
        );
        let a = tool
            .assess(&call(&small, "compress", json!({ "quality": "strong" })))
            .unwrap();
        assert!(a
            .summary
            .starts_with("Сжать 2 видео из папки «Мелочь» сильно ("));
        let a = tool
            .assess(&call(&small, "audio", json!({ "normalize": true })))
            .unwrap();
        assert!(
            a.summary
                .starts_with("Обработать звук в 2 файлах из папки «Мелочь» ("),
            "{}",
            a.summary
        );
    }

    #[test]
    fn the_level_is_the_highest_of_the_files_and_of_the_result_folder() {
        let t = av();
        let dir = folder(&t, "home");
        let programs = folder(&t, "home\\AppData\\Roaming");
        mkv(&t, &dir, "a.mkv", 60.0);
        mkv(&t, &programs, "b.mkv", 60.0);
        // The folder is the user's profile: AppData inside it is a place of programs.
        let mut env = t.av();
        env.user_profile = dir.display().to_string();
        let tool = AvFolderTool::new(env);

        let flat = tool
            .assess(&call(&dir, "convert", json!({ "format": "mp4" })))
            .unwrap();
        assert_eq!(flat.level, RiskLevel::Medium);
        let nested = tool
            .assess(&call(
                &dir,
                "convert",
                json!({ "format": "mp4", "recursive": true }),
            ))
            .unwrap();
        assert_eq!(
            nested.level,
            RiskLevel::High,
            "one file writes into AppData"
        );
        assert_eq!(nested.reason, "папка программ; работа пойдёт в фоне");

        // The result folder counts: while it exists, through every file...
        let into = tool
            .assess(&call(
                &dir,
                "convert",
                json!({ "format": "mp4", "subfolder": "AppData" }),
            ))
            .unwrap();
        assert_eq!(into.level, RiskLevel::High);
        // ...and when it is yet to be made, by its own place.
        std::fs::remove_dir_all(dir.join("AppData")).unwrap();
        let into = tool
            .assess(&call(
                &dir,
                "convert",
                json!({ "format": "mp4", "subfolder": "AppData" }),
            ))
            .unwrap();
        assert_eq!(into.level, RiskLevel::High);
        assert_eq!(into.reason, "папка программ; работа пойдёт в фоне");
        assert!(!dir.join("AppData").exists(), "the card makes nothing");
    }

    #[test]
    fn recognition_of_a_folder_names_the_upload_and_its_price() {
        let t = av();
        let short = folder(&t, "Заметки");
        for i in 1..=5 {
            mp3(&t, &short, &format!("{i}.mp3"), 60.0);
        }
        let tool = AvFolderTool::new(t.av());
        // Five minutes in total: a medium risk that still tells where the sound goes.
        let a = tool.assess(&call(&short, "transcribe", json!({}))).unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert_eq!(
            a.summary,
            "Распознать речь в 5 файлах из папки «Заметки» (10,0 МБ, 5:00)"
        );
        assert!(
            a.reason.contains("провайдеру распознавания") && a.reason.contains("платно"),
            "{}",
            a.reason
        );
        assert_eq!(
            a.reason,
            "звук уйдёт провайдеру распознавания, это платно; результаты — новые файлы; работа пойдёт в фоне"
        );
        // A paid upload is asked about every time: a rule for the folder
        // would cover files that nobody has seen on a card.
        assert_eq!(
            a.scope,
            format!(
                "transcribe: {0}\\* -> {0}\\*",
                short.display().to_string().to_lowercase()
            )
        );
        assert_eq!(forever_block(a.level, &a.scope), Some(WILDCARD_FOREVER));
        let into = tool
            .assess(&call(&short, "transcribe", json!({ "subfolder": "Текст" })))
            .unwrap();
        assert_eq!(
            forever_block(into.level, &into.scope),
            Some(WILDCARD_FOREVER)
        );
        // Work that stays on this computer may be allowed for the folder.
        let local = tool
            .assess(&call(&short, "audio", json!({ "normalize": true })))
            .unwrap();
        assert_eq!(forever_block(local.level, &local.scope), None);

        // Twenty files of a minute are twenty paid minutes.
        let long = folder(&t, "Лекции");
        for i in 1..=20 {
            mp3(&t, &long, &format!("{i:02}.mp3"), 61.0);
        }
        let a = tool
            .assess(&call(&long, "transcribe", json!({ "srt": true })))
            .unwrap();
        assert_eq!(a.level, RiskLevel::High);
        assert_eq!(
            a.summary,
            "Распознать 21 мин звука из папки «Лекции» — это платно"
        );
        assert!(a
            .reason
            .starts_with("звук уйдёт провайдеру распознавания, это платно"));
        assert_eq!(
            a.target.lines().nth(1).unwrap(),
            "20 файлов: 01.mp3, 02.mp3, 03.mp3, 04.mp3, 05.mp3 и ещё 15"
        );
        assert!(forever_block(a.level, &a.scope).is_some());
    }

    #[tokio::test]
    async fn a_folder_is_one_job_with_one_card() {
        let t = av();
        let dir = folder(&t, "v");
        for name in ["a.mkv", "b.mkv", "c.mkv"] {
            mkv(&t, &dir, name, 60.0);
        }
        let tool = AvFolderTool::new(t.av());
        let args = call(
            &dir,
            "convert",
            json!({ "format": "mp4", "subfolder": "Готово" }),
        );
        let a = tool.assess(&args).unwrap();
        let out_dir = dir.join("Готово");
        assert!(!out_dir.exists(), "the subfolder is made only by the run");
        let mut events = t.env.shared.bus.subscribe();
        let id = start(&tool, args).await;
        assert!(out_dir.is_dir());
        let job = ended(&t, id).await;
        assert_eq!(job.state, JobState::Done, "{}", job.text);
        assert_eq!(job.title, a.summary);
        assert_eq!(
            job.text,
            format!("Готово 3 из 3: результаты в {}.", out_dir.display())
        );
        let card = job.data.unwrap();
        assert_eq!(card["kind"], "av_files");
        let made: Vec<String> = card["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            made,
            ["a.mp4", "b.mp4", "c.mp4"].map(|n| out_dir.join(n).display().to_string())
        );
        for path in &made {
            assert!(Path::new(path).is_file());
            assert!(
                t.av().outputs.contains(Path::new(path)),
                "opens from the card"
            );
        }
        assert_eq!(std::fs::read(dir.join("a.mkv")).unwrap(), b"media");
        // The note counts the files and the share grows by thirds.
        let mut notes = Vec::new();
        let mut shares = Vec::new();
        while let Ok(e) = events.try_recv() {
            if let mika_core::Event::BgJobUpdated { job } = e {
                if !job.note.is_empty() && notes.last() != Some(&job.note) {
                    notes.push(job.note.clone());
                }
                if shares.last() != Some(&job.share) {
                    shares.push(job.share);
                }
            }
        }
        assert_eq!(notes, ["1 из 3: a.mkv", "2 из 3: b.mkv", "3 из 3: c.mkv"]);
        assert!(shares.windows(2).all(|w| w[0] <= w[1]), "{shares:?}");
        assert!(shares.contains(&33) && shares.contains(&66), "{shares:?}");
        assert_eq!(shares.last(), Some(&100));
    }

    #[tokio::test]
    async fn a_failing_file_is_passed_by_and_named() {
        let t = av();
        let dir = folder(&t, "v");
        for name in ["a.mkv", "b.mkv", "c.mkv"] {
            mkv(&t, &dir, name, 60.0);
        }
        // a: the run and the look at its result; then b's run fails.
        t.answer("");
        t.answer("");
        t.f.run_outputs.lock().push_back(RunOutput {
            code: Some(1),
            stderr: "Invalid data found when processing input\n".into(),
            ..Default::default()
        });
        let tool = AvFolderTool::new(t.av());
        let id = start(&tool, call(&dir, "convert", json!({ "format": "mp4" }))).await;
        let job = ended(&t, id).await;
        assert_eq!(job.state, JobState::Done);
        assert_eq!(
            job.text,
            format!(
                "Готово 2 из 3: результаты в {}. Не получилось: b.mkv — FFmpeg завершился с ошибкой: Invalid data found when processing input.",
                dir.display()
            )
        );
        assert_eq!(job.data.unwrap()["files"].as_array().unwrap().len(), 2);
        assert!(dir.join("a.mp4").is_file() && dir.join("c.mp4").is_file());
        assert!(!dir.join("b.mp4").exists());
    }

    #[tokio::test]
    async fn a_stop_in_the_middle_keeps_what_is_done() {
        let t = av();
        let dir = folder(&t, "v");
        for name in ["a.mkv", "b.mkv", "c.mkv"] {
            mkv(&t, &dir, name, 60.0);
        }
        *t.f.run_delay.lock() = Duration::from_millis(150);
        let tool = AvFolderTool::new(t.av());
        let id = start(&tool, call(&dir, "convert", json!({ "format": "mp4" }))).await;
        let jobs = t.av().jobs;
        // Wait until the second file is at work.
        let started = std::time::Instant::now();
        loop {
            let job = jobs.get(id).unwrap();
            if job.note == "2 из 3: b.mkv" {
                break;
            }
            assert!(job.state.active(), "{job:?}");
            assert!(started.elapsed() < WAIT);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        jobs.stop(id).unwrap();
        let job = ended(&t, id).await;
        assert_eq!(job.state, JobState::Stopped);
        assert_eq!(
            job.text,
            format!("Готово 1 из 3: результаты в {}.", dir.display())
        );
        let card = job.data.unwrap();
        assert_eq!(card["files"].as_array().unwrap().len(), 1);
        assert_eq!(
            card["files"][0]["path"],
            dir.join("a.mp4").display().to_string()
        );
        assert!(dir.join("a.mp4").is_file(), "the finished file stays");
        assert!(!dir.join("b.mp4").exists() && !dir.join("c.mp4").exists());
        // The half-written file of b goes away.
        super::super::wait_cleanup(Duration::from_secs(5));
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.contains(".mika-part"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[tokio::test]
    async fn a_folder_where_every_file_fails_is_a_failed_job() {
        let t = av();
        let dir = folder(&t, "v");
        mkv(&t, &dir, "a.mkv", 60.0);
        mkv(&t, &dir, "b.mkv", 60.0);
        *t.f.run_output.lock() = RunOutput {
            code: Some(1),
            stderr: "No space left on device\n".into(),
            ..Default::default()
        };
        let tool = AvFolderTool::new(t.av());
        let id = start(&tool, call(&dir, "convert", json!({ "format": "mp4" }))).await;
        let job = ended(&t, id).await;
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(
            job.text,
            "ни один файл не обработан: a.mkv — FFmpeg завершился с ошибкой: No space left on device; b.mkv — FFmpeg завершился с ошибкой: No space left on device"
        );
        assert!(job.data.is_none());
    }

    #[tokio::test]
    async fn recognition_of_a_folder_writes_a_text_for_every_file() {
        let t = av();
        let dir = folder(&t, "Заметки");
        mp3(&t, &dir, "раз.mp3", 20.0);
        mp3(&t, &dir, "два.mp3", 20.0);
        *t.f.stt_text.lock() = "привет".into();
        // The sound FFmpeg extracts: a second of silence as PCM.
        let wav = super::super::wav::slice_wav(&[0u8; 32_000], 0.0, 1.0);
        *t.f.run_write.lock() = Some(wav);
        let tool = AvFolderTool::new(t.av());
        let args = call(&dir, "transcribe", json!({}));
        assert_eq!(tool.assess(&args).unwrap().level, RiskLevel::Medium);
        let id = start(&tool, args).await;
        let job = ended(&t, id).await;
        assert_eq!(job.state, JobState::Done, "{}", job.text);
        assert_eq!(
            job.text,
            format!("Готово 2 из 2: результаты в {}.", dir.display()),
            "the transcripts themselves are not repeated to the model"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("раз.txt")).unwrap(),
            "привет\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("два.txt")).unwrap(),
            "привет\n"
        );
        assert_eq!(job.data.unwrap()["files"].as_array().unwrap().len(), 2);
    }

    fn refused(r: Result<ToolOutput, ToolError>) -> String {
        match r {
            Err(ToolError::Failed(m)) => m,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn the_job_takes_only_the_files_the_card_showed() {
        let t = av();
        let dir = folder(&t, "Записи");
        mp3(&t, &dir, "раз.mp3", 120.0);
        mp3(&t, &dir, "два.mp3", 120.0);
        let tool = AvFolderTool::new(t.av());
        let args = call(&dir, "transcribe", json!({ "subfolder": "Текст" }));
        let run = |args: Value| tool.run(args, Approval::for_tests(), CancellationToken::new());

        // Without a card nothing runs.
        assert_eq!(refused(run(args.clone()).await), NOT_SHOWN);

        // The card: two files, four minutes, a medium risk.
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert!(a.summary.contains("в 2 файлах"), "{}", a.summary);
        // While it waits, a two-hour recording finishes copying into the folder.
        mp3(&t, &dir, "лекция.mp3", 7200.0);
        assert_eq!(refused(run(args.clone()).await), FOLDER_CHANGED);
        assert!(t.av().jobs.list().is_empty(), "nothing was started");
        assert!(!dir.join("Текст").exists(), "and nothing was made");
        // The card is used up: the same call needs a new one.
        assert_eq!(refused(run(args.clone()).await), NOT_SHOWN);

        // Asked again, the card tells the truth about the folder: a high
        // risk with the minutes in its first line.
        let again = tool.assess(&args).unwrap();
        assert_eq!(again.level, RiskLevel::High);
        assert!(again.summary.contains("124 мин"), "{}", again.summary);

        // A file that is replaced while the card waits is refused as well.
        std::fs::remove_file(dir.join("лекция.mp3")).unwrap();
        tool.assess(&args).unwrap();
        std::fs::write(dir.join("раз.mp3"), b"another file of another size").unwrap();
        assert_eq!(refused(run(args.clone()).await), FOLDER_CHANGED);
        // And one that is gone.
        tool.assess(&args).unwrap();
        std::fs::remove_file(dir.join("два.mp3")).unwrap();
        let r = run(args).await;
        assert!(
            matches!(&r, Err(ToolError::BadArguments(m)) if m.starts_with("в папке нет подходящих файлов")),
            "{r:?}"
        );
        assert!(t.av().jobs.list().is_empty());
    }

    #[tokio::test]
    async fn a_card_serves_one_run_and_only_its_own_call() {
        let t = av();
        let dir = folder(&t, "v");
        mkv(&t, &dir, "a.mkv", 60.0);
        let tool = AvFolderTool::new(t.av());
        let args = call(&dir, "convert", json!({ "format": "mp4" }));
        tool.assess(&args).unwrap();
        // Another call over the same folder has no card of its own.
        let other = call(&dir, "convert", json!({ "format": "mov" }));
        let r = tool
            .run(other, Approval::for_tests(), CancellationToken::new())
            .await;
        assert_eq!(refused(r), NOT_SHOWN);
        let id = start(&tool, args.clone()).await;
        assert_eq!(ended(&t, id).await.state, JobState::Done);
        let r = tool
            .run(args, Approval::for_tests(), CancellationToken::new())
            .await;
        assert_eq!(refused(r), NOT_SHOWN, "the card was used by the first run");
    }

    #[tokio::test]
    async fn a_file_that_changed_while_the_job_waited_is_passed_by() {
        let t = av();
        let dir = folder(&t, "v");
        for name in ["a.mkv", "b.mkv", "c.mkv"] {
            mkv(&t, &dir, name, 60.0);
        }
        let jobs = t.av().jobs;
        // Another job holds the queue.
        let release = CancellationToken::new();
        let gate = release.clone();
        let hold = move |_ctx: JobCtx| -> mika_core::bgjobs::JobFuture {
            Box::pin(async move {
                gate.cancelled().await;
                Ok(ToolOutput::new("готово"))
            })
        };
        jobs.submit(JobSpec::queued("t", "занято"), Box::new(hold))
            .unwrap();
        let tool = AvFolderTool::new(t.av());
        let id = start(&tool, call(&dir, "convert", json!({ "format": "mp4" }))).await;
        assert_eq!(jobs.get(id).unwrap().state, JobState::Queued);

        // While it waits, another file is put in the place of b.
        std::fs::write(dir.join("b.mkv"), b"something else, longer than before").unwrap();
        let before = t.ffmpeg_runs().len();
        release.cancel();
        let job = ended(&t, id).await;
        assert_eq!(job.state, JobState::Done, "{}", job.text);
        assert_eq!(
            job.text,
            format!(
                "Готово 2 из 3: результаты в {}. Не получилось: b.mkv — файл изменился после подтверждения.",
                dir.display()
            )
        );
        assert!(dir.join("a.mp4").is_file() && dir.join("c.mp4").is_file());
        assert!(!dir.join("b.mp4").exists());
        let seen_b = t.ffmpeg_runs()[before..]
            .iter()
            .any(|a| a.iter().any(|x| x.contains("b.mkv")));
        assert!(!seen_b, "FFmpeg never saw the new file");
    }

    #[tokio::test]
    async fn texts_of_one_name_in_a_subfolder_are_counted_up() {
        let t = av();
        let dir = folder(&t, "Заметки");
        // A video and its sound track: one stem, two files.
        mp3(&t, &dir, "встреча.mp3", 20.0);
        clip(
            &t,
            &dir,
            "встреча.mp4",
            &probe_json(
                "mov,mp4",
                20.0,
                3 * MB,
                Some(("h264", 1280, 720)),
                Some("aac"),
            ),
        );
        *t.f.stt_text.lock() = "привет".into();
        let wav = super::super::wav::slice_wav(&[0u8; 32_000], 0.0, 1.0);
        *t.f.run_write.lock() = Some(wav);
        let tool = AvFolderTool::new(t.av());
        let args = call(&dir, "transcribe", json!({ "subfolder": "Текст" }));
        let id = start(&tool, args.clone()).await;
        let job = ended(&t, id).await;
        let out = dir.join("Текст");
        assert_eq!(job.state, JobState::Done, "{}", job.text);
        assert_eq!(
            job.text,
            format!("Готово 2 из 2: результаты в {}.", out.display())
        );
        assert!(out.join("встреча.txt").is_file());
        assert!(out.join("встреча (2).txt").is_file());

        // The same call again: the names are taken, not refused.
        let a = tool.assess(&args).unwrap();
        assert!(a.summary.contains("в 2 файлах"), "{}", a.summary);
        let id = start(&tool, args).await;
        let job = ended(&t, id).await;
        assert_eq!(job.state, JobState::Done, "{}", job.text);
        assert!(out.join("встреча (3).txt").is_file());
        assert!(out.join("встреча (4).txt").is_file());
    }

    #[tokio::test]
    async fn a_full_queue_makes_no_folder() {
        let t = av();
        let dir = folder(&t, "v");
        mkv(&t, &dir, "a.mkv", 60.0);
        let jobs = t.av().jobs;
        let hold = |ctx: JobCtx| -> mika_core::bgjobs::JobFuture {
            Box::pin(async move {
                ctx.cancel.cancelled().await;
                Err(ToolError::Cancelled)
            })
        };
        for i in 0..=mika_core::bgjobs::MAX_QUEUED {
            jobs.submit(JobSpec::queued("t", format!("j{i}")), Box::new(hold))
                .unwrap();
        }
        let tool = AvFolderTool::new(t.av());
        let args = call(
            &dir,
            "convert",
            json!({ "format": "mp4", "subfolder": "Готово" }),
        );
        tool.assess(&args).unwrap();
        let r = tool
            .run(args, Approval::for_tests(), CancellationToken::new())
            .await;
        assert!(
            matches!(&r, Err(ToolError::Failed(m)) if m.starts_with("очередь фоновых заданий полна")),
            "{r:?}"
        );
        assert!(!dir.join("Готово").exists());
        jobs.shutdown(WAIT).await;
    }

    #[test]
    fn without_ffmpeg_the_download_card_comes_back() {
        let t = av();
        let dir = folder(&t, "v");
        *t.env.shared.ffmpeg_path.write() = "C:\\нет\\такой\\папки".into();
        // PATH or the download folder of this machine may hold one.
        if locate("C:\\нет\\такой\\папки").is_some() {
            return;
        }
        let tool = AvFolderTool::new(t.av());
        match tool.assess(&call(&dir, "convert", json!({ "format": "mp4" }))) {
            Err(ToolError::NotReady { data, .. }) => {
                assert_eq!(data, json!({ "kind": "ffmpeg_missing" }))
            }
            other => panic!("{other:?}"),
        }
    }
}
