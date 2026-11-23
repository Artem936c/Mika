//! Voice messages in Telegram (decision 0029): a voice message of the owner
//! is recognised, shown as text and run as a command; the reply to it can
//! come back as a voice message too.
//!
//! A recognised text never answers a question about an action: it enters the
//! core through `Session::submit_new_from`, which can only start a turn.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use mika_core::events::{Event, EventBus};
use mika_core::i18n::tr;
use mika_core::llm::SpeechToText;
use mika_core::session::{Origin, Session, Submitted};
use mika_core::sound::{ogg_opus_length, SoundConverter, SoundKind};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::TelegramApi;
use crate::live::Live;
use crate::speech::SttFailure;

/// Longest voice message taken, seconds.
pub const VOICE_MAX_S: u32 = 120;
/// Largest file `getFile` of the Bot API gives.
pub const VOICE_MAX_BYTES: u64 = 20 * 1024 * 1024;
/// Largest file `sendVoice` and `sendDocument` take.
pub const SOUND_MAX_BYTES: usize = 50 * 1024 * 1024;
/// Stands before the recognised text the bot shows.
pub const HEARD_MARK: &str = "🎤 ";
pub const VOICE_OFF_REPLY: &str = "Голосовые сообщения выключены. Включить их можно на компьютере: Настройки → Расширения → Telegram. Пока напиши текстом.";
pub const VOICE_FORWARDED_REPLY: &str =
    "Пересланные голосовые я не выполняю: это чужие слова. Скажи своим голосом или напиши текстом.";
pub const VOICE_ASKED_REPLY: &str = "Сейчас открыт вопрос о действии. Ответь на него кнопкой или текстом: «да» или «нет». Голосом на такие вопросы не отвечают.";
pub const VOICE_SHORT_REPLY: &str = "Голосовое короче секунды, я его не распознавала.";
pub const VOICE_LONG_REPLY: &str = "Голосовое длиннее 2 минут. Распознавание платное, поэтому такие я не принимаю: скажи короче или напиши текстом.";
pub const VOICE_EMPTY_REPLY: &str = "Не расслышала слов в голосовом.";
pub const VOICE_STOPPED_REPLY: &str = "Не выполняю: пока я слушала, меня остановили.";
/// A voice message older than this, seconds, is not run: it was said to a
/// computer that was off, and by now it may mean something else.
pub const VOICE_FRESH_S: i64 = 300;
pub const VOICE_OLD_REPLY: &str = "Это голосовое записано больше 5 минут назад, такие я не выполняю: оно могло устареть. Скажи ещё раз или напиши текстом. Если оно свежее, проверь часы на компьютере.";
pub const VOICE_RESTART_REPLY: &str = "Мост Telegram перезапустился. Голосовые, которые я не успела дослушать, не выполнены: пришли их ещё раз.";
pub const VOICE_DOWNLOAD_FAILED: &str = "Не получилось скачать голосовое: ";
pub const VOICE_HEAR_FAILED: &str = "Не получилось распознать голосовое: ";
pub const VOICE_REPLY_FAILED: &str = "Голосом ответить не получилось: ";
pub const VOICE_NEEDS_WAV: &str = "Модель распознавания не приняла звук Telegram (OGG/Opus). Поставь FFmpeg в Настройки → Инструменты, и я переведу его в WAV, или выбери другую модель распознавания.";

/// Recognised texts kept until their turns start, at most.
const EXPECTED_KEPT: usize = 8;
/// A sound this much longer than `VOICE_MAX_S`, seconds, is still taken: the
/// length a sender names is rounded. The same margin FFmpeg cuts at.
const VOICE_SLACK_S: u32 = 5;
/// Most bytes a second of a voice message takes: 128 kbit/s, several times
/// what Opus needs for speech.
const VOICE_BYTES_PER_S: u64 = 16 * 1024;
/// Bytes of a voice message besides its sound: headers and tags.
const VOICE_BYTES_EXTRA: u64 = 64 * 1024;

/// A sound file and its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceFile {
    pub bytes: Vec<u8>,
    pub kind: SoundKind,
}

/// What the bridge needs for voice messages. The settings are read at every message.
#[async_trait]
pub trait TelegramVoice: Send + Sync {
    /// Voice messages are taken now.
    fn accepts(&self) -> bool;
    /// A voice message is answered by voice now.
    fn replies(&self) -> bool;
    /// The words of a voice message: OGG/Opus as Telegram gave it.
    async fn hear(&self, ogg: &[u8]) -> Result<String, String>;
    /// The reply as a sound file; `None` when it has no words to say.
    /// `spoken` is the copy with voice marks (decision 0026), if there is one.
    async fn say(&self, text: &str, spoken: Option<&str>) -> Result<Option<VoiceFile>, String>;
}

/// What is known about the turns that voice messages start.
struct Turn {
    /// Recognised texts whose turns have not started yet.
    expected: Mutex<VecDeque<String>>,
    /// The running turn began with a voice message and is not answered by voice yet.
    voiced: AtomicBool,
    /// Cancels the voice reply being made.
    reply: Mutex<CancellationToken>,
    /// A turn runs now, whoever started it: told by the bridge.
    running: AtomicBool,
    /// Held while a voice message is heard: one at a time.
    hearing: tokio::sync::Mutex<()>,
}

/// The voice half of the bridge: the service and what is known about the turns.
#[derive(Clone)]
pub struct VoiceBridge {
    service: Arc<dyn TelegramVoice>,
    bus: Option<EventBus>,
    turn: Arc<Turn>,
}

impl VoiceBridge {
    pub fn new(service: Arc<dyn TelegramVoice>) -> Self {
        Self {
            service,
            bus: None,
            turn: Arc::new(Turn {
                expected: Mutex::new(VecDeque::new()),
                voiced: AtomicBool::new(false),
                reply: Mutex::new(CancellationToken::new()),
                running: AtomicBool::new(false),
                hearing: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// Recognition time goes to the bus as `Event::Timing { stage: "stt", ms }`,
    /// as for the microphone, when the turn of the voice message starts at
    /// once. One that waits behind a running turn shows no time: the chat
    /// would count it to the reply being written.
    pub fn with_bus(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }

    pub fn service(&self) -> &Arc<dyn TelegramVoice> {
        &self.service
    }

    /// A voice reply still being made is dropped.
    fn drop_reply(&self) {
        let mut reply = self.turn.reply.lock();
        reply.cancel();
        *reply = CancellationToken::new();
    }

    /// The owner typed: a voice reply still being made is dropped.
    pub(crate) fn typed(&self) {
        self.drop_reply();
    }

    /// A stop from Telegram: a voice reply still being made is dropped, no
    /// turn is expected and the running one is not answered by voice. That
    /// voice messages which came before the stop are not run is told by the
    /// stop token of the session, which also knows of a stop on the computer.
    pub(crate) fn stopped(&self) {
        self.drop_reply();
        self.turn.expected.lock().clear();
        self.turn.voiced.store(false, Ordering::Relaxed);
    }

    /// A turn started or the assistant is idle again: told by a task of the
    /// bridge that never waits for the network.
    pub(crate) fn turn_runs(&self, runs: bool) {
        self.turn.running.store(runs, Ordering::Relaxed);
    }

    fn turn_running(&self) -> bool {
        self.turn.running.load(Ordering::Relaxed)
    }

    /// A turn started with `text`. One that a voice message started is
    /// answered by voice. Any other text changes nothing: it may be a typed
    /// answer to a question inside such a turn.
    pub(crate) fn turn_started(&self, text: &str) {
        let text = text.trim();
        let mut expected = self.turn.expected.lock();
        if let Some(at) = expected.iter().position(|t| t == text) {
            expected.remove(at);
            self.turn.voiced.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn turn_ended(&self) {
        self.turn.voiced.store(false, Ordering::Relaxed);
    }

    /// Whether the running turn is still to be answered by voice; it is asked once.
    pub(crate) fn take_voiced(&self) -> bool {
        self.turn.voiced.swap(false, Ordering::Relaxed)
    }

    /// The turn that `text` is about to start began with a voice message.
    fn expect(&self, text: &str) {
        let mut expected = self.turn.expected.lock();
        expected.push_back(text.trim().to_string());
        while expected.len() > EXPECTED_KEPT {
            expected.pop_front();
        }
    }

    /// No turn started with `text` after all.
    fn forget(&self, text: &str) {
        let text = text.trim();
        let mut expected = self.turn.expected.lock();
        if let Some(at) = expected.iter().rposition(|t| t == text) {
            expected.remove(at);
        }
    }
}

/// Whether a recognised text means "stop" and nothing else. The English word
/// is taken too, whatever the language of the interface: an owner who speaks
/// English says it, and stopping is the safe way to be wrong.
pub fn is_stop_phrase(text: &str) -> bool {
    let text = text.to_lowercase();
    let word =
        text.trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '!' | '?' | '…' | ','));
    word == "stop" || mika_core::pause::only_stop_words(&text)
}

/// Whether a file path Telegram gave is safe to put into the download
/// address: `voice/file_12.oga`.
pub fn safe_file_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && !path.starts_with('/')
        && !path.contains("..")
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
}

/// One line to the owner.
pub(crate) async fn tell(api: &dyn TelegramApi, owner: i64, text: &str) {
    if let Err(e) = api.send(owner, text, &[]).await {
        log::warn!(
            "send failed: what=line chars={} error={e}",
            text.chars().count()
        );
    }
}

/// A voice message as Telegram described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Incoming {
    pub file_id: String,
    /// Its length as the sender named it, seconds.
    pub duration_s: u32,
    /// Its size as Telegram named it; 0 when it did not.
    pub bytes: u64,
    pub forwarded: bool,
    /// When it was sent, Unix seconds; 0 when Telegram did not say.
    pub sent: i64,
}

/// The time now, Unix seconds.
pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Most bytes a voice message of `duration_s` seconds honestly has.
fn honest_bytes(duration_s: u32) -> u64 {
    u64::from(duration_s.min(VOICE_MAX_S)) * VOICE_BYTES_PER_S + VOICE_BYTES_EXTRA
}

/// Whether a downloaded voice message is longer than its sender said and
/// than the limit allows. The length in the update is the sender's claim,
/// and recognition is paid by the second: the real length is read from the
/// file, and a file too large for the named length is refused whatever its
/// pages say.
fn too_long(ogg: &[u8], duration_s: u32) -> bool {
    let limit = u64::from(VOICE_MAX_S + VOICE_SLACK_S);
    ogg.len() as u64 > honest_bytes(duration_s)
        || ogg_opus_length(ogg).is_some_and(|real| real.as_secs() > limit)
}

/// What a voice message gets while a question about an action is open. A
/// question whose card is in Telegram is answered there by a button or by
/// text. A question of this computer (a scenario, a scheduled task, a turn
/// started there) was never sent to Telegram: the owner is not told to answer
/// what he has not seen, only how to get it shown.
pub(crate) fn asked_reply(session: &Session) -> String {
    if session.question_unseen(Origin::Telegram) {
        tr("provider.tg_voice_question_unseen")
    } else {
        VOICE_ASKED_REPLY.to_string()
    }
}

/// Why a voice message of the owner is not taken, as the line he gets:
/// everything that is known before anything is downloaded or paid for, in
/// the order of decision 0029. `None`: it is to be heard.
pub(crate) fn refusal(
    session: &Session,
    voice: Option<&VoiceBridge>,
    message: &Incoming,
    now: i64,
) -> Option<String> {
    let refused = refused(session, voice, message, now);
    match &refused {
        Some((reason, _)) => log::info!(
            "incoming: kind=voice seconds={} bytes={} taken=false reason={reason}",
            message.duration_s,
            message.bytes
        ),
        None => log::info!(
            "incoming: kind=voice seconds={} bytes={} taken=true",
            message.duration_s,
            message.bytes
        ),
    }
    refused.map(|(_, line)| line)
}

/// `refusal` with the reason as the journal names it.
fn refused(
    session: &Session,
    voice: Option<&VoiceBridge>,
    message: &Incoming,
    now: i64,
) -> Option<(&'static str, String)> {
    if !voice.is_some_and(|v| v.service.accepts()) {
        return Some(("voice_off", VOICE_OFF_REPLY.into()));
    }
    if message.forwarded {
        return Some(("forwarded", VOICE_FORWARDED_REPLY.into()));
    }
    // One that waited at Telegram while the app was off. A message without
    // a time is taken for old.
    if now.saturating_sub(message.sent) > VOICE_FRESH_S {
        return Some(("old", VOICE_OLD_REPLY.into()));
    }
    if session.confirms.pending().is_some() {
        return Some(("question_open", asked_reply(session)));
    }
    if message.duration_s == 0 {
        return Some(("too_short", VOICE_SHORT_REPLY.into()));
    }
    if message.duration_s > VOICE_MAX_S
        || message.bytes > VOICE_MAX_BYTES
        || message.bytes > honest_bytes(message.duration_s)
    {
        return Some(("too_long", VOICE_LONG_REPLY.into()));
    }
    None
}

/// A voice message of the owner: checked, downloaded, recognised, shown and
/// run as a text command. The caller has already checked the owner. The
/// checks go in the order that downloads and pays for nothing in vain.
pub(crate) async fn handle_voice(
    api: &dyn TelegramApi,
    session: &Arc<Session>,
    owner: i64,
    message: &Incoming,
    voice: Option<&VoiceBridge>,
) {
    if let Some(why) = refusal(session, voice, message, unix_now()) {
        return tell(api, owner, &why).await;
    }
    if let Some(voice) = voice {
        hear_and_run(api, session, owner, message, voice, session.stop_token()).await;
    }
}

/// A voice message that passed `refusal`: downloaded, recognised, shown and
/// run. `stop` is the stop token of the session taken when the message
/// came: a stop since then, in Telegram or on the computer, drops it.
pub(crate) async fn hear_and_run(
    api: &dyn TelegramApi,
    session: &Arc<Session>,
    owner: i64,
    message: &Incoming,
    voice: &VoiceBridge,
    stop: CancellationToken,
) {
    // Voice messages are heard one at a time, in the order they came.
    let _hearing = voice.turn.hearing.lock().await;
    // What came while this one waited for its turn settles it before
    // anything is downloaded or paid for.
    if stop.is_cancelled() {
        log::info!("voice message dropped: a stop came while it waited");
        return tell(api, owner, VOICE_STOPPED_REPLY).await;
    }
    if session.confirms.pending().is_some() {
        log::info!("voice message dropped: a question opened while it waited");
        return tell(api, owner, &asked_reply(session)).await;
    }
    let ogg = match api.download(&message.file_id, VOICE_MAX_BYTES).await {
        Ok(ogg) => ogg,
        Err(why) => {
            log::warn!("voice message not downloaded: error={why}");
            return tell(api, owner, &format!("{VOICE_DOWNLOAD_FAILED}{why}")).await;
        }
    };
    if too_long(&ogg, message.duration_s) {
        log::warn!(
            "voice message refused after the download: longer than it said: named_s={} bytes={}",
            message.duration_s,
            ogg.len()
        );
        return tell(api, owner, VOICE_LONG_REPLY).await;
    }
    let asked = Instant::now();
    let heard = voice.service.hear(&ogg).await;
    let ms = asked.elapsed().as_millis() as u64;
    let text = match heard {
        Ok(text) => text.trim().to_string(),
        Err(why) => {
            log::warn!(
                "voice message not recognised: ms={ms} bytes={} error={}",
                ogg.len(),
                mika_core::journal::error_text(&why)
            );
            return tell(api, owner, &format!("{VOICE_HEAR_FAILED}{why}")).await;
        }
    };
    if text.is_empty() {
        log::info!("voice message recognised: ms={ms} chars=0: nothing was heard");
        return tell(api, owner, VOICE_EMPTY_REPLY).await;
    }
    // The text itself stays out of the log.
    log::info!(
        "voice message recognised: ms={ms} seconds={} chars={}",
        message.duration_s,
        text.chars().count()
    );
    // The owner sees what was heard, whatever happens to it next.
    tell(api, owner, &format!("{HEARD_MARK}{text}")).await;
    if stop.is_cancelled() {
        return tell(api, owner, VOICE_STOPPED_REPLY).await;
    }
    if is_stop_phrase(&text) {
        voice.stopped();
        session.stop();
        return tell(api, owner, "Остановила.").await;
    }
    // Never an answer to a question: one that opened meanwhile stays open.
    if session.confirms.pending().is_some() {
        return tell(api, owner, &asked_reply(session)).await;
    }
    // The recognition time belongs to the reply of this turn. While another
    // turn runs, this one waits behind it, and the chat would count the time
    // to the reply being written: then it is not shown at all.
    if let Some(bus) = voice.bus.as_ref().filter(|_| !voice.turn_running()) {
        bus.publish(Event::Timing {
            stage: "stt".into(),
            ms,
        });
    }
    voice.expect(&text);
    voice.drop_reply();
    // The question may have opened this very moment: the session refuses too.
    match session.submit_new_from(&text, Origin::Telegram) {
        None => {
            voice.forget(&text);
            tell(api, owner, &asked_reply(session)).await;
        }
        // «Усни» (decision 0050): no turn started, her line is sent from here.
        Some(Submitted::Asleep) => {
            voice.forget(&text);
            tell(api, owner, &tr("core.quick.sleep")).await;
        }
        Some(_) => {}
    }
}

/// Makes the reply a voice message and sends it. A new message of the owner
/// or a stop in Telegram drops it, and so do `stop`, the stop token of the
/// session (a stop on the computer), and `closed`, the end of the bridge:
/// nothing is sent then. The token is taken at the call, not when the task
/// first runs, so nothing that comes in between is missed.
pub(crate) fn reply(
    api: Arc<dyn TelegramApi>,
    voice: VoiceBridge,
    owner: i64,
    text: String,
    spoken: Option<String>,
    stop: CancellationToken,
    closed: CancellationToken,
) -> impl Future<Output = ()> + Send {
    let token = voice.turn.reply.lock().clone();
    async move {
        let began = std::time::Instant::now();
        // Why a reply by voice did not come: "she answered with text only"
        // has its record.
        let dropped = |reason: &str| {
            log::info!("voice reply not made: reason={reason}");
        };
        let said = tokio::select! {
            _ = token.cancelled() => return dropped("dropped_by_new_message"),
            _ = stop.cancelled() => return dropped("stopped"),
            _ = closed.cancelled() => return dropped("bridge_closed"),
            said = voice.service.say(&text, spoken.as_deref()) => said,
        };
        if token.is_cancelled() {
            return dropped("dropped_by_new_message");
        }
        if stop.is_cancelled() {
            return dropped("stopped");
        }
        if closed.is_cancelled() {
            return dropped("bridge_closed");
        }
        let failed = match said {
            Ok(Some(sound)) => {
                let bytes = sound.bytes.len();
                let sent = api.send_sound(owner, &sound).await.err();
                if sent.is_none() {
                    log::info!(
                        "voice reply sent: chars={} bytes={bytes} ms={}",
                        text.chars().count(),
                        began.elapsed().as_millis()
                    );
                }
                sent
            }
            Ok(None) => {
                dropped("nothing_to_say");
                None
            }
            Err(why) => Some(why),
        };
        if let Some(why) = failed {
            // The reason only: the text of the reply stays out of the log.
            log::warn!(
                "voice reply failed: chars={} error={}",
                text.chars().count(),
                mika_core::journal::error_text(&why)
            );
            tell(api.as_ref(), owner, &format!("{VOICE_REPLY_FAILED}{why}")).await;
        }
    }
}

/// The reply as one text for the voice: the app makes it with `mika_audio::speech::voice_note`.
pub type NoteMaker = Arc<dyn Fn(&str, Option<&str>) -> Option<String> + Send + Sync>;
/// What a provider voice is told besides the text: style and tone.
pub type Instructions = Arc<dyn Fn() -> String + Send + Sync>;

/// Speech address and recognition model of the providers that refused the
/// sound of Telegram and took WAV: until the app restarts they get WAV at once.
static NO_OGG: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Voice messages through the speech provider of the settings: the same
/// recognition as the microphone, the same voice as the replies.
pub struct LiveVoice {
    live: Live,
    converter: Arc<dyn SoundConverter>,
    note: NoteMaker,
    instructions: Instructions,
}

impl LiveVoice {
    pub fn new(
        live: Live,
        converter: Arc<dyn SoundConverter>,
        note: NoteMaker,
        instructions: Instructions,
    ) -> Self {
        Self {
            live,
            converter,
            note,
            instructions,
        }
    }
}

#[async_trait]
impl TelegramVoice for LiveVoice {
    fn accepts(&self) -> bool {
        self.live.config().lock().telegram.voice_in
    }

    fn replies(&self) -> bool {
        let config = self.live.config().lock();
        config.telegram.voice_replies() && config.voice.provider_speaks()
    }

    async fn hear(&self, ogg: &[u8]) -> Result<String, String> {
        let voice = self.live.config().lock().voice.clone();
        let provider = (
            self.live.base(&voice.speech_base_url),
            voice.stt_model.trim().to_string(),
        );
        let converts = self.converter.ready();
        if !(converts && NO_OGG.lock().contains(&provider)) {
            match self.live.transcribe_sound(ogg, SoundKind::of(ogg)).await {
                Ok(text) => return Ok(text),
                Err(SttFailure::Other(why)) => return Err(why),
                Err(SttFailure::Refused(why)) if !converts => {
                    return Err(format!("{why}. {VOICE_NEEDS_WAV}"))
                }
                Err(SttFailure::Refused(why)) => {
                    log::warn!(
                        "recognition refused the sound of Telegram, trying WAV: error={}",
                        mika_core::journal::error_text(&why)
                    );
                }
            }
        }
        // What the microphone sends: 16 kHz mono WAV as `speech.wav`.
        let wav = self.converter.to_wav16k(ogg).await?;
        let text = SpeechToText::transcribe(&self.live, &wav).await?;
        let mut refused = NO_OGG.lock();
        if !refused.contains(&provider) {
            refused.push(provider);
        }
        Ok(text)
    }

    async fn say(&self, text: &str, spoken: Option<&str>) -> Result<Option<VoiceFile>, String> {
        let performs = self.live.config().lock().voice.performs_marks();
        // Marks go only to a voice that performs them; another would read them aloud.
        let marked = spoken.filter(|_| performs);
        let Some(note) = (self.note)(text, marked) else {
            return Ok(None);
        };
        let sound = self
            .live
            .synthesize_format(&note, &(self.instructions)(), "mp3")
            .await?;
        if sound.len() > SOUND_MAX_BYTES {
            return Err("звук ответа больше 50 МБ".into());
        }
        // What came is told by the bytes, not by what was asked.
        let kind = SoundKind::of(&sound);
        if kind.voice_message() {
            return Ok(Some(VoiceFile { bytes: sound, kind }));
        }
        if self.converter.ready() {
            match self.converter.to_ogg_opus(&sound).await {
                Ok(ogg) if SoundKind::of(&ogg) == SoundKind::OggOpus => {
                    return Ok(Some(VoiceFile {
                        bytes: ogg,
                        kind: SoundKind::OggOpus,
                    }))
                }
                Ok(_) => log::warn!(
                    "the voice reply was converted into something else, it goes as a file"
                ),
                Err(why) => log::warn!(
                    "the voice reply was not converted, it goes as a file: error={}",
                    mika_core::journal::error_text(&why)
                ),
            }
        }
        if kind == SoundKind::Unknown {
            return Err("синтез вернул звук в неизвестном формате".into());
        }
        // Not a voice message for Telegram: it goes as a file.
        Ok(Some(VoiceFile { bytes: sound, kind }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::{
        handle_update, handle_update_with, parse_updates, run_bridge_with, Button, Update,
        FOREIGN_REPLY,
    };
    use futures::StreamExt;
    use mika_core::agent::Agent;
    use mika_core::confirm::Confirmations;
    use mika_core::gate::Gate;
    use mika_core::llm::{LlmChunk, LlmError, LlmProvider, LlmRequest, LlmStream, ToolCall};
    use mika_core::policy::{AuditLog, PolicyGate, RuleStore};
    use mika_core::storage::Db;
    use mika_core::tools::{TimeTool, ToolRegistry};
    use mika_core::{AssistantState, StateHandle};
    use serde_json::json;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio::sync::Notify;

    const OWNER: i64 = 42;
    const OGG: &[u8] = b"OggS-voice-message";

    /// What the fake bot was asked to send.
    #[derive(Debug, Clone, PartialEq)]
    enum Out {
        Text(i64, String),
        Sound(i64, VoiceFile),
    }

    struct FakeApi {
        /// Batches of updates the bridge polls.
        incoming: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<Update>>>,
        out: Mutex<Vec<Out>>,
        downloads: Mutex<Vec<(String, u64)>>,
        file: Mutex<Result<Vec<u8>, String>>,
        sound_refused: Mutex<Option<String>>,
        /// `send` waits here when set, as a slow network does.
        send_gate: Mutex<Option<Arc<Notify>>>,
    }

    fn api() -> (Arc<FakeApi>, mpsc::UnboundedSender<Vec<Update>>) {
        let (feed, incoming) = mpsc::unbounded_channel();
        let api = FakeApi {
            incoming: tokio::sync::Mutex::new(incoming),
            out: Mutex::new(Vec::new()),
            downloads: Mutex::new(Vec::new()),
            file: Mutex::new(Ok(OGG.to_vec())),
            sound_refused: Mutex::new(None),
            send_gate: Mutex::new(None),
        };
        (Arc::new(api), feed)
    }

    /// One Ogg page that ends at `position` and carries `data`.
    fn page(position: u64, data: &[u8]) -> Vec<u8> {
        let mut page = b"OggS".to_vec();
        page.extend_from_slice(&[0, 0]);
        page.extend_from_slice(&position.to_le_bytes());
        page.extend_from_slice(&[0; 12]);
        let sizes: Vec<u8> = data.chunks(255).map(|c| c.len() as u8).collect();
        page.push(sizes.len() as u8);
        page.extend_from_slice(&sizes);
        page.extend_from_slice(data);
        page
    }

    /// A small Ogg/Opus file whose pages say it plays for `seconds`.
    fn opus(seconds: u64) -> Vec<u8> {
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 1, 0, 0, 0x80, 0xBB, 0, 0, 0, 0, 0]);
        let mut file = page(0, &head);
        file.extend(page(48_000 * seconds, &[7; 300]));
        file
    }

    impl FakeApi {
        fn texts(&self) -> Vec<String> {
            self.out
                .lock()
                .iter()
                .filter_map(|o| match o {
                    Out::Text(_, text) => Some(text.clone()),
                    Out::Sound(..) => None,
                })
                .collect()
        }

        fn sounds(&self) -> usize {
            self.out
                .lock()
                .iter()
                .filter(|o| matches!(o, Out::Sound(..)))
                .count()
        }

        fn has_text(&self, text: &str) -> bool {
            self.texts().iter().any(|t| t == text)
        }
    }

    #[async_trait]
    impl TelegramApi for FakeApi {
        async fn updates(&self, offset: i64) -> Result<(Vec<Update>, i64), String> {
            let mut incoming = self.incoming.lock().await;
            match incoming.recv().await {
                Some(batch) => Ok((batch, offset + 1)),
                None => {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    Ok((Vec::new(), offset))
                }
            }
        }
        async fn send(&self, chat: i64, text: &str, _buttons: &[Button]) -> Result<(), String> {
            let gate = self.send_gate.lock().clone();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            self.out.lock().push(Out::Text(chat, text.into()));
            Ok(())
        }
        async fn answer_button(&self, _id: &str) -> Result<(), String> {
            Ok(())
        }
        async fn download(&self, file_id: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
            self.downloads.lock().push((file_id.into(), max_bytes));
            self.file.lock().clone()
        }
        async fn send_sound(&self, chat: i64, sound: &VoiceFile) -> Result<(), String> {
            if let Some(why) = self.sound_refused.lock().clone() {
                return Err(why);
            }
            self.out.lock().push(Out::Sound(chat, sound.clone()));
            Ok(())
        }
    }

    struct FakeVoice {
        accepts: AtomicBool,
        replies: AtomicBool,
        /// What `hear` answers, in turn; after them «какая завтра погода».
        heard: Mutex<VecDeque<Result<String, String>>>,
        /// The sounds `hear` was given.
        hears: Mutex<Vec<Vec<u8>>>,
        /// `hear` waits here when set.
        hear_gate: Mutex<Option<Arc<Notify>>>,
        /// What `say` was given.
        says: Mutex<Vec<(String, Option<String>)>>,
        say_answer: Mutex<Result<Option<VoiceFile>, String>>,
        /// `say` waits here when set.
        say_gate: Mutex<Option<Arc<Notify>>>,
    }

    fn mp3() -> VoiceFile {
        VoiceFile {
            bytes: vec![0xFF, 0xFB, 0x90, 0x00],
            kind: SoundKind::Mp3,
        }
    }

    fn fake_voice() -> Arc<FakeVoice> {
        Arc::new(FakeVoice {
            accepts: AtomicBool::new(true),
            replies: AtomicBool::new(true),
            heard: Mutex::new(VecDeque::new()),
            hears: Mutex::new(Vec::new()),
            hear_gate: Mutex::new(None),
            says: Mutex::new(Vec::new()),
            say_answer: Mutex::new(Ok(Some(mp3()))),
            say_gate: Mutex::new(None),
        })
    }

    #[async_trait]
    impl TelegramVoice for FakeVoice {
        fn accepts(&self) -> bool {
            self.accepts.load(Ordering::Relaxed)
        }
        fn replies(&self) -> bool {
            self.replies.load(Ordering::Relaxed)
        }
        async fn hear(&self, ogg: &[u8]) -> Result<String, String> {
            self.hears.lock().push(ogg.to_vec());
            let gate = self.hear_gate.lock().clone();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            let next = self.heard.lock().pop_front();
            next.unwrap_or_else(|| Ok("какая завтра погода".into()))
        }
        async fn say(&self, text: &str, spoken: Option<&str>) -> Result<Option<VoiceFile>, String> {
            self.says
                .lock()
                .push((text.into(), spoken.map(str::to_string)));
            let gate = self.say_gate.lock().clone();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            self.say_answer.lock().clone()
        }
    }

    /// Answers every message with the same words.
    struct Says(&'static str);

    #[async_trait]
    impl LlmProvider for Says {
        fn name(&self) -> &str {
            "fake"
        }
        async fn stream(&self, _req: LlmRequest) -> Result<LlmStream, LlmError> {
            Ok(futures::stream::iter(vec![Ok(LlmChunk::Text(self.0.into()))]).boxed())
        }
    }

    /// Calls get_time first, then answers with «готово».
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

    /// Answers every message with the same words, each time it is let go.
    struct Gated(Arc<Notify>, &'static str);

    #[async_trait]
    impl LlmProvider for Gated {
        fn name(&self) -> &str {
            "fake"
        }
        async fn stream(&self, _req: LlmRequest) -> Result<LlmStream, LlmError> {
            self.0.notified().await;
            Ok(futures::stream::iter(vec![Ok(LlmChunk::Text(self.1.into()))]).boxed())
        }
    }

    /// Never answers: the turn runs until it is stopped.
    struct Never;

    #[async_trait]
    impl LlmProvider for Never {
        fn name(&self) -> &str {
            "fake"
        }
        async fn stream(&self, _req: LlmRequest) -> Result<LlmStream, LlmError> {
            futures::future::pending().await
        }
    }

    fn session(provider: Arc<dyn LlmProvider>) -> (Arc<Session>, EventBus, StateHandle) {
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
            provider,
            Arc::new(reg),
            gate,
            bus.clone(),
            state.clone(),
            "s".into(),
        ));
        (Session::new(agent, confirms, bus.clone()), bus, state)
    }

    async fn wait_for(mut done: impl FnMut() -> bool) {
        for _ in 0..5000 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("condition not reached");
    }

    /// Long enough for something that must not happen to happen.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    fn voice_message(file_id: &str) -> Update {
        Update::Voice {
            from: OWNER,
            file_id: file_id.into(),
            duration_s: 4,
            bytes: 9_000,
            forwarded: false,
            sent: unix_now(),
        }
    }

    /// A voice message of `duration_s` seconds sent `age` seconds ago.
    fn incoming(duration_s: u32, age: i64) -> Incoming {
        Incoming {
            file_id: "f1".into(),
            duration_s,
            bytes: 9_000,
            forwarded: false,
            sent: unix_now() - age,
        }
    }

    fn typed(text: &str) -> Update {
        Update::Text {
            from: OWNER,
            text: text.into(),
        }
    }

    fn events(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<Event> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// A running bridge with fakes on every side.
    struct Rig {
        api: Arc<FakeApi>,
        feed: mpsc::UnboundedSender<Vec<Update>>,
        voice: Arc<FakeVoice>,
        bridge: VoiceBridge,
        session: Arc<Session>,
        bus: EventBus,
        state: StateHandle,
        cancel: CancellationToken,
        task: tokio::task::JoinHandle<()>,
    }

    impl Rig {
        async fn start(provider: Arc<dyn LlmProvider>) -> Rig {
            let (session, bus, state) = session(provider);
            let (api, feed) = api();
            let voice = fake_voice();
            let bridge = VoiceBridge::new(voice.clone()).with_bus(bus.clone());
            let cancel = CancellationToken::new();
            let task = tokio::spawn(run_bridge_with(
                api.clone(),
                session.clone(),
                bus.clone(),
                OWNER,
                cancel.clone(),
                Some(bridge.clone()),
            ));
            // The bridge subscribes to the bus before anything is published.
            tokio::time::sleep(Duration::from_millis(10)).await;
            Rig {
                api,
                feed,
                voice,
                bridge,
                session,
                bus,
                state,
                cancel,
                task,
            }
        }

        fn send(&self, update: Update) {
            self.feed.send(vec![update]).unwrap();
        }

        async fn idle(&self) {
            let state = self.state.clone();
            wait_for(|| state.get() == AssistantState::Idle).await;
        }

        async fn stop(self) {
            self.cancel.cancel();
            self.task.await.unwrap();
        }
    }

    #[test]
    fn updates_parser_keeps_a_private_voice_message() {
        let v = json!([
            { "update_id": 20, "message": { "from": { "id": 42 }, "chat": { "type": "private" },
                "date": 1_794_000_000,
                "voice": { "file_id": "AwACAg", "duration": 7, "mime_type": "audio/ogg", "file_size": 15000 } } },
            { "update_id": 21, "message": { "from": { "id": 42 }, "chat": { "type": "private" },
                "forward_origin": { "type": "user" }, "forward_date": 1731571200,
                "voice": { "file_id": "B", "duration": 3 } } },
            { "update_id": 22, "message": { "from": { "id": 42 }, "chat": { "type": "group" },
                "voice": { "file_id": "C", "duration": 3 } } },
            { "update_id": 23, "message": { "from": { "id": 42 }, "chat": { "type": "private" },
                "forward_date": 5, "voice": { "file_id": "D" } } },
            { "update_id": 24, "message": { "from": { "id": 42 }, "chat": { "type": "private" },
                "voice": { "file_id": "E", "duration": 99_999_999_999u64, "file_size": 30_000_000 } } },
            { "update_id": 25, "message": { "from": { "id": 42 }, "chat": { "type": "private" },
                "voice": { "duration": 3 } } }
        ]);
        let (updates, next) = parse_updates(&v, 0);
        assert_eq!(next, 26);
        let voice = |file_id: &str, duration_s, bytes, forwarded, sent| Update::Voice {
            from: 42,
            file_id: file_id.into(),
            duration_s,
            bytes,
            forwarded,
            sent,
        };
        // A message without a time has the time 0: it counts as old.
        assert_eq!(
            updates,
            vec![
                voice("AwACAg", 7, 15_000, false, 1_794_000_000),
                voice("B", 3, 0, true, 0),
                voice("D", 0, 0, true, 0),
                voice("E", u32::MAX, 30_000_000, false, 0),
            ]
        );
    }

    #[tokio::test]
    async fn a_foreign_voice_message_is_refused_and_nothing_is_downloaded() {
        let (s, bus, _) = session(Arc::new(Says("Завтра ясно.")));
        let mut rx = bus.subscribe();
        let (api, _feed) = api();
        let voice = fake_voice();
        let bridge = VoiceBridge::new(voice.clone()).with_bus(bus.clone());
        let stranger = Update::Voice {
            from: 666,
            file_id: "f1".into(),
            duration_s: 4,
            bytes: 9_000,
            forwarded: false,
            sent: unix_now(),
        };
        assert!(
            !handle_update_with(api.as_ref(), &s, OWNER, stranger.clone(), Some(&bridge)).await
        );
        // Owner 0 means nobody is allowed at all.
        assert!(!handle_update_with(api.as_ref(), &s, 0, stranger.clone(), Some(&bridge)).await);
        assert!(!handle_update(api.as_ref(), &s, OWNER, stranger).await);
        settle().await;
        assert!(rx.try_recv().is_err(), "no event may reach the bus");
        assert_eq!(
            *api.out.lock(),
            vec![Out::Text(666, FOREIGN_REPLY.to_string()); 3]
        );
        assert!(api.downloads.lock().is_empty());
        assert!(voice.hears.lock().is_empty());
    }

    #[tokio::test]
    async fn a_voice_message_that_is_not_taken_costs_nothing() {
        let (s, bus, _) = session(Arc::new(Says("Завтра ясно.")));
        let mut rx = bus.subscribe();
        let (api, _feed) = api();
        let voice = fake_voice();
        let bridge = VoiceBridge::new(voice.clone()).with_bus(bus.clone());
        let aged = |duration_s, bytes, forwarded, age: i64| Update::Voice {
            from: OWNER,
            file_id: "f1".into(),
            duration_s,
            bytes,
            forwarded,
            sent: unix_now() - age,
        };
        let message = |duration_s, bytes, forwarded| aged(duration_s, bytes, forwarded, 0);

        // No voice half at all: the old entry point.
        assert!(handle_update(api.as_ref(), &s, OWNER, message(4, 9_000, false)).await);
        // The switch is off.
        voice.accepts.store(false, Ordering::Relaxed);
        assert!(
            handle_update_with(
                api.as_ref(),
                &s,
                OWNER,
                message(4, 9_000, false),
                Some(&bridge)
            )
            .await
        );
        voice.accepts.store(true, Ordering::Relaxed);
        // Forwarded, shorter than a second, too long, too large, too large
        // for the length it names, said too long ago, without a time.
        for update in [
            message(4, 9_000, true),
            message(0, 9_000, false),
            message(VOICE_MAX_S + 1, 9_000, false),
            message(4, VOICE_MAX_BYTES + 1, false),
            message(4, honest_bytes(4) + 1, false),
            aged(4, 9_000, false, VOICE_FRESH_S + 5),
            Update::Voice {
                from: OWNER,
                file_id: "f1".into(),
                duration_s: 4,
                bytes: 9_000,
                forwarded: false,
                sent: 0,
            },
        ] {
            assert!(handle_update_with(api.as_ref(), &s, OWNER, update, Some(&bridge)).await);
        }
        settle().await;
        assert_eq!(
            api.texts(),
            vec![
                VOICE_OFF_REPLY,
                VOICE_OFF_REPLY,
                VOICE_FORWARDED_REPLY,
                VOICE_SHORT_REPLY,
                VOICE_LONG_REPLY,
                VOICE_LONG_REPLY,
                VOICE_LONG_REPLY,
                VOICE_OLD_REPLY,
                VOICE_OLD_REPLY,
            ]
        );
        assert!(api.downloads.lock().is_empty());
        assert!(voice.hears.lock().is_empty());
        assert!(rx.try_recv().is_err(), "no event may reach the bus");

        // The longest, largest and oldest one that is taken; a clock that
        // runs behind Telegram's makes it a message from the future.
        let longest = aged(
            VOICE_MAX_S,
            honest_bytes(VOICE_MAX_S),
            false,
            VOICE_FRESH_S - 5,
        );
        assert!(handle_update_with(api.as_ref(), &s, OWNER, longest, Some(&bridge)).await);
        assert!(refusal(&s, Some(&bridge), &incoming(4, -600), unix_now()).is_none());
        assert_eq!(
            *api.downloads.lock(),
            vec![("f1".to_string(), VOICE_MAX_BYTES)]
        );
        assert_eq!(*voice.hears.lock(), vec![OGG.to_vec()]);
    }

    #[tokio::test]
    async fn a_voice_message_does_not_ask_for_an_answer_to_a_question_telegram_never_got() {
        let (s, bus, state) = session(Arc::new(TimeThenAnswer));
        let (api, _feed) = api();
        let voice = fake_voice();
        let bridge = VoiceBridge::new(voice.clone()).with_bus(bus.clone());
        // A scenario or a scheduled task asks at the computer.
        s.submit_task("check the time");
        wait_for(|| s.confirms.pending().is_some()).await;
        let id = s.confirms.pending().unwrap().id;

        assert!(
            handle_update_with(api.as_ref(), &s, OWNER, voice_message("f1"), Some(&bridge)).await
        );
        // Not "answer yes or no": the owner has not seen what is asked.
        let line = tr("provider.tg_voice_question_unseen");
        assert_ne!(line, "provider.tg_voice_question_unseen");
        assert_ne!(line, VOICE_ASKED_REPLY);
        assert_eq!(api.texts(), vec![line]);
        assert!(api.downloads.lock().is_empty());
        assert!(voice.hears.lock().is_empty());
        assert_eq!(s.confirms.pending().unwrap().id, id, "the question stays");
        assert_eq!(s.origin(), Origin::Local, "and stays at the computer");
        s.stop();
        wait_for(|| state.get() == AssistantState::Idle).await;
    }

    #[tokio::test]
    async fn a_voice_message_is_not_heard_while_a_question_is_open() {
        let (s, bus, state) = session(Arc::new(TimeThenAnswer));
        let (api, _feed) = api();
        let voice = fake_voice();
        let bridge = VoiceBridge::new(voice.clone()).with_bus(bus.clone());
        s.submit_from("который час", Origin::Telegram);
        wait_for(|| s.confirms.pending().is_some()).await;
        let id = s.confirms.pending().unwrap().id;

        assert!(
            handle_update_with(api.as_ref(), &s, OWNER, voice_message("f1"), Some(&bridge)).await
        );
        assert_eq!(api.texts(), vec![VOICE_ASKED_REPLY]);
        assert!(api.downloads.lock().is_empty());
        assert!(voice.hears.lock().is_empty());
        assert_eq!(s.confirms.pending().unwrap().id, id, "the question stays");
        s.stop();
        wait_for(|| state.get() == AssistantState::Idle).await;
    }

    #[tokio::test]
    async fn a_voice_message_is_heard_shown_run_and_answered_by_voice() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        let mut rx = rig.bus.subscribe();
        rig.send(voice_message("f1"));
        wait_for(|| rig.api.sounds() == 1).await;
        rig.idle().await;
        settle().await;

        assert_eq!(
            *rig.api.downloads.lock(),
            vec![("f1".to_string(), VOICE_MAX_BYTES)]
        );
        assert_eq!(*rig.voice.hears.lock(), vec![OGG.to_vec()]);
        // What was heard, then the reply as text, then the same reply by voice.
        assert_eq!(
            *rig.api.out.lock(),
            vec![
                Out::Text(OWNER, format!("{HEARD_MARK}какая завтра погода")),
                Out::Text(OWNER, "Завтра ясно.".into()),
                Out::Sound(OWNER, mp3()),
            ]
        );
        assert_eq!(
            *rig.voice.says.lock(),
            vec![("Завтра ясно.".to_string(), None)]
        );
        assert_eq!(rig.session.origin(), Origin::Telegram);
        // The recognition time is on the bus before the turn, as for the microphone.
        let events = events(&mut rx);
        let timing = events
            .iter()
            .position(|e| matches!(e, Event::Timing { stage, .. } if stage == "stt"))
            .expect("the recognition time");
        let turn = events
            .iter()
            .position(|e| matches!(e, Event::UserText { text } if text == "какая завтра погода"))
            .expect("a turn with the heard text");
        assert!(timing < turn, "{events:?}");
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_typed_message_is_answered_by_text_only() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        rig.send(typed("какая завтра погода"));
        wait_for(|| rig.api.has_text("Завтра ясно.")).await;
        rig.idle().await;
        settle().await;
        assert_eq!(rig.api.texts(), vec!["Завтра ясно."]);
        assert_eq!(rig.api.sounds(), 0);
        assert!(rig.voice.says.lock().is_empty());
        rig.stop().await;
    }

    #[tokio::test]
    async fn without_voice_replies_a_voice_message_is_run_and_answered_by_text() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        rig.voice.replies.store(false, Ordering::Relaxed);
        rig.send(voice_message("f1"));
        wait_for(|| rig.api.has_text("Завтра ясно.")).await;
        rig.idle().await;
        settle().await;
        assert_eq!(
            rig.api.texts(),
            vec![
                format!("{HEARD_MARK}какая завтра погода"),
                "Завтра ясно.".to_string()
            ]
        );
        assert_eq!(rig.voice.hears.lock().len(), 1);
        assert_eq!(rig.api.sounds(), 0);
        assert!(rig.voice.says.lock().is_empty(), "no speech is ordered");
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_reply_without_words_is_not_sent_as_sound() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        *rig.voice.say_answer.lock() = Ok(None);
        rig.send(voice_message("f1"));
        wait_for(|| rig.voice.says.lock().len() == 1).await;
        rig.idle().await;
        settle().await;
        assert_eq!(rig.api.sounds(), 0);
        assert_eq!(rig.api.texts().len(), 2, "{:?}", rig.api.texts());
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_typed_yes_inside_a_voiced_turn_keeps_the_voice_reply() {
        let rig = Rig::start(Arc::new(TimeThenAnswer)).await;
        rig.send(voice_message("f1"));
        wait_for(|| rig.session.confirms.pending().is_some()).await;
        // The question itself goes as text with buttons, never by voice.
        wait_for(|| rig.api.texts().len() == 2).await;
        assert!(rig.api.texts()[1].contains("Нужно подтверждение"));
        assert_eq!(rig.api.sounds(), 0);
        assert!(rig.voice.says.lock().is_empty());

        rig.send(typed("да"));
        wait_for(|| rig.api.sounds() == 1).await;
        rig.idle().await;
        settle().await;
        assert_eq!(rig.api.sounds(), 1);
        assert_eq!(*rig.voice.says.lock(), vec![("готово".to_string(), None)]);
        let out = rig.api.out.lock().clone();
        assert_eq!(
            out[out.len() - 2..],
            [Out::Text(OWNER, "готово".into()), Out::Sound(OWNER, mp3())]
        );
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_hint_about_an_open_question_is_not_said_by_voice() {
        let rig = Rig::start(Arc::new(TimeThenAnswer)).await;
        rig.send(voice_message("f1"));
        wait_for(|| rig.session.confirms.pending().is_some()).await;
        wait_for(|| rig.api.texts().len() == 2).await;
        let id = rig.session.confirms.pending().unwrap().id;

        // Typed text that is no answer: the session answers with a hint.
        rig.send(typed("а погода?"));
        wait_for(|| rig.api.texts().len() == 3).await;
        settle().await;
        assert!(rig.session.confirms.pending().is_some());
        assert_eq!(
            rig.api.sounds(),
            0,
            "nothing by voice while a question is open"
        );
        assert!(rig.voice.says.lock().is_empty());

        // The turn goes on, and its reply is still said by voice.
        rig.send(Update::Button {
            from: OWNER,
            callback_id: "cb".into(),
            data: format!("c:{id}:once"),
        });
        wait_for(|| rig.api.sounds() == 1).await;
        rig.idle().await;
        settle().await;
        assert_eq!(*rig.voice.says.lock(), vec![("готово".to_string(), None)]);
        assert_eq!(rig.api.sounds(), 1);
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_heard_yes_never_answers_a_question_that_opened_meanwhile() {
        let rig = Rig::start(Arc::new(TimeThenAnswer)).await;
        let mut rx = rig.bus.subscribe();
        let gate = Arc::new(Notify::new());
        *rig.voice.hear_gate.lock() = Some(gate.clone());
        rig.voice.heard.lock().push_back(Ok("Да.".into()));

        rig.send(voice_message("f1"));
        wait_for(|| rig.voice.hears.lock().len() == 1).await;
        // While the voice message is being recognised, a typed command asks a question.
        rig.send(typed("который час"));
        wait_for(|| rig.session.confirms.pending().is_some()).await;
        let id = rig.session.confirms.pending().unwrap().id;
        gate.notify_one();

        wait_for(|| rig.api.has_text(VOICE_ASKED_REPLY)).await;
        settle().await;
        assert!(rig.api.has_text(&format!("{HEARD_MARK}Да.")));
        assert_eq!(
            rig.session.confirms.pending().map(|p| p.id),
            Some(id),
            "the question is still open"
        );
        let events = events(&mut rx);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::ConfirmResolved { .. })),
            "nothing is approved: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::UserText { text } if text == "Да.")),
            "the heard text starts nothing: {events:?}"
        );
        assert!(rig.bridge.turn.expected.lock().is_empty());
        assert_eq!(rig.api.sounds(), 0);
        rig.session.stop();
        rig.idle().await;
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_stop_while_a_voice_message_is_heard_drops_it() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        let mut rx = rig.bus.subscribe();
        let gate = Arc::new(Notify::new());
        *rig.voice.hear_gate.lock() = Some(gate.clone());

        rig.send(voice_message("f1"));
        wait_for(|| rig.voice.hears.lock().len() == 1).await;
        // The command is not held up by the recognition.
        rig.send(typed("/stop"));
        wait_for(|| rig.api.has_text("Остановила.")).await;
        gate.notify_one();
        wait_for(|| rig.api.has_text(VOICE_STOPPED_REPLY)).await;
        settle().await;

        assert_eq!(
            rig.api.texts(),
            vec![
                "Остановила.".to_string(),
                format!("{HEARD_MARK}какая завтра погода"),
                VOICE_STOPPED_REPLY.to_string(),
            ]
        );
        let events = events(&mut rx);
        assert!(
            !events.iter().any(|e| matches!(e, Event::UserText { .. })),
            "no turn starts: {events:?}"
        );
        assert_eq!(rig.api.sounds(), 0);
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_stop_drops_a_voice_message_that_waits_for_its_turn_unheard() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        let mut rx = rig.bus.subscribe();
        let gate = Arc::new(Notify::new());
        *rig.voice.hear_gate.lock() = Some(gate.clone());

        rig.feed
            .send(vec![voice_message("f1"), voice_message("f2")])
            .unwrap();
        wait_for(|| rig.voice.hears.lock().len() == 1).await;
        settle().await;
        assert_eq!(rig.api.downloads.lock().len(), 1, "one at a time");
        rig.send(typed("/stop"));
        wait_for(|| rig.api.has_text("Остановила.")).await;
        gate.notify_one();
        wait_for(|| {
            rig.api
                .texts()
                .iter()
                .filter(|t| *t == VOICE_STOPPED_REPLY)
                .count()
                == 2
        })
        .await;
        settle().await;

        // The second one was neither downloaded nor recognised.
        assert_eq!(
            *rig.api.downloads.lock(),
            vec![("f1".to_string(), VOICE_MAX_BYTES)]
        );
        assert_eq!(rig.voice.hears.lock().len(), 1);
        let events = events(&mut rx);
        assert!(
            !events.iter().any(|e| matches!(e, Event::UserText { .. })),
            "no turn starts: {events:?}"
        );
        rig.stop().await;
    }

    #[tokio::test]
    async fn voice_messages_are_heard_one_at_a_time_in_the_order_they_came() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        let mut rx = rig.bus.subscribe();
        let gate = Arc::new(Notify::new());
        *rig.voice.hear_gate.lock() = Some(gate.clone());
        for text in ["первое дело", "второе дело", "третье дело"] {
            rig.voice.heard.lock().push_back(Ok(text.into()));
        }

        rig.feed
            .send(vec![
                voice_message("f1"),
                voice_message("f2"),
                voice_message("f3"),
            ])
            .unwrap();
        wait_for(|| rig.voice.hears.lock().len() == 1).await;
        settle().await;
        assert_eq!(rig.voice.hears.lock().len(), 1, "the others wait");
        // A text command is not held up meanwhile.
        rig.send(typed("/help"));
        wait_for(|| rig.api.has_text(crate::telegram::HELLO)).await;

        *rig.voice.hear_gate.lock() = None;
        gate.notify_one();
        wait_for(|| rig.api.has_text(&format!("{HEARD_MARK}третье дело"))).await;
        wait_for(|| {
            rig.api
                .texts()
                .iter()
                .filter(|t| *t == "Завтра ясно.")
                .count()
                == 3
        })
        .await;
        rig.idle().await;

        let files: Vec<String> = rig
            .api
            .downloads
            .lock()
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        assert_eq!(files, ["f1", "f2", "f3"]);
        let turns: Vec<String> = events(&mut rx)
            .into_iter()
            .filter_map(|e| match e {
                Event::UserText { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(turns, ["первое дело", "второе дело", "третье дело"]);
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_heard_stop_stops_the_running_turn_and_starts_none() {
        let rig = Rig::start(Arc::new(Never)).await;
        let mut rx = rig.bus.subscribe();
        rig.send(typed("долгая задача"));
        wait_for(|| rig.state.get() == AssistantState::Thinking).await;
        rig.voice.heard.lock().push_back(Ok(" Стоп. ".into()));

        rig.send(voice_message("f1"));
        wait_for(|| rig.api.has_text("Остановила.")).await;
        rig.idle().await;
        settle().await;

        assert_eq!(
            rig.api.texts(),
            vec![format!("{HEARD_MARK}Стоп."), "Остановила.".to_string()]
        );
        let events = events(&mut rx);
        let turns: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(e, Event::UserText { .. }))
            .collect();
        assert_eq!(
            turns,
            [&Event::UserText {
                text: "долгая задача".into()
            }]
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::AssistantText { .. })),
            "the stopped turn gives no reply"
        );
        assert_eq!(rig.api.sounds(), 0);
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_new_message_drops_a_voice_reply_that_is_still_being_made() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        let gate = Arc::new(Notify::new());
        *rig.voice.say_gate.lock() = Some(gate.clone());

        rig.send(voice_message("f1"));
        wait_for(|| rig.voice.says.lock().len() == 1).await;
        // The owner writes on while the speech is being made.
        rig.send(typed("а послезавтра?"));
        wait_for(|| {
            rig.api
                .texts()
                .iter()
                .filter(|t| *t == "Завтра ясно.")
                .count()
                == 2
        })
        .await;
        gate.notify_one();
        rig.idle().await;
        settle().await;

        assert_eq!(rig.api.sounds(), 0, "{:?}", rig.api.out.lock());
        assert_eq!(rig.voice.says.lock().len(), 1, "the typed turn orders none");
        assert!(!rig
            .api
            .texts()
            .iter()
            .any(|t| t.starts_with(VOICE_REPLY_FAILED)));
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_failed_voice_reply_is_told_after_the_text() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        *rig.voice.say_answer.lock() = Err("синтез речи ответил 500: сбой".into());
        rig.send(voice_message("f1"));
        let failed = format!("{VOICE_REPLY_FAILED}синтез речи ответил 500: сбой");
        wait_for(|| rig.api.has_text(&failed)).await;
        rig.idle().await;
        assert_eq!(
            rig.api.texts(),
            vec![
                format!("{HEARD_MARK}какая завтра погода"),
                "Завтра ясно.".to_string(),
                failed,
            ]
        );
        assert_eq!(rig.api.sounds(), 0);

        // Telegram refuses the sound: the same.
        *rig.voice.say_answer.lock() = Ok(Some(mp3()));
        *rig.api.sound_refused.lock() = Some("Telegram ответил ошибкой: file is too big".into());
        rig.send(voice_message("f2"));
        let refused = format!("{VOICE_REPLY_FAILED}Telegram ответил ошибкой: file is too big");
        wait_for(|| rig.api.has_text(&refused)).await;
        rig.idle().await;
        assert_eq!(rig.api.sounds(), 0);
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_voice_message_that_cannot_be_heard_is_explained() {
        let (s, bus, _) = session(Arc::new(Says("Завтра ясно.")));
        let mut rx = bus.subscribe();
        let (api, _feed) = api();
        let voice = fake_voice();
        let bridge = VoiceBridge::new(voice.clone()).with_bus(bus.clone());

        *api.file.lock() = Err("файл больше 20 МБ".into());
        handle_update_with(api.as_ref(), &s, OWNER, voice_message("f1"), Some(&bridge)).await;
        assert!(voice.hears.lock().is_empty(), "nothing to recognise");

        *api.file.lock() = Ok(OGG.to_vec());
        voice
            .heard
            .lock()
            .push_back(Err("распознавание ответило 401: bad key".into()));
        handle_update_with(api.as_ref(), &s, OWNER, voice_message("f2"), Some(&bridge)).await;

        voice.heard.lock().push_back(Ok("  ".into()));
        handle_update_with(api.as_ref(), &s, OWNER, voice_message("f3"), Some(&bridge)).await;

        assert_eq!(
            api.texts(),
            vec![
                format!("{VOICE_DOWNLOAD_FAILED}файл больше 20 МБ"),
                format!("{VOICE_HEAR_FAILED}распознавание ответило 401: bad key"),
                VOICE_EMPTY_REPLY.to_string(),
            ]
        );
        settle().await;
        assert!(rx.try_recv().is_err(), "no event may reach the bus");
    }

    #[test]
    fn only_expected_texts_make_a_turn_voiced() {
        let bridge = VoiceBridge::new(fake_voice());
        bridge.turn_started("привет");
        assert!(!bridge.take_voiced());

        bridge.expect(" какая погода ");
        // A typed answer inside the turn changes nothing.
        bridge.turn_started("да");
        assert!(!bridge.take_voiced());
        bridge.turn_started("какая погода");
        bridge.turn_started("да");
        assert!(bridge.take_voiced());
        assert!(!bridge.take_voiced(), "asked once");

        bridge.expect("раз");
        bridge.turn_started("раз");
        bridge.turn_ended();
        assert!(!bridge.take_voiced());

        // The same words twice are two turns.
        bridge.expect("ещё");
        bridge.expect("ещё");
        bridge.forget("ещё");
        bridge.turn_started("ещё");
        assert!(bridge.take_voiced());
        bridge.turn_started("ещё");
        assert!(!bridge.take_voiced());

        // A stop: nothing is expected, the turn is not voiced.
        bridge.expect("два");
        bridge.expect("три");
        bridge.turn_started("два");
        bridge.stopped();
        assert!(!bridge.take_voiced());
        bridge.turn_started("три");
        assert!(!bridge.take_voiced());

        // Old texts whose turns never started are let go.
        for n in 0..20 {
            bridge.expect(&format!("текст {n}"));
        }
        assert_eq!(bridge.turn.expected.lock().len(), EXPECTED_KEPT);
        bridge.turn_started("текст 0");
        assert!(!bridge.take_voiced());
        bridge.turn_started("текст 19");
        assert!(bridge.take_voiced());
    }

    #[test]
    fn a_reply_being_made_is_dropped_by_typing_a_stop_and_nothing_else() {
        let bridge = VoiceBridge::new(fake_voice());
        let first = bridge.turn.reply.lock().clone();
        bridge.turn_started("x");
        bridge.turn_ended();
        assert!(!first.is_cancelled());
        bridge.typed();
        assert!(first.is_cancelled());
        let second = bridge.turn.reply.lock().clone();
        assert!(!second.is_cancelled());
        bridge.stopped();
        assert!(second.is_cancelled());
        assert!(!bridge.turn.reply.lock().is_cancelled());
    }

    #[test]
    fn stop_phrases_are_the_word_alone() {
        for stop in [
            "стоп",
            "Стоп.",
            " СТОП! ",
            "остановись",
            "Хватит…",
            "стоп,",
            "stop",
            "Stop.",
            "Стой",
            "Перестань слушать",
        ] {
            assert!(is_stop_phrase(stop), "{stop}");
        }
        for other in [
            "",
            "стоп машина",
            "не останавливайся",
            "поставь на стоп",
            "хватит ли места на диске",
            "stop the music",
            "don't stop",
        ] {
            assert!(!is_stop_phrase(other), "{other}");
        }
    }

    #[test]
    fn only_plain_file_paths_are_downloaded() {
        for ok in [
            "voice/file_12.oga",
            "voice/file-3.ogg",
            "a",
            "documents/A_b-1.OGG",
        ] {
            assert!(safe_file_path(ok), "{ok}");
        }
        let long = "a".repeat(257);
        for bad in [
            "",
            "/voice/file.oga",
            "../x",
            "voice/../../x",
            "voice/..",
            "voice/file 1.oga",
            "voice/file.oga?x=1",
            "voice/file.oga#x",
            "voice\\file.oga",
            "voice/файл.oga",
            "https://evil.test/x",
            "voice/%2e%2e/x",
            "voice/file.oga\n",
            long.as_str(),
        ] {
            assert!(!safe_file_path(bad), "{bad:?}");
        }
        assert!(safe_file_path(&"a".repeat(256)));
    }

    #[tokio::test]
    async fn a_stop_on_the_computer_drops_voice_messages_heard_and_waiting() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        let mut rx = rig.bus.subscribe();
        let gate = Arc::new(Notify::new());
        *rig.voice.hear_gate.lock() = Some(gate.clone());

        rig.feed
            .send(vec![voice_message("f1"), voice_message("f2")])
            .unwrap();
        wait_for(|| rig.voice.hears.lock().len() == 1).await;
        // The Stop button of the chat: the bridge is not told, the session is.
        rig.session.stop();
        gate.notify_one();
        wait_for(|| {
            rig.api
                .texts()
                .iter()
                .filter(|t| *t == VOICE_STOPPED_REPLY)
                .count()
                == 2
        })
        .await;
        settle().await;

        assert_eq!(
            rig.api.texts(),
            vec![
                format!("{HEARD_MARK}какая завтра погода"),
                VOICE_STOPPED_REPLY.to_string(),
                VOICE_STOPPED_REPLY.to_string(),
            ]
        );
        // The one that waited was neither downloaded nor recognised.
        assert_eq!(
            *rig.api.downloads.lock(),
            vec![("f1".to_string(), VOICE_MAX_BYTES)]
        );
        assert_eq!(rig.voice.hears.lock().len(), 1);
        let events = events(&mut rx);
        assert!(
            !events.iter().any(|e| matches!(e, Event::UserText { .. })),
            "no turn starts after the stop: {events:?}"
        );

        // One sent after the stop is heard and run as usual.
        *rig.voice.hear_gate.lock() = None;
        rig.send(voice_message("f3"));
        wait_for(|| rig.api.sounds() == 1).await;
        rig.idle().await;
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_stop_on_the_computer_drops_a_voice_reply_that_is_still_being_made() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        let gate = Arc::new(Notify::new());
        *rig.voice.say_gate.lock() = Some(gate.clone());

        rig.send(voice_message("f1"));
        wait_for(|| rig.voice.says.lock().len() == 1).await;
        rig.session.stop();
        gate.notify_one();
        settle().await;

        assert_eq!(rig.api.sounds(), 0, "{:?}", rig.api.out.lock());
        assert!(!rig
            .api
            .texts()
            .iter()
            .any(|t| t.starts_with(VOICE_REPLY_FAILED)));
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_voice_message_said_long_ago_is_not_downloaded() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        let mut rx = rig.bus.subscribe();
        // Left at Telegram while the app was off: both come at the start.
        let old = |file_id: &str, age: i64| Update::Voice {
            from: OWNER,
            file_id: file_id.into(),
            duration_s: 4,
            bytes: 9_000,
            forwarded: false,
            sent: unix_now() - age,
        };
        rig.feed
            .send(vec![old("f1", 3 * 3600), old("f2", VOICE_FRESH_S + 30)])
            .unwrap();
        wait_for(|| rig.api.texts().len() == 2).await;
        settle().await;
        assert_eq!(rig.api.texts(), vec![VOICE_OLD_REPLY, VOICE_OLD_REPLY]);
        assert!(rig.api.downloads.lock().is_empty());
        assert!(rig.voice.hears.lock().is_empty());
        assert!(events(&mut rx).is_empty(), "nothing reaches the bus");
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_voice_message_longer_than_it_says_is_not_recognised() {
        let (s, bus, _) = session(Arc::new(Says("Завтра ясно.")));
        let mut rx = bus.subscribe();
        let (api, _feed) = api();
        let voice = fake_voice();
        let bridge = VoiceBridge::new(voice.clone()).with_bus(bus.clone());

        // It names 4 seconds; its pages say more than an hour.
        *api.file.lock() = Ok(opus(4000));
        handle_update_with(api.as_ref(), &s, OWNER, voice_message("f1"), Some(&bridge)).await;
        // Its pages cannot be read, and it is far too large for 4 seconds.
        let mut heavy = OGG.to_vec();
        heavy.resize(honest_bytes(4) as usize + 1, 7);
        *api.file.lock() = Ok(heavy);
        handle_update_with(api.as_ref(), &s, OWNER, voice_message("f2"), Some(&bridge)).await;
        // A little over the limit is the rounding of the sender, a lot is not.
        *api.file.lock() = Ok(opus(u64::from(VOICE_MAX_S + VOICE_SLACK_S) + 1));
        let long = Update::Voice {
            from: OWNER,
            file_id: "f3".into(),
            duration_s: VOICE_MAX_S,
            bytes: 9_000,
            forwarded: false,
            sent: unix_now(),
        };
        handle_update_with(api.as_ref(), &s, OWNER, long.clone(), Some(&bridge)).await;
        settle().await;

        assert_eq!(api.texts(), vec![VOICE_LONG_REPLY; 3]);
        assert_eq!(api.downloads.lock().len(), 3);
        assert!(voice.hears.lock().is_empty(), "nothing is paid for");
        assert!(rx.try_recv().is_err(), "no event may reach the bus");

        // As long as it says, and as long as the limit with its margin.
        *api.file.lock() = Ok(opus(u64::from(VOICE_MAX_S + VOICE_SLACK_S)));
        handle_update_with(api.as_ref(), &s, OWNER, long, Some(&bridge)).await;
        assert_eq!(voice.hears.lock().len(), 1);
        assert!(api.has_text(&format!("{HEARD_MARK}какая завтра погода")));
    }

    #[tokio::test]
    async fn the_recognition_time_is_not_published_into_a_reply_being_written() {
        let gate = Arc::new(Notify::new());
        let rig = Rig::start(Arc::new(Gated(gate.clone(), "Готово."))).await;
        let mut rx = rig.bus.subscribe();
        rig.send(typed("долгая задача"));
        wait_for(|| rig.state.get() == AssistantState::Thinking).await;

        // A voice message is recognised while that reply is still on its way.
        rig.send(voice_message("f1"));
        wait_for(|| {
            rig.api
                .has_text(&format!("{HEARD_MARK}какая завтра погода"))
        })
        .await;
        settle().await;
        let early = events(&mut rx);
        assert!(
            !early.iter().any(|e| matches!(e, Event::Timing { .. })),
            "the chat would count it to the running reply: {early:?}"
        );
        assert!(
            !early
                .iter()
                .any(|e| matches!(e, Event::UserText { text } if text == "какая завтра погода")),
            "its turn waits: {early:?}"
        );

        // The first reply ends, then the turn of the voice message runs.
        gate.notify_one();
        wait_for(|| rig.api.has_text("Готово.")).await;
        gate.notify_one();
        wait_for(|| rig.api.sounds() == 1).await;
        rig.idle().await;
        settle().await;
        let late = events(&mut rx);
        assert!(
            !late
                .iter()
                .any(|e| matches!(e, Event::Timing { stage, .. } if stage == "stt")),
            "{late:?}"
        );
        assert!(late
            .iter()
            .any(|e| matches!(e, Event::UserText { text } if text == "какая завтра погода")));
        rig.stop().await;
    }

    #[tokio::test]
    async fn a_hint_is_not_voiced_when_its_question_closes_while_it_is_sent() {
        let rig = Rig::start(Arc::new(TimeThenAnswer)).await;
        rig.send(voice_message("f1"));
        wait_for(|| rig.session.confirms.pending().is_some()).await;
        wait_for(|| rig.api.texts().len() == 2).await;
        let id = rig.session.confirms.pending().unwrap().id;

        // The network is slow: the hint about the open question hangs in `send`.
        let gate = Arc::new(Notify::new());
        *rig.api.send_gate.lock() = Some(gate.clone());
        rig.send(typed("а погода?"));
        settle().await;
        // Meanwhile the question is answered and the turn ends.
        rig.send(Update::Button {
            from: OWNER,
            callback_id: "cb".into(),
            data: format!("c:{id}:once"),
        });
        wait_for(|| rig.session.confirms.pending().is_none()).await;
        rig.idle().await;
        settle().await;
        assert_eq!(rig.api.texts().len(), 2, "the hint is still being sent");
        *rig.api.send_gate.lock() = None;
        gate.notify_one();

        wait_for(|| rig.api.sounds() == 1).await;
        settle().await;
        // The reply of the turn is said, not the hint.
        assert_eq!(*rig.voice.says.lock(), vec![("готово".to_string(), None)]);
        assert_eq!(rig.api.sounds(), 1);
        let out = rig.api.out.lock().clone();
        assert_eq!(
            out[out.len() - 2..],
            [Out::Text(OWNER, "готово".into()), Out::Sound(OWNER, mp3())]
        );
        rig.stop().await;
    }

    #[tokio::test]
    async fn the_end_of_the_bridge_tells_of_voice_messages_it_did_not_hear() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        let mut rx = rig.bus.subscribe();
        let gate = Arc::new(Notify::new());
        *rig.voice.hear_gate.lock() = Some(gate.clone());
        rig.feed
            .send(vec![voice_message("f1"), voice_message("f2")])
            .unwrap();
        wait_for(|| rig.voice.hears.lock().len() == 1).await;
        settle().await;

        // The settings were saved: the bridge is restarted.
        let (api, voice) = (rig.api.clone(), rig.voice.clone());
        rig.stop().await;
        gate.notify_one();
        settle().await;

        // One line for both, and neither is run.
        assert_eq!(api.texts(), vec![VOICE_RESTART_REPLY]);
        assert_eq!(api.downloads.lock().len(), 1);
        assert_eq!(voice.hears.lock().len(), 1);
        let events = events(&mut rx);
        assert!(
            !events.iter().any(|e| matches!(e, Event::UserText { .. })),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn a_bridge_that_heard_everything_ends_without_a_word() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        rig.send(voice_message("f1"));
        wait_for(|| rig.api.sounds() == 1).await;
        rig.idle().await;
        let api = rig.api.clone();
        rig.stop().await;
        assert!(!api.has_text(VOICE_RESTART_REPLY));
    }

    #[tokio::test]
    async fn a_voice_reply_does_not_outlive_the_bridge() {
        let rig = Rig::start(Arc::new(Says("Завтра ясно."))).await;
        let gate = Arc::new(Notify::new());
        *rig.voice.say_gate.lock() = Some(gate.clone());
        rig.send(voice_message("f1"));
        wait_for(|| rig.voice.says.lock().len() == 1).await;
        rig.idle().await;

        // Telegram is switched off while the speech is being made.
        let api = rig.api.clone();
        rig.stop().await;
        gate.notify_one();
        settle().await;
        assert_eq!(api.sounds(), 0, "{:?}", api.out.lock());
        assert!(!api
            .texts()
            .iter()
            .any(|t| t.starts_with(VOICE_REPLY_FAILED)));
    }

    #[test]
    fn the_refusal_of_a_long_voice_message_names_the_limit() {
        assert_eq!(VOICE_MAX_S % 60, 0);
        assert!(
            VOICE_LONG_REPLY.contains(&format!("длиннее {} минут", VOICE_MAX_S / 60)),
            "{VOICE_LONG_REPLY}"
        );
        assert_eq!(VOICE_FRESH_S % 60, 0);
        assert!(
            VOICE_OLD_REPLY.contains(&format!("больше {} минут", VOICE_FRESH_S / 60)),
            "{VOICE_OLD_REPLY}"
        );
        // A voice message of the longest length fits the largest honest size.
        assert!(honest_bytes(VOICE_MAX_S) < VOICE_MAX_BYTES);
        assert_eq!(honest_bytes(u32::MAX), honest_bytes(VOICE_MAX_S));
        assert_eq!(VOICE_MAX_BYTES, 20 * 1024 * 1024);
        assert_eq!(SOUND_MAX_BYTES, 50 * 1024 * 1024);
    }

    /// One synthesized voice note goes to the saved owner through the saved
    /// bot: how the client shows it is seen on the phone. The saved settings,
    /// key and token are used as the app uses them.
    ///
    /// `cargo test -p mika-providers live_telegram_voice_note -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "calls the speech provider (paid) and sends a message through the saved Telegram bot"]
    async fn live_telegram_voice_note() {
        use mika_core::config::{Config, TELEGRAM_SECRET_KEY};
        use mika_core::paths::config_path;
        use mika_core::secrets::{KeyringSecretStore, SecretStore};
        use mika_core::sound::NoConverter;

        let config = Config::load(&config_path())
            .unwrap_or_else(|e| panic!("the settings of Mika do not load: {e}"));
        let owner = config.telegram.user_id;
        assert!(owner != 0, "no Telegram owner in the settings");
        let secrets = Arc::new(KeyringSecretStore::new());
        let token = secrets
            .get(TELEGRAM_SECRET_KEY)
            .ok()
            .flatten()
            .filter(|t| !t.is_empty())
            .expect("no bot token is saved");
        let live = Live::new(Arc::new(Mutex::new(config)), secrets);
        let voice = LiveVoice::new(
            live,
            Arc::new(NoConverter),
            Arc::new(|text: &str, _: Option<&str>| Some(text.to_string())),
            Arc::new(String::new),
        );
        let sound = voice
            .say("Проверка голосового сообщения.", None)
            .await
            .unwrap_or_else(|e| panic!("speech synthesis failed: {e}"))
            .expect("the note has words");
        println!("sound: {:?}, {} bytes", sound.kind, sound.bytes.len());
        crate::telegram::HttpTelegram::new(token)
            .send_sound(owner, &sound)
            .await
            .unwrap_or_else(|e| panic!("Telegram did not take the sound: {e}"));
        println!(
            "sent as {}",
            if sound.kind.voice_message() {
                "a voice message"
            } else {
                "a document"
            }
        );
    }
}
