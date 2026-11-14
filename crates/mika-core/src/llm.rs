//! Provider-neutral LLM types.

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// PNG pictures in base64, for models that can see.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

impl ChatMessage {
    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            images: Vec::new(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::plain(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::plain(Role::User, content)
    }

    /// A user message with one PNG picture in base64.
    pub fn user_with_image(content: impl Into<String>, png_base64: impl Into<String>) -> Self {
        Self {
            images: vec![png_base64.into()],
            ..Self::plain(Role::User, content)
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::plain(Role::Assistant, content)
    }

    pub fn assistant_with_calls(content: impl Into<String>, calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls: calls,
            ..Self::plain(Role::Assistant, content)
        }
    }

    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(id.into()),
            ..Self::plain(Role::Tool, content)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LlmChunk {
    Text(String),
    ToolCall(ToolCall),
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LlmError {
    #[error("нет связи с провайдером: {0}")]
    Network(String),
    #[error("провайдер ответил {status}: {body}")]
    Http { status: u16, body: String },
    /// The answer is not what the protocol promises. The text is the app's
    /// own and quotes nothing of the answer: it is written to the journal
    /// as it is.
    #[error("непонятный ответ провайдера: {0}")]
    Protocol(String),
    /// A piece of the answer is not JSON. `why` is what the parser said,
    /// `data` is the piece itself: what the model sent.
    #[error("непонятный ответ провайдера: {why}: {data}")]
    BadJson { why: String, data: String },
    /// The provider itself reported an error inside its answer, after the
    /// status 200: overloaded, no credits, the context is too long.
    #[error("провайдер сообщил об ошибке: {0}")]
    Provider(String),
    #[error("остановлено")]
    Cancelled,
    #[error("провайдер не настроен: {0}")]
    NotConfigured(String),
}

impl LlmError {
    /// The kind of the error as the journal names it.
    pub fn kind(&self) -> &'static str {
        match self {
            LlmError::Network(_) => "network",
            LlmError::Http { .. } => "http",
            LlmError::Protocol(_) => "protocol",
            LlmError::BadJson { .. } => "bad_json",
            LlmError::Provider(_) => "provider_error",
            LlmError::Cancelled => "stopped",
            LlmError::NotConfigured(_) => "not_configured",
        }
    }

    /// The text of the error for a record. What the provider says about its
    /// own refusal is written, in the answer as in the status. A piece that
    /// could not be read is what the model sent: of it only what the parser
    /// said is written, and the piece itself while texts are allowed.
    pub fn journal_text(&self) -> String {
        match self {
            LlmError::Network(why) | LlmError::NotConfigured(why) | LlmError::Protocol(why) => {
                crate::journal::error_text(why)
            }
            LlmError::Http { body, .. } => crate::journal::error_text(body),
            LlmError::Provider(said) => crate::journal::error_text(said),
            LlmError::BadJson { why, data } => format!(
                "{} data={}",
                crate::journal::error_text(why),
                crate::journal::content(data)
            ),
            LlmError::Cancelled => String::new(),
        }
    }
}

pub type LlmStream = BoxStream<'static, Result<LlmChunk, LlmError>>;

/// A model that can look at a picture.
#[async_trait]
pub trait Vision: Send + Sync {
    async fn ask(&self, png: &[u8], question: &str) -> Result<String, String>;

    /// Several frames of one video, in time order. By default only the first
    /// frame is shown.
    async fn ask_frames(&self, pngs: &[Vec<u8>], question: &str) -> Result<String, String> {
        match pngs.first() {
            Some(png) => self.ask(png, question).await,
            None => Err("нет кадров".into()),
        }
    }
}

/// A recognised phrase with its time, seconds from the start of the WAV.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Speech recognition. Takes a WAV file, returns the text that was said.
#[async_trait]
pub trait SpeechToText: Send + Sync {
    async fn transcribe(&self, wav: &[u8]) -> Result<String, String>;

    /// Phrases with their times. `None` when the provider cannot tell times.
    async fn transcribe_segments(&self, _wav: &[u8]) -> Result<Option<Vec<SpeechSegment>>, String> {
        Ok(None)
    }
}

/// Speech synthesis. Returns an audio file: WAV, MP3 or Ogg.
#[async_trait]
pub trait TextToSpeech: Send + Sync {
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>, String>;
}

/// A model that draws pictures. Returns PNG, JPEG or WebP bytes.
#[async_trait]
pub trait ImageGen: Send + Sync {
    async fn generate(&self, prompt: &str) -> Result<Vec<u8>, String>;
}

/// Least room for reasoning inside one text request, in characters.
const REASONING_ROOM: usize = 3000;

/// Most characters one text request reads from the model before it stops: the
/// answer and room for reasoning the model writes into its text, as much again
/// as the answer and 3000 characters at least. A request cannot cost more
/// output than this, as far as the text shows it.
pub fn stream_cap(max_chars: usize) -> usize {
    max_chars.saturating_add(max_chars.max(REASONING_ROOM))
}

/// The answer of a text request and what was read to get it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextAnswer {
    pub text: String,
    /// Characters the model streamed, reasoning blocks included: the output
    /// the request cost.
    pub streamed: usize,
}

/// Why a text request gave no answer, and what was read before it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFailure {
    pub reason: String,
    /// Characters the model streamed before the failure.
    pub streamed: usize,
}

impl TextFailure {
    /// A failure before anything was read.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            streamed: 0,
        }
    }
}

impl std::fmt::Display for TextFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// A model asked for plain text by a tool. The request carries no tools, and
/// tool calls the model tries anyway are dropped.
#[async_trait]
pub trait TextModel: Send + Sync {
    /// The model id that `model` means: an empty `model` is the chat model.
    fn model_id(&self, model: &str) -> String;

    /// One request, the whole answer with reasoning blocks cut out. Reading
    /// stops once the answer is longer than `max_chars` characters, or once
    /// the model has streamed more than `stream_cap(max_chars)` characters
    /// with its reasoning. Both outcomes say how much was streamed.
    async fn complete(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_chars: usize,
    ) -> Result<TextAnswer, TextFailure>;
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError>;
}

/// Reasoning that some models write into the reply text, as (open, close) markers:
/// Gemma's thought channel and the `<think>` tags of Qwen and DeepSeek.
/// Blocks that never reach the user: thoughts, and Gemma's own tool-call and
/// tool-result tokens when it writes them into the text instead of the
/// tool channel (`<|tool_call>[]`).
const THOUGHT_MARKERS: &[(&str, &str)] = &[
    ("<|channel>", "<channel|>"),
    ("<think>", "</think>"),
    ("<|tool_call>", "<tool_call|>"),
    ("<|tool_response>", "<tool_response|>"),
];

/// Cuts reasoning blocks out of streamed text. A marker may arrive split across
/// chunks, so a tail that could start one is held back.
#[derive(Debug, Default)]
pub struct ThoughtFilter {
    held: String,
    /// Index into `THOUGHT_MARKERS` of the open block.
    inside: Option<usize>,
}

impl ThoughtFilter {
    /// Length in bytes of the longest tail of `s` that is a proper prefix of one of `marks`.
    fn partial_tail(s: &str, marks: &[&str]) -> usize {
        marks
            .iter()
            .flat_map(|m| {
                (1..m.len())
                    .filter(|k| m.is_char_boundary(*k))
                    .map(move |k| &m[..k])
            })
            .filter(|p| s.ends_with(p))
            .map(str::len)
            .max()
            .unwrap_or(0)
    }

    pub fn push(&mut self, chunk: &str) -> String {
        self.held.push_str(chunk);
        let mut out = String::new();
        loop {
            match self.inside {
                None => {
                    let found = THOUGHT_MARKERS
                        .iter()
                        .enumerate()
                        .filter_map(|(i, (open, _))| self.held.find(open).map(|at| (at, i)))
                        .min();
                    if let Some((at, i)) = found {
                        out.push_str(&self.held[..at]);
                        self.held.drain(..at + THOUGHT_MARKERS[i].0.len());
                        self.inside = Some(i);
                        continue;
                    }
                    let opens: Vec<&str> = THOUGHT_MARKERS.iter().map(|(o, _)| *o).collect();
                    let keep = Self::partial_tail(&self.held, &opens);
                    let cut = self.held.len() - keep;
                    out.push_str(&self.held[..cut]);
                    self.held.drain(..cut);
                    return out;
                }
                Some(i) => {
                    let close = THOUGHT_MARKERS[i].1;
                    if let Some(at) = self.held.find(close) {
                        self.held.drain(..at + close.len());
                        self.inside = None;
                        continue;
                    }
                    let keep = Self::partial_tail(&self.held, &[close]);
                    let cut = self.held.len() - keep;
                    self.held.drain(..cut);
                    return out;
                }
            }
        }
    }

    /// Text still held when the stream ends; an unclosed block is dropped.
    pub fn finish(&mut self) -> String {
        let held = std::mem::take(&mut self.held);
        if self.inside.take().is_some() {
            String::new()
        } else {
            held
        }
    }
}

/// One short answer without tools: the whole text the model streams back,
/// reasoning blocks cut out, trimmed. `None` on an error, an empty answer or
/// after `timeout`.
pub async fn complete(
    provider: &dyn LlmProvider,
    system: &str,
    user: &str,
    timeout: std::time::Duration,
) -> Option<String> {
    use futures::StreamExt;
    let req = LlmRequest {
        messages: vec![ChatMessage::system(system), ChatMessage::user(user)],
        tools: Vec::new(),
    };
    let collect = async {
        let mut stream = provider.stream(req).await.ok()?;
        let mut text = String::new();
        // A retelling is read aloud: the model's reasoning must not be.
        let mut thoughts = ThoughtFilter::default();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(LlmChunk::Text(t)) => text.push_str(&thoughts.push(&t)),
                Ok(LlmChunk::ToolCall(_)) => {}
                Err(_) => return None,
            }
        }
        text.push_str(&thoughts.finish());
        let text = text.trim().to_string();
        (!text.is_empty()).then_some(text)
    };
    tokio::time::timeout(timeout, collect).await.ok().flatten()
}

/// What `complete_capped` says when the model spent the whole request on
/// reasoning.
pub const ONLY_REASONING: &str = "модель рассуждала дольше, чем отведено на ответ";

/// One request without tools: the text the model streams back, reasoning
/// blocks cut out, at most `max_chars` characters, trimmed.
///
/// Tool calls the model tries anyway are dropped. Reading stops as soon as the
/// answer is longer than `max_chars`, or the model has streamed more than
/// `stream_cap(max_chars)` characters, reasoning included: text the filter
/// cuts out is paid for like any other. The stream is then dropped, and the
/// provider's reader ends when it sees that. There is no timeout inside:
/// callers wrap it.
pub async fn complete_capped(
    provider: &dyn LlmProvider,
    system: &str,
    user: &str,
    max_chars: usize,
) -> Result<TextAnswer, TextFailure> {
    use futures::StreamExt;
    let req = LlmRequest {
        messages: vec![ChatMessage::system(system), ChatMessage::user(user)],
        tools: Vec::new(),
    };
    let mut stream = provider
        .stream(req)
        .await
        .map_err(|e| TextFailure::new(e.to_string()))?;
    let raw_cap = stream_cap(max_chars);
    let mut text = String::new();
    let mut chars = 0usize;
    let mut streamed = 0usize;
    let mut thoughts = ThoughtFilter::default();
    let mut capped = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| TextFailure {
            reason: e.to_string(),
            streamed,
        })?;
        match chunk {
            LlmChunk::Text(t) => {
                streamed += t.chars().count();
                let kept = thoughts.push(&t);
                chars += kept.chars().count();
                text.push_str(&kept);
                if chars > max_chars || streamed > raw_cap {
                    capped = true;
                    break;
                }
            }
            LlmChunk::ToolCall(_) => {}
        }
    }
    drop(stream);
    if !capped {
        text.push_str(&thoughts.finish());
    }
    if let Some((at, _)) = text.char_indices().nth(max_chars) {
        text.truncate(at);
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        let reason = if capped {
            ONLY_REASONING.to_string()
        } else {
            LlmError::Protocol("пустой ответ".into()).to_string()
        };
        return Err(TextFailure { reason, streamed });
    }
    Ok(TextAnswer { text, streamed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_serde_roundtrip() {
        let msgs = vec![
            ChatMessage::user("привет"),
            ChatMessage::assistant_with_calls(
                "",
                vec![ToolCall {
                    id: "c1".into(),
                    name: "get_time".into(),
                    arguments: serde_json::json!({}),
                }],
            ),
            ChatMessage::tool_result("c1", "12:00"),
        ];
        let json = serde_json::to_string(&msgs).unwrap();
        let back: Vec<ChatMessage> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msgs);
        assert!(!json.contains("tool_call_id\":null"));
    }

    fn thoughts(chunks: &[&str]) -> String {
        let mut f = ThoughtFilter::default();
        let mut out: String = chunks.iter().map(|c| f.push(c)).collect();
        out.push_str(&f.finish());
        out
    }

    #[test]
    fn thought_blocks_are_cut_even_when_split() {
        assert_eq!(
            thoughts(&["<|channel>thought\nсчитаю…<channel|>Ответ: 51"]),
            "Ответ: 51"
        );
        assert_eq!(
            thoughts(&["Сейчас <|chan", "nel>thought думаю<chan", "nel|> готово"]),
            "Сейчас  готово"
        );
        assert_eq!(thoughts(&["<think>план</think>Привет"]), "Привет");
        assert_eq!(thoughts(&["<|channel>thought и обрыв"]), "");
        assert_eq!(
            thoughts(&["<|tool_call>[]"]),
            "",
            "an unclosed block is dropped"
        );
        assert_eq!(
            thoughts(&["a < b и <think"]),
            "a < b и <think",
            "a lone prefix comes back at the end"
        );
        assert_eq!(thoughts(&["обычный текст"]), "обычный текст");
    }

    /// Streams the given text chunks once.
    struct Chunks(Vec<&'static str>);

    #[async_trait]
    impl LlmProvider for Chunks {
        fn name(&self) -> &str {
            "chunks"
        }
        async fn stream(&self, _req: LlmRequest) -> Result<LlmStream, LlmError> {
            use futures::StreamExt;
            let chunks: Vec<Result<LlmChunk, LlmError>> = self
                .0
                .iter()
                .map(|t| Ok(LlmChunk::Text(t.to_string())))
                .collect();
            Ok(futures::stream::iter(chunks).boxed())
        }
    }

    async fn retell(chunks: Vec<&'static str>) -> Option<String> {
        complete(
            &Chunks(chunks),
            "system",
            "user",
            std::time::Duration::from_secs(5),
        )
        .await
    }

    #[tokio::test]
    async fn complete_cuts_out_reasoning() {
        assert_eq!(
            retell(vec![
                "<think>Okay, the user wants",
                " a short retelling…</th",
                "ink> Нашла три варианта.",
            ])
            .await
            .as_deref(),
            Some("Нашла три варианта.")
        );
        assert_eq!(
            retell(vec!["<|channel>thought\nretell it<channel|>Готово."])
                .await
                .as_deref(),
            Some("Готово.")
        );
        assert_eq!(
            retell(vec!["<think>only thinking, no answer"]).await,
            None,
            "nothing but reasoning is no answer"
        );
        assert_eq!(
            retell(vec!["  Просто ", "ответ "]).await.as_deref(),
            Some("Просто ответ")
        );
    }

    /// Streams the given chunks once, remembers the request and counts the
    /// chunks that were really read.
    struct Script {
        chunks: Vec<Result<LlmChunk, LlmError>>,
        request: std::sync::Mutex<Option<LlmRequest>>,
        read: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Script {
        fn new(chunks: Vec<Result<LlmChunk, LlmError>>) -> Self {
            Self {
                chunks,
                request: std::sync::Mutex::new(None),
                read: Default::default(),
            }
        }

        fn texts(chunks: &[&str]) -> Self {
            Self::new(
                chunks
                    .iter()
                    .map(|t| Ok(LlmChunk::Text(t.to_string())))
                    .collect(),
            )
        }

        fn read(&self) -> usize {
            self.read.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LlmProvider for Script {
        fn name(&self) -> &str {
            "script"
        }
        async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError> {
            use futures::StreamExt;
            *self.request.lock().unwrap() = Some(req);
            let read = self.read.clone();
            Ok(futures::stream::iter(self.chunks.clone())
                .inspect(move |_| {
                    read.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })
                .boxed())
        }
    }

    fn answer(text: &str, streamed: usize) -> Result<TextAnswer, TextFailure> {
        Ok(TextAnswer {
            text: text.into(),
            streamed,
        })
    }

    #[tokio::test]
    async fn capped_request_carries_no_tools() {
        let p = Script::texts(&["Ответ"]);
        let got = complete_capped(&p, "система", "вопрос", 100).await;
        assert_eq!(got, answer("Ответ", 5));
        let req = p.request.lock().unwrap().clone().unwrap();
        assert!(req.tools.is_empty());
        assert_eq!(
            req.messages,
            vec![ChatMessage::system("система"), ChatMessage::user("вопрос")]
        );
    }

    #[tokio::test]
    async fn capped_drops_tool_calls_and_reasoning() {
        let chunks = ["<think>открою файл</th", "ink> Факт один. "];
        let p = Script::new(vec![
            Ok(LlmChunk::Text(chunks[0].into())),
            Ok(LlmChunk::ToolCall(ToolCall {
                id: "c1".into(),
                name: "delete_file".into(),
                arguments: serde_json::json!({"path": "C:\\x"}),
            })),
            Ok(LlmChunk::Text(chunks[1].into())),
        ]);
        let got = complete_capped(&p, "s", "u", 100).await;
        // The reasoning is cut out of the answer and stays in the count.
        let streamed = chunks.iter().map(|c| c.chars().count()).sum();
        assert_eq!(got, answer("Факт один.", streamed));
        assert_eq!(p.read(), 3);
    }

    #[tokio::test]
    async fn capped_stops_reading_at_the_limit() {
        let p = Script::texts(&["абвгд", "еёжзи", "клмно", "прсту"]);
        let got = complete_capped(&p, "s", "u", 7).await;
        assert_eq!(got, answer("абвгдеё", 10));
        assert_eq!(p.read(), 2, "the rest of the stream is not read");
        // An answer of exactly the limit is read to its end and kept whole.
        let p = Script::texts(&["абвгд", "её"]);
        assert_eq!(complete_capped(&p, "s", "u", 7).await, answer("абвгдеё", 7));
    }

    #[test]
    fn reasoning_gets_room_beside_the_answer() {
        assert_eq!(stream_cap(800), 3800);
        assert_eq!(stream_cap(2000), 5000);
        assert_eq!(stream_cap(9000), 18_000);
        assert_eq!(stream_cap(usize::MAX), usize::MAX);
    }

    #[tokio::test]
    async fn capped_stops_a_model_that_only_reasons() {
        // Ten chunks of 1000 characters inside a block that never closes.
        let block: &'static str = Box::leak("я".repeat(1000).into_boxed_str());
        let mut chunks = vec!["<think>"];
        chunks.extend(std::iter::repeat_n(block, 10));
        let p = Script::texts(&chunks);
        let got = complete_capped(&p, "s", "u", 100).await;
        // 100 characters of answer leave 3000 for reasoning: the fourth block
        // goes over, the six after it are never read.
        assert_eq!(
            got,
            Err(TextFailure {
                reason: ONLY_REASONING.into(),
                streamed: 7 + 4000,
            })
        );
        assert_eq!(p.read(), 5);

        // Reasoning that fits is counted, and the answer after it is kept.
        let p = Script::texts(&["<think>", block, block, "</think>", "Готово."]);
        assert_eq!(
            complete_capped(&p, "s", "u", 100).await,
            answer("Готово.", 7 + 2000 + 8 + 7)
        );
    }

    #[tokio::test]
    async fn capped_empty_answer_and_stream_error_are_errors() {
        let empty = Script::texts(&["  ", "<think>только мысли"]);
        assert_eq!(
            complete_capped(&empty, "s", "u", 100).await,
            Err(TextFailure {
                reason: "непонятный ответ провайдера: пустой ответ".into(),
                streamed: 21,
            })
        );
        let broken = Script::new(vec![
            Ok(LlmChunk::Text("начало".into())),
            Err(LlmError::Network("обрыв".into())),
        ]);
        let failure = complete_capped(&broken, "s", "u", 100).await.unwrap_err();
        assert_eq!(failure.to_string(), "нет связи с провайдером: обрыв");
        assert_eq!(failure.streamed, 6);
    }

    /// A provider that cannot even start the request.
    struct Refusing;

    #[async_trait]
    impl LlmProvider for Refusing {
        fn name(&self) -> &str {
            "refusing"
        }
        async fn stream(&self, _req: LlmRequest) -> Result<LlmStream, LlmError> {
            Err(LlmError::NotConfigured("не указана модель".into()))
        }
    }

    #[tokio::test]
    async fn capped_request_that_never_starts_costs_nothing() {
        assert_eq!(
            complete_capped(&Refusing, "s", "u", 100).await,
            Err(TextFailure::new("провайдер не настроен: не указана модель"))
        );
    }
}
