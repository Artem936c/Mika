//! A message to a person in Telegram Desktop, by a fixed recipe instead of the
//! model wandering through a window that hides its text from UI Automation.
//!
//! The chat is found through the global search (Esc closes the open chat and
//! its search, Ctrl+F then searches all chats). Before the text is sent, the
//! top strip of the chat pane is read on this computer by OCR before and after
//! the search result is opened: the name must appear there only after. So the
//! query typed into the search box, which may reach into the strip, never
//! passes for the header. When the screen may not be read, or the check fails,
//! the chat is only opened and the user is asked; a second call with
//! `chat_open` sends.

use std::time::Duration;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::screen::ScreenEnv;
use crate::sys::WindowInfo;
use crate::util::{arg_bool, failed};

/// Telegram Desktop and its forks, by the start of the executable name.
const TELEGRAM: [&str; 3] = ["telegram", "ayugram", "kotatogram"];

const VK_ESCAPE: u16 = 0x1B;
const VK_RETURN: u16 = 0x0D;
const VK_CONTROL: u16 = 0x11;
const VK_DELETE: u16 = 0x2E;

pub struct MessengerSendTool(pub ScreenEnv);

/// Lower case, «ё» as «е», only letters and digits: OCR and names agree on that.
fn plain(text: &str) -> String {
    text.to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect()
}

/// True when every word of the name is in the text read from the chat header.
pub fn header_names(header: &str, contact: &str) -> bool {
    let header = plain(header);
    let words: Vec<String> = plain(contact)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    !words.is_empty()
        && words
            .iter()
            .all(|w| header.split_whitespace().any(|h| h == w))
}

/// The top strip of the chat pane: right of the chat list, a few lines high.
/// A picture that cannot be cut is read whole.
fn header_strip(png: &[u8]) -> Vec<u8> {
    crop(png, 0.4, 0.0, 1.0, 0.08).unwrap_or_else(|| png.to_vec())
}

/// Cuts a part of an RGB or RGBA PNG; the bounds are shares of its size.
fn crop(png: &[u8], left: f32, top: f32, right: f32, bottom: f32) -> Option<Vec<u8>> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return None,
    };
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let (w, h) = (info.width as f32, info.height as f32);
    let (x0, x1) = ((w * left) as usize, (w * right) as usize);
    let (y0, y1) = ((h * top) as usize, ((h * bottom) as usize).max(1));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let stride = info.line_size;
    let mut out_px = Vec::with_capacity((x1 - x0) * (y1 - y0) * 3);
    for y in y0..y1 {
        let row = &buf[y * stride..(y + 1) * stride];
        for x in x0..x1 {
            let p = &row[x * channels..x * channels + 3];
            out_px.extend_from_slice(p);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, (x1 - x0) as u32, (y1 - y0) as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().ok()?;
        w.write_image_data(&out_px).ok()?;
    }
    Some(out)
}

impl MessengerSendTool {
    fn telegram(&self) -> Option<WindowInfo> {
        self.0.ui.windows.list().into_iter().find(|w| {
            let p = w.process.to_lowercase();
            TELEGRAM.iter().any(|t| p.starts_with(t))
        })
    }

    async fn keys(&self, keys: &[u16], pause_ms: u64) -> Result<(), ToolError> {
        self.0.ui.input.chord(keys).map_err(failed)?;
        tokio::time::sleep(Duration::from_millis(pause_ms)).await;
        Ok(())
    }

    /// Text of the opened chat's header, or `None` when the screen may not be read.
    async fn header(&self, w: &WindowInfo) -> Result<Option<String>, ToolError> {
        if !self.0.enabled.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(None);
        }
        let (screen, id) = (self.0.screen.clone(), w.id);
        let text = tokio::task::spawn_blocking(move || {
            let shot = screen.capture(Some(id), 0)?;
            screen.ocr(&header_strip(&shot.png))
        })
        .await
        .map_err(|e| failed(e.to_string()))?
        .map_err(failed)?;
        Ok(Some(text))
    }

    async fn send_here(&self, w: &WindowInfo, text: &str) -> Result<(), ToolError> {
        let ui = &self.0.ui;
        if let Some((to, sent, at)) = ui.sent.lock().clone() {
            if to == w.id && sent == text && at.elapsed() < Duration::from_secs(90) {
                return Err(failed(
                    "это сообщение уже отправлено в Telegram только что, второй раз не отправляю",
                ));
            }
        }
        ui.windows.focus(w.id).map_err(failed)?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        ui.type_text(text).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.keys(&[VK_RETURN], 400).await?;
        *ui.sent.lock() = Some((w.id, text.to_string(), std::time::Instant::now()));
        Ok(())
    }
}

#[async_trait]
impl Tool for MessengerSendTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "messenger_send",
            "Написать человеку в Telegram: сама находит чат через поиск по всем чатам и отправляет текст. \
Для Telegram используй её, а не ui_click и ui_type. Если она ответила, что чат открыт, но не проверен, \
спроси пользователя, тот ли это чат, и после «да» вызови снова с chat_open=true.",
            json!({
                "type": "object",
                "properties": {
                    "contact": { "type": "string", "description": "Имя чата или человека, как в Telegram" },
                    "text": { "type": "string", "description": "Текст сообщения" },
                    "chat_open": { "type": "boolean", "description": "Нужный чат уже открыт и пользователь это подтвердил: отправить в него" }
                },
                "required": ["contact", "text"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let contact = arg_str(args, "contact")?;
        let text = arg_str(args, "text")?;
        Ok(Assessment::new(
            RiskLevel::High,
            format!("Написать в Telegram: {contact}"),
            format!("«{}»", text.chars().take(300).collect::<String>()),
            "сообщение уйдёт человеку от твоего имени, отозвать его можно только вручную",
        )
        .with_scope("telegram"))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let contact = arg_str(&args, "contact")?.trim().to_string();
        let text = arg_str(&args, "text")?.to_string();
        let w = self.telegram().ok_or_else(|| {
            failed("Telegram не открыт: открой его через open_app («телеграм») и повтори")
        })?;
        if arg_bool(&args, "chat_open") {
            self.send_here(&w, &text).await?;
            return Ok(ToolOutput::new(format!(
                "Отправила в открытый чат «{contact}»: «{text}»."
            )));
        }
        let ui = &self.0.ui;
        ui.windows.focus(w.id).map_err(failed)?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        // Out of the chat search and the chat itself, then the search of all chats.
        self.keys(&[VK_ESCAPE], 250).await?;
        self.keys(&[VK_ESCAPE], 250).await?;
        self.keys(&[VK_CONTROL, 0x46], 400).await?;
        self.keys(&[VK_CONTROL, 0x41], 100).await?;
        self.keys(&[VK_DELETE], 150).await?;
        ui.type_text(&contact).await?;
        // Results come from the server as well: give them a moment.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let before = self.header(&w).await?;
        self.keys(&[VK_RETURN], 1200).await?;
        let after = self.header(&w).await?;
        // The name must come with the opened chat, not be there already.
        let was_there = before.as_deref().is_some_and(|b| header_names(b, &contact));
        match after {
            Some(header) if header_names(&header, &contact) && !was_there => {
                self.send_here(&w, &text).await?;
                Ok(ToolOutput::new(format!(
                    "Открыла чат «{contact}» (в заголовке: {}) и отправила: «{text}».",
                    header.split_whitespace().collect::<Vec<_>>().join(" ")
                )))
            }
            Some(_) if was_there => Ok(ToolOutput::new(format!(
                "Открыла первый результат поиска «{contact}», но не смогла убедиться по заголовку, что открылся именно он. \
Ничего не отправила. Спроси пользователя, открылся ли чат «{contact}»; если да — вызови messenger_send ещё раз с chat_open=true."
            ))),
            Some(header) => Ok(ToolOutput::new(format!(
                "Открыла первый результат поиска «{contact}», но в заголовке чата другое: «{}». \
Ничего не отправила. Спроси пользователя, как точно называется чат.",
                header.split_whitespace().collect::<Vec<_>>().join(" ")
            ))),
            None => Ok(ToolOutput::new(format!(
                "Открыла первый результат поиска «{contact}», но проверить заголовок не могу: «Экран» выключен. \
Ничего не отправила. Спроси пользователя, открылся ли чат «{contact}»; если да — вызови messenger_send ещё раз с chat_open=true."
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env, run};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    fn tool() -> (MessengerSendTool, Arc<crate::testing::Fakes>) {
        let (e, f) = env();
        f.windows.lock().push(WindowInfo {
            id: 5,
            title: "Telegram (4)".into(),
            process: "Telegram.exe".into(),
        });
        let ui = crate::ui::UiEnv {
            windows: e.windows.clone(),
            ui: e.ui.clone(),
            input: e.input.clone(),
            bus: e.shared.bus.clone(),
            own_process: "mika.exe".into(),
            seen: Default::default(),
            clipboard: Some(e.clipboard.clone()),
            sent: Default::default(),
        };
        let screen = ScreenEnv {
            ui,
            screen: e.screen.clone(),
            enabled: e.shared.screen_enabled.clone(),
            user_profile: "C:\\Users\\me".into(),
        };
        (MessengerSendTool(screen), f)
    }

    #[test]
    fn the_header_strip_is_cut_from_a_real_picture() {
        let (w, h) = (100u32, 50u32);
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, w, h);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&vec![200u8; (w * h * 3) as usize])
                .unwrap();
        }
        let strip = header_strip(&png_bytes);
        let reader = png::Decoder::new(std::io::Cursor::new(&strip))
            .read_info()
            .unwrap();
        assert_eq!((reader.info().width, reader.info().height), (60, 4));
        assert_eq!(header_strip(b"not a png"), b"not a png".to_vec());
    }

    #[test]
    fn the_header_must_carry_every_word_of_the_name() {
        assert!(header_names(
            "Алексей Новиков\nбыл(а) недавно",
            "алексей новиков"
        ));
        assert!(header_names("Алёна", "Алена"));
        assert!(!header_names("Избранное", "Алексей Новиков"));
        assert!(!header_names("Алексей Петров", "Алексей Новиков"));
        assert!(!header_names("что угодно", " "));
    }

    #[test]
    fn sending_is_high_risk_and_names_the_person_and_text() {
        let (t, _) = tool();
        let a = t
            .assess(&json!({"contact": "Алексей", "text": "Привет"}))
            .unwrap();
        assert_eq!(a.level, RiskLevel::High);
        assert!(a.summary.contains("Алексей"));
        assert!(a.target.contains("Привет"));
    }

    #[tokio::test]
    async fn without_the_screen_it_opens_the_chat_and_sends_only_when_confirmed() {
        let (t, f) = tool();
        let out = run(&t, json!({"contact": "Алексей Новиков", "text": "Привет"}))
            .await
            .unwrap()
            .text;
        assert!(out.contains("Ничего не отправила"), "{out}");
        // Esc, Esc, Ctrl+F, Ctrl+A, Delete, the name, Enter to open: no second Enter.
        let chords = f.chords.lock().clone();
        assert_eq!(chords.first(), Some(&vec![VK_ESCAPE]));
        assert!(chords.contains(&vec![VK_CONTROL, 0x46]));
        assert_eq!(chords.iter().filter(|c| **c == vec![VK_RETURN]).count(), 1);
        assert_eq!(f.typed.lock().clone(), vec!["Алексей Новиков".to_string()]);

        let out = run(
            &t,
            json!({"contact": "Алексей Новиков", "text": "Привет", "chat_open": true}),
        )
        .await
        .unwrap()
        .text;
        assert!(out.contains("Отправила"), "{out}");
        assert_eq!(f.typed.lock().last().unwrap(), "Привет");
        // The same message again is refused.
        let again = run(
            &t,
            json!({"contact": "Алексей Новиков", "text": "Привет", "chat_open": true}),
        )
        .await;
        assert!(again.is_err());
    }

    #[tokio::test]
    async fn with_the_screen_it_checks_the_header_before_sending() {
        let (t, f) = tool();
        t.0.enabled.store(true, Ordering::Relaxed);
        *f.ocr_text.lock() = "Избранное".into();
        let out = run(&t, json!({"contact": "Алексей Новиков", "text": "Привет"}))
            .await
            .unwrap()
            .text;
        assert!(out.contains("другое"), "{out}");
        assert!(!f.typed.lock().contains(&"Привет".to_string()));

        // The query in the search box reaches the strip: the name is there before
        // and after, so nothing is sent.
        *f.ocr_text.lock() = "Алексей Новиков".into();
        let out = run(&t, json!({"contact": "Алексей Новиков", "text": "Привет"}))
            .await
            .unwrap()
            .text;
        assert!(out.contains("не смогла убедиться"), "{out}");
        assert!(!f.typed.lock().contains(&"Привет".to_string()));
        // The header changes from another chat to the person: sent.
        *f.ocr_seq.lock() = vec!["Избранное".into(), "Алексей Новиков\nбыл(а) недавно".into()];
        let out = run(&t, json!({"contact": "Алексей Новиков", "text": "Привет"}))
            .await
            .unwrap()
            .text;
        assert!(out.contains("отправила"), "{out}");
        assert_eq!(f.typed.lock().last().unwrap(), "Привет");
        assert!(t.0.enabled.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn without_telegram_it_says_to_open_it() {
        let (t, f) = tool();
        f.windows.lock().clear();
        let e = run(&t, json!({"contact": "А", "text": "Б"}))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("open_app"), "{e}");
    }
}
