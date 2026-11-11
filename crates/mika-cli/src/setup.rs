//! One-shot setup commands: provider, key, web tools.
//!
//! `mika-cli provider <openai|anthropic|ollama> <base_url|-> <model>`
//! `mika-cli key <openai|anthropic|ollama>`   reads the key from stdin, stores it in Credential Manager
//! `mika-cli web <on|off>`
//! `mika-cli status`
//! `mika-cli probe`                         time to the first answer chunk

use std::io::BufRead;

use mika_core::config::{Config, ProviderKind};
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
