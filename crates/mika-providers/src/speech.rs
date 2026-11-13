//! Speech through a provider: recognition by `/audio/transcriptions`,
//! synthesis by `/audio/speech` (OpenAI-compatible, as RouterAI serves them).

use async_trait::async_trait;
use mika_core::config::SPEECH_SECRET_KEY;
use mika_core::llm::{SpeechToText, TextToSpeech};
use mika_core::speech_cache::SoundKey;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::factory::redact;
use crate::live::Live;

/// Longest wait for one phrase.
const SPEECH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Address and model of the speech providers that refused `instructions`:
/// until the app restarts they get every phrase without them at once, not
/// after a refused request.
static NO_INSTRUCTIONS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// Sample rate assumed for sound that came without a header.
const RAW_PCM_RATE: u32 = 24_000;

/// True when the bytes start like a sound file a player understands.
pub fn has_audio_header(bytes: &[u8]) -> bool {
    bytes.starts_with(b"RIFF")
        || bytes.starts_with(b"ID3")
        || bytes.starts_with(b"OggS")
        || bytes.starts_with(b"fLaC")
        // MPEG audio frame sync.
        || (bytes.len() > 1 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0)
}

/// A WAV file around 16-bit mono samples.
pub fn wav_from_pcm(pcm: &[u8], rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Sample rate of raw sound from its content type: `audio/pcm; rate=44100`.
pub fn pcm_rate(content_type: &str) -> Option<u32> {
    content_type
        .split(';')
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("rate"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .filter(|r| (8_000..=96_000).contains(r))
}

/// Sound a player understands. The content type alone cannot be trusted:
/// RouterAI labels a WAV file as `audio/pcm`. A file with a header stays as it
/// is; raw samples get a WAV header at the rate the answer names (Fish Audio
/// sends 44.1 kHz), or 24 kHz when it names none.
pub fn playable(bytes: &[u8], content_type: &str) -> Vec<u8> {
    let named_file = [b"RIFF".as_slice(), b"ID3", b"OggS", b"fLaC"]
        .iter()
        .any(|h| bytes.starts_with(h));
    if named_file {
        return bytes.to_vec();
    }
    // Raw samples may start with 0xFFFF, which looks like an MPEG frame.
    if let Some(rate) = pcm_rate(content_type) {
        return wav_from_pcm(bytes, rate);
    }
    if has_audio_header(bytes) {
        bytes.to_vec()
    } else {
        wav_from_pcm(bytes, RAW_PCM_RATE)
    }
}

/// Text of an OpenAI-style transcription answer.
pub fn parse_transcription(body: &str) -> Result<String, String> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| format!("непонятный ответ распознавания: {e}"))?;
    v["text"]
        .as_str()
        .map(|t| t.trim().to_string())
        .ok_or_else(|| "в ответе распознавания нет текста".to_string())
}

/// Message of a provider error body: `{"error":{"message":"voice is missing"}}`.
fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

impl Live {
    fn speech_settings(&self) -> mika_core::config::VoiceConfig {
        self.config().lock().voice.clone()
    }
}

#[async_trait]
impl SpeechToText for Live {
    async fn transcribe(&self, wav: &[u8]) -> Result<String, String> {
        let voice = self.speech_settings();
        let model = voice.stt_model.trim().to_string();
        if model.is_empty() {
            return Err(
                "модель распознавания речи не задана: укажи её в настройках, раздел «Голос»".into(),
            );
        }
        let key = self.key(SPEECH_SECRET_KEY);
        let hide = |text: String| redact(&text, key.as_ref());
        let file = reqwest::multipart::Part::bytes(wav.to_vec())
            .file_name("speech.wav")
            .mime_str("audio/wav")
            .map_err(|e| e.to_string())?;
        let mut form = reqwest::multipart::Form::new()
            .text("model", model)
            .part("file", file);
        let language = voice.stt_language.trim().to_string();
        if !language.is_empty() {
            form = form.text("language", language);
        }
        let mut req = self
            .http()
            .post(format!(
                "{}/audio/transcriptions",
                self.base(&voice.speech_base_url)
            ))
            .timeout(SPEECH_TIMEOUT)
            .multipart(form);
        if let Some(k) = &key {
            req = req.bearer_auth(k.expose());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| hide(format!("нет связи с распознаванием: {}", e.without_url())))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| hide(e.without_url().to_string()))?;
        if !status.is_success() {
            return Err(hide(format!(
                "распознавание ответило {}: {}",
                status.as_u16(),
                error_message(&body)
            )));
        }
        parse_transcription(&body)
    }
}

#[async_trait]
impl TextToSpeech for Live {
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>, String> {
        let style = self.speech_settings().tts_style;
        self.synthesize_with(text, style.trim()).await
    }
}

impl Live {
    /// Speech with `instructions`: the voice and tone in words, as
    /// `gpt-4o-mini-tts` takes them. A provider that refuses the field gets the
    /// request again without it, and later phrases go to it without it at once.
    pub async fn synthesize_with(&self, text: &str, instructions: &str) -> Result<Vec<u8>, String> {
        let instructions = instructions.trim();
        let voice = self.speech_settings();
        let provider = (
            self.base(&voice.speech_base_url),
            voice.tts_model.trim().to_string(),
        );
        if instructions.is_empty() || NO_INSTRUCTIONS.lock().contains(&provider) {
            return self.speak_once(text, "").await;
        }
        match self.speak_once(text, instructions).await {
            Err(e) if e.starts_with("синтез речи ответил 4") => {
                log::warn!("speech provider refused instructions, asking without them: {e}");
                let sound = self.speak_once(text, "").await;
                // Too many requests is not a refusal of the field.
                if sound.is_ok() && !e.starts_with("синтез речи ответил 429") {
                    let mut refused = NO_INSTRUCTIONS.lock();
                    if !refused.contains(&provider) {
                        refused.push(provider);
                    }
                }
                sound
            }
            other => other,
        }
    }

    async fn speak_once(&self, text: &str, instructions: &str) -> Result<Vec<u8>, String> {
        let voice = self.speech_settings();
        let model = voice.tts_model.trim().to_string();
        if model.is_empty() {
            return Err("модель синтеза речи не задана".into());
        }
        let base = self.base(&voice.speech_base_url);
        let name = voice.tts_voice.trim();
        // The same words said in another tone are another sound.
        let voice_key = if instructions.is_empty() {
            name.to_string()
        } else {
            format!("{name}|{instructions}")
        };
        let sound_key = SoundKey {
            base_url: &base,
            model: &model,
            voice: &voice_key,
            text,
        };
        let cache = self.speech_cache().filter(|_| voice.tts_cache);
        if let Some(sound) = cache.and_then(|c| c.get(&sound_key)) {
            return Ok(sound);
        }
        let key = self.key(SPEECH_SECRET_KEY);
        let hide = |text: String| redact(&text, key.as_ref());
        let mut body = json!({ "model": model, "input": text });
        if !name.is_empty() {
            body["voice"] = json!(name);
        }
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions);
        }
        let mut req = self
            .http()
            .post(format!("{base}/audio/speech"))
            .timeout(SPEECH_TIMEOUT)
            .json(&body);
        if let Some(k) = &key {
            req = req.bearer_auth(k.expose());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| hide(format!("нет связи с синтезом речи: {}", e.without_url())))?;
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| hide(e.without_url().to_string()))?;
        if !status.is_success() {
            return Err(hide(format!(
                "синтез речи ответил {}: {}",
                status.as_u16(),
                error_message(&String::from_utf8_lossy(&bytes))
            )));
        }
        if bytes.is_empty() {
            return Err("синтез речи вернул пустой звук".into());
        }
        let sound = playable(&bytes, &content_type);
        if let Some(c) = cache {
            c.put(&sound_key, &sound);
        }
        Ok(sound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_sound_gets_the_rate_its_answer_names() {
        assert_eq!(pcm_rate("audio/pcm; rate=44100; channels=1"), Some(44_100));
        assert_eq!(pcm_rate("audio/pcm;rate=24000"), Some(24_000));
        assert_eq!(pcm_rate("audio/pcm"), None);
        assert_eq!(pcm_rate("audio/pcm; rate=abc"), None);
        let rate = |wav: &[u8]| u32::from_le_bytes(wav[24..28].try_into().unwrap());
        // Fish Audio: raw samples at 44.1 kHz, the first one -1 (0xFFFF).
        let fish = playable(&[0xFF, 0xFF, 1, 0], "audio/pcm; rate=44100; channels=1");
        assert_eq!(&fish[..4], b"RIFF");
        assert_eq!(rate(&fish), 44_100);
        // RouterAI calls a WAV file audio/pcm: it is kept as it is.
        let wav = wav_from_pcm(&[1, 0], 24_000);
        assert_eq!(playable(&wav, "audio/pcm"), wav);
        assert_eq!(rate(&playable(&[1, 0, 2, 0], "audio/pcm")), 24_000);
        let mp3 = [0xFF, 0xFB, 0x90, 0x00];
        assert_eq!(playable(&mp3, "audio/mpeg"), mp3);
    }

    #[test]
    fn known_sound_files_are_recognised_by_their_first_bytes() {
        assert!(has_audio_header(b"RIFF\x24\0\0\0WAVEfmt "));
        assert!(has_audio_header(b"ID3\x04\0"));
        assert!(has_audio_header(&[0xFF, 0xFB, 0x90, 0x00]));
        assert!(has_audio_header(b"OggS\0"));
        assert!(!has_audio_header(&[0x12, 0x00, 0x34, 0x00]));
        assert!(!has_audio_header(b""));
    }

    #[test]
    fn raw_samples_get_a_wav_header() {
        let wav = wav_from_pcm(&[1, 0, 2, 0], 24_000);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert!(has_audio_header(&wav));
    }

    #[test]
    fn transcription_text_is_trimmed_and_errors_are_plain() {
        assert_eq!(
            parse_transcription(r#"{"text":" Привет, Мика. ","usage":{"seconds":4.3}}"#),
            Ok("Привет, Мика.".into())
        );
        assert!(parse_transcription(r#"{"error":"x"}"#).is_err());
        assert!(parse_transcription("<html>").is_err());
        assert_eq!(
            error_message(r#"{"error":{"message":"voice is missing","code":400}}"#),
            "voice is missing"
        );
        assert_eq!(error_message("plain"), "plain");
    }

    /// The usual extension of a sound file, by its first bytes.
    fn sound_extension(sound: &[u8]) -> &'static str {
        if sound.starts_with(b"RIFF") {
            "wav"
        } else if sound.starts_with(b"OggS") {
            "ogg"
        } else if sound.starts_with(b"fLaC") {
            "flac"
        } else if has_audio_header(sound) {
            "mp3"
        } else {
            "bin"
        }
    }

    #[test]
    fn sound_files_get_their_usual_extension() {
        assert_eq!(sound_extension(&wav_from_pcm(&[1, 0], 24_000)), "wav");
        assert_eq!(sound_extension(b"ID3\x04\0"), "mp3");
        assert_eq!(sound_extension(&[0xFF, 0xFB, 0x90, 0x00]), "mp3");
        assert_eq!(sound_extension(b"OggS\0"), "ogg");
        assert_eq!(sound_extension(&[0x12, 0x00]), "bin");
    }

    /// A phrase marked up for Fish Audio Drama 3 is said by the user's speech
    /// provider and heard back by their recognition model: the marks must be
    /// performed, not read aloud (decision 0026). The saved settings and key
    /// are used as the app uses them; the changes stay in memory.
    ///
    /// `cargo test -p mika-providers live_drama -- --ignored --nocapture`.
    /// `MIKA_DRAMA_VOICE` names the voice, else the provider's first one is taken.
    #[tokio::test]
    #[ignore = "calls RouterAI: paid speech synthesis and recognition"]
    async fn live_drama_marks_are_not_spoken() {
        use mika_core::config::{Config, TtsEngine};
        use mika_core::paths::config_path;
        use mika_core::secrets::KeyringSecretStore;
        use std::sync::Arc;

        const MODEL: &str = "fish-audio/drama-3";
        const PHRASE: &str = "[sighs, tired] Ну что ж. <whisper>Иди без меня.</whisper> Я догоню.";

        let mut config = Config::load(&config_path())
            .unwrap_or_else(|e| panic!("the settings of Mika do not load: {e}"));
        config.voice.tts = TtsEngine::Provider;
        config.voice.tts_model = MODEL.into();
        config.voice.tts_cache = false;
        config.voice.tts_style = String::new();
        let live = Live::new(
            Arc::new(Mutex::new(config)),
            Arc::new(KeyringSecretStore::new()),
        );

        // The voices the settings page offers: the same address and key.
        let named = std::env::var("MIKA_DRAMA_VOICE")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let voice = match named {
            Some(v) => v,
            None => {
                let base = live.base(&live.speech_settings().speech_base_url);
                let key = live.key(SPEECH_SECRET_KEY);
                crate::catalog::fetch_voices(&base, key.as_ref(), MODEL)
                    .await
                    .into_iter()
                    .next()
                    .expect(
                        "the provider lists no voices for fish-audio/drama-3: \
                         set MIKA_DRAMA_VOICE to the id of a voice",
                    )
            }
        };
        println!("voice: {voice}");
        live.config().lock().voice.tts_voice = voice;

        let sound = live
            .synthesize_with(PHRASE, "")
            .await
            .unwrap_or_else(|e| panic!("speech synthesis failed: {e}"));
        let ext = sound_extension(&sound);
        let path = std::env::temp_dir().join(format!("mika-drama-live.{ext}"));
        std::fs::write(&path, &sound).expect("the sound is written to the temporary folder");
        println!("sound: {} ({} bytes)", path.display(), sound.len());

        assert!(
            !live.speech_settings().stt_model.trim().is_empty(),
            "no recognition model in the settings: listen to the file instead"
        );
        let heard = match live.transcribe(&sound).await {
            Ok(heard) => heard,
            // The recognizer is sent a WAV file; another sound may not suit it.
            Err(e) if ext != "wav" => {
                println!(
                    "note: the recognizer did not take the {ext} sound ({e}); \
                     listen to the file instead"
                );
                return;
            }
            Err(e) => panic!("speech recognition failed: {e}"),
        };
        println!("heard: {heard}");
        let heard = heard.to_lowercase().replace('ё', "е");
        assert!(
            heard.contains("без меня"),
            "the words are not heard: {heard}"
        );
        for mark in ["whisper", "виспер", "sigh", "сай", "tired"] {
            assert!(
                !heard.contains(mark),
                "the mark {mark:?} is read aloud: {heard}"
            );
        }
    }
}
