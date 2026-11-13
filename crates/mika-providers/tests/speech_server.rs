//! Speech adapters against a fake HTTP server on 127.0.0.1.

use std::sync::Arc;

use mika_core::config::{Config, SPEECH_SECRET_KEY};
use mika_core::llm::{SpeechToText, TextToSpeech};
use mika_core::secrets::{MemorySecretStore, Secret, SecretStore};
use mika_providers::live::Live;
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Serves one request. Returns the base URL and the raw request bytes.
async fn serve(
    status: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (String, oneshot::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    tokio::spawn(async move {
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
        sock.write_all(head.as_bytes()).await.unwrap();
        sock.write_all(&body).await.unwrap();
        sock.shutdown().await.ok();
    });
    (format!("http://{addr}/v1"), rx)
}

fn live(base: &str, edit: impl FnOnce(&mut Config)) -> Live {
    let mut c = Config::default();
    c.voice.speech_base_url = base.to_string();
    edit(&mut c);
    let secrets = MemorySecretStore::default();
    secrets
        .set(SPEECH_SECRET_KEY, &Secret::new("sk-speech-secret"))
        .unwrap();
    Live::new(Arc::new(Mutex::new(c)), Arc::new(secrets))
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

#[tokio::test]
async fn transcription_sends_the_file_model_and_language() {
    let (base, request) = serve(
        "200 OK",
        "application/json",
        r#"{"text":" Какая погода? ","usage":{"seconds":2.1}}"#.into(),
    )
    .await;
    let l = live(&base, |c| c.voice.stt_model = "qwen/qwen3-asr-0.6b".into());
    let text = l.transcribe(b"RIFFfakewav").await.unwrap();
    assert_eq!(text, "Какая погода?");
    let req = request.await.unwrap();
    assert!(contains(&req, "POST /v1/audio/transcriptions"));
    assert!(contains(&req, "authorization: Bearer sk-speech-secret"));
    assert!(contains(&req, "name=\"model\"\r\n\r\nqwen/qwen3-asr-0.6b"));
    assert!(contains(&req, "name=\"language\"\r\n\r\nru"));
    assert!(contains(&req, "filename=\"speech.wav\""));
    assert!(contains(&req, "RIFFfakewav"));
}

#[tokio::test]
async fn synthesis_sends_voice_and_keeps_a_wav_as_is() {
    let wav = b"RIFF\x24\0\0\0WAVEfmt sound".to_vec();
    // RouterAI answers with this content type for a real WAV file.
    let (base, request) = serve("200 OK", "audio/pcm", wav.clone()).await;
    let l = live(&base, |c| {
        c.voice.tts_model = "google/gemini-3.8-flash-lite-tts".into();
        c.voice.tts_voice = "Kore".into();
    });
    assert_eq!(l.synthesize("Привет").await.unwrap(), wav);
    let req = request.await.unwrap();
    assert!(contains(&req, "POST /v1/audio/speech"));
    let body = String::from_utf8_lossy(&req);
    let json: serde_json::Value =
        serde_json::from_str(&body[body.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(json["model"], "google/gemini-3.8-flash-lite-tts");
    assert_eq!(json["voice"], "Kore");
    assert_eq!(json["input"], "Привет");
}

#[tokio::test]
async fn the_voice_in_words_goes_as_instructions() {
    let wav = b"RIFF\x24\0\0\0WAVEfmt sound".to_vec();
    let (base, request) = serve("200 OK", "audio/wav", wav.clone()).await;
    let l = live(&base, |c| {
        c.voice.tts_model = "openai/gpt-4o-mini-tts".into();
        c.voice.tts_voice = "coral".into();
        c.voice.tts_cache = false;
    });
    let out = l
        .synthesize_with("Привет", "Молодой тёплый голос. Сейчас радостно.")
        .await
        .unwrap();
    assert_eq!(out, wav);
    let req = request.await.unwrap();
    let body = String::from_utf8_lossy(&req);
    let json: serde_json::Value =
        serde_json::from_str(&body[body.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(
        json["instructions"],
        "Молодой тёплый голос. Сейчас радостно."
    );
    assert_eq!(json["voice"], "coral");
}

#[tokio::test]
async fn headerless_sound_is_wrapped_into_wav() {
    let (base, _request) = serve("200 OK", "audio/pcm", vec![1, 0, 2, 0, 3, 0]).await;
    let l = live(&base, |c| c.voice.tts_model = "m".into());
    let out = l.synthesize("x").await.unwrap();
    assert_eq!(&out[..4], b"RIFF");
    assert_eq!(out.len(), 44 + 6);
}

#[tokio::test]
async fn provider_error_is_reported_without_the_key() {
    let (base, _request) = serve(
        "400 Bad Request",
        "application/json",
        r#"{"error":{"message":"voice is missing, key sk-speech-secret","code":400}}"#.into(),
    )
    .await;
    let l = live(&base, |c| c.voice.tts_model = "m".into());
    let err = l.synthesize("x").await.unwrap_err();
    assert!(
        err.contains("синтез речи ответил 400: voice is missing"),
        "{err}"
    );
    assert!(!err.contains("sk-speech-secret"), "{err}");
}

#[tokio::test]
async fn nothing_is_sent_without_a_model() {
    let l = live("http://127.0.0.1:1/v1", |_| {});
    assert!(l.transcribe(b"x").await.unwrap_err().contains("не задана"));
    assert!(l.synthesize("x").await.unwrap_err().contains("не задана"));
}
