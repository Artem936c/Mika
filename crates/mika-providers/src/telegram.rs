//! Commands from the phone through a Telegram bot.
//!
//! Only one account may talk to the bot. Its messages go to the assistant like
//! chat messages; replies and questions about actions come back to Telegram
//! while the last message came from there. Risk rules are the same as in the chat.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::confirm::{Answer, ConfirmError, DecidedBy};
use mika_core::events::{Event, EventBus};
use mika_core::schedule::ReminderKind;
use mika_core::secrets::Secret;
use mika_core::session::{Origin, Session};
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
}

/// (label, callback data)
pub type Button = (String, String);

#[async_trait]
pub trait TelegramApi: Send + Sync {
    /// Updates after `offset`, and the offset for the next call.
    async fn updates(&self, offset: i64) -> Result<(Vec<Update>, i64), String>;
    async fn send(&self, chat: i64, text: &str, buttons: &[Button]) -> Result<(), String>;
    async fn answer_button(&self, callback_id: &str) -> Result<(), String>;
}

pub struct HttpTelegram {
    token: Secret,
    base: String,
    http: reqwest::Client,
}

impl HttpTelegram {
    pub fn new(token: Secret) -> Self {
        Self {
            token,
            base: "https://api.telegram.org".into(),
            http: client(),
        }
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
pub async fn handle_update(
    api: &dyn TelegramApi,
    session: &Arc<Session>,
    owner: i64,
    update: Update,
) -> bool {
    let from = match &update {
        Update::Text { from, .. } | Update::Button { from, .. } => *from,
    };
    if owner == 0 || from != owner {
        log::warn!("telegram message from a foreign account was refused");
        if let Update::Text { .. } = update {
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
                session.stop();
                let _ = api.send(owner, "Остановила.", &[]).await;
            }
            other => {
                session.submit_from(other, Origin::Telegram);
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
    }
    true
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
        _ if origin != Origin::Telegram => None,
        Event::AssistantText { text } if !text.trim().is_empty() => {
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
pub async fn run_bridge(
    api: Arc<dyn TelegramApi>,
    session: Arc<Session>,
    bus: EventBus,
    owner: i64,
    cancel: CancellationToken,
) {
    let mut events = bus.subscribe();
    let sender = {
        let (api, session, cancel) = (api.clone(), session.clone(), cancel.clone());
        tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    _ = cancel.cancelled() => break,
                    e = events.recv() => e,
                };
                match event {
                    Ok(e) => {
                        if let Some((text, buttons)) = outgoing(&e, session.origin()) {
                            if let Err(err) = api.send(owner, &text, &buttons).await {
                                log::warn!("telegram send failed: {err}");
                            }
                        }
                    }
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break,
                }
            }
        })
    };

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
                    handle_update(api.as_ref(), &session, owner, u).await;
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
            text: "ок".into()
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
        let delta = Event::AssistantDelta { text: "о".into() };
        assert_eq!(outgoing(&delta, Origin::Telegram), None);
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
}
