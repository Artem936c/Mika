//! Metering of paid calls (decision 0028) against a fake HTTP server on 127.0.0.1.
//!
//! The tracker is one per process and the tests run side by side: every test
//! uses a model name of its own and looks only at its own line of the report.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::StreamExt;
use mika_core::config::{Config, SPEECH_SECRET_KEY};
use mika_core::costs::{CostKind, Costs, UsageLine};
use mika_core::llm::{
    ChatMessage, ImageGen, LlmChunk, LlmProvider, LlmRequest, SpeechToText, TextToSpeech,
};
use mika_core::memory::Embedder;
use mika_core::secrets::{MemorySecretStore, Secret, SecretStore};
use mika_core::speech_cache::SpeechCache;
use mika_core::storage::Db;
use mika_providers::live::Live;
use mika_providers::openai::OpenAiProvider;
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// One answer of the fake server.
struct Reply {
    status: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    /// With a length the client knows where the body ends; a stream has none
    /// and ends when the connection closes.
    with_length: bool,
    /// How long the connection stays open after the body.
    hold: Duration,
}

impl Reply {
    fn json(status: &'static str, body: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type", "application/json".into())],
            body: body.as_bytes().to_vec(),
            with_length: true,
            hold: Duration::ZERO,
        }
    }

    fn sse(events: &[&str]) -> Self {
        let body: String = events.iter().map(|e| format!("{e}\n\n")).collect();
        Self {
            status: "200 OK",
            headers: vec![("content-type", "text/event-stream".into())],
            body: body.into_bytes(),
            with_length: false,
            hold: Duration::ZERO,
        }
    }

    fn sound(bytes: &[u8]) -> Self {
        Self {
            status: "200 OK",
            headers: vec![("content-type", "audio/wav".into())],
            body: bytes.to_vec(),
            with_length: true,
            hold: Duration::ZERO,
        }
    }

    fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_string()));
        self
    }

    fn hold(mut self, hold: Duration) -> Self {
        self.hold = hold;
        self
    }
}

/// Serves the requests one by one with these answers. Returns the port and
/// the raw text of each request.
async fn serve(replies: Vec<Reply>) -> (u16, mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        for reply in replies {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    // The head is ASCII, so its byte length equals its length in `text`.
                    if buf.len() >= end + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
            let mut head = format!("HTTP/1.1 {}\r\nconnection: close\r\n", reply.status);
            for (name, value) in &reply.headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            if reply.with_length {
                head.push_str(&format!("content-length: {}\r\n", reply.body.len()));
            }
            head.push_str("\r\n");
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&reply.body).await.unwrap();
            sock.flush().await.ok();
            tokio::time::sleep(reply.hold).await;
            sock.shutdown().await.ok();
        }
    });
    (port, rx)
}

/// The address of the fake server as a server on this computer: its calls are free.
fn local(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1")
}

/// The same server under a name the tracker does not take for a local one:
/// the short spelling of 127.0.0.1. Its calls are paid, at an unknown price.
fn paid(port: u16) -> String {
    format!("http://127.1:{port}/v1")
}

fn tracker() -> Costs {
    static COSTS: OnceLock<Costs> = OnceLock::new();
    COSTS
        .get_or_init(|| {
            let costs = Costs::new(Arc::new(Db::open_in_memory().unwrap()));
            mika_core::costs::install(costs.clone());
            costs
        })
        .clone()
}

/// The report line of one model today.
fn line(model: &str) -> Option<UsageLine> {
    let today = mika_core::costs::today();
    tracker()
        .report(today, today)
        .unwrap()
        .models
        .into_iter()
        .find(|l| l.model == model)
}

/// Waits until the model has `requests` requests counted: a stream is counted
/// when its reading task ends, a moment after the last chunk.
async fn counted(model: &str, requests: u64, within: Duration) -> UsageLine {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match line(model) {
            Some(l) if l.requests >= requests => return l,
            _ if tokio::time::Instant::now() > deadline => {
                panic!("{model}: {requests} requests were not counted in time")
            }
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

const SOON: Duration = Duration::from_secs(3);

fn request() -> LlmRequest {
    LlmRequest {
        messages: vec![ChatMessage::user("привет")],
        tools: vec![],
    }
}

async fn text_of(p: &dyn LlmProvider, req: LlmRequest) -> String {
    let mut stream = p.stream(req).await.unwrap();
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        if let LlmChunk::Text(t) = chunk.unwrap() {
            text.push_str(&t);
        }
    }
    text
}

/// The JSON body of a raw HTTP request.
fn json_body(raw: &str) -> serde_json::Value {
    serde_json::from_str(&raw[raw.find("\r\n\r\n").unwrap() + 4..]).unwrap()
}

fn live(edit: impl FnOnce(&mut Config)) -> Live {
    let mut c = Config::default();
    edit(&mut c);
    let secrets = MemorySecretStore::default();
    secrets
        .set(SPEECH_SECRET_KEY, &Secret::new("sk-speech-secret"))
        .unwrap();
    secrets
        .set("llm.openai.api_key", &Secret::new("sk-llm-secret"))
        .unwrap();
    Live::new(Arc::new(Mutex::new(c)), Arc::new(secrets))
}

const TEXT_CHUNKS: [&str; 2] = [
    r#"data: {"choices":[{"index":0,"delta":{"content":"При"}}],"usage":null}"#,
    r#"data: {"choices":[{"index":0,"delta":{"content":"вет"},"finish_reason":"stop"}],"usage":null}"#,
];

#[tokio::test]
async fn a_chat_stream_with_usage_and_cost_is_counted_once() {
    tracker();
    let model = "metering/chat-cost";
    let (port, mut requests) = serve(vec![Reply::sse(&[
        TEXT_CHUNKS[0],
        TEXT_CHUNKS[1],
        r#"data: {"choices":[],"usage":{"prompt_tokens":1200,"completion_tokens":34,"prompt_tokens_details":{"cached_tokens":1000},"cost":0.0123}}"#,
        "data: [DONE]",
    ])])
    .await;
    let p = OpenAiProvider::new(paid(port), model.into(), Some(Secret::new("sk-1")));
    assert_eq!(text_of(&p, request()).await, "Привет");
    // The request asks the provider to report usage.
    let sent = json_body(&requests.recv().await.unwrap());
    assert_eq!(sent["stream_options"]["include_usage"], true);
    assert_eq!(sent["stream"], true);

    let l = counted(model, 1, SOON).await;
    assert_eq!(l.kind, CostKind::Chat);
    assert_eq!((l.requests, l.blind, l.unpriced), (1, 0, 0));
    assert_eq!(
        (l.tokens_in, l.tokens_cached, l.tokens_out),
        (200, 1000, 34)
    );
    assert_eq!(l.cost.len(), 1);
    assert_eq!(l.cost[0].cost, 0.0123);
    assert_eq!(l.provider, format!("127.1:{port}"));
    // Once: nothing more comes later.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(line(model).unwrap().requests, 1);
}

#[tokio::test]
async fn usage_without_cost_gives_tokens_and_a_picture_makes_it_vision() {
    tracker();
    let model = "metering/vision-usage";
    let (port, mut requests) = serve(vec![Reply::sse(&[
        TEXT_CHUNKS[0],
        TEXT_CHUNKS[1],
        r#"data: {"choices":[],"usage":{"prompt_tokens":900,"completion_tokens":20,"total_tokens":920}}"#,
        "data: [DONE]",
    ])])
    .await;
    let p = OpenAiProvider::new(paid(port), model.into(), None);
    let req = LlmRequest {
        messages: vec![ChatMessage::user_with_image("что тут?", "QUJD")],
        tools: vec![],
    };
    assert_eq!(text_of(&p, req).await, "Привет");
    assert!(requests.recv().await.is_some());
    let l = counted(model, 1, SOON).await;
    assert_eq!(l.kind, CostKind::Vision);
    assert_eq!((l.tokens_in, l.tokens_cached, l.tokens_out), (900, 0, 20));
    // A paid provider and no price anywhere: units only.
    assert_eq!((l.requests, l.blind, l.unpriced), (1, 0, 1));
    assert!(l.cost.is_empty() && !l.free);

    // The same from a server on this computer costs nothing.
    let model = "metering/local-usage";
    let (port, _requests) = serve(vec![Reply::sse(&[
        TEXT_CHUNKS[1],
        r#"data: {"choices":[],"usage":{"prompt_tokens":40,"completion_tokens":8}}"#,
        "data: [DONE]",
    ])])
    .await;
    let p = OpenAiProvider::new(local(port), model.into(), None).named("ollama");
    assert_eq!(text_of(&p, request()).await, "вет");
    let l = counted(model, 1, SOON).await;
    assert!(l.free && l.cost.is_empty());
    assert_eq!(l.units, "48 ток.");

    // Ollama at an address that is not this computer or the home network may
    // be a paid service: units only, not «free».
    let model = "metering/hosted-ollama";
    let (port, _requests) = serve(vec![Reply::sse(&[
        TEXT_CHUNKS[1],
        r#"data: {"choices":[],"usage":{"prompt_tokens":40,"completion_tokens":8}}"#,
        "data: [DONE]",
    ])])
    .await;
    let p = OpenAiProvider::new(paid(port), model.into(), None).named("ollama");
    assert_eq!(text_of(&p, request()).await, "вет");
    let l = counted(model, 1, SOON).await;
    assert_eq!((l.requests, l.unpriced, l.free), (1, 1, false));
}

#[tokio::test]
async fn a_stream_dropped_after_the_first_chunk_is_counted_without_tokens() {
    tracker();
    let model = "metering/dropped";
    // The first chunk comes, then the provider keeps silent and hangs up.
    let (port, _requests) = serve(vec![
        Reply::sse(&[TEXT_CHUNKS[0]]).hold(Duration::from_millis(300))
    ])
    .await;
    let p = OpenAiProvider::new(paid(port), model.into(), None);
    let mut stream = p.stream(request()).await.unwrap();
    assert_eq!(stream.next().await, Some(Ok(LlmChunk::Text("При".into()))));
    // «Стоп»: the reader is dropped in the middle of the answer.
    drop(stream);
    let l = counted(model, 1, SOON).await;
    assert_eq!((l.requests, l.blind, l.unpriced), (1, 1, 0));
    assert_eq!(l.units, "токены не сообщены");
    assert!(l.cost.is_empty());
}

#[tokio::test]
async fn a_refused_request_is_not_counted() {
    tracker();
    let model = "metering/refused";
    let (port, _requests) = serve(vec![Reply::json(
        "401 Unauthorized",
        r#"{"error":{"message":"bad key"}}"#,
    )])
    .await;
    let p = OpenAiProvider::new(paid(port), model.into(), None);
    assert!(p.stream(request()).await.is_err());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(line(model).is_none());
}

#[tokio::test]
async fn a_refusal_of_stream_options_is_retried_without_it_and_remembered() {
    tracker();
    let model = "metering/strict";
    let plain = || Reply::sse(&[TEXT_CHUNKS[0], TEXT_CHUNKS[1], "data: [DONE]"]);
    let (port, mut requests) = serve(vec![
        Reply::json(
            "400 Bad Request",
            r#"{"error":{"message":"Unrecognized request argument supplied: stream_options","code":400}}"#,
        ),
        plain(),
        plain(),
    ])
    .await;
    let p = OpenAiProvider::new(paid(port), model.into(), None);
    assert_eq!(text_of(&p, request()).await, "Привет");
    let first = json_body(&requests.recv().await.unwrap());
    assert!(first.get("stream_options").is_some());
    let again = json_body(&requests.recv().await.unwrap());
    assert!(again.get("stream_options").is_none(), "{again}");
    assert_eq!(again["messages"], first["messages"]);
    // The next request goes without the field at once: no refused one first.
    assert_eq!(text_of(&p, request()).await, "Привет");
    let next = json_body(&requests.recv().await.unwrap());
    assert!(next.get("stream_options").is_none(), "{next}");
    // Both answers are counted, without token data; the refusal is not.
    let l = counted(model, 2, SOON).await;
    assert_eq!((l.requests, l.blind), (2, 2));
}

#[tokio::test]
async fn a_refusal_that_does_not_name_the_field_is_retried_without_it_too() {
    tracker();
    let model = "metering/strict-silent";
    let plain = || Reply::sse(&[TEXT_CHUNKS[0], TEXT_CHUNKS[1], "data: [DONE]"]);
    let (port, mut requests) = serve(vec![
        Reply::json(
            "400 Bad Request",
            r#"{"error":{"message":"Unsupported parameter"}}"#,
        ),
        plain(),
        plain(),
    ])
    .await;
    let p = OpenAiProvider::new(paid(port), model.into(), None);
    // The chat request does not fail because usage was asked for.
    assert_eq!(text_of(&p, request()).await, "Привет");
    let first = json_body(&requests.recv().await.unwrap());
    assert!(first.get("stream_options").is_some());
    let again = json_body(&requests.recv().await.unwrap());
    assert!(again.get("stream_options").is_none(), "{again}");
    // Remembered: the next request goes without the field at once.
    assert_eq!(text_of(&p, request()).await, "Привет");
    let next = json_body(&requests.recv().await.unwrap());
    assert!(next.get("stream_options").is_none(), "{next}");
    let l = counted(model, 2, SOON).await;
    assert_eq!((l.requests, l.blind), (2, 2));
}

#[tokio::test]
async fn a_request_refused_for_another_reason_returns_its_first_error() {
    tracker();
    let model = "metering/other-400";
    let usage = r#"data: {"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#;
    let (port, mut requests) = serve(vec![
        Reply::json(
            "400 Bad Request",
            r#"{"error":{"message":"model not found"}}"#,
        ),
        // The same request without the field is refused as well.
        Reply::json(
            "400 Bad Request",
            r#"{"error":{"message":"second answer"}}"#,
        ),
        Reply::sse(&[TEXT_CHUNKS[0], TEXT_CHUNKS[1], usage, "data: [DONE]"]),
        Reply::json(
            "400 Bad Request",
            r#"{"error":{"message":"context is too long"}}"#,
        ),
    ])
    .await;
    let p = OpenAiProvider::new(paid(port), model.into(), None);
    let failed = |r: Result<_, mika_core::llm::LlmError>| match r {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected an error"),
    };
    let err = failed(p.stream(request()).await.map(|_| ()));
    assert!(err.contains("model not found"), "{err}");
    assert!(!err.contains("second answer"), "{err}");
    let first = json_body(&requests.recv().await.unwrap());
    assert!(first.get("stream_options").is_some());
    let again = json_body(&requests.recv().await.unwrap());
    assert!(again.get("stream_options").is_none(), "{again}");
    assert!(
        line(model).is_none(),
        "a refused request is not a paid call"
    );

    // Nothing was learnt of the field: the next request asks for usage
    // again, and the provider takes it.
    assert_eq!(text_of(&p, request()).await, "Привет");
    let third = json_body(&requests.recv().await.unwrap());
    assert!(third.get("stream_options").is_some());
    let l = counted(model, 1, SOON).await;
    assert_eq!((l.requests, l.blind, l.tokens_in), (1, 0, 3));

    // From now on a refusal is about the request: it is returned as it is,
    // with no second try.
    let err = failed(p.stream(request()).await.map(|_| ()));
    assert!(err.contains("context is too long"), "{err}");
    assert!(requests.recv().await.is_some());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(requests.try_recv().is_err());
    assert_eq!(line(model).unwrap().requests, 1);
}

#[tokio::test]
async fn embeddings_and_pictures_are_counted_with_their_usage() {
    tracker();
    let (port, mut requests) = serve(vec![
        Reply::json(
            "200 OK",
            r#"{"data":[{"index":0,"embedding":[0.5,1.0]}],"usage":{"prompt_tokens":7,"total_tokens":7,"cost":"0.00002"}}"#,
        ),
        Reply::json(
            "200 OK",
            r#"{"data":[{"b64_json":"QUJD"}],"usage":{"input_tokens":12,"output_tokens":4000}}"#,
        ),
        Reply::json("500 Internal Server Error", r#"{"error":"boom"}"#),
    ])
    .await;
    let l = live(|c| {
        c.embedding.base_url = paid(port);
        c.embedding.model = "metering/embed".into();
        c.image.base_url = paid(port);
        c.image.model = "metering/image".into();
    });
    assert_eq!(l.embed(&["привет".into()]).await, Ok(vec![vec![0.5, 1.0]]));
    assert!(requests
        .recv()
        .await
        .unwrap()
        .starts_with("POST /v1/embeddings"));
    let e = line("metering/embed").unwrap();
    assert_eq!(e.kind, CostKind::Embedding);
    assert_eq!(
        (e.requests, e.blind, e.tokens_in, e.tokens_out),
        (1, 0, 7, 0)
    );
    assert_eq!(e.cost[0].cost, 0.00002);

    assert_eq!(l.generate("кот").await, Ok(b"ABC".to_vec()));
    let i = line("metering/image").unwrap();
    assert_eq!(i.kind, CostKind::Image);
    assert_eq!(
        (i.requests, i.images, i.tokens_in, i.tokens_out),
        (1, 1, 12, 4000)
    );
    assert_eq!(i.unpriced, 1);
    // An answer with an error status is not a paid call.
    assert!(l.generate("кот").await.is_err());
    assert_eq!(line("metering/image").unwrap().requests, 1);
}

#[tokio::test]
async fn a_long_transcription_comes_back_whole_and_its_seconds_are_counted() {
    tracker();
    let model = "metering/asr-long";
    // Longer than the 300 characters an error text is cut to, with the key in it.
    let said = format!("{} ключ sk-speech-secret конец", "слово ".repeat(120));
    let body = serde_json::json!({ "text": said, "usage": { "seconds": 31.5, "cost": 0.011 } });
    let (port, _requests) = serve(vec![Reply::json("200 OK", &body.to_string())]).await;
    let l = live(|c| {
        c.voice.speech_base_url = paid(port);
        c.voice.stt_model = model.into();
    });
    let wav = mika_providers::speech::wav_from_pcm(&[0u8; 64_000], 16_000);
    let text = l.transcribe(&wav).await.unwrap();
    assert!(text.chars().count() > 700, "{}", text.chars().count());
    assert!(text.ends_with("ключ *** конец"), "the key is hidden");
    assert!(!text.contains("sk-speech-secret"));
    let got = line(model).unwrap();
    assert_eq!(got.kind, CostKind::Transcription);
    // The provider's own count, not the two seconds of the file.
    assert_eq!((got.requests, got.seconds), (1, 31.5));
    assert_eq!(got.cost[0].cost, 0.011);
}

#[tokio::test]
async fn a_transcription_without_usage_is_counted_by_the_length_of_the_wav() {
    tracker();
    let model = "metering/asr-wav";
    let (port, _requests) = serve(vec![
        Reply::json("200 OK", r#"{"text":" Какая погода? "}"#),
        Reply::json(
            "400 Bad Request",
            r#"{"error":{"message":"response_format verbose_json is not supported"}}"#,
        ),
        Reply::json("401 Unauthorized", r#"{"error":{"message":"bad key"}}"#),
    ])
    .await;
    let l = live(|c| {
        c.voice.speech_base_url = paid(port);
        c.voice.stt_model = model.into();
    });
    let wav = mika_providers::speech::wav_from_pcm(&[0u8; 80_000], 16_000);
    assert_eq!(l.transcribe(&wav).await.unwrap(), "Какая погода?");
    let got = line(model).unwrap();
    assert_eq!((got.requests, got.seconds), (1, 2.5));
    assert_eq!((got.unpriced, got.blind, got.free), (1, 0, false));
    // A refusal of times and an error are not paid calls.
    assert_eq!(l.transcribe_segments(&wav).await, Ok(None));
    assert!(l.transcribe(&wav).await.is_err());
    assert_eq!(line(model).unwrap().requests, 1);
}

/// One Ogg page that ends at `position` and carries `data`.
fn ogg_page(position: u64, data: &[u8]) -> Vec<u8> {
    let mut page = b"OggS\0\0".to_vec();
    page.extend_from_slice(&position.to_le_bytes());
    page.extend_from_slice(&[0; 12]);
    let sizes: Vec<u8> = data.chunks(255).map(|c| c.len() as u8).collect();
    page.push(sizes.len() as u8);
    page.extend_from_slice(&sizes);
    page.extend_from_slice(data);
    page
}

/// An Ogg/Opus stream of `seconds`, as Telegram sends a voice message.
fn opus_voice(seconds: u64) -> Vec<u8> {
    let mut head = b"OpusHead\x01\x01".to_vec();
    head.extend_from_slice(&0u16.to_le_bytes());
    head.extend_from_slice(&[0x80, 0xBB, 0, 0, 0, 0, 0]);
    let mut voice = ogg_page(0, &head);
    voice.extend(ogg_page(0, b"OpusTags\0\0\0\0\0\0\0\0"));
    voice.extend(ogg_page(48_000 * seconds, &[7; 300]));
    voice
}

/// A Telegram voice message goes to recognition as it came, OGG with Opus,
/// through the same request as a phrase from the microphone: it is counted
/// there with no code of its own (decisions 0028 and 0029). Without the
/// provider's own count it is counted by its length, read from its pages.
#[tokio::test]
async fn a_voice_message_is_counted_by_the_seconds_of_the_provider_or_by_its_own_length() {
    use mika_core::sound::SoundKind;
    use mika_providers::speech::SttFailure;
    tracker();
    let model = "metering/asr-voice";
    let with_usage = serde_json::json!({
        "text": " Какая погода? ",
        "usage": { "seconds": 7.5, "cost": 0.004 }
    });
    let (port, mut requests) = serve(vec![
        Reply::json("200 OK", &with_usage.to_string()),
        Reply::json("200 OK", r#"{"text":"Спасибо"}"#),
        Reply::json("200 OK", r#"{"text":"nine seconds"}"#),
        Reply::json(
            "400 Bad Request",
            r#"{"error":{"message":"unsupported audio format"}}"#,
        ),
    ])
    .await;
    let l = live(|c| {
        c.voice.speech_base_url = paid(port);
        c.voice.stt_model = model.into();
    });
    // Called Ogg, but not readable Opus: no page of it says how long it is.
    let voice = b"OggS\0\x02a voice message that is not really Opus".to_vec();

    assert_eq!(
        l.transcribe_sound(&voice, SoundKind::OggOpus)
            .await
            .unwrap(),
        "Какая погода?"
    );
    let raw = requests.recv().await.unwrap();
    assert!(raw.contains("filename=\"voice.ogg\""), "sent as it came");
    let got = line(model).unwrap();
    assert_eq!(got.kind, CostKind::Transcription);
    assert_eq!((got.requests, got.seconds), (1, 7.5));
    assert_eq!(got.cost[0].cost, 0.004);
    assert_eq!((got.unpriced, got.blind, got.free), (0, 0, false));

    // No usage in the answer: a request of unknown size. It is neither a free
    // one nor a request with units and no price, and it adds no money.
    assert_eq!(
        l.transcribe_sound(&voice, SoundKind::OggOpus)
            .await
            .unwrap(),
        "Спасибо"
    );
    let got = line(model).unwrap();
    assert_eq!((got.requests, got.seconds), (2, 7.5));
    assert_eq!((got.unpriced, got.blind, got.free), (0, 1, false));
    assert_eq!(got.cost.len(), 1);
    assert_eq!(got.cost[0].cost, 0.004);

    // A real voice message and no usage in the answer: its nine seconds are
    // read from its pages. So it has units, gets a price where one is known,
    // and counts toward the budget; it is not one more request of unknown size.
    let real = opus_voice(9);
    assert_eq!(
        l.transcribe_sound(&real, SoundKind::OggOpus).await.unwrap(),
        "nine seconds"
    );
    let got = line(model).unwrap();
    assert_eq!((got.requests, got.seconds), (3, 16.5));
    assert_eq!((got.unpriced, got.blind, got.free), (1, 1, false));

    // A refused sound is not a paid call.
    assert!(matches!(
        l.transcribe_sound(&voice, SoundKind::OggOpus).await,
        Err(SttFailure::Refused(_))
    ));
    assert_eq!(line(model).unwrap().requests, 3);
}

#[tokio::test]
async fn speech_is_counted_by_characters_and_a_cached_phrase_is_not_counted() {
    tracker();
    let model = "metering/tts";
    let wav = b"RIFF\x24\0\0\0WAVEfmt sound".to_vec();
    // One answer only: a second request would fail.
    let (port, mut requests) = serve(vec![Reply::sound(&wav)]).await;
    let dir = tempfile::tempdir().unwrap();
    let l = live(|c| {
        c.voice.speech_base_url = local(port);
        c.voice.tts_model = model.into();
    })
    .with_speech_cache(Arc::new(SpeechCache::new(dir.path())));
    assert_eq!(l.synthesize("Привет, мир").await.unwrap(), wav);
    assert!(requests
        .recv()
        .await
        .unwrap()
        .starts_with("POST /v1/audio/speech"));
    let got = line(model).unwrap();
    assert_eq!(got.kind, CostKind::Speech);
    assert_eq!((got.requests, got.chars), (1, 11));
    assert_eq!(got.units, "11 зн.");
    // The same phrase again comes from the disk: no request, nothing counted.
    assert_eq!(l.synthesize("Привет, мир").await.unwrap(), wav);
    assert!(requests.try_recv().is_err());
    assert_eq!(line(model).unwrap().requests, 1);
}

#[tokio::test]
async fn the_cost_of_a_phrase_without_a_price_is_asked_by_its_generation_id() {
    tracker();
    let model = "metering/tts-generation";
    let wav = b"RIFF\x24\0\0\0WAVEfmt sound".to_vec();
    let (port, mut requests) = serve(vec![
        Reply::sound(&wav).header("X-Generation-Id", "gen-1731571200-AbC"),
        Reply::json(
            "200 OK",
            r#"{"data":{"id":"gen-1731571200-AbC","total_cost":0.0042}}"#,
        ),
    ])
    .await;
    let l = live(|c| {
        c.voice.speech_base_url = paid(port);
        c.voice.tts_model = model.into();
        c.voice.tts_cache = false;
    });
    // The sound comes back at once; the cost is asked for in the background.
    let asked = std::time::Instant::now();
    assert_eq!(l.synthesize("Привет").await.unwrap(), wav);
    assert!(asked.elapsed() < Duration::from_secs(2));
    assert!(line(model).is_none(), "counted when the cost is known");
    assert!(requests
        .recv()
        .await
        .unwrap()
        .starts_with("POST /v1/audio/speech"));

    let got = counted(model, 1, Duration::from_secs(10)).await;
    let cost_request = requests.recv().await.unwrap();
    assert!(
        cost_request.starts_with("GET /v1/generation?id=gen-1731571200-AbC "),
        "{cost_request}"
    );
    // The same key as the speech request, to the same address.
    assert!(cost_request
        .to_ascii_lowercase()
        .contains("authorization: bearer sk-speech-secret"));
    assert_eq!((got.requests, got.chars, got.unpriced), (1, 6, 0));
    assert_eq!(got.cost.len(), 1);
    assert_eq!(got.cost[0].cost, 0.0042);
}

#[tokio::test]
async fn an_odd_generation_id_is_not_put_into_an_address() {
    tracker();
    let model = "metering/tts-odd-id";
    let wav = b"RIFF\x24\0\0\0WAVEfmt sound".to_vec();
    let (port, mut requests) = serve(vec![
        Reply::sound(&wav).header("X-Generation-Id", "a/../b?x=1")
    ])
    .await;
    let l = live(|c| {
        c.voice.speech_base_url = paid(port);
        c.voice.tts_model = model.into();
        c.voice.tts_cache = false;
    });
    assert_eq!(l.synthesize("Привет").await.unwrap(), wav);
    // Counted at once, without money, and nobody is asked.
    let got = line(model).unwrap();
    assert_eq!((got.requests, got.chars, got.unpriced), (1, 6, 1));
    assert!(requests.recv().await.is_some());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(requests.try_recv().is_err());
}
