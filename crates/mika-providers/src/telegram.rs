//! Commands from the phone through a Telegram bot.
//!
//! Only one account may talk to the bot. Its messages go to the assistant like
//! chat messages; replies and questions about actions come back to Telegram
//! while the last message came from there. Risk rules are the same as in the chat.

pub mod voice;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::bgjobs::JobState;
use mika_core::confirm::{Answer, ConfirmError, DecidedBy};
use mika_core::events::{Event, EventBus};
use mika_core::i18n::tr;
use mika_core::schedule::ReminderKind;
use mika_core::secrets::Secret;
use mika_core::session::{Origin, Session, Submitted};
use mika_core::sound::SoundKind;
use mika_core::state::AssistantState;
use mika_core::tools::{Assessment, RiskLevel};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use crate::common::client;
use crate::factory::redact;

/// Seconds one `getUpdates` call waits for a message.
const POLL_S: u64 = 25;
/// Telegram cuts messages longer than 4096 characters.
const MAX_MESSAGE: usize = 3900;

#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Text {
        from: i64,
        text: String,
    },
    /// A press on an inline button.
    Button {
        from: i64,
        callback_id: String,
        data: String,
    },
    /// A voice message in a private chat.
    Voice {
        from: i64,
        file_id: String,
        duration_s: u32,
        bytes: u64,
        forwarded: bool,
        /// When it was sent, Unix seconds; 0 when Telegram did not say.
        sent: i64,
    },
}

/// (label, callback data)
pub type Button = (String, String);

#[async_trait]
pub trait TelegramApi: Send + Sync {
    /// Updates after `offset`, and the offset for the next call.
    async fn updates(&self, offset: i64) -> Result<(Vec<Update>, i64), String>;
    async fn send(&self, chat: i64, text: &str, buttons: &[Button]) -> Result<(), String>;
    async fn answer_button(&self, callback_id: &str) -> Result<(), String>;
    /// The bytes of a file of at most `max_bytes`: `getFile`, then the download.
    async fn download(&self, _file_id: &str, _max_bytes: u64) -> Result<Vec<u8>, String> {
        Err("этот клиент Telegram не скачивает файлы".into())
    }
    /// A sound file: a voice message when Telegram can show it as one
    /// (OGG/Opus, MP3, M4A), otherwise a document.
    async fn send_sound(&self, _chat: i64, _sound: &voice::VoiceFile) -> Result<(), String> {
        Err("этот клиент Telegram не отправляет звук".into())
    }
}

pub struct HttpTelegram {
    token: Secret,
    base: String,
    http: reqwest::Client,
}

impl HttpTelegram {
    pub fn new(token: Secret) -> Self {
        Self::with_base(token, "https://api.telegram.org")
    }

    /// A client of the Bot API at another address: for tests against a local server.
    pub fn with_base(token: Secret, base: &str) -> Self {
        Self {
            token,
            base: base.trim_end_matches('/').to_string(),
            http: client(),
        }
    }

    /// `text` without the bot token: the token is part of every address.
    fn hide(&self, text: String) -> String {
        redact(&text, Some(&self.token))
    }

    async fn call(&self, method: &str, body: Value, timeout: Duration) -> Result<Value, String> {
        let url = format!("{}/bot{}/{method}", self.base, self.token.expose());
        let hide = |text: String| redact(&text, Some(&self.token));
        let resp = self
            .http
            .post(url)
            .timeout(timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| hide(e.without_url().to_string()))?;
        let v: Value = resp
            .json()
            .await
            .map_err(|e| hide(e.without_url().to_string()))?;
        if v["ok"].as_bool() != Some(true) {
            return Err(hide(format!(
                "Telegram ответил ошибкой: {}",
                v["description"].as_str().unwrap_or("без описания")
            )));
        }
        Ok(v["result"].clone())
    }

    /// Name of the bot, to check the token in settings.
    pub async fn me(&self) -> Result<String, String> {
        let v = self
            .call("getMe", json!({}), Duration::from_secs(15))
            .await?;
        Ok(v["username"].as_str().unwrap_or("").to_string())
    }
}

/// Updates of one `getUpdates` answer and the next offset.
pub fn parse_updates(result: &Value, offset: i64) -> (Vec<Update>, i64) {
    let mut next = offset;
    let mut out = Vec::new();
    for u in result.as_array().into_iter().flatten() {
        if let Some(id) = u["update_id"].as_i64() {
            next = next.max(id + 1);
        }
        if let (Some(from), Some(text)) = (
            u["message"]["from"]["id"].as_i64(),
            u["message"]["text"].as_str(),
        ) {
            // Only private chats: in a group the bot would answer everyone.
            if u["message"]["chat"]["type"] == "private" {
                out.push(Update::Text {
                    from,
                    text: text.to_string(),
                });
            }
        } else if let (Some(from), Some(file_id)) = (
            u["message"]["from"]["id"].as_i64(),
            u["message"]["voice"]["file_id"].as_str(),
        ) {
            let message = &u["message"];
            // Only private chats, as for texts.
            if message["chat"]["type"] == "private" {
                let duration = message["voice"]["duration"].as_u64().unwrap_or(0);
                out.push(Update::Voice {
                    from,
                    file_id: file_id.to_string(),
                    duration_s: u32::try_from(duration).unwrap_or(u32::MAX),
                    bytes: message["voice"]["file_size"].as_u64().unwrap_or(0),
                    // Words of somebody else, whoever sent them on.
                    forwarded: !message["forward_origin"].is_null()
                        || !message["forward_date"].is_null(),
                    sent: message["date"].as_i64().unwrap_or(0),
                });
            }
        } else if let (Some(from), Some(id)) = (
            u["callback_query"]["from"]["id"].as_i64(),
            u["callback_query"]["id"].as_str(),
        ) {
            out.push(Update::Button {
                from,
                callback_id: id.to_string(),
                data: u["callback_query"]["data"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    (out, next)
}

#[async_trait]
impl TelegramApi for HttpTelegram {
    async fn updates(&self, offset: i64) -> Result<(Vec<Update>, i64), String> {
        let v = self
            .call(
                "getUpdates",
                json!({
                    "offset": offset,
                    "timeout": POLL_S,
                    "allowed_updates": ["message", "callback_query"],
                }),
                Duration::from_secs(POLL_S + 15),
            )
            .await?;
        Ok(parse_updates(&v, offset))
    }

    async fn send(&self, chat: i64, text: &str, buttons: &[Button]) -> Result<(), String> {
        let mut body = json!({ "chat_id": chat, "text": cut(text) });
        if !buttons.is_empty() {
            let row: Vec<Value> = buttons
                .iter()
                .map(|(label, data)| json!({ "text": label, "callback_data": data }))
                .collect();
            body["reply_markup"] = json!({ "inline_keyboard": [row] });
        }
        self.call("sendMessage", body, Duration::from_secs(20))
            .await
            .map(|_| ())
    }

    async fn answer_button(&self, callback_id: &str) -> Result<(), String> {
        self.call(
            "answerCallbackQuery",
            json!({ "callback_query_id": callback_id }),
            Duration::from_secs(15),
        )
        .await
        .map(|_| ())
    }

    async fn download(&self, file_id: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
        let too_big = || format!("файл больше {} МБ", max_bytes / 1_048_576);
        let file = self
            .call(
                "getFile",
                json!({ "file_id": file_id }),
                Duration::from_secs(20),
            )
            .await?;
        if file["file_size"].as_u64().is_some_and(|n| n > max_bytes) {
            return Err(too_big());
        }
        let Some(path) = file["file_path"].as_str() else {
            return Err("Telegram не дал путь к файлу".into());
        };
        if !voice::safe_file_path(path) {
            return Err("Telegram дал странный путь к файлу".into());
        }
        // The address holds the bot token: it is never logged and never
        // reaches an error text.
        let url = format!("{}/file/bot{}/{path}", self.base, self.token.expose());
        let mut resp = self
            .http
            .get(url)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| self.hide(e.without_url().to_string()))?;
        if !resp.status().is_success() {
            return Err(self.hide(format!(
                "файл не скачался: ответ {}",
                resp.status().as_u16()
            )));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| self.hide(e.without_url().to_string()))?
        {
            // The size Telegram named is not trusted: extra bytes end the download.
            if (bytes.len() + chunk.len()) as u64 > max_bytes {
                return Err(too_big());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn send_sound(&self, chat: i64, sound: &voice::VoiceFile) -> Result<(), String> {
        let kind = sound.kind;
        if kind == SoundKind::Unknown {
            return Err("звук в неизвестном формате".into());
        }
        if sound.bytes.len() > voice::SOUND_MAX_BYTES {
            return Err("звук больше 50 МБ".into());
        }
        let (method, field, name) = if kind.voice_message() {
            ("sendVoice", "voice", format!("voice.{}", kind.extension()))
        } else {
            (
                "sendDocument",
                "document",
                format!("mika-voice.{}", kind.extension()),
            )
        };
        let file = reqwest::multipart::Part::bytes(sound.bytes.clone())
            .file_name(name)
            .mime_str(kind.mime())
            .map_err(|e| e.to_string())?;
        let form = reqwest::multipart::Form::new()
            .text("chat_id", chat.to_string())
            .part(field, file);
        let url = format!("{}/bot{}/{method}", self.base, self.token.expose());
        let resp = self
            .http
            .post(url)
            .timeout(Duration::from_secs(120))
            .multipart(form)
            .send()
            .await
            .map_err(|e| self.hide(e.without_url().to_string()))?;
        let v: Value = resp
            .json()
            .await
            .map_err(|e| self.hide(e.without_url().to_string()))?;
        if v["ok"].as_bool() != Some(true) {
            return Err(self.hide(format!(
                "Telegram ответил ошибкой: {}",
                v["description"].as_str().unwrap_or("без описания")
            )));
        }
        Ok(())
    }
}

fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_MESSAGE {
        text.to_string()
    } else {
        text.chars().take(MAX_MESSAGE).collect::<String>() + "…"
    }
}

fn risk_word(level: RiskLevel) -> &'static str {
    match level {
        RiskLevel::Low => "низкий",
        RiskLevel::Medium => "средний",
        RiskLevel::High => "ВЫСОКИЙ",
    }
}

/// Text and buttons of a question about an action.
pub fn confirm_message(id: u64, a: &Assessment, allow_always: bool) -> (String, Vec<Button>) {
    let text = format!(
        "Нужно подтверждение · риск {}\n{}\n\n{}\n\nПочему: {}",
        risk_word(a.level),
        a.summary,
        a.target,
        a.reason
    );
    let mut buttons = vec![("Разрешить".to_string(), format!("c:{id}:once"))];
    if allow_always {
        buttons.push(("Всегда".to_string(), format!("c:{id}:always")));
    }
    buttons.push(("Отклонить".to_string(), format!("c:{id}:deny")));
    (text, buttons)
}

/// `c:17:once` as the question id and the answer.
pub fn parse_button(data: &str) -> Option<(u64, Answer)> {
    let mut parts = data.split(':');
    if parts.next()? != "c" {
        return None;
    }
    let id = parts.next()?.parse().ok()?;
    let answer = match parts.next()? {
        "once" => Answer::Once,
        "always" => Answer::Always,
        "deny" => Answer::Deny,
        _ => return None,
    };
    parts.next().is_none().then_some((id, answer))
}

pub const FOREIGN_REPLY: &str = "Я принимаю команды только от своего владельца.";
pub const HELLO: &str =
    "Я на связи. Пиши, что сделать на компьютере. /stop останавливает текущее действие.";

/// Handles one update. Returns false for an update from a foreign account.
/// A voice message is answered with a note that voice messages are off.
pub async fn handle_update(
    api: &dyn TelegramApi,
    session: &Arc<Session>,
    owner: i64,
    update: Update,
) -> bool {
    handle_update_with(api, session, owner, update, None).await
}

/// Handles one update, a voice message too when `voice` is there (decision
/// 0029). Returns false for an update from a foreign account.
pub async fn handle_update_with(
    api: &dyn TelegramApi,
    session: &Arc<Session>,
    owner: i64,
    update: Update,
    voice: Option<&voice::VoiceBridge>,
) -> bool {
    let from = match &update {
        Update::Text { from, .. } | Update::Button { from, .. } | Update::Voice { from, .. } => {
            *from
        }
    };
    if owner == 0 || from != owner {
        log::warn!("telegram message from a foreign account was refused");
        // Nothing of a foreign voice message is downloaded.
        if let Update::Text { .. } | Update::Voice { .. } = update {
            let _ = api.send(from, FOREIGN_REPLY, &[]).await;
        }
        return false;
    }
    match update {
        Update::Text { text, .. } => match text.trim() {
            "/start" | "/help" => {
                let _ = api.send(owner, HELLO, &[]).await;
            }
            "/stop" => {
                if let Some(v) = voice {
                    v.stopped();
                }
                session.stop();
                let _ = api.send(owner, "Остановила.", &[]).await;
            }
            other => {
                if let Some(v) = voice {
                    v.typed();
                }
                if session.submit_from(other, Origin::Telegram) == Submitted::Unseen {
                    show_unseen(api, session, owner, other).await;
                }
            }
        },
        Update::Button {
            callback_id, data, ..
        } => {
            let _ = api.answer_button(&callback_id).await;
            let reply = match parse_button(&data) {
                Some((id, answer)) => {
                    match session.confirms.answer(id, answer, DecidedBy::Telegram) {
                        Ok(()) => None,
                        Err(ConfirmError::NotPending) => {
                            Some("Этот вопрос уже закрыт.".to_string())
                        }
                        Err(ConfirmError::ForeverNotAllowed(why)) => Some(format!(
                            "Навсегда нельзя: {why}. Нажми «Разрешить» или «Отклонить»."
                        )),
                    }
                }
                None => Some("Не поняла кнопку.".to_string()),
            };
            if let Some(text) = reply {
                let _ = api.send(owner, &text, &[]).await;
            }
        }
        Update::Voice {
            file_id,
            duration_s,
            bytes,
            forwarded,
            sent,
            ..
        } => {
            let message = voice::Incoming {
                file_id,
                duration_s,
                bytes,
                forwarded,
                sent,
            };
            voice::handle_voice(api, session, owner, &message, voice).await;
        }
    }
    true
}

/// A typed message came while a question is open that Telegram was never
/// sent: the question of a scenario, of a scheduled task or of a turn started
/// at the computer. The message is not an answer to it. The owner gets the
/// card now, with its buttons, and from then on the turn goes on in Telegram.
async fn show_unseen(api: &dyn TelegramApi, session: &Arc<Session>, owner: i64, typed: &str) {
    let Some(p) = session.confirms.pending() else {
        // Closed this very moment: the message is an ordinary one.
        session.submit_from(typed, Origin::Telegram);
        return;
    };
    let (card, buttons) = confirm_message(p.id, &p.assessment, p.allow_always);
    let text = format!("{}\n\n{card}", tr("provider.tg_question_unseen"));
    match api.send(owner, &text, &buttons).await {
        // Only a card that really went out makes the next message an answer.
        Ok(()) => session.question_shown(p.id, Origin::Telegram),
        Err(err) => log::warn!("telegram send failed: {err}"),
    }
}

/// What goes to Telegram for a bus event, if anything.
pub fn outgoing(event: &Event, origin: Origin) -> Option<(String, Vec<Button>)> {
    match event {
        // Reminders reach the phone wherever the last message came from.
        Event::ReminderFired {
            text,
            kind: ReminderKind::Remind,
            ..
        } => Some((format!("⏰ {text}"), Vec::new())),
        // What she was asked to watch for reaches the phone wherever the last message came from.
        Event::Remark { text, kind } if kind == mika_core::triggers::REMARK_WATCH => {
            Some((format!("👁 {text}"), Vec::new()))
        }
        // The end of a background job goes where the job was started, not
        // where the last message came from: a job asked for in Telegram is
        // told there even after a scenario has run at the computer, and what
        // a job of the computer made (the text of a recording, file names)
        // never leaves it. Not when its caller reports the end itself. Files
        // are not sent.
        Event::BgJobUpdated { job } => {
            let told =
                job.remote && !job.silent && matches!(job.state, JobState::Done | JobState::Failed);
            told.then(|| {
                let mark = if job.state == JobState::Done {
                    "✅"
                } else {
                    "⚠️"
                };
                (format!("{mark} {}\n{}", job.title, job.text), Vec::new())
            })
        }
        _ if origin != Origin::Telegram => None,
        Event::AssistantText { text, .. } if !text.trim().is_empty() => {
            Some((text.clone(), Vec::new()))
        }
        Event::ConfirmRequested {
            id,
            assessment,
            allow_always,
            ..
        } => Some(confirm_message(*id, assessment, *allow_always)),
        Event::Error { message } => Some((format!("Не получилось: {message}"), Vec::new())),
        _ => None,
    }
}

/// Runs until `cancel`: polls Telegram and forwards bus events.
/// A voice message is answered with a note that voice messages are off.
pub async fn run_bridge(
    api: Arc<dyn TelegramApi>,
    session: Arc<Session>,
    bus: EventBus,
    owner: i64,
    cancel: CancellationToken,
) {
    run_bridge_with(api, session, bus, owner, cancel, None).await
}

/// Runs until `cancel`: polls Telegram and forwards bus events. With `voice`,
/// voice messages are recognised and run, and a turn that began with one is
/// answered by voice too (decision 0029).
pub async fn run_bridge_with(
    api: Arc<dyn TelegramApi>,
    session: Arc<Session>,
    bus: EventBus,
    owner: i64,
    cancel: CancellationToken,
    voice: Option<voice::VoiceBridge>,
) {
    let mut events = bus.subscribe();
    let sender = {
        let (api, session, cancel) = (api.clone(), session.clone(), cancel.clone());
        let voice = voice.clone();
        tokio::spawn(async move {
            // A question about an action is open, by the events seen so far.
            let mut asked = false;
            loop {
                let event = tokio::select! {
                    _ = cancel.cancelled() => break,
                    e = events.recv() => e,
                };
                match event {
                    Ok(e) => {
                        // Read before anything is sent: a send takes time,
                        // and the origin may change meanwhile.
                        let origin = session.origin();
                        match &e {
                            Event::ConfirmRequested { .. } => asked = true,
                            Event::ConfirmResolved { .. } => asked = false,
                            Event::StateChanged {
                                to: AssistantState::Idle,
                                ..
                            } => asked = false,
                            _ => {}
                        }
                        if let Some(v) = &voice {
                            match &e {
                                Event::UserText { text } => v.turn_started(text),
                                Event::StateChanged {
                                    to: AssistantState::Idle,
                                    ..
                                } => v.turn_ended(),
                                _ => {}
                            }
                        }
                        if let Some((text, buttons)) = outgoing(&e, origin) {
                            if let Err(err) = api.send(owner, &text, &buttons).await {
                                log::warn!("telegram send failed: {err}");
                            }
                        }
                        // The text went out just above, so the voice message
                        // always comes after it. What is said while a
                        // question is open is a hint about it, not the reply
                        // of the turn: `asked` is told by the order of the
                        // events, not by the state after the send above.
                        if let (Some(v), Event::AssistantText { text, spoken }) = (&voice, &e) {
                            if origin == Origin::Telegram
                                && !text.trim().is_empty()
                                && !asked
                                && v.take_voiced()
                                && v.service().replies()
                            {
                                tokio::spawn(voice::reply(
                                    api.clone(),
                                    v.clone(),
                                    owner,
                                    text.clone(),
                                    spoken.clone(),
                                    session.stop_token(),
                                    cancel.clone(),
                                ));
                            }
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        // Events were lost: which turn runs now is not known.
                        if let Some(v) = &voice {
                            v.turn_ended();
                        }
                        asked = session.confirms.pending().is_some();
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        })
    };
    // Whether a turn runs is followed by a task of its own that never waits
    // for the network: the sender above can be seconds behind the bus.
    let watcher = voice.clone().map(|v| {
        let mut events = bus.subscribe();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    _ = cancel.cancelled() => break,
                    e = events.recv() => e,
                };
                match event {
                    Ok(Event::UserText { .. }) => v.turn_runs(true),
                    Ok(Event::StateChanged {
                        to: AssistantState::Idle,
                        ..
                    }) => v.turn_runs(false),
                    Ok(_) => {}
                    // Events were lost: a turn may be running.
                    Err(RecvError::Lagged(_)) => v.turn_runs(true),
                    Err(RecvError::Closed) => break,
                }
            }
        })
    });
    // Voice messages are heard by a task of their own, one at a time and in
    // the order they came, so buttons and /stop are not held up.
    let hearer = voice.clone().map(|v| {
        let (queue, mut waiting) =
            tokio::sync::mpsc::unbounded_channel::<(voice::Incoming, CancellationToken)>();
        let (api, session, cancel) = (api.clone(), session.clone(), cancel.clone());
        let task = tokio::spawn(async move {
            // Voice messages the bridge took and did not hear to the end.
            let mut unheard = 0;
            loop {
                let (message, stop) = tokio::select! {
                    _ = cancel.cancelled() => break,
                    next = waiting.recv() => match next {
                        Some(next) => next,
                        None => break,
                    },
                };
                tokio::select! {
                    biased;
                    _ = voice::hear_and_run(api.as_ref(), &session, owner, &message, &v, stop) => {}
                    _ = cancel.cancelled() => {
                        unheard += 1;
                        break;
                    }
                }
            }
            // The bridge ends. Telegram will not send these again, so the
            // owner is told rather than left to wait for a reply.
            waiting.close();
            while waiting.try_recv().is_ok() {
                unheard += 1;
            }
            if unheard > 0 {
                voice::tell(api.as_ref(), owner, voice::VOICE_RESTART_REPLY).await;
            }
        });
        (queue, task)
    });

    let mut offset = 0;
    loop {
        let polled = tokio::select! {
            _ = cancel.cancelled() => break,
            r = api.updates(offset) => r,
        };
        match polled {
            Ok((updates, next)) => {
                offset = next;
                for u in updates {
                    let u = match (&hearer, u) {
                        // A foreign one is refused at once, as a text is.
                        (
                            Some((queue, _)),
                            Update::Voice {
                                from,
                                file_id,
                                duration_s,
                                bytes,
                                forwarded,
                                sent,
                            },
                        ) if owner != 0 && from == owner => {
                            let message = voice::Incoming {
                                file_id,
                                duration_s,
                                bytes,
                                forwarded,
                                sent,
                            };
                            // What can be refused at once is, here and now:
                            // its age is counted from the moment it came,
                            // not from the end of the wait for its turn.
                            let now = voice::unix_now();
                            match voice::refusal(&session, voice.as_ref(), &message, now) {
                                Some(why) => voice::tell(api.as_ref(), owner, &why).await,
                                None => {
                                    let _ = queue.send((message, session.stop_token()));
                                }
                            }
                            continue;
                        }
                        (_, u) => u,
                    };
                    handle_update_with(api.as_ref(), &session, owner, u, voice.as_ref()).await;
                }
            }
            Err(e) => {
                log::warn!("telegram poll failed: {e}");
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {}
                }
            }
        }
    }
    sender.abort();
    if let Some(task) = watcher {
        task.abort();
    }
    if let Some((queue, task)) = hearer {
        drop(queue);
        // It tells the owner of what was not heard and ends by itself.
        let _ = task.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use mika_core::agent::Agent;
    use mika_core::confirm::Confirmations;
    use mika_core::gate::Gate;
    use mika_core::llm::{LlmChunk, LlmError, LlmProvider, LlmRequest, LlmStream, ToolCall};
    use mika_core::policy::{AuditLog, PolicyGate, RuleStore};
    use mika_core::storage::Db;
    use mika_core::tools::{TimeTool, ToolRegistry};
    use mika_core::{AssistantState, StateHandle};
    use parking_lot::Mutex;

    #[derive(Default)]
    struct FakeApi {
        sent: Mutex<Vec<(i64, String, Vec<Button>)>>,
        answered: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl TelegramApi for FakeApi {
        async fn updates(&self, offset: i64) -> Result<(Vec<Update>, i64), String> {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok((Vec::new(), offset))
        }
        async fn send(&self, chat: i64, text: &str, buttons: &[Button]) -> Result<(), String> {
            self.sent.lock().push((chat, text.into(), buttons.to_vec()));
            Ok(())
        }
        async fn answer_button(&self, id: &str) -> Result<(), String> {
            self.answered.lock().push(id.into());
            Ok(())
        }
    }

    /// Calls get_time first, then answers with the tool result.
    struct TimeThenAnswer;

    #[async_trait]
    impl LlmProvider for TimeThenAnswer {
        fn name(&self) -> &str {
            "fake"
        }
        async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError> {
            let last = req.messages.last().unwrap();
            let chunk = if last.role == mika_core::llm::Role::Tool {
                LlmChunk::Text("готово".into())
            } else {
                LlmChunk::ToolCall(ToolCall {
                    id: "c1".into(),
                    name: "get_time".into(),
                    arguments: json!({}),
                })
            };
            Ok(futures::stream::iter(vec![Ok(chunk)]).boxed())
        }
    }

    fn session() -> (Arc<Session>, EventBus, StateHandle) {
        let bus = EventBus::new(256);
        let state = StateHandle::new(bus.clone());
        let db = Arc::new(Db::open_in_memory().unwrap());
        let confirms = Confirmations::new(bus.clone(), state.clone());
        let gate: Arc<dyn Gate> = Arc::new(PolicyGate::new(
            RuleStore::new(db.clone()),
            AuditLog::new(db),
            confirms.clone(),
        ));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TimeTool));
        let agent = Arc::new(Agent::new(
            Arc::new(TimeThenAnswer),
            Arc::new(reg),
            gate,
            bus.clone(),
            state.clone(),
            "s".into(),
        ));
        (Session::new(agent, confirms, bus.clone()), bus, state)
    }

    async fn wait_for(mut done: impl FnMut() -> bool) {
        for _ in 0..2000 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("condition not reached");
    }

    #[tokio::test]
    async fn foreign_account_is_refused_and_nothing_starts() {
        let (s, bus, _) = session();
        let mut rx = bus.subscribe();
        let api = FakeApi::default();
        let stranger = Update::Text {
            from: 666,
            text: "выключи компьютер".into(),
        };
        assert!(!handle_update(&api, &s, 42, stranger.clone()).await);
        // Owner 0 means nobody is allowed at all.
        assert!(!handle_update(&api, &s, 0, stranger).await);
        let press = Update::Button {
            from: 666,
            callback_id: "q".into(),
            data: "c:1:once".into(),
        };
        assert!(!handle_update(&api, &s, 42, press).await);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(rx.try_recv().is_err(), "no event may reach the bus");
        assert_eq!(api.sent.lock()[0], (666, FOREIGN_REPLY.to_string(), vec![]));
        assert!(api.answered.lock().is_empty());
    }

    #[tokio::test]
    async fn owner_message_runs_and_the_question_is_answered_by_a_button() {
        let (s, bus, state) = session();
        let api = Arc::new(FakeApi::default());
        let cancel = CancellationToken::new();
        let bridge = tokio::spawn(run_bridge(
            api.clone(),
            s.clone(),
            bus.clone(),
            42,
            cancel.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;

        let msg = Update::Text {
            from: 42,
            text: "который час".into(),
        };
        assert!(handle_update(api.as_ref(), &s, 42, msg).await);
        wait_for(|| !api.sent.lock().is_empty()).await;
        let (chat, text, buttons) = api.sent.lock()[0].clone();
        assert_eq!(chat, 42);
        assert!(
            text.contains("риск низкий") && text.contains("Узнать время"),
            "{text}"
        );
        assert_eq!(buttons.len(), 3);
        let id = s.confirms.pending().unwrap().id;
        assert_eq!(buttons[0].1, format!("c:{id}:once"));

        let press = Update::Button {
            from: 42,
            callback_id: "cb1".into(),
            data: buttons[0].1.clone(),
        };
        handle_update(api.as_ref(), &s, 42, press).await;
        wait_for(|| api.sent.lock().len() >= 2).await;
        assert_eq!(api.sent.lock()[1].1, "готово");
        assert_eq!(*api.answered.lock(), vec!["cb1".to_string()]);
        wait_for(|| state.get() == AssistantState::Idle).await;
        cancel.cancel();
        bridge.await.unwrap();
    }

    #[tokio::test]
    async fn a_question_of_the_computer_is_shown_in_telegram_before_a_message_from_there_answers_it(
    ) {
        let (s, bus, state) = session();
        let api = Arc::new(FakeApi::default());
        let cancel = CancellationToken::new();
        let bridge = tokio::spawn(run_bridge(
            api.clone(),
            s.clone(),
            bus.clone(),
            42,
            cancel.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;

        // A scenario or a scheduled task: a turn of this computer.
        s.submit_task("check the time");
        wait_for(|| s.confirms.pending().is_some()).await;
        let asked = s.confirms.pending().unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(api.sent.lock().is_empty(), "its card stays at the computer");

        // The owner types a yes without having seen the question.
        let yes = || Update::Text {
            from: 42,
            text: "yes".into(),
        };
        assert!(handle_update(api.as_ref(), &s, 42, yes()).await);
        assert_eq!(
            s.confirms.pending().map(|p| p.id),
            Some(asked.id),
            "nothing is approved"
        );
        // He gets the whole card with its buttons, and is told why.
        let head = tr("provider.tg_question_unseen");
        assert_ne!(head, "provider.tg_question_unseen", "the text is missing");
        let (card, card_buttons) = confirm_message(asked.id, &asked.assessment, asked.allow_always);
        assert_eq!(
            *api.sent.lock(),
            vec![(42, format!("{head}\n\n{card}"), card_buttons.clone())]
        );
        assert_eq!(s.origin(), Origin::Telegram, "the turn goes on there");

        // Now the question was shown: the next yes answers it, and the
        // reply comes to Telegram.
        assert!(handle_update(api.as_ref(), &s, 42, yes()).await);
        wait_for(|| s.confirms.pending().is_none()).await;
        wait_for(|| api.sent.lock().len() >= 2).await;
        let (_, reply, buttons) = api.sent.lock()[1].clone();
        assert!(!reply.starts_with(&head) && buttons.is_empty(), "{reply}");
        wait_for(|| state.get() == AssistantState::Idle).await;
        cancel.cancel();
        bridge.await.unwrap();
    }

    #[tokio::test]
    async fn a_card_that_did_not_reach_telegram_leaves_the_question_unseen() {
        /// Sends nothing.
        struct Offline;
        #[async_trait]
        impl TelegramApi for Offline {
            async fn updates(&self, offset: i64) -> Result<(Vec<Update>, i64), String> {
                Ok((Vec::new(), offset))
            }
            async fn send(&self, _: i64, _: &str, _: &[Button]) -> Result<(), String> {
                Err("offline".into())
            }
            async fn answer_button(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let (s, _bus, state) = session();
        s.submit_task("check the time");
        wait_for(|| s.confirms.pending().is_some()).await;
        let id = s.confirms.pending().unwrap().id;
        for _ in 0..2 {
            let yes = Update::Text {
                from: 42,
                text: "yes".into(),
            };
            assert!(handle_update(&Offline, &s, 42, yes).await);
            assert_eq!(s.confirms.pending().map(|p| p.id), Some(id));
            assert_eq!(s.origin(), Origin::Local);
        }
        s.stop();
        wait_for(|| state.get() == AssistantState::Idle).await;
    }

    #[test]
    fn high_risk_question_has_no_always_button() {
        let a = Assessment::new(
            RiskLevel::High,
            "Выключить компьютер",
            "shutdown",
            "необратимо",
        );
        let (text, buttons) = confirm_message(7, &a, false);
        assert!(text.contains("риск ВЫСОКИЙ"));
        let data: Vec<&str> = buttons.iter().map(|b| b.1.as_str()).collect();
        assert_eq!(data, vec!["c:7:once", "c:7:deny"]);
    }

    #[tokio::test]
    async fn always_for_high_risk_is_refused_even_if_the_button_is_forged() {
        let (s, _bus, _) = session();
        let api = FakeApi::default();
        let confirms = s.confirms.clone();
        let high = Assessment::new(RiskLevel::High, "Выключить", "shutdown", "необратимо");
        let ask =
            tokio::spawn(async move { confirms.ask("power", &high, Duration::from_secs(5)).await });
        wait_for(|| s.confirms.pending().is_some()).await;
        let id = s.confirms.pending().unwrap().id;
        let forged = Update::Button {
            from: 42,
            callback_id: "cb".into(),
            data: format!("c:{id}:always"),
        };
        handle_update(&api, &s, 42, forged).await;
        assert!(api.sent.lock()[0].1.starts_with("Навсегда нельзя"));
        assert!(s.confirms.pending().is_some());
        s.confirms.deny_pending(DecidedBy::Policy);
        assert_eq!(ask.await.unwrap().0, Answer::Deny);
    }

    #[test]
    fn replies_follow_the_origin_but_reminders_always_go() {
        let reply = Event::AssistantText {
            text: "ок".into(),
            spoken: None,
        };
        assert_eq!(outgoing(&reply, Origin::Local), None);
        assert_eq!(outgoing(&reply, Origin::Telegram).unwrap().0, "ок");
        let reminder = Event::ReminderFired {
            id: 1,
            text: "чайник".into(),
            kind: ReminderKind::Remind,
            late_s: 0,
        };
        assert_eq!(outgoing(&reminder, Origin::Local).unwrap().0, "⏰ чайник");
        let delta = Event::AssistantDelta {
            text: "о".into(),
            spoken: None,
        };
        assert_eq!(outgoing(&delta, Origin::Telegram), None);
    }

    #[test]
    fn a_finished_job_is_told_in_telegram_only_when_it_was_started_there_and_not_when_silent() {
        let made = |state: JobState, silent: bool, remote: bool| Event::BgJobUpdated {
            job: mika_core::bgjobs::JobView {
                id: 3,
                kind: "video_compress".into(),
                title: "Сжать «отпуск.mp4»".into(),
                state,
                live: false,
                share: 100,
                note: String::new(),
                text: "Готово: отпуск (сжато).mp4".into(),
                data: Some(json!({ "kind": "av_files", "files": [] })),
                silent,
                remote,
            },
        };
        // A job started at the computer: its end, with the text it made,
        // stays there even when the last message came from Telegram.
        for origin in [Origin::Local, Origin::Voice, Origin::Telegram] {
            for state in [JobState::Done, JobState::Failed] {
                assert_eq!(outgoing(&made(state, false, false), origin), None);
            }
        }
        // A job started from Telegram.
        let job = |state: JobState, silent: bool| made(state, silent, true);
        let done = job(JobState::Done, false);
        // Told there whatever turn runs when it ends: a scenario or a
        // message typed at the computer since then does not lose it.
        assert_eq!(
            outgoing(&done, Origin::Local),
            outgoing(&done, Origin::Telegram)
        );
        assert_eq!(
            outgoing(&done, Origin::Voice),
            outgoing(&done, Origin::Telegram)
        );
        assert_eq!(
            outgoing(&done, Origin::Telegram).unwrap(),
            (
                "✅ Сжать «отпуск.mp4»\nГотово: отпуск (сжато).mp4".to_string(),
                Vec::new()
            )
        );
        assert_eq!(
            outgoing(&job(JobState::Failed, false), Origin::Telegram)
                .unwrap()
                .0,
            "⚠️ Сжать «отпуск.mp4»\nГотово: отпуск (сжато).mp4"
        );
        // The caller tells the end itself, and a job on its way is not news.
        assert_eq!(outgoing(&job(JobState::Done, true), Origin::Telegram), None);
        assert_eq!(
            outgoing(&job(JobState::Failed, true), Origin::Telegram),
            None
        );
        for state in [
            JobState::Queued,
            JobState::Running,
            JobState::Stopping,
            JobState::Stopped,
        ] {
            assert_eq!(outgoing(&job(state, false), Origin::Telegram), None);
        }
    }

    #[test]
    fn updates_parser_keeps_private_texts_and_buttons() {
        let v = json!([
            { "update_id": 10, "message": { "from": { "id": 42 }, "chat": { "type": "private" }, "text": "привет" } },
            { "update_id": 11, "message": { "from": { "id": 42 }, "chat": { "type": "group" }, "text": "в группе" } },
            { "update_id": 12, "callback_query": { "id": "cb", "from": { "id": 42 }, "data": "c:3:deny" } },
            { "update_id": 13, "message": { "from": { "id": 42 }, "chat": { "type": "private" }, "photo": [] } }
        ]);
        let (updates, next) = parse_updates(&v, 0);
        assert_eq!(next, 14);
        assert_eq!(
            updates,
            vec![
                Update::Text {
                    from: 42,
                    text: "привет".into()
                },
                Update::Button {
                    from: 42,
                    callback_id: "cb".into(),
                    data: "c:3:deny".into()
                },
            ]
        );
        assert_eq!(parse_updates(&json!(null), 5), (vec![], 5));
    }

    #[test]
    fn button_data_parser() {
        assert_eq!(parse_button("c:17:once"), Some((17, Answer::Once)));
        assert_eq!(parse_button("c:17:always"), Some((17, Answer::Always)));
        assert_eq!(parse_button("c:x:once"), None);
        assert_eq!(parse_button("c:17:once:extra"), None);
        assert_eq!(parse_button("d:17:once"), None);
    }

    #[test]
    fn what_she_watched_for_goes_to_the_phone_but_other_lines_of_hers_do_not() {
        let remark = |kind: &str| Event::Remark {
            text: "Процесс cargo.exe завершился.".into(),
            kind: kind.into(),
        };
        for origin in [Origin::Local, Origin::Voice, Origin::Telegram] {
            assert_eq!(
                outgoing(&remark("watch"), origin),
                Some(("👁 Процесс cargo.exe завершился.".to_string(), Vec::new()))
            );
        }
        // A line about a scenario and her older lines stay on the computer.
        for kind in ["trigger", "break", "focus", "calendar", "life"] {
            assert_eq!(outgoing(&remark(kind), Origin::Local), None, "{kind}");
            assert_eq!(outgoing(&remark(kind), Origin::Telegram), None, "{kind}");
        }
        assert_eq!(mika_core::triggers::REMARK_WATCH, "watch");
    }
}
