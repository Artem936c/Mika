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
    pub avatar: AvatarConfig,
    pub island: IslandConfig,
    pub image: ImageConfig,
    pub mcp: McpConfig,
    pub telegram: TelegramConfig,
    pub embedding: EmbeddingConfig,
    pub updates: UpdatesConfig,
}

/// Where to look for new versions. Nothing is checked while `manifest_url` is empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatesConfig {
    /// https address of a JSON file: version, installer address, SHA-256, notes.
    pub manifest_url: String,
    /// Look for a new version at start. Download and install only after a click.
    pub check_at_start: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self {
            manifest_url: String::new(),
            check_at_start: true,
        }
    }
}

/// Key of the token for an MCP server reached over HTTP.
pub fn mcp_token_key(server: &str) -> String {
    format!("mcp.{}.token", server.trim())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AssistantConfig {
    pub name: String,
    pub wake_phrase: String,
    pub persona: Persona,
    /// Used when `persona` is `custom`.
    pub custom_prompt: String,
    /// The first-run screen was passed.
    pub onboarded: bool,
}

impl Default for AssistantConfig {
    fn default() -> Self {
        Self {
            name: "Мика".into(),
            wake_phrase: "Мика".into(),
            persona: Persona::default(),
            custom_prompt: String::new(),
            onboarded: false,
        }
    }
}

/// Character preset; its text goes into the system prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Persona {
    #[default]
    Caring,
    Cheerful,
    Calm,
    Bold,
    Custom,
}

/// Longest custom character text that goes into the system prompt, characters.
pub const MAX_CUSTOM_PROMPT: usize = 1000;

impl AssistantConfig {
    /// One paragraph about the character for the system prompt.
    pub fn persona_prompt(&self) -> String {
        let custom = self.custom_prompt.trim();
        match self.persona {
            Persona::Custom if !custom.is_empty() => {
                custom.chars().take(MAX_CUSTOM_PROMPT).collect()
            }
            Persona::Caring | Persona::Custom => "Характер: заботливая и внимательная. \
Замечаешь, когда собеседнику тяжело, и поддерживаешь его."
                .into(),
            Persona::Cheerful => "Характер: весёлая и игривая. \
Любишь лёгкие шутки, но делу они не мешают."
                .into(),
            Persona::Calm => "Характер: спокойная и собранная. \
Говоришь ровно и по делу, без восклицаний."
                .into(),
            Persona::Bold => "Характер: дерзкая и остроумная. \
Можешь по-дружески поддеть, но без грубости, и всегда помогаешь."
                .into(),
        }
    }
}

/// Where the avatar is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AvatarConfig {
    /// Left column of the open chat.
    pub in_island: bool,
    /// Separate click-through window in the corner of the desktop.
    pub on_desktop: bool,
}

impl Default for AvatarConfig {
    fn default() -> Self {
        Self {
            in_island: true,
            on_desktop: false,
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
    /// Where the ordinary window was last: x, y, width, height in physical pixels.
    pub rect: Option<[i32; 4]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    pub speak_replies: bool,
    /// Name of the Windows voice; empty picks a Russian one.
    pub voice: String,
    /// Speech rate of the Windows voice, 0.5..=2.0.
    pub rate: f32,
    /// Who reads replies aloud.
    pub tts: TtsEngine,
    /// Speech model of the provider, for `tts = "provider"`.
    pub tts_model: String,
    /// Voice name the provider model knows, such as `Kore`.
    pub tts_voice: String,
    /// Recognition model of the provider; empty means voice input is off.
    pub stt_model: String,
    /// Language hint for recognition: `ru`.
    pub stt_language: String,
    /// Address for speech requests; empty means the LLM address.
    pub speech_base_url: String,
    /// Name of the recording device; empty means the Windows default.
    pub input_device: String,
    /// Listen for the wake phrase while the microphone is allowed.
    pub wake_enabled: bool,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            speak_replies: true,
            voice: String::new(),
            rate: 1.0,
            tts: TtsEngine::Windows,
            tts_model: String::new(),
            tts_voice: String::new(),
            stt_model: String::new(),
            stt_language: "ru".into(),
            speech_base_url: String::new(),
            input_device: String::new(),
            wake_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsEngine {
    /// The voice built into Windows: local, no network.
    #[default]
    Windows,
    /// The provider's `/audio/speech`.
    Provider,
}

/// Key for speech requests; without one the LLM key is used.
pub const SPEECH_SECRET_KEY: &str = "speech.api_key";

/// What the island shows when idle and when it hides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IslandConfig {
    /// City for the idle weather; empty means no weather.
    pub city: String,
    /// Hide the island while a full-screen program is in front.
    pub hide_in_games: bool,
}

impl Default for IslandConfig {
    fn default() -> Self {
        Self {
            city: String::new(),
            hide_in_games: true,
        }
    }
}

/// Picture provider. The key lives in the secret store under `IMAGE_SECRET_KEY`;
/// without one the LLM key is used.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageConfig {
    /// Empty means the LLM address.
    pub base_url: String,
    /// Empty means pictures are off.
    pub model: String,
}

/// Embeddings for memory search by meaning. The key lives in the secret store
/// under `EMBEDDING_SECRET_KEY`; without one the LLM key is used.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// Empty means the LLM address.
    pub base_url: String,
    /// Empty means search by words.
    pub model: String,
}

pub const EMBEDDING_SECRET_KEY: &str = "embedding.api_key";
pub const IMAGE_SECRET_KEY: &str = "image.api_key";
pub const TELEGRAM_SECRET_KEY: &str = "telegram.bot_token";

/// One MCP server started as a child process.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub name: String,
    /// Program to start for a local server. Empty for a server reached by `url`.
    pub command: String,
    pub args: Vec<String>,
    pub enabled: bool,
    /// Address of a server reached over HTTP. Empty for a local server.
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    pub servers: Vec<McpServerConfig>,
}

/// Commands from the phone. The bot token lives in the secret store.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub enabled: bool,
    /// Numeric id of the only account that may send commands; 0 means nobody.
    pub user_id: i64,
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
    /// Model for questions about a screenshot; empty means `model`.
    pub vision_model: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Openai,
            base_url: String::new(),
            model: "gpt-4o-mini".into(),
            vision_model: String::new(),
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    pub web_enabled: bool,
    /// RSS feed for the news tool.
    pub news_feed: String,
    /// Tool groups the user switched off; their tools are not offered to the model.
    pub disabled_groups: Vec<String>,
}

pub const DEFAULT_NEWS_FEED: &str = "https://lenta.ru/rss/news";

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            web_enabled: false,
            news_feed: DEFAULT_NEWS_FEED.into(),
            disabled_groups: Vec::new(),
        }
    }
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

    #[test]
    fn old_config_without_persona_and_avatar_loads() {
        let (_d, p) = tmp();
        std::fs::write(&p, "[assistant]\nname = \"Айри\"\nwake_phrase = \"Айри\"\n").unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.assistant.persona, Persona::Caring);
        assert_eq!(c.assistant.custom_prompt, "");
        assert!(c.avatar.in_island && !c.avatar.on_desktop);
    }

    #[test]
    fn persona_is_stored_in_snake_case() {
        let (_d, p) = tmp();
        let mut c = Config::default();
        c.assistant.persona = Persona::Bold;
        c.avatar.on_desktop = true;
        c.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("persona = \"bold\""), "{text}");
        assert_eq!(Config::load(&p).unwrap(), c);
    }

    #[test]
    fn new_sections_have_safe_defaults_and_roundtrip() {
        let (_d, p) = tmp();
        let mut c = Config::default();
        assert!(!c.telegram.enabled && c.telegram.user_id == 0);
        assert!(c.mcp.servers.is_empty() && c.image.model.is_empty());
        assert!(c.island.hide_in_games && c.island.city.is_empty());
        assert!(c.embedding.model.is_empty());
        assert_eq!(c.voice.tts, TtsEngine::Windows);
        assert!(c.voice.stt_model.is_empty() && c.voice.stt_language == "ru");
        c.voice.tts = TtsEngine::Provider;
        c.voice.tts_voice = "Kore".into();
        assert_eq!(c.tools.news_feed, DEFAULT_NEWS_FEED);
        c.mcp.servers.push(McpServerConfig {
            name: "files".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "server".into()],
            enabled: true,
            url: String::new(),
        });
        c.telegram.user_id = 42;
        c.island.city = "Пермь".into();
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p).unwrap(), c);
    }

    #[test]
    fn empty_custom_persona_falls_back_to_caring() {
        let mut a = AssistantConfig {
            persona: Persona::Custom,
            ..Default::default()
        };
        assert!(a.persona_prompt().contains("заботливая"));
        a.custom_prompt = "я".repeat(MAX_CUSTOM_PROMPT + 50);
        assert_eq!(a.persona_prompt().chars().count(), MAX_CUSTOM_PROMPT);
    }
}
