//! One-shot setup commands: provider, key, web tools.
//!
//! `mika-cli provider <openai|anthropic|ollama> <base_url|-> <model>`
//! `mika-cli key <openai|anthropic|ollama>`   reads the key from stdin, stores it in Credential Manager
//! `mika-cli model <embedding|vision|image> <model|->`   extra models; `-` switches one off
//! `mika-cli web <on|off>`
//! `mika-cli status`
//! `mika-cli probe`                         time to the first answer chunk
//! `mika-cli model <tts|stt|tts-voice> <value|->`         speech through the provider
//! `mika-cli voices`                        Windows voices for speech
//! `mika-cli say <text>`                    speaks the text with the configured voice
//! `mika-cli roundtrip <text>`              provider speech, then provider recognition of it
//! `mika-cli hear`                          records one phrase and prints what was recognised
//! `mika-cli mic <on|off>`                  the microphone switch

use std::io::BufRead;

use std::sync::Arc;

use mika_core::config::{Config, ProviderKind, TtsEngine};
use mika_core::llm::{SpeechToText, TextToSpeech};
use mika_core::paths::config_path;
use mika_core::secrets::{KeyringSecretStore, Secret, SecretStore};

pub fn parse_kind(s: &str) -> Result<ProviderKind, String> {
    match s {
        "openai" => Ok(ProviderKind::Openai),
        "anthropic" => Ok(ProviderKind::Anthropic),
        "ollama" => Ok(ProviderKind::Ollama),
        other => Err(format!(
            "неизвестный провайдер {other}: openai, anthropic или ollama"
        )),
    }
}

fn load() -> Result<Config, String> {
    Config::load(&config_path()).map_err(|e| e.to_string())
}

fn save(c: &Config) -> Result<(), String> {
    c.save(&config_path()).map_err(|e| e.to_string())
}

/// A runtime and the provider client for one-shot speech commands.
fn speech_runtime(
    config: Config,
) -> Result<(Arc<tokio::runtime::Runtime>, mika_providers::live::Live), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let live = mika_providers::live::Live::new(
        Arc::new(parking_lot::Mutex::new(config)),
        Arc::new(KeyringSecretStore::new()),
    );
    Ok((Arc::new(rt), live))
}

/// Runs a setup command. `None` means the arguments are not a setup command.
pub fn run(args: &[String]) -> Option<Result<String, String>> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    Some(match a.as_slice() {
        ["provider", kind, url, model] => (|| {
            let mut c = load()?;
            c.llm.kind = parse_kind(kind)?;
            c.llm.base_url = if *url == "-" {
                String::new()
            } else {
                (*url).to_string()
            };
            c.llm.model = (*model).to_string();
            save(&c)?;
            Ok(format!(
                "провайдер {} {} {}",
                c.llm.kind.as_str(),
                c.llm.effective_base_url(),
                c.llm.model
            ))
        })(),
        ["key", kind] => (|| {
            let kind = parse_kind(kind)?;
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            let key = Secret::new(line.trim());
            KeyringSecretStore::new()
                .set(&kind.secret_key(), &key)
                .map_err(|e| e.to_string())?;
            Ok(if key.is_empty() {
                format!("ключ {} удалён", kind.as_str())
            } else {
                format!(
                    "ключ {} сохранён в диспетчере учётных данных",
                    kind.as_str()
                )
            })
        })(),
        ["model", what @ ("embedding" | "vision" | "image" | "tts" | "stt" | "tts-voice"), model] => {
            (|| {
                let mut c = load()?;
                let value = if *model == "-" { "" } else { model };
                match *what {
                    "embedding" => c.embedding.model = value.to_string(),
                    "vision" => c.llm.vision_model = value.to_string(),
                    "tts" => {
                        c.voice.tts_model = value.to_string();
                        c.voice.tts = if value.is_empty() {
                            TtsEngine::Windows
                        } else {
                            TtsEngine::Provider
                        };
                    }
                    "stt" => c.voice.stt_model = value.to_string(),
                    "tts-voice" => c.voice.tts_voice = value.to_string(),
                    _ => c.image.model = value.to_string(),
                }
                save(&c)?;
                Ok(format!(
                    "модель {what}: {}",
                    if value.is_empty() {
                        "выключена"
                    } else {
                        value
                    }
                ))
            })()
        }
        ["web", v @ ("on" | "off")] => (|| {
            let mut c = load()?;
            c.tools.web_enabled = *v == "on";
            save(&c)?;
            Ok(format!("интернет-инструменты: {v}"))
        })(),
        ["status"] => (|| {
            let c = load()?;
            let has_key = matches!(
                KeyringSecretStore::new().get(&c.llm.kind.secret_key()),
                Ok(Some(_))
            );
            Ok(format!(
                "config: {}\nпровайдер: {} {} {}\nключ: {}\nинтернет-инструменты: {}",
                config_path().display(),
                c.llm.kind.as_str(),
                c.llm.effective_base_url(),
                c.llm.model,
                if has_key { "есть" } else { "нет" },
                if c.tools.web_enabled { "on" } else { "off" }
            ))
        })(),
        ["voices"] => {
            use mika_audio::Voice;
            let voices = mika_audio::win::WindowsVoice::new().voices();
            Ok(if voices.is_empty() {
                "голосов Windows не найдено".to_string()
            } else {
                voices
                    .iter()
                    .map(|v| format!("{} ({})", v.name, v.language))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        ["say", text] => (|| {
            use mika_audio::Voice;
            let c = load()?;
            let voice = mika_audio::win::WindowsVoice::new();
            voice.configure(&c.voice.voice, c.voice.rate);
            let through_provider =
                c.voice.tts == TtsEngine::Provider && !c.voice.tts_model.trim().is_empty();
            if through_provider {
                let (rt, live) = speech_runtime(c)?;
                voice.set_remote(Some(Arc::new(move |t: &str| {
                    rt.block_on(live.synthesize(t))
                })));
            }
            voice.speak(text);
            // The voice plays on its own thread; give it time before the process exits.
            let seconds =
                2 + text.chars().count() as u64 / 12 + if through_provider { 4 } else { 0 };
            std::thread::sleep(std::time::Duration::from_secs(seconds.min(40)));
            Ok(format!(
                "сказано за {seconds} с, голос: {}",
                if through_provider {
                    "провайдер"
                } else {
                    "Windows"
                }
            ))
        })(),
        ["roundtrip", text] => (|| {
            let (rt, live) = speech_runtime(load()?)?;
            let started = std::time::Instant::now();
            let audio = rt.block_on(live.synthesize(text))?;
            let spoken = started.elapsed();
            let started = std::time::Instant::now();
            let heard = rt.block_on(live.transcribe(&audio))?;
            Ok(format!(
                "синтез: {} байт за {} мс\nраспознано за {} мс: {heard}",
                audio.len(),
                spoken.as_millis(),
                started.elapsed().as_millis()
            ))
        })(),
        ["hear"] => (|| {
            use mika_audio::Microphone;
            let c = load()?;
            if !c.privacy.microphone_enabled {
                return Err("микрофон выключен: включи его командой `mika-cli mic on`".into());
            }
            let (rt, live) = speech_runtime(c.clone())?;
            eprintln!("говори…");
            let input = c.voice.input_device.clone();
            let wav = mika_audio::SystemMicrophone::new(&input)
                .record(&mika_audio::MicControl::default())
                .map_err(|e| e.to_string())?;
            rt.block_on(live.transcribe(&wav))
        })(),
        ["mic", v @ ("on" | "off")] => (|| {
            let mut c = load()?;
            c.privacy.microphone_enabled = *v == "on";
            save(&c)?;
            Ok(format!("микрофон: {v}"))
        })(),
        ["probe"] => (|| {
            let c = load()?;
            let provider = mika_providers::build_provider(&c.llm, &KeyringSecretStore::new())
                .map_err(|e| e.to_string())?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            let d = rt
                .block_on(mika_providers::probe(provider.as_ref()))
                .map_err(|e| e.to_string())?;
            Ok(format!(
                "{} отвечает, первый фрагмент через {} мс",
                c.llm.model,
                d.as_millis()
            ))
        })(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_args_are_not_setup() {
        assert!(run(&["привет".to_string()]).is_none());
        assert!(run(&[]).is_none());
    }

    #[test]
    fn parse_kind_rejects_unknown() {
        assert_eq!(parse_kind("ollama"), Ok(ProviderKind::Ollama));
        assert!(parse_kind("gpt").is_err());
    }
}
