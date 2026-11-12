//! Speech through a provider: recognition by `/audio/transcriptions`,
//! synthesis by `/audio/speech` (OpenAI-compatible, as RouterAI serves them).

use async_trait::async_trait;
use mika_core::config::SPEECH_SECRET_KEY;
use mika_core::llm::{SpeechToText, TextToSpeech};
use serde_json::{json, Value};

use crate::factory::redact;
use crate::live::Live;

/// Longest wait for one phrase.
const SPEECH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
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
        let voice = self.speech_settings();
        let model = voice.tts_model.trim().to_string();
        if model.is_empty() {
            return Err("модель синтеза речи не задана".into());
        }
        let key = self.key(SPEECH_SECRET_KEY);
        let hide = |text: String| redact(&text, key.as_ref());
        let mut body = json!({ "model": model, "input": text });
        let name = voice.tts_voice.trim();
        if !name.is_empty() {
            body["voice"] = json!(name);
        }
        let mut req = self
            .http()
            .post(format!(
                "{}/audio/speech",
                self.base(&voice.speech_base_url)
            ))
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
        // The content type cannot be trusted: RouterAI labels a WAV file as audio/pcm.
        Ok(if has_audio_header(&bytes) {
            bytes.to_vec()
        } else {
            wav_from_pcm(&bytes, RAW_PCM_RATE)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
