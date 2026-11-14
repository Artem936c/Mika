//! Records of the journal about requests to providers: one record when a
//! request is over, never one per piece of a stream.
//!
//! A record names the provider, the host, the model, sizes and times. The
//! text of a request or of an answer never gets here.

use std::time::Instant;

use mika_core::costs::{provider_host, CostKind, Tokens};
use mika_core::journal;
use mika_core::llm::{LlmError, LlmRequest};

/// How a streamed request ended.
#[derive(Debug, Clone, PartialEq)]
enum End {
    /// Nobody said: the reader went away before the end. The user stopped
    /// the reply, or the caller needed only the beginning of the answer.
    Open,
    /// The provider closed the answer itself.
    Finished,
    Failed {
        kind: &'static str,
        text: String,
    },
}

/// One streamed request to a model. Its record is written exactly once, when
/// this is dropped: the stream was read to the end, cut by an error, or left.
#[derive(Debug)]
pub(crate) struct RequestLog {
    kind: CostKind,
    provider: String,
    host: String,
    model: String,
    started: Instant,
    messages: usize,
    tools: usize,
    http: Option<u16>,
    retries: u32,
    tokens: Option<Tokens>,
    stop: Option<String>,
    end: End,
    /// The turn the request was made for; `None` for one made by something
    /// else: a background job, a summary, the voice, a question about a
    /// picture.
    turn: Option<u64>,
}

/// Longest stop reason kept: providers send a short word.
const MAX_STOP: usize = 32;

impl RequestLog {
    pub fn new(
        provider: &str,
        base_url: &str,
        model: &str,
        kind: CostKind,
        req: &LlmRequest,
    ) -> Self {
        Self {
            kind,
            provider: provider.to_string(),
            host: provider_host(base_url),
            model: model.trim().to_string(),
            started: Instant::now(),
            messages: req.messages.len(),
            tools: req.tools.len(),
            http: None,
            retries: 0,
            tokens: None,
            stop: None,
            end: End::Open,
            // Taken here: the stream may be read to its end by another task.
            turn: journal::turn(),
        }
    }

    /// The request was sent once more.
    pub fn retried(&mut self) {
        self.retries += 1;
    }

    /// The provider took the request and started to answer.
    pub fn answered(&mut self, status: u16) {
        self.http = Some(status);
    }

    /// What the answer says about its tokens so far; the last one stays.
    pub fn tokens(&mut self, tokens: Option<Tokens>) {
        if tokens.is_some() {
            self.tokens = tokens;
        }
    }

    /// Why the model stopped, in the provider's word: `stop`, `tool_calls`,
    /// `length`, `end_turn`.
    pub fn stop(&mut self, reason: &str) {
        let reason: String = reason
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            .take(MAX_STOP)
            .collect();
        if !reason.is_empty() {
            self.stop = Some(reason);
        }
    }

    /// The answer came to its end.
    pub fn finished(&mut self) {
        if self.end == End::Open {
            self.end = End::Finished;
        }
    }

    /// The request or its answer failed. The first failure stays.
    pub fn failed(&mut self, error: &LlmError) {
        if let LlmError::Http { status, .. } = error {
            self.http = Some(*status);
        }
        if self.end == End::Open {
            self.end = End::Failed {
                kind: error.kind(),
                text: error.journal_text(),
            };
        }
    }

    /// The record: its level and its text.
    fn line(&self, ms: u128) -> (log::Level, String) {
        let turn = self
            .turn
            .map_or_else(String::new, |turn| format!("turn={turn} "));
        let mut line = format!(
            "{turn}kind={} provider={} host={} model={} ms={ms} messages={} tools={}",
            self.kind.as_str(),
            self.provider,
            self.host,
            self.model,
            self.messages,
            self.tools
        );
        if let Some(status) = self.http {
            line.push_str(&format!(" http={status}"));
        }
        line.push_str(&format!(" retries={}", self.retries));
        if let Some(t) = self.tokens {
            line.push_str(&format!(
                " tokens_in={} tokens_cached={} tokens_out={}",
                t.input, t.cached, t.output
            ));
        }
        if let Some(stop) = &self.stop {
            line.push_str(&format!(" stop={stop}"));
        }
        match &self.end {
            End::Finished => (
                log::Level::Info,
                format!("request finished: {line} outcome=ok"),
            ),
            End::Open => (
                log::Level::Info,
                format!("request finished: {line} outcome=dropped"),
            ),
            End::Failed { kind, text } => (
                log::Level::Warn,
                format!("request failed: {line} outcome=error error_kind={kind} error={text}"),
            ),
        }
    }
}

impl Drop for RequestLog {
    fn drop(&mut self) {
        let (level, line) = self.line(self.started.elapsed().as_millis());
        log::log!(target: "llm", level, "{line}");
    }
}

/// The record of a request that is one question and one answer: embeddings,
/// a picture, speech. `what` names it, `sizes` is what went in and came out,
/// already as `name=value` pairs.
pub(crate) fn call_line(
    what: &str,
    base_url: &str,
    model: &str,
    ms: u128,
    sizes: &str,
    outcome: Result<(), &str>,
) -> (log::Level, String) {
    let mut line = format!(
        "{what}: host={} model={} ms={ms}",
        provider_host(base_url),
        model.trim()
    );
    if !sizes.is_empty() {
        line.push(' ');
        line.push_str(sizes);
    }
    match outcome {
        Ok(()) => (log::Level::Info, format!("{line} outcome=ok")),
        Err(why) => (
            log::Level::Warn,
            format!("{line} outcome=error error={}", journal::error_text(&why)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::llm::ChatMessage;

    fn request() -> LlmRequest {
        LlmRequest {
            messages: vec![ChatMessage::system("s"), ChatMessage::user("тайный вопрос")],
            tools: Vec::new(),
        }
    }

    fn log() -> RequestLog {
        RequestLog::new(
            "openai",
            "https://user:pass@Api.Example.test/v1?x=1",
            " gpt-test ",
            CostKind::Chat,
            &request(),
        )
    }

    #[test]
    fn a_finished_request_names_sizes_and_never_the_text() {
        let mut l = log();
        l.answered(200);
        l.tokens(Some(Tokens {
            input: 10,
            cached: 2,
            output: 30,
        }));
        l.tokens(None);
        l.stop("tool_calls");
        l.finished();
        let (level, line) = l.line(1234);
        assert_eq!(level, log::Level::Info);
        assert_eq!(
            line,
            "request finished: kind=chat provider=openai host=api.example.test model=gpt-test \
ms=1234 messages=2 tools=0 http=200 retries=0 tokens_in=10 tokens_cached=2 tokens_out=30 \
stop=tool_calls outcome=ok"
        );
        assert!(!line.contains("тайный"));
        assert!(!line.contains("pass"));
    }

    #[test]
    fn a_request_nobody_read_to_the_end_is_dropped_not_failed() {
        let mut l = log();
        l.answered(200);
        let (level, line) = l.line(5);
        assert_eq!(level, log::Level::Info);
        assert!(line.ends_with("outcome=dropped"), "{line}");
    }

    #[test]
    fn a_failed_request_names_the_status_and_the_first_error() {
        let mut l = log();
        l.retried();
        l.failed(&LlmError::Http {
            status: 401,
            body: "invalid api key".into(),
        });
        l.failed(&LlmError::Network("later".into()));
        l.finished();
        let (level, line) = l.line(7);
        assert_eq!(level, log::Level::Warn);
        assert!(line.starts_with("request failed: kind=chat"), "{line}");
        assert!(line.contains(" http=401 retries=1 "), "{line}");
        assert!(
            line.ends_with("outcome=error error_kind=http error=invalid api key"),
            "{line}"
        );
    }

    #[test]
    fn what_the_model_sent_stays_out_of_a_record_and_the_reason_does_not() {
        let mut l = log();
        l.failed(&LlmError::BadJson {
            why: "expected value at line 1 column 1".into(),
            data: "привет, это ответ".into(),
        });
        let (_, line) = l.line(1);
        // Texts are off in tests: what the parser said is written, of the
        // piece itself only the size.
        assert!(
            line.ends_with(
                "error_kind=bad_json error=expected value at line 1 column 1 data=<hidden 17 chars>"
            ),
            "{line}"
        );
        assert!(!line.contains("привет"), "{line}");
    }

    #[test]
    fn an_error_the_provider_sends_inside_its_answer_is_written() {
        // After the status 200: the reason must not be lost.
        let mut l = log();
        l.answered(200);
        l.failed(&LlmError::Provider(
            "{\"message\":\"Rate limit exceeded\",\"code\":429}".into(),
        ));
        let (level, line) = l.line(1);
        assert_eq!(level, log::Level::Warn);
        assert!(line.contains(" http=200 "), "{line}");
        assert!(
            line.ends_with(
                "outcome=error error_kind=provider_error error={\"message\":\"Rate limit exceeded\",\"code\":429}"
            ),
            "{line}"
        );
    }

    #[tokio::test]
    async fn a_request_of_a_turn_carries_its_number() {
        // Outside a turn there is no number: a background job, the voice.
        let (_, line) = log().line(1);
        assert!(line.starts_with("request finished: kind=chat "), "{line}");
        let inside = journal::in_turn(41, async { log().line(1).1 }).await;
        assert!(
            inside.starts_with("request finished: turn=41 kind=chat "),
            "{inside}"
        );
    }

    #[test]
    fn a_stop_reason_is_a_short_plain_word() {
        let mut l = log();
        l.stop("end turn\twith \"junk\" and a very long tail that never ends at all");
        assert_eq!(l.stop.as_deref(), Some("endturnwithjunkandaverylongtailt"));
        let mut l = log();
        l.stop("  ");
        assert_eq!(l.stop, None);
    }

    #[test]
    fn a_single_call_is_one_line_with_its_sizes() {
        let (level, line) = call_line(
            "embeddings",
            "https://api.example.test/v1",
            "embed-1",
            40,
            "inputs=3",
            Ok(()),
        );
        assert_eq!(level, log::Level::Info);
        assert_eq!(
            line,
            "embeddings: host=api.example.test model=embed-1 ms=40 inputs=3 outcome=ok"
        );
        let (level, line) = call_line("picture", "http://x.test", "m", 1, "", Err("ответ 500"));
        assert_eq!(level, log::Level::Warn);
        assert_eq!(
            line,
            "picture: host=x.test model=m ms=1 outcome=error error=ответ 500"
        );
    }
}
