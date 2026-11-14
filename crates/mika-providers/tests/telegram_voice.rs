//! Voice messages of Telegram (decision 0029) against fake HTTP servers on
//! 127.0.0.1: recognition of a sound file as it is, speech in a named format,
//! the file download and the sound upload of the Bot API, and the service
//! that joins them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::config::{Config, TtsEngine, SPEECH_SECRET_KEY};
use mika_core::secrets::{MemorySecretStore, Secret, SecretStore};
use mika_core::sound::{NoConverter, SoundConverter, SoundKind};
use mika_core::speech_cache::SpeechCache;
use mika_providers::live::Live;
use mika_providers::speech::SttFailure;
use mika_providers::telegram::voice::{
    Instructions, LiveVoice, NoteMaker, TelegramVoice, VoiceFile, SOUND_MAX_BYTES, VOICE_NEEDS_WAV,
};
use mika_providers::telegram::{HttpTelegram, TelegramApi};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// One answer of the fake server: status, content type, body. An empty
/// status closes the connection without an answer.
type Reply = (&'static str, &'static str, Vec<u8>);

/// Serves the requests one by one with these answers. Returns the address
/// and the raw bytes of each request.
async fn serve_many(replies: Vec<Reply>) -> (String, mpsc::UnboundedReceiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        for (status, content_type, body) in replies {
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
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
            let _ = tx.send(buf);
            if status.is_empty() {
                continue;
            }
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&body).await.unwrap();
            sock.shutdown().await.ok();
        }
    });
    (format!("http://{addr}"), rx)
}

/// The JSON body of a raw HTTP request.
fn json_body(req: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(req);
    serde_json::from_str(&text[text.find("\r\n\r\n").unwrap() + 4..]).unwrap()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

const SPEECH_KEY: &str = "sk-speech-secret";

/// The settings and a speech client that reads them at every call.
fn live(base: &str, edit: impl FnOnce(&mut Config)) -> (Live, Arc<Mutex<Config>>) {
    let mut c = Config::default();
    c.voice.speech_base_url = format!("{base}/v1");
    edit(&mut c);
    let config = Arc::new(Mutex::new(c));
    let secrets = MemorySecretStore::default();
    secrets
        .set(SPEECH_SECRET_KEY, &Secret::new(SPEECH_KEY))
        .unwrap();
    (Live::new(config.clone(), Arc::new(secrets)), config)
}

/// The start of an OGG file with Opus inside, as a Telegram voice message.
fn ogg_opus() -> Vec<u8> {
    let mut page = b"OggS\0\x02".to_vec();
    page.extend_from_slice(&[0; 20]);
    page.extend_from_slice(&[1, 19]);
    page.extend_from_slice(b"OpusHead\x01\x01\0\0\x80\xbb\0\0\0\0\0");
    page.extend_from_slice(b"voice-message-sound");
    page
}

fn wav() -> Vec<u8> {
    b"RIFF\x24\0\0\0WAVEfmt sound".to_vec()
}

fn mp3() -> Vec<u8> {
    let mut frame = vec![0xFF, 0xFB, 0x90, 0x00];
    frame.extend_from_slice(b"mp3-sound");
    frame
}

fn text_reply(text: &str) -> Reply {
    (
        "200 OK",
        "application/json",
        format!(r#"{{"text":"{text}"}}"#).into_bytes(),
    )
}

fn refusal(status: &'static str, message: &str) -> Reply {
    (
        status,
        "application/json",
        format!(r#"{{"error":{{"message":"{message}"}}}}"#).into_bytes(),
    )
}

// ---------------------------------------------------------------- recognition

#[tokio::test]
async fn a_sound_file_goes_to_recognition_under_its_own_name() {
    let (base, mut requests) = serve_many(vec![text_reply(" Какая погода? ")]).await;
    let (l, _) = live(&base, |c| c.voice.stt_model = "qwen/qwen3-asr-0.6b".into());
    let ogg = ogg_opus();
    let text = l.transcribe_sound(&ogg, SoundKind::of(&ogg)).await;
    assert_eq!(text, Ok("Какая погода?".into()));
    let req = requests.recv().await.unwrap();
    assert!(contains(&req, "POST /v1/audio/transcriptions"));
    assert!(contains(&req, "authorization: Bearer sk-speech-secret"));
    assert!(contains(&req, "name=\"model\"\r\n\r\nqwen/qwen3-asr-0.6b"));
    assert!(contains(&req, "name=\"language\"\r\n\r\nru"));
    assert!(contains(&req, "name=\"file\"; filename=\"voice.ogg\""));
    assert!(contains(&req, "audio/ogg"));
    assert!(!contains(&req, "speech.wav") && !contains(&req, "audio/wav"));
    assert!(contains(&req, "voice-message-sound"));

    // Each kind goes under its extension: the provider tells the format by it.
    let (base, mut requests) = serve_many(vec![text_reply("раз")]).await;
    let (l, _) = live(&base, |c| c.voice.stt_model = "m".into());
    assert_eq!(
        l.transcribe_sound(&mp3(), SoundKind::Mp3).await,
        Ok("раз".into())
    );
    let req = requests.recv().await.unwrap();
    assert!(contains(&req, "filename=\"voice.mp3\"") && contains(&req, "audio/mpeg"));
}

#[tokio::test]
async fn a_refusal_of_the_request_is_told_from_other_failures() {
    let ogg = ogg_opus();
    for (status, code, refused) in [
        ("400 Bad Request", 400, true),
        ("415 Unsupported Media Type", 415, true),
        ("422 Unprocessable Entity", 422, true),
        ("401 Unauthorized", 401, false),
        ("429 Too Many Requests", 429, false),
        ("500 Internal Server Error", 500, false),
    ] {
        let (base, _requests) = serve_many(vec![refusal(
            status,
            "format ogg is not supported, key sk-speech-secret",
        )])
        .await;
        let (l, _) = live(&base, |c| c.voice.stt_model = "m".into());
        let failure = l
            .transcribe_sound(&ogg, SoundKind::OggOpus)
            .await
            .unwrap_err();
        let why = format!("распознавание ответило {code}: format ogg is not supported");
        assert!(failure.to_string().starts_with(&why), "{failure}");
        assert!(!failure.to_string().contains(SPEECH_KEY), "{failure}");
        assert_eq!(
            matches!(failure, SttFailure::Refused(_)),
            refused,
            "{status}"
        );
    }
}

#[tokio::test]
async fn nothing_is_sent_for_an_unknown_sound_or_without_a_model() {
    // Port 1: a request would fail with another text.
    let (l, _) = live("http://127.0.0.1:1", |c| c.voice.stt_model = "m".into());
    assert_eq!(
        l.transcribe_sound(b"no sound here", SoundKind::Unknown)
            .await,
        Err(SttFailure::Other("неизвестный формат звука".into()))
    );
    let (l, _) = live("http://127.0.0.1:1", |_| {});
    match l.transcribe_sound(&ogg_opus(), SoundKind::OggOpus).await {
        Err(SttFailure::Other(why)) => assert!(why.contains("не задана"), "{why}"),
        other => panic!("{other:?}"),
    }
    // A broken answer is not a refusal of the format.
    let (base, _requests) = serve_many(vec![("200 OK", "text/html", b"<html>".to_vec())]).await;
    let (l, _) = live(&base, |c| c.voice.stt_model = "m".into());
    assert!(matches!(
        l.transcribe_sound(&ogg_opus(), SoundKind::OggOpus).await,
        Err(SttFailure::Other(_))
    ));
}

// ------------------------------------------------------------------ synthesis

#[tokio::test]
async fn speech_is_asked_for_in_the_named_format() {
    let (base, mut requests) = serve_many(vec![("200 OK", "audio/mpeg", mp3())]).await;
    let (l, _) = live(&base, |c| {
        c.voice.tts_model = "openai/gpt-4o-mini-tts".into();
        c.voice.tts_voice = "coral".into();
    });
    let sound = l
        .synthesize_format("Привет", "Сейчас радостно.", "mp3")
        .await
        .unwrap();
    assert_eq!(sound, mp3());
    let body = json_body(&requests.recv().await.unwrap());
    assert_eq!(body["response_format"], "mp3");
    assert_eq!(body["input"], "Привет");
    assert_eq!(body["voice"], "coral");
    assert_eq!(body["instructions"], "Сейчас радостно.");

    // The usual speech still names no format.
    let (base, mut requests) = serve_many(vec![("200 OK", "audio/wav", wav())]).await;
    let (l, _) = live(&base, |c| c.voice.tts_model = "m".into());
    l.synthesize_with("Привет", "").await.unwrap();
    let body = json_body(&requests.recv().await.unwrap());
    assert!(body.get("response_format").is_none(), "{body}");
}

#[tokio::test]
async fn a_refusal_of_the_format_is_remembered() {
    let (base, mut requests) = serve_many(vec![
        refusal("400 Bad Request", "response_format mp3 is not supported"),
        ("200 OK", "audio/pcm", wav()),
        ("200 OK", "audio/pcm", wav()),
    ])
    .await;
    let (l, _) = live(&base, |c| c.voice.tts_model = "google/gemini-tts".into());
    assert_eq!(l.synthesize_format("Раз", "", "mp3").await.unwrap(), wav());
    assert_eq!(
        json_body(&requests.recv().await.unwrap())["response_format"],
        "mp3"
    );
    let second = json_body(&requests.recv().await.unwrap());
    assert_eq!(second["input"], "Раз");
    assert!(second.get("response_format").is_none(), "{second}");
    // The next reply is asked for without the field at once.
    assert_eq!(l.synthesize_format("Два", "", "mp3").await.unwrap(), wav());
    let third = json_body(&requests.recv().await.unwrap());
    assert_eq!(third["input"], "Два");
    assert!(third.get("response_format").is_none(), "{third}");
    assert!(requests.try_recv().is_err(), "three requests in all");
}

#[tokio::test]
async fn too_many_requests_is_not_a_refusal_of_the_format() {
    let (base, mut requests) = serve_many(vec![
        refusal("429 Too Many Requests", "slow down"),
        ("200 OK", "audio/mpeg", mp3()),
    ])
    .await;
    let (l, _) = live(&base, |c| c.voice.tts_model = "m".into());
    let err = l.synthesize_format("Раз", "", "mp3").await.unwrap_err();
    assert!(err.starts_with("синтез речи ответил 429"), "{err}");
    assert_eq!(
        json_body(&requests.recv().await.unwrap())["response_format"],
        "mp3"
    );
    // Nothing was remembered: the format is asked for again.
    assert_eq!(l.synthesize_format("Раз", "", "mp3").await.unwrap(), mp3());
    assert_eq!(
        json_body(&requests.recv().await.unwrap())["response_format"],
        "mp3"
    );
    assert!(requests.try_recv().is_err());
}

#[tokio::test]
async fn a_refusal_that_stays_without_the_format_is_not_remembered() {
    // A bad key refuses both requests: the format is not to blame.
    let (base, mut requests) = serve_many(vec![
        refusal("401 Unauthorized", "bad key sk-speech-secret"),
        refusal("401 Unauthorized", "bad key sk-speech-secret"),
        ("200 OK", "audio/mpeg", mp3()),
    ])
    .await;
    let (l, _) = live(&base, |c| c.voice.tts_model = "m".into());
    let err = l.synthesize_format("Раз", "", "mp3").await.unwrap_err();
    assert!(err.starts_with("синтез речи ответил 401"), "{err}");
    assert!(!err.contains(SPEECH_KEY), "{err}");
    requests.recv().await.unwrap();
    requests.recv().await.unwrap();
    assert_eq!(l.synthesize_format("Раз", "", "mp3").await.unwrap(), mp3());
    assert_eq!(
        json_body(&requests.recv().await.unwrap())["response_format"],
        "mp3"
    );
}

#[tokio::test]
async fn a_sound_kept_without_a_format_is_not_served_for_mp3() {
    let dir = tempfile::tempdir().unwrap();
    let (base, mut requests) = serve_many(vec![
        ("200 OK", "audio/wav", wav()),
        ("200 OK", "audio/mpeg", mp3()),
    ])
    .await;
    let (l, _) = live(&base, |c| c.voice.tts_model = "m".into());
    let l = l.with_speech_cache(Arc::new(SpeechCache::new(dir.path().to_path_buf())));
    assert_eq!(l.synthesize_with("Привет", "").await.unwrap(), wav());
    // The same words again: from the cache, no request.
    assert_eq!(l.synthesize_with("Привет", "").await.unwrap(), wav());
    assert!(json_body(&requests.recv().await.unwrap())
        .get("response_format")
        .is_none());
    assert!(requests.try_recv().is_err());

    assert_eq!(
        l.synthesize_format("Привет", "", "mp3").await.unwrap(),
        mp3()
    );
    assert_eq!(
        json_body(&requests.recv().await.unwrap())["response_format"],
        "mp3"
    );
    // Each is kept under its own key.
    assert_eq!(
        l.synthesize_format("Привет", "", "mp3").await.unwrap(),
        mp3()
    );
    assert_eq!(l.synthesize_with("Привет", "").await.unwrap(), wav());
    assert!(requests.try_recv().is_err(), "two requests in all");
}

// ------------------------------------------------------------------- Telegram

const TOKEN: &str = "123456:bot-token-secret";

fn bot(base: &str) -> HttpTelegram {
    HttpTelegram::with_base(Secret::new(TOKEN), &format!("{base}/"))
}

fn ok_json(result: &str) -> Reply {
    (
        "200 OK",
        "application/json",
        format!(r#"{{"ok":true,"result":{result}}}"#).into_bytes(),
    )
}

#[tokio::test]
async fn a_file_is_asked_for_and_then_downloaded() {
    let (base, mut requests) = serve_many(vec![
        ok_json(r#"{"file_id":"AwACAg","file_size":38,"file_path":"voice/file_12.oga"}"#),
        ("200 OK", "application/octet-stream", ogg_opus()),
    ])
    .await;
    let bytes = bot(&base).download("AwACAg", 1024).await.unwrap();
    assert_eq!(bytes, ogg_opus());
    let asked = requests.recv().await.unwrap();
    assert!(contains(&asked, "POST /bot123456:bot-token-secret/getFile"));
    assert_eq!(json_body(&asked)["file_id"], "AwACAg");
    let got = requests.recv().await.unwrap();
    assert!(contains(
        &got,
        "GET /file/bot123456:bot-token-secret/voice/file_12.oga "
    ));
}

#[tokio::test]
async fn a_file_over_the_limit_is_not_downloaded() {
    // The size Telegram names is over the limit: no second request.
    let (base, mut requests) = serve_many(vec![ok_json(
        r#"{"file_id":"f","file_size":2097153,"file_path":"voice/file_1.oga"}"#,
    )])
    .await;
    let err = bot(&base).download("f", 2 * 1024 * 1024).await.unwrap_err();
    assert_eq!(err, "файл больше 2 МБ");
    requests.recv().await.unwrap();
    assert!(requests.try_recv().is_err(), "the file was not asked for");

    // The size is not named, or named wrong: the body is cut at the limit.
    for named in [r#""file_size":10,"#, ""] {
        let (base, _requests) = serve_many(vec![
            ok_json(&format!(
                r#"{{"file_id":"f",{named}"file_path":"voice/file_1.oga"}}"#
            )),
            ("200 OK", "application/octet-stream", vec![7; 5000]),
        ])
        .await;
        let err = bot(&base).download("f", 1024).await.unwrap_err();
        assert_eq!(err, "файл больше 0 МБ", "{named}");
    }

    // Exactly the limit is taken.
    let (base, _requests) = serve_many(vec![
        ok_json(r#"{"file_id":"f","file_size":1024,"file_path":"voice/file_1.oga"}"#),
        ("200 OK", "application/octet-stream", vec![7; 1024]),
    ])
    .await;
    assert_eq!(bot(&base).download("f", 1024).await.unwrap().len(), 1024);
}

#[tokio::test]
async fn a_strange_file_path_is_not_followed() {
    for path in ["../x", "/etc/passwd", "voice/../../x", "voice/a b.oga", ""] {
        let (base, mut requests) = serve_many(vec![ok_json(&format!(
            r#"{{"file_id":"f","file_size":10,"file_path":"{path}"}}"#
        ))])
        .await;
        let err = bot(&base).download("f", 1024).await.unwrap_err();
        assert_eq!(err, "Telegram дал странный путь к файлу", "{path}");
        requests.recv().await.unwrap();
        assert!(requests.try_recv().is_err(), "{path}");
    }
    let (base, _requests) = serve_many(vec![ok_json(r#"{"file_id":"f"}"#)]).await;
    assert_eq!(
        bot(&base).download("f", 1024).await.unwrap_err(),
        "Telegram не дал путь к файлу"
    );
}

#[tokio::test]
async fn the_bot_token_is_in_no_error_text() {
    let file = r#"{"file_id":"f","file_size":10,"file_path":"voice/file_1.oga"}"#;
    let cases: Vec<Vec<Reply>> = vec![
        // Telegram refuses and repeats the token.
        vec![(
            "401 Unauthorized",
            "application/json",
            format!(r#"{{"ok":false,"description":"Unauthorized: {TOKEN}"}}"#).into_bytes(),
        )],
        // The connection breaks at getFile.
        vec![("", "", Vec::new())],
        // The file is gone.
        vec![
            ok_json(file),
            ("404 Not Found", "text/plain", TOKEN.as_bytes().to_vec()),
        ],
        // The connection breaks at the file.
        vec![ok_json(file), ("", "", Vec::new())],
    ];
    for replies in cases {
        let (base, _requests) = serve_many(replies).await;
        let err = bot(&base).download("f", 1024).await.unwrap_err();
        assert!(!err.is_empty());
        assert!(!err.contains("bot-token-secret"), "{err}");
        assert!(
            !err.contains("/file/bot") && !err.contains("127.0.0.1"),
            "{err}"
        );
    }
    let (base, _requests) = serve_many(vec![
        ok_json(file),
        ("404 Not Found", "text/plain", Vec::new()),
    ])
    .await;
    assert_eq!(
        bot(&base).download("f", 1024).await.unwrap_err(),
        "файл не скачался: ответ 404"
    );

    // The same for a sound that Telegram refuses or never answers about.
    let sound = VoiceFile {
        bytes: mp3(),
        kind: SoundKind::Mp3,
    };
    let (base, _requests) = serve_many(vec![(
        "400 Bad Request",
        "application/json",
        format!(r#"{{"ok":false,"description":"Bad Request: {TOKEN} file is too big"}}"#)
            .into_bytes(),
    )])
    .await;
    let err = bot(&base).send_sound(42, &sound).await.unwrap_err();
    assert!(
        err.starts_with("Telegram ответил ошибкой: Bad Request"),
        "{err}"
    );
    assert!(!err.contains("bot-token-secret"), "{err}");
    let (base, _requests) = serve_many(vec![("", "", Vec::new())]).await;
    let err = bot(&base).send_sound(42, &sound).await.unwrap_err();
    assert!(
        !err.contains("bot-token-secret") && !err.contains("/bot"),
        "{err}"
    );
}

#[tokio::test]
async fn a_sound_goes_as_a_voice_message_or_as_a_document() {
    let (base, mut requests) = serve_many(vec![ok_json("{}"), ok_json("{}"), ok_json("{}")]).await;
    let bot = bot(&base);

    let sound = VoiceFile {
        bytes: mp3(),
        kind: SoundKind::Mp3,
    };
    bot.send_sound(42, &sound).await.unwrap();
    let req = requests.recv().await.unwrap();
    assert!(contains(
        &req,
        "POST /bot123456:bot-token-secret/sendVoice "
    ));
    assert!(contains(&req, "name=\"chat_id\"\r\n\r\n42"));
    assert!(contains(&req, "name=\"voice\"; filename=\"voice.mp3\""));
    assert!(contains(&req, "audio/mpeg"));
    assert!(contains(&req, "mp3-sound"));

    let sound = VoiceFile {
        bytes: ogg_opus(),
        kind: SoundKind::OggOpus,
    };
    bot.send_sound(42, &sound).await.unwrap();
    let req = requests.recv().await.unwrap();
    assert!(contains(&req, "/sendVoice "));
    assert!(contains(&req, "name=\"voice\"; filename=\"voice.ogg\""));
    assert!(contains(&req, "audio/ogg"));

    // Telegram shows no WAV as a voice message: it goes as a file.
    let sound = VoiceFile {
        bytes: wav(),
        kind: SoundKind::Wav,
    };
    bot.send_sound(42, &sound).await.unwrap();
    let req = requests.recv().await.unwrap();
    assert!(contains(
        &req,
        "POST /bot123456:bot-token-secret/sendDocument "
    ));
    assert!(contains(
        &req,
        "name=\"document\"; filename=\"mika-voice.wav\""
    ));
    assert!(contains(&req, "audio/wav"));
    assert!(!contains(&req, "name=\"voice\""));

    // Nothing is sent for a sound of no known kind or over the limit.
    let unknown = VoiceFile {
        bytes: b"no sound".to_vec(),
        kind: SoundKind::Unknown,
    };
    assert_eq!(
        bot.send_sound(42, &unknown).await.unwrap_err(),
        "звук в неизвестном формате"
    );
    let huge = VoiceFile {
        bytes: vec![0; SOUND_MAX_BYTES + 1],
        kind: SoundKind::Mp3,
    };
    assert_eq!(
        bot.send_sound(42, &huge).await.unwrap_err(),
        "звук больше 50 МБ"
    );
    assert!(requests.try_recv().is_err(), "three requests in all");
}

// ---------------------------------------------------------------- the service

/// A converter that only records what it was given.
#[derive(Default)]
struct FakeConverter {
    ready: AtomicBool,
    broken: AtomicBool,
    to_wav: Mutex<Vec<Vec<u8>>>,
    to_ogg: Mutex<Vec<Vec<u8>>>,
}

impl FakeConverter {
    fn ready() -> Arc<Self> {
        let c = Self::default();
        c.ready.store(true, Ordering::Relaxed);
        Arc::new(c)
    }
}

const CONVERTED_WAV: &[u8] = b"RIFF\x24\0\0\0WAVEfmt converted-to-wav";

#[async_trait]
impl SoundConverter for FakeConverter {
    fn ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }
    async fn to_wav16k(&self, sound: &[u8]) -> Result<Vec<u8>, String> {
        self.to_wav.lock().push(sound.to_vec());
        Ok(CONVERTED_WAV.to_vec())
    }
    async fn to_ogg_opus(&self, sound: &[u8]) -> Result<Vec<u8>, String> {
        self.to_ogg.lock().push(sound.to_vec());
        if self.broken.load(Ordering::Relaxed) {
            return Err("FFmpeg завершился с ошибкой: Unknown encoder 'libopus'".into());
        }
        Ok(ogg_opus())
    }
}

/// What the note maker was given, in order.
type Notes = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A service whose note is the reply itself, or its marked copy when there is one.
fn service(live: Live, converter: Arc<dyn SoundConverter>) -> (LiveVoice, Notes) {
    let notes: Notes = Arc::default();
    let seen = notes.clone();
    let note: NoteMaker = Arc::new(move |text: &str, spoken: Option<&str>| {
        seen.lock()
            .push((text.to_string(), spoken.map(str::to_string)));
        Some(spoken.unwrap_or(text).to_string())
    });
    let instructions: Instructions = Arc::new(|| "Сейчас радостно.".to_string());
    (LiveVoice::new(live, converter, note, instructions), notes)
}

#[tokio::test]
async fn a_voice_message_is_recognised_as_telegram_gave_it() {
    let (base, mut requests) = serve_many(vec![text_reply("Какая погода?")]).await;
    let (l, _) = live(&base, |c| c.voice.stt_model = "m".into());
    let converter = FakeConverter::ready();
    let (voice, _) = service(l, converter.clone());
    assert_eq!(voice.hear(&ogg_opus()).await, Ok("Какая погода?".into()));
    let req = requests.recv().await.unwrap();
    assert!(contains(&req, "filename=\"voice.ogg\""));
    assert!(requests.try_recv().is_err(), "one request");
    assert!(converter.to_wav.lock().is_empty(), "nothing is converted");
}

#[tokio::test]
async fn a_refused_voice_message_goes_again_as_wav_and_that_is_remembered() {
    let (base, mut requests) = serve_many(vec![
        refusal("400 Bad Request", "unsupported audio format"),
        text_reply("Привет"),
        text_reply("Ещё раз"),
        text_reply("Снова как есть"),
    ])
    .await;
    let (l, _) = live(&base, |c| c.voice.stt_model = "strict/asr".into());
    let converter = FakeConverter::ready();
    let (voice, _) = service(l, converter.clone());

    assert_eq!(voice.hear(&ogg_opus()).await, Ok("Привет".into()));
    assert!(contains(
        &requests.recv().await.unwrap(),
        "filename=\"voice.ogg\""
    ));
    // The second request is what the microphone sends.
    let second = requests.recv().await.unwrap();
    assert!(contains(&second, "filename=\"speech.wav\""));
    assert!(contains(&second, "converted-to-wav"));
    assert_eq!(*converter.to_wav.lock(), vec![ogg_opus()]);

    // The next voice message goes straight to WAV: no refused request first.
    assert_eq!(voice.hear(&ogg_opus()).await, Ok("Ещё раз".into()));
    let third = requests.recv().await.unwrap();
    assert!(contains(&third, "filename=\"speech.wav\""));
    assert!(requests.try_recv().is_err(), "three requests so far");
    assert_eq!(converter.to_wav.lock().len(), 2);

    // Without the converter the sound is tried as it is again.
    converter.ready.store(false, Ordering::Relaxed);
    assert_eq!(voice.hear(&ogg_opus()).await, Ok("Снова как есть".into()));
    assert!(contains(
        &requests.recv().await.unwrap(),
        "filename=\"voice.ogg\""
    ));
    assert_eq!(converter.to_wav.lock().len(), 2);
}

#[tokio::test]
async fn a_refused_voice_message_without_a_converter_says_what_to_do() {
    let (base, mut requests) =
        serve_many(vec![refusal("400 Bad Request", "unsupported audio format")]).await;
    let (l, _) = live(&base, |c| c.voice.stt_model = "m".into());
    let (voice, _) = service(l, Arc::new(NoConverter));
    let err = voice.hear(&ogg_opus()).await.unwrap_err();
    assert!(
        err.starts_with("распознавание ответило 400: unsupported audio format. "),
        "{err}"
    );
    assert!(err.ends_with(VOICE_NEEDS_WAV), "{err}");
    requests.recv().await.unwrap();
    assert!(requests.try_recv().is_err(), "one request");

    // Another failure is no reason to convert, with a converter or without.
    let (base, mut requests) = serve_many(vec![refusal(
        "401 Unauthorized",
        "bad key sk-speech-secret",
    )])
    .await;
    let (l, _) = live(&base, |c| c.voice.stt_model = "m".into());
    let converter = FakeConverter::ready();
    let (voice, _) = service(l, converter.clone());
    let err = voice.hear(&ogg_opus()).await.unwrap_err();
    assert!(err.starts_with("распознавание ответило 401"), "{err}");
    assert!(
        !err.contains(VOICE_NEEDS_WAV) && !err.contains(SPEECH_KEY),
        "{err}"
    );
    requests.recv().await.unwrap();
    assert!(requests.try_recv().is_err());
    assert!(converter.to_wav.lock().is_empty());
}

fn provider_voice(c: &mut Config) {
    c.voice.tts = TtsEngine::Provider;
    c.voice.tts_model = "openai/gpt-4o-mini-tts".into();
}

#[tokio::test]
async fn an_mp3_reply_is_a_voice_message_as_it_is() {
    let (base, mut requests) = serve_many(vec![("200 OK", "audio/mpeg", mp3())]).await;
    let (l, _) = live(&base, provider_voice);
    let converter = FakeConverter::ready();
    let (voice, notes) = service(l, converter.clone());
    let sound = voice.say("Завтра ясно.", None).await.unwrap().unwrap();
    assert_eq!(
        sound,
        VoiceFile {
            bytes: mp3(),
            kind: SoundKind::Mp3
        }
    );
    // One request for the whole reply, MP3 asked for, with the tone of the moment.
    let body = json_body(&requests.recv().await.unwrap());
    assert_eq!(body["input"], "Завтра ясно.");
    assert_eq!(body["response_format"], "mp3");
    assert_eq!(body["instructions"], "Сейчас радостно.");
    assert!(requests.try_recv().is_err());
    assert_eq!(*notes.lock(), vec![("Завтра ясно.".to_string(), None)]);
    assert!(converter.to_ogg.lock().is_empty(), "nothing is converted");
}

#[tokio::test]
async fn a_wav_reply_becomes_a_voice_message_only_with_a_converter() {
    // With a converter: OGG/Opus.
    let (base, _requests) = serve_many(vec![("200 OK", "audio/pcm", wav())]).await;
    let (l, _) = live(&base, provider_voice);
    let converter = FakeConverter::ready();
    let (voice, _) = service(l, converter.clone());
    let sound = voice.say("Завтра ясно.", None).await.unwrap().unwrap();
    assert_eq!(sound.kind, SoundKind::OggOpus);
    assert_eq!(sound.bytes, ogg_opus());
    assert_eq!(*converter.to_ogg.lock(), vec![wav()]);

    // Without one: the WAV itself, which goes as a file.
    let (base, _requests) = serve_many(vec![("200 OK", "audio/pcm", wav())]).await;
    let (l, _) = live(&base, provider_voice);
    let (voice, _) = service(l, Arc::new(NoConverter));
    let sound = voice.say("Завтра ясно.", None).await.unwrap().unwrap();
    assert_eq!(
        sound,
        VoiceFile {
            bytes: wav(),
            kind: SoundKind::Wav
        }
    );

    // A conversion that fails: the WAV too.
    let (base, _requests) = serve_many(vec![("200 OK", "audio/pcm", wav())]).await;
    let (l, _) = live(&base, provider_voice);
    let converter = FakeConverter::ready();
    converter.broken.store(true, Ordering::Relaxed);
    let (voice, _) = service(l, converter.clone());
    let sound = voice.say("Завтра ясно.", None).await.unwrap().unwrap();
    assert_eq!(sound.kind, SoundKind::Wav);
    assert_eq!(converter.to_ogg.lock().len(), 1);

    // Raw samples that start with -1 are WAV as well, not an MP3 frame.
    let (base, _requests) =
        serve_many(vec![("200 OK", "audio/pcm", vec![0xFF, 0xFF, 0x01, 0x00])]).await;
    let (l, _) = live(&base, provider_voice);
    let (voice, _) = service(l, Arc::new(NoConverter));
    let sound = voice.say("Завтра ясно.", None).await.unwrap().unwrap();
    assert_eq!(sound.kind, SoundKind::Wav);
}

#[tokio::test]
async fn a_reply_without_words_orders_no_speech() {
    // Port 1: a request would fail.
    let (l, _) = live("http://127.0.0.1:1", provider_voice);
    let note: NoteMaker = Arc::new(|_: &str, _: Option<&str>| None);
    let instructions: Instructions = Arc::new(String::new);
    let voice = LiveVoice::new(l, Arc::new(NoConverter), note, instructions);
    assert_eq!(voice.say("```\ncode\n```", None).await, Ok(None));
}

#[tokio::test]
async fn a_failed_synthesis_is_an_error_without_the_key() {
    let (base, _requests) = serve_many(vec![refusal(
        "500 Internal Server Error",
        "boom sk-speech-secret",
    )])
    .await;
    let (l, _) = live(&base, provider_voice);
    let (voice, _) = service(l, Arc::new(NoConverter));
    let err = voice.say("Завтра ясно.", None).await.unwrap_err();
    assert!(err.starts_with("синтез речи ответил 500"), "{err}");
    assert!(!err.contains(SPEECH_KEY), "{err}");
}

#[tokio::test]
async fn marks_reach_the_note_only_for_a_voice_that_performs_them() {
    let (base, mut requests) = serve_many(vec![
        ("200 OK", "audio/mpeg", mp3()),
        ("200 OK", "audio/mpeg", mp3()),
        ("200 OK", "audio/mpeg", mp3()),
    ])
    .await;
    let (l, config) = live(&base, |c| {
        c.voice.tts = TtsEngine::Provider;
        c.voice.tts_model = "fish-audio/drama-3".into();
        c.voice.tts_expressive = true;
        // Replies need not be spoken on this computer.
        c.voice.speak_replies = false;
    });
    let (voice, notes) = service(l, Arc::new(NoConverter));
    let marked = "[sigh] Ну вот.";

    voice.say("Ну вот.", Some(marked)).await.unwrap().unwrap();
    assert_eq!(json_body(&requests.recv().await.unwrap())["input"], marked);

    // The switch of expressive speech goes off: the next reply has no marks.
    config.lock().voice.tts_expressive = false;
    voice.say("Ну вот.", Some(marked)).await.unwrap().unwrap();
    assert_eq!(
        json_body(&requests.recv().await.unwrap())["input"],
        "Ну вот."
    );

    // Another model would read them aloud.
    config.lock().voice.tts_expressive = true;
    config.lock().voice.tts_model = "fish-audio/s2-pro".into();
    voice.say("Ну вот.", Some(marked)).await.unwrap().unwrap();
    assert_eq!(
        json_body(&requests.recv().await.unwrap())["input"],
        "Ну вот."
    );

    assert_eq!(
        *notes.lock(),
        vec![
            ("Ну вот.".to_string(), Some(marked.to_string())),
            ("Ну вот.".to_string(), None),
            ("Ну вот.".to_string(), None),
        ]
    );
}

#[test]
fn the_switches_are_read_at_every_message() {
    let (l, config) = live("http://127.0.0.1:1", |_| {});
    let (voice, _) = service(l, Arc::new(NoConverter));
    assert!(!voice.accepts() && !voice.replies());

    config.lock().telegram.voice_in = true;
    assert!(voice.accepts());
    assert!(!voice.replies());

    // The second switch needs a provider voice with a model.
    config.lock().telegram.voice_reply = true;
    assert!(!voice.replies(), "the Windows voice makes no voice message");
    config.lock().voice.tts = TtsEngine::Provider;
    assert!(!voice.replies(), "no speech model");
    config.lock().voice.tts_model = "google/gemini-tts".into();
    assert!(voice.replies());

    // And the first switch.
    config.lock().telegram.voice_in = false;
    assert!(!voice.accepts() && !voice.replies());
}

/// Recognition and synthesis for Telegram need neither switch of this
/// computer: the microphone and the spoken replies stay as they are.
#[tokio::test]
async fn the_microphone_and_the_speaker_of_the_computer_are_not_needed() {
    let (base, _requests) =
        serve_many(vec![text_reply("Привет"), ("200 OK", "audio/mpeg", mp3())]).await;
    let (l, config) = live(&base, |c| {
        provider_voice(c);
        c.voice.stt_model = "m".into();
        c.voice.speak_replies = false;
    });
    assert!(!config.lock().privacy.microphone_enabled);
    let (voice, _) = service(l, Arc::new(NoConverter));
    assert_eq!(voice.hear(&ogg_opus()).await, Ok("Привет".into()));
    assert!(voice.say("Привет", None).await.unwrap().is_some());
    assert!(!config.lock().privacy.microphone_enabled);
}
