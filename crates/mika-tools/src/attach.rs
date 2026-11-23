//! Files the user attaches to a chat message (decision 0053): read, checked
//! against the limits and kept as a copy in the folder of the conversation.
//! A picture is kept as PNG no bigger than `IMAGE_SIDE`; any other file as
//! its text, read the way the document search reads it (`docs::extract`).
//! The user picks the file himself: the blind zone of windows has nothing to
//! do with it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mika_core::attach::{
    clean_name, conversation_dir, folder_name, is_image_name, truncate_text, IMAGE_SIDE,
    MAX_FILE_BYTES, MAX_IMAGE_BYTES,
};
use mika_core::i18n::tr_args;
use mika_core::llm::{Attachment, AttachmentKind};

/// Why a file was not attached.
#[derive(Debug, Clone, PartialEq)]
pub enum AttachError {
    /// Bigger than its limit, megabytes.
    TooBig { limit_mb: u64 },
    /// A folder, not a file.
    Folder,
    /// Neither a picture nor text nor a document read here.
    NotSupported,
    /// A document without text: a scan with no recognition language, an empty file.
    NoText,
    /// Windows could not read the picture.
    BadPicture,
    /// Reading or keeping failed; the reason is the system's.
    Failed(String),
}

impl AttachError {
    /// The line the chat shows, in the interface language.
    pub fn message(&self, name: &str) -> String {
        match self {
            AttachError::TooBig { limit_mb } => tr_args(
                "chat.attach_err_big",
                &[("name", name), ("mb", &limit_mb.to_string())],
            ),
            AttachError::Folder => tr_args("chat.attach_err_folder", &[("name", name)]),
            AttachError::NotSupported => tr_args("chat.attach_err_kind", &[("name", name)]),
            AttachError::NoText => tr_args("chat.attach_err_empty", &[("name", name)]),
            AttachError::BadPicture => tr_args("chat.attach_err_picture", &[("name", name)]),
            AttachError::Failed(why) => {
                tr_args("chat.attach_err_read", &[("name", name), ("error", why)])
            }
        }
    }

    /// The kind of the refusal as the journal names it.
    pub fn kind(&self) -> &'static str {
        match self {
            AttachError::TooBig { .. } => "too_big",
            AttachError::Folder => "folder",
            AttachError::NotSupported => "not_supported",
            AttachError::NoText => "no_text",
            AttachError::BadPicture => "bad_picture",
            AttachError::Failed(_) => "failed",
        }
    }
}

/// A file kept for a message.
#[derive(Debug, Clone, PartialEq)]
pub struct Kept {
    pub attachment: Attachment,
    /// The text was longer than `MAX_TEXT_CHARS` and was cut.
    pub cut: bool,
}

/// A name for a kept copy, new in this run and after a restart.
fn new_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S%3f"),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Whether the first bytes are of a picture Windows reads.
fn looks_like_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G'])
        || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(b"GIF8")
        || bytes.starts_with(b"BM")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
}

fn megabytes(bytes: u64) -> u64 {
    bytes / 1024 / 1024
}

/// Where the copies of `conversation` go, made when missing.
fn folder(root: &Path, conversation: &str) -> Result<(PathBuf, String), AttachError> {
    let dir = conversation_dir(root, conversation);
    std::fs::create_dir_all(&dir).map_err(|e| AttachError::Failed(e.to_string()))?;
    Ok((dir, folder_name(conversation)))
}

/// Attaches the file at `src` to the next message of `conversation`. Slow
/// for a PDF (each page is recognised): call it off the async threads.
pub fn from_path(src: &Path, root: &Path, conversation: &str) -> Result<Kept, AttachError> {
    let name = clean_name(&src.to_string_lossy());
    let meta = std::fs::metadata(src).map_err(|e| AttachError::Failed(e.to_string()))?;
    if meta.is_dir() {
        return Err(AttachError::Folder);
    }
    if is_image_name(&name) {
        if meta.len() > MAX_IMAGE_BYTES {
            return Err(AttachError::TooBig {
                limit_mb: megabytes(MAX_IMAGE_BYTES),
            });
        }
        let bytes = std::fs::read(src).map_err(|e| AttachError::Failed(e.to_string()))?;
        return keep_picture(bytes, name, root, conversation);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(AttachError::TooBig {
            limit_mb: megabytes(MAX_FILE_BYTES),
        });
    }
    keep_text(src, name, root, conversation)
}

/// Attaches a file that came as bytes: a picture pasted from the clipboard
/// or a file pasted into the field. `name` is what the webview called it.
pub fn from_bytes(
    name: &str,
    bytes: Vec<u8>,
    root: &Path,
    conversation: &str,
) -> Result<Kept, AttachError> {
    let name = clean_name(name);
    let size = bytes.len() as u64;
    if is_image_name(&name) || looks_like_image(&bytes) {
        if size > MAX_IMAGE_BYTES {
            return Err(AttachError::TooBig {
                limit_mb: megabytes(MAX_IMAGE_BYTES),
            });
        }
        return keep_picture(bytes, name, root, conversation);
    }
    if size > MAX_FILE_BYTES {
        return Err(AttachError::TooBig {
            limit_mb: megabytes(MAX_FILE_BYTES),
        });
    }
    // The extractor reads files: the bytes wait in the folder meanwhile.
    let (dir, _) = folder(root, conversation)?;
    let ext = Path::new(&name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .filter(|e| e.len() <= 10 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "txt".into());
    let tmp = dir.join(format!("{}.part.{ext}", new_id()));
    std::fs::write(&tmp, &bytes).map_err(|e| AttachError::Failed(e.to_string()))?;
    let kept = keep_text(&tmp, name, root, conversation);
    let _ = std::fs::remove_file(&tmp);
    kept
}

fn keep_picture(
    bytes: Vec<u8>,
    name: String,
    root: &Path,
    conversation: &str,
) -> Result<Kept, AttachError> {
    let png = crate::win::picture::to_png(bytes, IMAGE_SIDE).map_err(|e| {
        log::info!(
            target: "tool",
            "attached picture not read: error={}",
            mika_core::journal::guarded(&e)
        );
        AttachError::BadPicture
    })?;
    let (dir, folder) = folder(root, conversation)?;
    let id = new_id();
    std::fs::write(dir.join(format!("{id}.png")), &png.bytes)
        .map_err(|e| AttachError::Failed(e.to_string()))?;
    Ok(Kept {
        attachment: Attachment {
            kind: AttachmentKind::Image,
            name,
            path: format!("{folder}/{id}.png"),
            description: None,
        },
        cut: false,
    })
}

fn keep_text(
    src: &Path,
    name: String,
    root: &Path,
    conversation: &str,
) -> Result<Kept, AttachError> {
    let pages = crate::docs::extract(src).map_err(|e| {
        if e == "в файле не текст" {
            AttachError::NotSupported
        } else {
            AttachError::Failed(e)
        }
    })?;
    let numbered = pages.len() > 1;
    let text = pages
        .into_iter()
        .filter(|(_, t)| !t.trim().is_empty())
        .map(|(n, t)| {
            if numbered {
                format!("[страница {n}]\n{}", t.trim_end())
            } else {
                t.trim_end().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Err(AttachError::NoText);
    }
    let (text, cut) = truncate_text(text);
    let (dir, folder) = folder(root, conversation)?;
    let id = new_id();
    std::fs::write(dir.join(format!("{id}.txt")), text.as_bytes())
        .map_err(|e| AttachError::Failed(e.to_string()))?;
    Ok(Kept {
        attachment: Attachment {
            kind: AttachmentKind::Text,
            name,
            path: format!("{folder}/{id}.txt"),
            description: None,
        },
        cut,
    })
}

/// Removes the kept copy of an attachment that was taken off before sending.
pub fn discard(root: &Path, attachment: &Attachment) {
    if let Some(file) = mika_core::attach::resolve(root, &attachment.path) {
        let _ = std::fs::remove_file(file);
    }
}

/// Moves the kept copy into the folder of `conversation` when the chat
/// changed conversation between attaching and sending. The attachment then
/// names its new place.
pub fn move_to(root: &Path, attachment: &mut Attachment, conversation: &str) -> Result<(), String> {
    let wanted = folder_name(conversation);
    let Some(file) = mika_core::attach::resolve(root, &attachment.path) else {
        return Err("bad attachment path".into());
    };
    let Some(file_name) = file.file_name().map(|f| f.to_string_lossy().into_owned()) else {
        return Err("bad attachment path".into());
    };
    if attachment.path == format!("{wanted}/{file_name}") {
        return Ok(());
    }
    let dir = conversation_dir(root, conversation);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::rename(&file, dir.join(&file_name)).map_err(|e| e.to_string())?;
    attachment.path = format!("{wanted}/{file_name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::attach::{resolve, MAX_TEXT_CHARS};

    fn root() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("attachments");
        (d, root)
    }

    fn kept_text(root: &Path, a: &Attachment) -> String {
        std::fs::read_to_string(resolve(root, &a.path).unwrap()).unwrap()
    }

    #[test]
    fn a_pasted_picture_becomes_a_smaller_png() {
        let (_d, root) = root();
        let rgb = vec![200u8; 2000 * 3000 * 3];
        let big = crate::win::picture::tests_png(&rgb, 2000, 3000);
        let kept = from_bytes("image.png", big, &root, "c-1").unwrap();
        assert_eq!(kept.attachment.kind, AttachmentKind::Image);
        assert_eq!(kept.attachment.name, "image.png");
        assert!(kept.attachment.path.starts_with("c-1/"));
        let file = resolve(&root, &kept.attachment.path).unwrap();
        let reader = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(file).unwrap()))
            .read_info()
            .unwrap();
        assert_eq!((reader.info().width, reader.info().height), (1067, 1600));
    }

    #[test]
    fn a_picture_without_its_extension_is_known_by_its_bytes() {
        let (_d, root) = root();
        let png = crate::win::picture::tests_png(&[1, 2, 3], 1, 1);
        let kept = from_bytes("вставка", png, &root, "c").unwrap();
        assert_eq!(kept.attachment.kind, AttachmentKind::Image);
    }

    #[test]
    fn a_broken_picture_is_refused() {
        let (_d, root) = root();
        assert_eq!(
            from_bytes("кот.jpg", b"not a picture".to_vec(), &root, "c").unwrap_err(),
            AttachError::BadPicture
        );
    }

    #[test]
    fn text_goes_in_and_a_long_one_is_cut() {
        let (_d, root) = root();
        let kept = from_bytes("заметки.md", "# Список\n- хлеб\n".into(), &root, "c").unwrap();
        assert_eq!(kept.attachment.kind, AttachmentKind::Text);
        assert!(!kept.cut);
        assert_eq!(kept_text(&root, &kept.attachment), "# Список\n- хлеб");
        let long = "слово ".repeat(MAX_TEXT_CHARS / 3);
        let kept = from_bytes("long.log", long.into_bytes(), &root, "c").unwrap();
        assert!(kept.cut);
        let text = kept_text(&root, &kept.attachment);
        assert!(text.contains("файл обрезан"));
        assert!(text.chars().count() < MAX_TEXT_CHARS + 100);
        // The bytes waited in the folder only while they were read.
        let left: Vec<String> = std::fs::read_dir(root.join("c"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(left
            .iter()
            .all(|n| n.ends_with(".txt") && !n.contains(".part.")));
    }

    #[test]
    fn russian_text_of_windows_1251_is_read() {
        let (_d, root) = root();
        // «Привет» in Windows-1251.
        let bytes = vec![0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        let kept = from_bytes("old.csv", bytes, &root, "c").unwrap();
        assert_eq!(kept_text(&root, &kept.attachment), "Привет");
    }

    #[test]
    fn word_and_excel_go_through_the_document_reader() {
        let (d, root) = root();
        let docx = crate::docs::tests::zip_of(&[(
            "word/document.xml",
            "<w:document><w:body><w:p><w:r><w:t>Договор</w:t></w:r></w:p></w:body></w:document>",
        )]);
        let path = d.path().join("Договор.docx");
        std::fs::write(&path, docx).unwrap();
        let kept = from_path(&path, &root, "c").unwrap();
        assert_eq!(kept.attachment.name, "Договор.docx");
        assert_eq!(kept_text(&root, &kept.attachment), "Договор");
        let kept = from_bytes("Отчёт.xlsx", crate::docs::tests::xlsx(), &root, "c").unwrap();
        assert!(kept_text(&root, &kept.attachment).starts_with("## Итоги & план\nМесяц\tСумма"));
    }

    #[test]
    #[ignore = "needs a PDF: MIKA_TEST_PDF, and a recognition language in Windows"]
    fn a_pdf_goes_through_the_document_reader() {
        let (_d, root) = root();
        let pdf = PathBuf::from(std::env::var("MIKA_TEST_PDF").unwrap());
        let kept = from_path(&pdf, &root, "c").unwrap();
        assert!(!kept_text(&root, &kept.attachment).trim().is_empty());
    }

    #[test]
    fn files_that_are_no_text_and_folders_are_refused() {
        let (d, root) = root();
        let exe = d.path().join("app.exe");
        std::fs::write(&exe, b"MZ\x90\x00\x03\x00\x00\x00").unwrap();
        assert_eq!(
            from_path(&exe, &root, "c").unwrap_err(),
            AttachError::NotSupported
        );
        assert_eq!(
            from_path(d.path(), &root, "c").unwrap_err(),
            AttachError::Folder
        );
        assert_eq!(
            from_bytes("пусто.txt", b"  \n".to_vec(), &root, "c").unwrap_err(),
            AttachError::NoText
        );
    }

    #[test]
    fn limits_refuse_big_files_before_reading_them() {
        let (_d, root) = root();
        let big = vec![b'a'; MAX_FILE_BYTES as usize + 1];
        assert_eq!(
            from_bytes("big.txt", big, &root, "c").unwrap_err(),
            AttachError::TooBig { limit_mb: 50 }
        );
        let mut picture = vec![0u8; MAX_IMAGE_BYTES as usize + 1];
        picture[..4].copy_from_slice(&[0x89, b'P', b'N', b'G']);
        assert_eq!(
            from_bytes("big.png", picture, &root, "c").unwrap_err(),
            AttachError::TooBig { limit_mb: 20 }
        );
        assert!(!root.join("c").exists(), "nothing was kept");
    }

    #[test]
    fn refusals_are_plain_lines() {
        let line = AttachError::TooBig { limit_mb: 20 }.message("кот.png");
        assert!(line.contains("кот.png") && line.contains("20"), "{line}");
        for e in [
            AttachError::Folder,
            AttachError::NotSupported,
            AttachError::NoText,
            AttachError::BadPicture,
            AttachError::Failed("нет доступа".into()),
        ] {
            let line = e.message("a.bin");
            assert!(
                line.contains("a.bin") && !line.starts_with("chat."),
                "{line}"
            );
        }
    }

    #[test]
    fn a_copy_moves_with_the_conversation_and_can_be_discarded() {
        let (_d, root) = root();
        let mut a = from_bytes("a.txt", b"x".to_vec(), &root, "old")
            .unwrap()
            .attachment;
        move_to(&root, &mut a, "new").unwrap();
        assert!(a.path.starts_with("new/"));
        assert!(resolve(&root, &a.path).unwrap().exists());
        move_to(&root, &mut a, "new").unwrap();
        discard(&root, &a);
        assert!(!resolve(&root, &a.path).unwrap().exists());
    }
}
