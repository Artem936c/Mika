//! Files attached to the message being typed (decision 0053). The list lives
//! here, not in the webview: the chat only draws it, so it outlives the
//! chat folding into the island, and the webview never names a file. A file
//! comes from the Windows dialog, from a drop onto the window (Windows gives
//! the paths to this side) or as bytes pasted into the field. The copies
//! wait in a folder of their own (`mika_core::attach::WAITING`) and move to
//! the conversation only when the message is sent.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mika_core::attach::MAX_PER_MESSAGE;
use mika_core::llm::{Attachment, AttachmentKind};
use mika_tools::attach::{AttachError, Kept};
use parking_lot::Mutex;
use tauri::Emitter;

use crate::app::Ctx;
use crate::strings::tr;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// The event the chat redraws the list from after a drop or a send.
pub const CHANGED_EVENT: &str = "attachments-changed";

/// One file waiting to be sent.
#[derive(Debug, Clone, PartialEq)]
struct Waiting {
    id: u64,
    attachment: Attachment,
    cut: bool,
}

/// The files of the message being typed.
#[derive(Default)]
pub struct Pending {
    files: Mutex<Vec<Waiting>>,
    next: Mutex<u64>,
    /// A picture was sent in this run: the hint about the provider is said.
    picture_sent: AtomicBool,
}

/// A chip under the field.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Chip {
    id: u64,
    kind: AttachmentKind,
    name: String,
    /// The text was longer than the limit and was cut.
    cut: bool,
}

/// What the chat draws: the chips, the lines of the files that were refused
/// just now, and whether to say that a picture goes to the provider.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AttachView {
    items: Vec<Chip>,
    refused: Vec<String>,
    hint: bool,
}

impl Pending {
    /// The view with `refused` lines.
    fn view(&self, refused: Vec<String>) -> AttachView {
        let files = self.files.lock();
        let hint = !self.picture_sent.load(Ordering::Relaxed)
            && files
                .iter()
                .any(|f| f.attachment.kind == AttachmentKind::Image);
        AttachView {
            items: files
                .iter()
                .map(|f| Chip {
                    id: f.id,
                    kind: f.attachment.kind,
                    name: f.attachment.name.clone(),
                    cut: f.cut,
                })
                .collect(),
            refused,
            hint,
        }
    }

    /// Room for how many more files.
    fn room(&self) -> usize {
        MAX_PER_MESSAGE.saturating_sub(self.files.lock().len())
    }

    /// Adds a kept file; false when the message is full already.
    fn add(&self, kept: Kept) -> bool {
        let mut files = self.files.lock();
        if files.len() >= MAX_PER_MESSAGE {
            return false;
        }
        let mut next = self.next.lock();
        *next += 1;
        files.push(Waiting {
            id: *next,
            attachment: kept.attachment,
            cut: kept.cut,
        });
        true
    }

    /// Takes a file off the list.
    fn remove(&self, id: u64) -> Option<Attachment> {
        let mut files = self.files.lock();
        let at = files.iter().position(|f| f.id == id)?;
        Some(files.remove(at).attachment)
    }

    /// Takes every file for sending; `put_back` returns them if the message
    /// did not go.
    fn take(&self) -> Vec<Waiting> {
        std::mem::take(&mut *self.files.lock())
    }

    /// Returns the files that no longer fit: their copies are to be deleted.
    fn put_back(&self, mut taken: Vec<Waiting>) -> Vec<Attachment> {
        let mut files = self.files.lock();
        taken.append(&mut files);
        let over = taken
            .split_off(MAX_PER_MESSAGE.min(taken.len()))
            .into_iter()
            .map(|w| w.attachment)
            .collect();
        *files = taken;
        over
    }

    /// The files waiting now.
    fn attachments(&self) -> Vec<Attachment> {
        self.files
            .lock()
            .iter()
            .map(|w| w.attachment.clone())
            .collect()
    }
}

fn refusal_of_many(name: &str) -> String {
    mika_core::i18n::tr_args(
        "chat.attach_err_many",
        &[("max", &MAX_PER_MESSAGE.to_string()), ("name", name)],
    )
}

/// The folder of the copies and the conversation open now.
fn place(ctx: &Ctx) -> Option<(PathBuf, String)> {
    let root = ctx.db.attachments_dir()?.to_path_buf();
    Some((root, ctx.session.agent.conversation()?))
}

/// Where a new copy goes: the waiting folder, tied to no conversation, so
/// deleting a conversation never takes a file that waits under the field.
fn waiting_place(ctx: &Ctx) -> Option<(PathBuf, &'static str)> {
    place(ctx).map(|(root, _)| (root, mika_core::attach::WAITING))
}

/// Notes a refusal in the journal: the kind, never the name of the file.
fn log_refusal(e: &AttachError) {
    log::info!(target: "tool", "file not attached: reason={}", e.kind());
}

/// Notes a kept file in the journal: its kind, and its name only while texts are written.
fn log_kept(kept: &Kept) {
    log::info!(
        target: "tool",
        "file attached: kind={:?} cut={} name={}",
        kept.attachment.kind,
        kept.cut,
        mika_core::journal::content(&kept.attachment.name)
    );
}

/// Attaches files from their paths. Blocking: PDF pages are recognised.
pub fn attach_paths(ctx: &Ctx, paths: Vec<PathBuf>) -> AttachView {
    let mut refused = Vec::new();
    let Some((root, conversation)) = waiting_place(ctx) else {
        return ctx.attachments.view(vec![tr("error.attach_no_store")]);
    };
    for path in paths {
        let name = mika_core::attach::clean_name(&path.to_string_lossy());
        if ctx.attachments.room() == 0 {
            refused.push(refusal_of_many(&name));
            continue;
        }
        match mika_tools::attach::from_path(&path, &root, conversation) {
            Ok(kept) => {
                log_kept(&kept);
                let copy = kept.attachment.clone();
                if !ctx.attachments.add(kept) {
                    mika_tools::attach::discard(&root, &copy);
                    refused.push(refusal_of_many(&name));
                }
            }
            Err(e) => {
                log_refusal(&e);
                refused.push(e.message(&name));
            }
        }
    }
    ctx.attachments.view(refused)
}

/// Files dropped onto the window: attached off the event thread, and the
/// chat redraws from the event.
pub fn on_drop(app: &tauri::AppHandle, ctx: Arc<Ctx>, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let worker = ctx.clone();
        let got = tauri::async_runtime::spawn_blocking(move || attach_paths(&worker, paths))
            .await
            .map_err(|e| e.to_string());
        let view = dropped_view(&ctx.attachments, got);
        if let Err(e) = app.emit(CHANGED_EVENT, &view) {
            log::warn!(target: "app", "the chat was not told of dropped files: {e}");
        }
    });
}

/// What the chat draws after a drop. It is told even when attaching
/// failed: it waits for this event to let the paperclip go.
fn dropped_view(pending: &Pending, got: Result<AttachView, String>) -> AttachView {
    got.unwrap_or_else(|e| {
        log::warn!(target: "app", "dropped files not attached: {e}");
        pending.view(vec![tr("chat.attach_err_drop")])
    })
}

/// The paperclip: the Windows dialog, several files at once.
#[tauri::command]
pub async fn pick_attachments(
    window: tauri::WebviewWindow,
    ctx: CtxState<'_>,
) -> Result<AttachView, String> {
    let owner = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let known = format!(
            "*.{}",
            [
                "png", "jpg", "jpeg", "webp", "gif", "bmp", "txt", "md", "csv", "json", "log",
                "pdf", "docx", "xlsx",
            ]
            .join(";*.")
        );
        let paths = crate::file_dialog::pick_open_many(
            owner,
            &tr("chat.attach_dialog"),
            &[
                (&tr("chat.attach_filter_known"), &known),
                (&tr("chat.attach_filter_all"), "*.*"),
            ],
        )?;
        Ok(attach_paths(&ctx, paths))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Decodes the name sent in a header: the webview encodes it as a URI part.
fn decode_name(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A file pasted into the field: the bytes are the body of the request, the
/// name is in the header `x-name`. The webview gives content, not a path.
#[tauri::command]
pub async fn attach_bytes(
    request: tauri::ipc::Request<'_>,
    ctx: CtxState<'_>,
) -> Result<AttachView, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected the bytes of the file".into());
    };
    let bytes = bytes.clone();
    let name = request
        .headers()
        .get("x-name")
        .and_then(|v| v.to_str().ok())
        .map(decode_name)
        .unwrap_or_default();
    let name = mika_core::attach::clean_name(&name);
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some((root, conversation)) = waiting_place(&ctx) else {
            return ctx.attachments.view(vec![tr("error.attach_no_store")]);
        };
        if ctx.attachments.room() == 0 {
            return ctx.attachments.view(vec![refusal_of_many(&name)]);
        }
        let refused = match mika_tools::attach::from_bytes(&name, bytes, &root, conversation) {
            Ok(kept) => {
                log_kept(&kept);
                let copy = kept.attachment.clone();
                if ctx.attachments.add(kept) {
                    Vec::new()
                } else {
                    mika_tools::attach::discard(&root, &copy);
                    vec![refusal_of_many(&name)]
                }
            }
            Err(e) => {
                log_refusal(&e);
                vec![e.message(&name)]
            }
        };
        ctx.attachments.view(refused)
    })
    .await
    .map_err(|e| e.to_string())
}

/// The × on a chip: the file leaves the message and its copy is removed.
#[tauri::command]
pub fn remove_attachment(ctx: CtxState<'_>, id: u64) -> AttachView {
    if let Some(a) = ctx.attachments.remove(id) {
        if let Some(root) = ctx.db.attachments_dir() {
            mika_tools::attach::discard(root, &a);
        }
    }
    ctx.attachments.view(Vec::new())
}

/// The chips as they are, for a chat that opens again.
#[tauri::command]
pub fn list_attachments(ctx: CtxState<'_>) -> AttachView {
    ctx.attachments.view(Vec::new())
}

/// Moves the copies of `files` from the waiting folder into the folder of
/// `conversation`, all or none: when one cannot move (its copy is gone, the
/// disk refuses), those moved already go back to wait, and the error gives
/// the name of that file and the files as they are now.
fn ready_to_send(
    root: &Path,
    conversation: &str,
    mut files: Vec<Attachment>,
) -> Result<Vec<Attachment>, (String, Vec<Attachment>)> {
    let mut failed = None;
    for (i, a) in files.iter_mut().enumerate() {
        if let Err(e) = mika_tools::attach::move_to(root, a, conversation) {
            log::warn!(target: "tool", "attached file not moved to its conversation: {e}");
            failed = Some((i, a.name.clone()));
            break;
        }
    }
    match failed {
        Some((i, name)) => {
            wait_again(root, &mut files[..i]);
            Err((name, files))
        }
        None => Ok(files),
    }
}

/// Copies of a message that did not go go back to the waiting folder.
fn wait_again(root: &Path, files: &mut [Attachment]) {
    for a in files {
        if let Err(e) = mika_tools::attach::move_to(root, a, mika_core::attach::WAITING) {
            log::warn!(target: "tool", "attached file not moved back to wait: {e}");
        }
    }
}

/// Returns `taken` with the attachments `files` to the list; copies that no
/// longer fit are deleted.
fn give_back(ctx: &Ctx, root: Option<&Path>, taken: Vec<Waiting>, files: Vec<Attachment>) {
    let back = taken
        .into_iter()
        .zip(files)
        .map(|(w, attachment)| Waiting { attachment, ..w })
        .collect();
    for a in ctx.attachments.put_back(back) {
        if let Some(root) = root {
            mika_tools::attach::discard(root, &a);
        }
    }
}

/// Removes copies in the waiting folder that no chip names, such as those
/// left by a run that ended with files under the field. A copy younger than
/// `fresh` stays: it may be on its way to the list. Returns how many went.
fn sweep_waiting(root: &Path, keep: &[Attachment], fresh: Duration) -> usize {
    let dir = mika_core::attach::conversation_dir(root, mika_core::attach::WAITING);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let named: HashSet<PathBuf> = keep
        .iter()
        .filter_map(|a| mika_core::attach::resolve(root, &a.path))
        .collect();
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let old = entry
            .metadata()
            .ok()
            .filter(std::fs::Metadata::is_file)
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= fresh);
        if old && !named.contains(&path) && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

impl Ctx {
    /// Removes the copies of the waiting folder no chip names (see `sweep_waiting`).
    pub fn sweep_waiting_files(&self) {
        let Some(root) = self.db.attachments_dir() else {
            return;
        };
        let n = sweep_waiting(
            root,
            &self.attachments.attachments(),
            Duration::from_secs(3600),
        );
        if n > 0 {
            log::info!(target: "app", "left attachment copies removed: count={n}");
        }
    }
}

/// Sends `text` with the files waiting (see `Session::submit_with_attachments`).
/// The copies move from the waiting folder to the conversation open now; if
/// one cannot move, nothing is sent and the chat says which file. If the
/// message did not go, they wait again. Must run inside the async runtime.
/// The view to draw when there were files.
pub fn send_with_files(ctx: &Ctx, text: &str) -> Option<AttachView> {
    let taken = ctx.attachments.take();
    if taken.is_empty() {
        ctx.session.submit(text);
        return None;
    }
    let files: Vec<Attachment> = taken.iter().map(|w| w.attachment.clone()).collect();
    let Some((root, conversation)) = place(ctx) else {
        give_back(ctx, None, taken, files);
        return Some(ctx.attachments.view(vec![tr("error.attach_no_store")]));
    };
    let mut files = match ready_to_send(&root, &conversation, files) {
        Ok(files) => files,
        Err((name, files)) => {
            give_back(ctx, Some(&root), taken, files);
            let line = mika_core::i18n::tr_args("chat.attach_err_send", &[("name", &name)]);
            return Some(ctx.attachments.view(vec![line]));
        }
    };
    let pictures = files.iter().any(|a| a.kind == AttachmentKind::Image);
    match ctx.session.submit_with_attachments(text, files.clone()) {
        mika_core::session::Submitted::Started => {
            if pictures {
                ctx.attachments.picture_sent.store(true, Ordering::Relaxed);
            }
        }
        _ => {
            wait_again(&root, &mut files);
            give_back(ctx, Some(&root), taken, files);
        }
    }
    Some(ctx.attachments.view(Vec::new()))
}

/// Tells the chat the list changed.
pub fn changed(app: &tauri::AppHandle, view: &AttachView) {
    if let Err(e) = app.emit(CHANGED_EVENT, view) {
        log::warn!(target: "app", "the chat was not told of attached files: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(kind: AttachmentKind, name: &str) -> Kept {
        Kept {
            attachment: Attachment {
                kind,
                name: name.into(),
                path: format!("c/{name}"),
                description: None,
            },
            cut: false,
        }
    }

    #[test]
    fn a_message_takes_five_files() {
        let p = Pending::default();
        for i in 0..MAX_PER_MESSAGE {
            assert!(p.add(kept(AttachmentKind::Text, &format!("{i}.txt"))));
        }
        assert_eq!(p.room(), 0);
        assert!(!p.add(kept(AttachmentKind::Text, "6.txt")));
        assert_eq!(p.view(Vec::new()).items.len(), MAX_PER_MESSAGE);
        let line = refusal_of_many("6.txt");
        assert!(line.contains('5') && line.contains("6.txt"), "{line}");
    }

    #[test]
    fn the_hint_shows_with_a_picture_until_one_is_sent() {
        let p = Pending::default();
        p.add(kept(AttachmentKind::Text, "a.txt"));
        assert!(!p.view(Vec::new()).hint);
        p.add(kept(AttachmentKind::Image, "b.png"));
        assert!(p.view(Vec::new()).hint);
        p.picture_sent.store(true, Ordering::Relaxed);
        assert!(!p.view(Vec::new()).hint);
    }

    #[test]
    fn removed_and_returned_files() {
        let p = Pending::default();
        p.add(kept(AttachmentKind::Text, "a.txt"));
        p.add(kept(AttachmentKind::Image, "b.png"));
        let id = p.view(Vec::new()).items[0].id;
        assert_eq!(p.remove(id).unwrap().name, "a.txt");
        assert!(p.remove(id).is_none());
        let taken = p.take();
        assert!(p.view(Vec::new()).items.is_empty());
        p.add(kept(AttachmentKind::Text, "c.txt"));
        p.put_back(taken);
        let names: Vec<String> = p
            .view(Vec::new())
            .items
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["b.png", "c.txt"]);
    }

    #[test]
    fn files_that_no_longer_fit_come_back_to_be_deleted() {
        let p = Pending::default();
        for i in 0..4 {
            p.add(kept(AttachmentKind::Text, &format!("{i}.txt")));
        }
        let taken = p.take();
        // Meanwhile three more were attached.
        for name in ["a.txt", "b.txt", "c.txt"] {
            p.add(kept(AttachmentKind::Text, name));
        }
        let over: Vec<String> = p.put_back(taken).into_iter().map(|a| a.name).collect();
        assert_eq!(over, ["b.txt", "c.txt"]);
        assert_eq!(p.view(Vec::new()).items.len(), MAX_PER_MESSAGE);
    }

    #[test]
    fn a_failed_drop_still_tells_the_chat() {
        let p = Pending::default();
        p.add(kept(AttachmentKind::Text, "a.txt"));
        let view = dropped_view(&p, Err("panicked".into()));
        assert_eq!(view.items.len(), 1);
        assert_eq!(view.refused, [tr("chat.attach_err_drop")]);
        let fine = p.view(Vec::new());
        assert_eq!(dropped_view(&p, Ok(fine.clone())), fine);
    }

    /// A kept copy in the waiting folder of `root`.
    fn waiting_file(root: &std::path::Path, name: &str) -> Attachment {
        let folder = mika_core::attach::folder_name(mika_core::attach::WAITING);
        let dir = root.join(&folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), b"x").unwrap();
        Attachment {
            kind: AttachmentKind::Text,
            name: name.into(),
            path: format!("{folder}/{name}"),
            description: None,
        }
    }

    #[test]
    fn files_move_to_the_conversation_only_all_together() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let a = waiting_file(root, "1.txt");
        let b = waiting_file(root, "2.txt");
        let sent = ready_to_send(root, "c-1", vec![a.clone(), b.clone()]).unwrap();
        assert_eq!(sent[0].path, "c-1/1.txt");
        assert!(root.join("c-1").join("2.txt").exists());

        // One copy is gone: nothing goes, the other copy waits where it was.
        let a = waiting_file(root, "3.txt");
        let mut gone = waiting_file(root, "4.txt");
        std::fs::remove_file(mika_core::attach::resolve(root, &gone.path).unwrap()).unwrap();
        gone.name = "отчёт.txt".into();
        let (name, back) = ready_to_send(root, "c-1", vec![a.clone(), gone]).unwrap_err();
        assert_eq!(name, "отчёт.txt");
        assert_eq!(back[0].path, a.path);
        assert!(mika_core::attach::resolve(root, &a.path).unwrap().exists());
    }

    #[test]
    fn copies_no_chip_names_leave_the_waiting_folder() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let kept = waiting_file(root, "1.txt");
        let left = waiting_file(root, "2.txt");
        assert_eq!(
            sweep_waiting(root, std::slice::from_ref(&kept), std::time::Duration::ZERO),
            1
        );
        assert!(mika_core::attach::resolve(root, &kept.path)
            .unwrap()
            .exists());
        assert!(!mika_core::attach::resolve(root, &left.path)
            .unwrap()
            .exists());
        // A fresh copy may be on its way to the list: it stays.
        let fresh = waiting_file(root, "3.txt");
        assert_eq!(
            sweep_waiting(root, &[], std::time::Duration::from_secs(3600)),
            0
        );
        assert!(mika_core::attach::resolve(root, &fresh.path)
            .unwrap()
            .exists());
    }

    #[test]
    fn names_from_the_webview_are_decoded() {
        assert_eq!(decode_name("%D0%BA%D0%BE%D1%82.png"), "кот.png");
        assert_eq!(decode_name("a%20b%2"), "a b%2");
        assert_eq!(decode_name("100%"), "100%");
        assert_eq!(decode_name("plain.txt"), "plain.txt");
    }
}
