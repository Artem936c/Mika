//! Files attached to chat messages (decision 0053): the limits, where the
//! kept copies live, and how a stored message becomes what the provider
//! gets. Reading a file and turning a picture into PNG is the work of
//! `mika_tools::attach`; this module touches no platform.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::LlmConfig;
use crate::llm::{Attachment, AttachmentKind, ChatMessage, Role, Vision};

/// The folder of the copies inside the data folder, next to `mika.db`.
pub const FOLDER: &str = "attachments";
/// Files in one message.
pub const MAX_PER_MESSAGE: usize = 5;
/// A picture before it is made smaller, bytes.
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Any other file, bytes.
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// Text of one file put into the message, characters.
pub const MAX_TEXT_CHARS: usize = 100_000;
/// The long side of a kept picture, pixels.
pub const IMAGE_SIDE: u32 = 1600;
/// Messages whose files go to the model again: the newest three with
/// pictures send their pictures, the newest three with texts their texts.
pub const RECENT: usize = 3;
/// The longest file name kept, characters.
pub const MAX_NAME: usize = 120;
/// Where files wait until their message is sent, used as a conversation
/// id: no conversation has it, so deleting a conversation or the cleanup of
/// gone conversations never takes a file that waits under the field. Its
/// folder is a hash (`folder_name`), never a plain conversation id.
pub const WAITING: &str = ".waiting";

/// Picture files by extension: PNG, JPEG, WebP, GIF (the first frame), BMP.
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];

/// The question to the vision model when the main model gets no pictures.
pub const DESCRIBE_QUESTION: &str = "Это картинка, которую пользователь приложил к сообщению, а не снимок экрана. Опиши подробно, что на ней: предметы, люди, весь текст дословно, цифры, таблицы и графики. Не отвечай на вопросы о ней, только опиши.";

/// Whether a file name is of a picture.
pub fn is_image_name(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    IMAGE_EXTS.contains(&ext.as_str())
}

/// The name a chip shows: the last part of the path, no control characters,
/// at most `MAX_NAME` characters with the extension kept.
pub fn clean_name(name: &str) -> String {
    let last = name.rsplit(['\\', '/']).next().unwrap_or(name);
    let clean: String = last.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return "file".into();
    }
    if clean.chars().count() <= MAX_NAME {
        return clean.to_string();
    }
    let ext = Path::new(clean)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .filter(|e| e.chars().count() <= 10)
        .unwrap_or_default();
    let keep = MAX_NAME - ext.chars().count() - 2;
    let head: String = clean.chars().take(keep).collect();
    if ext.is_empty() {
        format!("{head}…")
    } else {
        format!("{head}….{ext}")
    }
}

/// The folder of a conversation inside the attachments folder. An id of
/// letters, digits, `-` and `_` is used as it is; any other (an id from a
/// copy for transfer may be anything) becomes a hash, so it never names a
/// folder outside.
pub fn folder_name(conversation: &str) -> String {
    let plain = !conversation.is_empty()
        && conversation.len() <= 100
        && conversation
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if plain {
        return conversation.to_string();
    }
    let hash = Sha256::digest(conversation.as_bytes());
    let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("x-{hex}")
}

/// The folder of the copies of `conversation`.
pub fn conversation_dir(root: &Path, conversation: &str) -> PathBuf {
    root.join(folder_name(conversation))
}

/// The kept copy named by `Attachment::path`. Only `<folder>/<file>` inside
/// `root` is taken: a stored path never reaches out of it, whatever a
/// message restored from a copy says.
pub fn resolve(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    let parts: Vec<&std::ffi::OsStr> = rel
        .components()
        .map(|c| match c {
            Component::Normal(p) => Some(p),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (parts.len() == 2).then(|| root.join(parts[0]).join(parts[1]))
}

/// Text cut to `MAX_TEXT_CHARS` with a note at the cut; true when cut.
pub fn truncate_text(text: &str) -> (String, bool) {
    truncate_text_to(text, MAX_TEXT_CHARS)
}

fn truncate_text_to(text: &str, max: usize) -> (String, bool) {
    let total = text.chars().count();
    if total <= max {
        return (text.to_string(), false);
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str(&format!(
        "\n\n[… файл обрезан: показаны первые {max} знаков из {total}]"
    ));
    (out, true)
}

/// What the chat shows of an attachment: the kind and the name, never the path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentCard {
    pub kind: AttachmentKind,
    pub name: String,
}

impl From<&Attachment> for AttachmentCard {
    fn from(a: &Attachment) -> Self {
        Self {
            kind: a.kind,
            name: a.name.clone(),
        }
    }
}

/// Cards of a message's attachments.
pub fn cards(attachments: &[Attachment]) -> Vec<AttachmentCard> {
    attachments.iter().map(AttachmentCard::from).collect()
}

/// Whether the main model gets the pictures themselves. The settings say
/// so: the vision model is «пусто — та же, что для ответов». A vision model
/// of its own means the main one may not see, and it describes the
/// pictures in words first (decision 0053).
pub fn main_sees(llm: &LlmConfig) -> bool {
    let vision = llm.vision_model.trim();
    vision.is_empty() || vision == llm.model.trim()
}

/// How pictures reach the model: the vision model and whether the main
/// model gets them itself, asked at every message.
#[derive(Clone)]
pub struct Eyes {
    pub vision: Arc<dyn Vision>,
    pub main_sees: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// The messages as the provider gets them (decision 0053). Each user message
/// with attachments gets a line per file: «[картинка: имя]» or «[файл: имя]».
/// The newest `RECENT` messages with pictures carry the pictures themselves
/// when `pictures` is on; a picture the vision model described carries the
/// description instead. The newest `RECENT` messages with texts carry the
/// texts. A copy that is gone is said to be gone. The attachment lists are
/// dropped from the copy: the provider never sees a path.
pub fn for_request(
    mut messages: Vec<ChatMessage>,
    root: Option<&Path>,
    pictures: bool,
) -> Vec<ChatMessage> {
    let newest = |kind: AttachmentKind| -> Vec<usize> {
        messages
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, m)| m.role == Role::User && m.attachments.iter().any(|a| a.kind == kind))
            .map(|(i, _)| i)
            .take(RECENT)
            .collect()
    };
    let fresh_images = newest(AttachmentKind::Image);
    let fresh_texts = newest(AttachmentKind::Text);
    for (i, m) in messages.iter_mut().enumerate() {
        if m.attachments.is_empty() {
            continue;
        }
        let attachments = std::mem::take(&mut m.attachments);
        if m.role != Role::User {
            continue;
        }
        let mut parts = Vec::new();
        for a in &attachments {
            let file = root.and_then(|r| resolve(r, &a.path));
            match a.kind {
                AttachmentKind::Image => {
                    if let Some(said) = a.description.as_deref().filter(|d| !d.trim().is_empty()) {
                        parts.push(format!(
                            "[картинка: {}. Что на ней: {}]",
                            a.name,
                            said.trim()
                        ));
                        continue;
                    }
                    if !(pictures && fresh_images.contains(&i)) {
                        parts.push(format!("[картинка: {}]", a.name));
                        continue;
                    }
                    match file.and_then(|f| std::fs::read(f).ok()) {
                        Some(png) => {
                            m.images.push(BASE64.encode(png));
                            parts.push(format!("[картинка: {}]", a.name));
                        }
                        None => parts.push(format!("[картинка: {} — файла больше нет]", a.name)),
                    }
                }
                AttachmentKind::Text => {
                    if !fresh_texts.contains(&i) {
                        parts.push(format!("[файл: {}]", a.name));
                        continue;
                    }
                    match file.and_then(|f| std::fs::read_to_string(f).ok()) {
                        Some(text) => parts.push(format!(
                            "[файл: {name}]\n{text}\n[конец файла: {name}]",
                            name = a.name,
                            text = text.trim_end()
                        )),
                        None => parts.push(format!("[файл: {} — файла больше нет]", a.name)),
                    }
                }
            }
        }
        if parts.is_empty() {
            continue;
        }
        let added = parts.join("\n\n");
        m.content = if m.content.trim().is_empty() {
            added
        } else {
            format!("{}\n\n{added}", m.content)
        };
    }
    messages
}

/// Removes the copies of a conversation. A folder that is not there is fine.
pub fn remove_conversation(root: &Path, conversation: &str) {
    let dir = conversation_dir(root, conversation);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => log::debug!(target: "app", "attachments of a deleted conversation removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!(
            target: "app",
            "cannot remove the attachments of a deleted conversation: {e}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(name: &str, path: &str) -> Attachment {
        Attachment {
            kind: AttachmentKind::Image,
            name: name.into(),
            path: path.into(),
            description: None,
        }
    }

    fn text_file(name: &str, path: &str) -> Attachment {
        Attachment {
            kind: AttachmentKind::Text,
            name: name.into(),
            path: path.into(),
            description: None,
        }
    }

    #[test]
    fn an_old_message_without_attachments_reads() {
        let old = r#"{"role":"user","content":"привет"}"#;
        let m: ChatMessage = serde_json::from_str(old).unwrap();
        assert!(m.attachments.is_empty());
        assert!(m.images.is_empty());
        // And a message without files is stored as before, with no new field.
        let back = serde_json::to_string(&ChatMessage::user("привет")).unwrap();
        assert_eq!(back, old);
    }

    #[test]
    fn a_message_with_attachments_survives_the_store_format() {
        let m = ChatMessage::user_with_attachments("что тут?", vec![image("кот.png", "c-1/a.png")]);
        let back: ChatMessage = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn text_is_cut_with_a_note() {
        let (short, cut) = truncate_text("коротко");
        assert_eq!((short.as_str(), cut), ("коротко", false));
        let long = "я".repeat(MAX_TEXT_CHARS + 10);
        let (out, cut) = truncate_text(&long);
        assert!(cut);
        assert!(out.starts_with(&"я".repeat(MAX_TEXT_CHARS)));
        assert!(!out.starts_with(&"я".repeat(MAX_TEXT_CHARS + 1)));
        assert!(out.contains("файл обрезан"));
        assert!(out.contains(&(MAX_TEXT_CHARS + 10).to_string()));
    }

    #[test]
    fn folder_names_stay_inside() {
        assert_eq!(
            folder_name("c-20261122-101010123-0"),
            "c-20261122-101010123-0"
        );
        assert_eq!(folder_name("main"), "main");
        for bad in ["..", "a/b", "a\\b", "C:", "", "имя"] {
            let f = folder_name(bad);
            assert!(f.starts_with("x-"), "{bad} -> {f}");
            assert_eq!(f.len(), 18);
        }
        assert_ne!(folder_name("a/b"), folder_name("a\\b"));
    }

    #[test]
    fn a_stored_path_never_leaves_the_folder() {
        let root = Path::new("C:\\data\\attachments");
        assert_eq!(
            resolve(root, "c-1/a.png"),
            Some(root.join("c-1").join("a.png"))
        );
        for bad in [
            "../a.png",
            "c-1/../../a.png",
            "C:\\Windows\\win.ini",
            "\\\\server\\share\\a",
            "a.png",
            "c-1/x/a.png",
            "",
        ] {
            assert_eq!(resolve(root, bad), None, "{bad}");
        }
    }

    #[test]
    fn names_are_cleaned_and_shortened() {
        assert_eq!(clean_name("C:\\Users\\me\\Отчёт.pdf"), "Отчёт.pdf");
        assert_eq!(clean_name("a\u{0}b.txt"), "ab.txt");
        assert_eq!(clean_name("  "), "file");
        let long = format!("{}.docx", "д".repeat(300));
        let short = clean_name(&long);
        assert!(short.ends_with(".docx"));
        assert!(short.chars().count() <= MAX_NAME);
    }

    #[test]
    fn picture_names_by_extension() {
        for n in ["a.PNG", "b.jpg", "c.jpeg", "d.webp", "e.gif", "f.bmp"] {
            assert!(is_image_name(n), "{n}");
        }
        for n in ["a.pdf", "b.txt", "png", "c.png.exe"] {
            assert!(!is_image_name(n), "{n}");
        }
    }

    #[test]
    fn the_main_model_sees_unless_a_vision_model_of_its_own_is_set() {
        let mut llm = LlmConfig::default();
        assert!(main_sees(&llm));
        llm.vision_model = format!(" {} ", llm.model);
        assert!(main_sees(&llm));
        llm.vision_model = "gpt-4o".into();
        llm.model = "deepseek-chat".into();
        assert!(!main_sees(&llm));
    }

    fn with_files() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(FOLDER);
        std::fs::create_dir_all(root.join("c")).unwrap();
        for i in 0..5 {
            std::fs::write(root.join("c").join(format!("{i}.png")), [i as u8; 3]).unwrap();
        }
        std::fs::write(root.join("c").join("t.txt"), "строки файла\n").unwrap();
        (dir, root)
    }

    #[test]
    fn only_the_newest_three_messages_with_pictures_carry_them() {
        let (_dir, root) = with_files();
        let mut messages = vec![ChatMessage::system("s")];
        for i in 0..5 {
            messages.push(ChatMessage::user_with_attachments(
                format!("вопрос {i}"),
                vec![image(&format!("p{i}.png"), &format!("c/{i}.png"))],
            ));
            messages.push(ChatMessage::assistant("ответ"));
        }
        let out = for_request(messages, Some(&root), true);
        let users: Vec<&ChatMessage> = out.iter().filter(|m| m.role == Role::User).collect();
        let with_pictures: Vec<usize> = users
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.images.is_empty())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(with_pictures, [2, 3, 4]);
        assert_eq!(users[4].images, [BASE64.encode([4u8; 3])]);
        assert_eq!(users[0].content, "вопрос 0\n\n[картинка: p0.png]");
        assert_eq!(users[4].content, "вопрос 4\n\n[картинка: p4.png]");
        assert!(out.iter().all(|m| m.attachments.is_empty()));
    }

    #[test]
    fn a_main_model_that_does_not_see_gets_names_and_descriptions() {
        let (_dir, root) = with_files();
        let mut described = image("кот.png", "c/0.png");
        described.description = Some("рыжий кот на диване".into());
        let messages = vec![ChatMessage::user_with_attachments(
            "",
            vec![described, image("пёс.png", "c/1.png")],
        )];
        let out = for_request(messages, Some(&root), false);
        assert!(out[0].images.is_empty());
        assert_eq!(
            out[0].content,
            "[картинка: кот.png. Что на ней: рыжий кот на диване]\n\n[картинка: пёс.png]"
        );
    }

    #[test]
    fn texts_go_in_for_the_newest_three_and_gone_files_are_said() {
        let (_dir, root) = with_files();
        let mut messages = Vec::new();
        for i in 0..4 {
            messages.push(ChatMessage::user_with_attachments(
                format!("q{i}"),
                vec![text_file("заметки.txt", "c/t.txt")],
            ));
        }
        messages.push(ChatMessage::user_with_attachments(
            "",
            vec![
                text_file("нет.txt", "c/none.txt"),
                image("нет.png", "c/none.png"),
            ],
        ));
        let out = for_request(messages, Some(&root), true);
        assert_eq!(out[0].content, "q0\n\n[файл: заметки.txt]");
        assert_eq!(out[1].content, "q1\n\n[файл: заметки.txt]");
        assert_eq!(
            out[3].content,
            "q3\n\n[файл: заметки.txt]\nстроки файла\n[конец файла: заметки.txt]"
        );
        assert_eq!(
            out[4].content,
            "[файл: нет.txt — файла больше нет]\n\n[картинка: нет.png — файла больше нет]"
        );
        assert!(out[4].images.is_empty());
    }

    #[test]
    fn without_a_folder_only_names_go() {
        let messages = vec![ChatMessage::user_with_attachments(
            "x",
            vec![image("a.png", "c/0.png")],
        )];
        let out = for_request(messages, None, true);
        assert_eq!(out[0].content, "x\n\n[картинка: a.png — файла больше нет]");
    }

    #[test]
    fn removing_a_conversation_removes_its_folder_only() {
        let (_dir, root) = with_files();
        std::fs::create_dir_all(root.join("other")).unwrap();
        remove_conversation(&root, "c");
        assert!(!root.join("c").exists());
        assert!(root.join("other").exists());
        // Twice is fine.
        remove_conversation(&root, "c");
    }
}
