//! Settings stored in `config.toml`. Secrets never go here.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub assistant: AssistantConfig,
    pub window: WindowConfig,
    pub voice: VoiceConfig,
    pub privacy: PrivacyConfig,
    pub llm: LlmConfig,
    pub tools: ToolsConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AssistantConfig {
    pub name: String,
    pub wake_phrase: String,
}

impl Default for AssistantConfig {
    fn default() -> Self {
        Self {
            name: "Мика".into(),
            wake_phrase: "Мика".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowMode {
    #[default]
    Pinned,
    Island,
    Window,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub mode: WindowMode,
    pub monitor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    pub speak_replies: bool,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            speak_replies: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig {
    pub microphone_enabled: bool,
    pub screen_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    Openai,
    Anthropic,
    Ollama,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Openai => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Ollama => "ollama",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::Openai => "https://api.openai.com/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com",
            ProviderKind::Ollama => "http://localhost:11434/v1",
        }
    }

    /// Name of the key in the secret store.
    pub fn secret_key(self) -> String {
        format!("llm.{}.api_key", self.as_str())
    }
}

/// LLM provider. The key lives in the secret store, see `ProviderKind::secret_key`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub kind: ProviderKind,
    /// Empty means the default address of the kind.
    pub base_url: String,
    pub model: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Openai,
            base_url: String::new(),
            model: "gpt-4o-mini".into(),
        }
    }
}

impl LlmConfig {
    pub fn effective_base_url(&self) -> String {
        let url = self.base_url.trim();
        let url = if url.is_empty() {
            self.kind.default_base_url()
        } else {
            url
        };
        url.trim_end_matches('/').to_string()
    }
}

/// Tools that reach the network are off until the user turns them on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    pub web_enabled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config parse error in {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("config serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl Config {
    /// A missing file gives defaults. A UTF-8 BOM (Notepad) is ignored.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e.into()),
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Never fails. A broken file is copied to `config.broken.toml` next to it.
    pub fn load_or_default(path: &Path) -> (Config, Option<ConfigError>) {
        match Config::load(path) {
            Ok(c) => (c, None),
            Err(err) => {
                if matches!(err, ConfigError::Parse { .. }) {
                    let backup = path.with_file_name("config.broken.toml");
                    if let Err(e) = std::fs::copy(path, &backup) {
                        log::warn!("cannot back up broken config: {e}");
                    }
                }
                (Config::default(), Some(err))
            }
        }
    }

    /// Writes to `config.toml.tmp` and renames, so a crash mid-write keeps the old file.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_file_name("config.toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        (d, p)
    }

    #[test]
    fn default_has_microphone_and_screen_disabled() {
        let c = Config::default();
        assert!(!c.privacy.microphone_enabled && !c.privacy.screen_enabled);
        assert!(c.voice.speak_replies);
        assert_eq!(c.assistant.name, "Мика");
        assert_eq!(c.assistant.wake_phrase, "Мика");
        assert_eq!(c.window.mode, WindowMode::Pinned);
        assert!(!c.tools.web_enabled);
    }

    #[test]
    fn llm_base_url_defaults_per_kind_and_trims_slash() {
        let mut l = LlmConfig::default();
        assert_eq!(l.effective_base_url(), "https://api.openai.com/v1");
        l.kind = ProviderKind::Ollama;
        assert_eq!(l.effective_base_url(), "http://localhost:11434/v1");
        l.base_url = "http://h:1/v1/ ".into();
        assert_eq!(l.effective_base_url(), "http://h:1/v1");
        assert_eq!(
            ProviderKind::Anthropic.secret_key(),
            "llm.anthropic.api_key"
        );
    }

    #[test]
    fn load_missing_file_returns_default() {
        let (_d, p) = tmp();
        assert_eq!(Config::load(&p).unwrap(), Config::default());
        assert!(!p.exists());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let (_d, p) = tmp();
        let mut c = Config::default();
        c.assistant.name = "Айри".into();
        c.window.mode = WindowMode::Window;
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p).unwrap(), c);
    }

    #[test]
    fn save_creates_parent_dir_and_leaves_no_tmp_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("nested").join("config.toml");
        Config::default().save(&p).unwrap();
        assert!(p.exists());
        assert!(!d.path().join("nested").join("config.toml.tmp").exists());
    }

    #[test]
    fn partial_file_fills_defaults() {
        let (_d, p) = tmp();
        std::fs::write(&p, "[assistant]\nname = \"Айри\"\n").unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.assistant.name, "Айри");
        assert_eq!(c.assistant.wake_phrase, "Мика");
        assert!(c.voice.speak_replies);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let (_d, p) = tmp();
        std::fs::write(
            &p,
            "future_key = 1\n[window]\nmode = \"island\"\nextra = true\n",
        )
        .unwrap();
        assert_eq!(Config::load(&p).unwrap().window.mode, WindowMode::Island);
    }

    #[test]
    fn load_strips_utf8_bom() {
        let (_d, p) = tmp();
        std::fs::write(&p, "\u{feff}[assistant]\nname = \"Айри\"\n").unwrap();
        assert_eq!(Config::load(&p).unwrap().assistant.name, "Айри");
    }

    #[test]
    fn broken_file_returns_parse_error() {
        let (_d, p) = tmp();
        std::fs::write(&p, "[assistant\nname = ").unwrap();
        assert!(matches!(Config::load(&p), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn load_or_default_keeps_broken_file_as_backup() {
        let (d, p) = tmp();
        std::fs::write(&p, "[assistant\nname = ").unwrap();
        let (c, err) = Config::load_or_default(&p);
        assert_eq!(c, Config::default());
        assert!(matches!(err, Some(ConfigError::Parse { .. })));
        assert_eq!(
            std::fs::read_to_string(d.path().join("config.broken.toml")).unwrap(),
            "[assistant\nname = "
        );
    }
}
