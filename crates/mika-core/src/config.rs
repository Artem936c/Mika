//! Settings stored in `config.toml`. Secrets never go here.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub assistant: AssistantConfig,
    pub i18n: crate::i18n::I18nConfig,
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
    pub notifications: NotificationsConfig,
    pub events: EventsConfig,
    pub briefing: BriefingConfig,
    pub rhythm: RhythmConfig,
    pub costs: crate::costs::CostsConfig,
    pub tracking: TrackingConfig,
    pub calendar: CalendarConfig,
    pub hotkeys: HotkeysConfig,
    pub dictation: DictationConfig,
    pub screen_ask: ScreenAskConfig,
    pub docs: DocsConfig,
    pub research: ResearchConfig,
    pub log: LogConfig,
}

/// The journal (decision 0036): what is written and how long it is kept.
/// Applied at once, without a restart (`mika_core::journal::apply`).
///
/// The section is read leniently, see `LogConfig::deserialize`: it is the
/// one a user is sent to edit by hand, and a slip in it must not cost the
/// rest of the settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogConfig {
    /// The least severe level that is written: `error`, `warn`, `info`, `debug`.
    pub level: crate::journal::Level,
    /// Write texts of the user and the model and arguments of tools. Off:
    /// only names, sizes, durations and errors are written.
    pub content: bool,
    /// Earlier files older than this many days are removed.
    pub keep_days: u32,
    /// How many earlier files are kept besides the current one.
    pub keep_files: u32,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: crate::journal::Level::Info,
            content: false,
            keep_days: 14,
            keep_files: 5,
        }
    }
}

pub const LOG_KEEP_DAYS: (u32, u32) = (1, 365);
pub const LOG_KEEP_FILES: (u32, u32) = (1, 50);

/// A value of `[log]` as it stands in the file, whatever its kind.
#[derive(Deserialize)]
#[serde(untagged)]
enum Loose {
    Flag(bool),
    Whole(i64),
    Part(f64),
    Text(String),
    Other(serde::de::IgnoredAny),
}

impl Loose {
    /// The value for a record about it: a few plain characters.
    fn brief(&self) -> String {
        let all = match self {
            Loose::Flag(flag) => flag.to_string(),
            Loose::Whole(number) => number.to_string(),
            Loose::Part(number) => number.to_string(),
            Loose::Text(written) => written.clone(),
            Loose::Other(_) => "not a simple value".to_string(),
        };
        all.chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '.' | '_'))
            .take(24)
            .collect()
    }

    fn level(&self) -> Option<crate::journal::Level> {
        use crate::journal::Level;
        let Loose::Text(written) = self else {
            return None;
        };
        match written.trim().to_lowercase().as_str() {
            "warning" => Some(Level::Warn),
            "trace" => Some(Level::Debug),
            name => Level::parse(name),
        }
    }

    fn flag(&self) -> Option<bool> {
        match self {
            Loose::Flag(flag) => Some(*flag),
            Loose::Text(written) => written.trim().to_lowercase().parse().ok(),
            _ => None,
        }
    }

    /// A count that is not below zero. One past `u32` is the largest: the
    /// bounds of the field cut it down.
    fn count(&self) -> Option<u32> {
        let number = match self {
            Loose::Whole(number) => *number,
            Loose::Part(number) if number.is_finite() => number.round() as i64,
            Loose::Text(written) => written.trim().parse().ok()?,
            _ => return None,
        };
        (number >= 0).then(|| u32::try_from(number).unwrap_or(u32::MAX))
    }
}

/// `[log]` is read value by value. A value that cannot be understood (a
/// level in another letter case is understood) gives way to the default of
/// its field, with a warning; the other sections of the file are not
/// touched by it. Strict reading would fail the whole file, and the app
/// would start with the defaults of everything.
impl<'de> Deserialize<'de> for LogConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Raw {
            level: Option<Loose>,
            content: Option<Loose>,
            keep_days: Option<Loose>,
            keep_files: Option<Loose>,
        }

        fn read<T>(
            field: &str,
            given: Option<Loose>,
            by: fn(&Loose) -> Option<T>,
            default: T,
        ) -> T {
            let Some(given) = given else {
                return default;
            };
            by(&given).unwrap_or_else(|| {
                log::warn!(
                    "settings: a value of [log] is not understood, the default is used: field={field} given={}",
                    given.brief()
                );
                default
            })
        }

        let raw = Raw::deserialize(deserializer)?;
        let default = LogConfig::default();
        Ok(LogConfig {
            level: read("level", raw.level, Loose::level, default.level),
            content: read("content", raw.content, Loose::flag, default.content),
            keep_days: read("keep_days", raw.keep_days, Loose::count, default.keep_days),
            keep_files: read(
                "keep_files",
                raw.keep_files,
                Loose::count,
                default.keep_files,
            ),
        })
    }
}

impl LogConfig {
    /// The same settings with the retention inside its bounds.
    pub fn normalized(&self) -> LogConfig {
        LogConfig {
            keep_days: self.keep_days.clamp(LOG_KEEP_DAYS.0, LOG_KEEP_DAYS.1),
            keep_files: self.keep_files.clamp(LOG_KEEP_FILES.0, LOG_KEEP_FILES.1),
            ..self.clone()
        }
    }
}

/// Breaks and her small lines of her own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RhythmConfig {
    /// Suggest a break after long work without a pause.
    pub breaks: bool,
    /// Minutes of work after which a break is suggested.
    pub break_every_min: u32,
    /// Greetings, «уже поздно», joy at a finished task. Made without the model.
    pub lively: bool,
}

impl Default for RhythmConfig {
    fn default() -> Self {
        Self {
            breaks: false,
            break_every_min: 50,
            lively: true,
        }
    }
}

/// Scenarios by event (decision 0031).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EventsConfig {
    /// Scenarios run when their event happens. Off pauses all of them.
    pub enabled: bool,
    /// Most scenario runs in any hour, all scenarios together. 1..=60.
    pub max_per_hour: u32,
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_per_hour: 12,
        }
    }
}

/// Things she keeps track of on this computer only. Off until the user turns them on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackingConfig {
    /// Time in each program: the process name of the window in front.
    pub app_time: bool,
    /// The last copied texts, in memory until she exits.
    pub clipboard: bool,
}

/// Meetings from a calendar published as an ICS link. The link is a secret:
/// it lives in the secret store under `CALENDAR_SECRET_KEY`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarConfig {
    pub enabled: bool,
    /// Minutes before a meeting when she reminds of it; 0 means no reminders.
    pub remind_min: u32,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            remind_min: 10,
        }
    }
}

pub const CALENDAR_SECRET_KEY: &str = "calendar.ics_url";

/// Folders whose documents she reads to answer questions about them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DocsConfig {
    pub folders: Vec<String>,
}

/// Deep search: where reports go, which model reads the pages, limits of one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResearchConfig {
    /// Folder for reports; empty means `Documents\Mika\Research` in the user profile.
    pub folder: String,
    /// Model for the inner requests; empty means the chat model.
    pub model: String,
    /// Search requests in one run.
    pub max_searches: u32,
    /// Pages whose text goes to the model in one run.
    pub max_pages: u32,
    /// Minutes one run may take.
    pub max_minutes: u32,
    /// Tokens of all inner model requests of one run, estimated.
    pub max_tokens: u32,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self {
            folder: String::new(),
            model: String::new(),
            max_searches: 5,
            max_pages: 8,
            max_minutes: 5,
            max_tokens: 50_000,
        }
    }
}

pub const RESEARCH_SEARCHES: (u32, u32) = (2, 10);
pub const RESEARCH_PAGES: (u32, u32) = (2, 16);
pub const RESEARCH_MINUTES: (u32, u32) = (2, 15);
pub const RESEARCH_TOKENS: (u32, u32) = (20_000, 200_000);
/// Above this many tokens a run is high risk: asked every time.
pub const RESEARCH_HIGH_TOKENS: u32 = 100_000;

/// Limits of one deep search, inside their bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResearchLimits {
    pub searches: u32,
    pub pages: u32,
    pub minutes: u32,
    pub tokens: u32,
}

impl ResearchConfig {
    /// The limits of one run, each inside its bounds.
    pub fn limits(&self) -> ResearchLimits {
        let within = |value: u32, (min, max): (u32, u32)| value.clamp(min, max);
        ResearchLimits {
            searches: within(self.max_searches, RESEARCH_SEARCHES),
            pages: within(self.max_pages, RESEARCH_PAGES),
            minutes: within(self.max_minutes, RESEARCH_MINUTES),
            tokens: within(self.max_tokens, RESEARCH_TOKENS),
        }
    }
}

impl ResearchLimits {
    /// Most paid model requests of one run: the plan, one per page, the report.
    pub fn llm_calls(&self) -> u32 {
        self.pages + 2
    }

    /// Whether a run with these limits is asked every time.
    pub fn high(&self) -> bool {
        self.tokens > RESEARCH_HIGH_TOKENS
    }
}

/// Notifications of other programs. Off until the user turns them on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationsConfig {
    pub enabled: bool,
    /// Only messengers and mail; otherwise every program.
    pub only_messengers: bool,
    /// Say each one aloud when replies are spoken.
    pub read_aloud: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            only_messengers: true,
            read_aloud: true,
        }
    }
}

/// The morning summary: weather, the day's plans, rates, news.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BriefingConfig {
    pub enabled: bool,
    /// Local time `HH:MM` after which the summary comes, once a day.
    pub time: String,
    /// Day of the last summary, `YYYY-MM-DD`.
    pub last_date: String,
    /// The evening summary of the day (decision 0028).
    pub evening_enabled: bool,
    /// Local time `HH:MM` after which the evening summary comes, once a day.
    pub evening_time: String,
    /// Day of the last evening summary, `YYYY-MM-DD`.
    pub evening_last_date: String,
}

impl Default for BriefingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            time: "08:00".into(),
            last_date: String::new(),
            evening_enabled: false,
            evening_time: "21:00".into(),
            evening_last_date: String::new(),
        }
    }
}

impl BriefingConfig {
    /// Whether the summary is due at `now`: on, past the time, not yet today.
    pub fn due(&self, now: chrono::NaiveDateTime) -> bool {
        let Ok(at) = chrono::NaiveTime::parse_from_str(self.time.trim(), "%H:%M") else {
            return false;
        };
        self.enabled
            && now.time() >= at
            && self.last_date != now.date().format("%Y-%m-%d").to_string()
    }

    /// Whether the evening summary is due at `now`: the same rule, on its own fields.
    pub fn evening_due(&self, now: chrono::NaiveDateTime) -> bool {
        let Ok(at) = chrono::NaiveTime::parse_from_str(self.evening_time.trim(), "%H:%M") else {
            return false;
        };
        self.evening_enabled
            && now.time() >= at
            && self.evening_last_date != now.date().format("%Y-%m-%d").to_string()
    }
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

/// Where the avatar is shown, which one and how it looks (decision 0040).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AvatarConfig {
    /// Left column of the open chat.
    pub in_island: bool,
    /// Separate click-through window in the corner of the desktop.
    pub on_desktop: bool,
    /// Id of the chosen model in the models folder; empty is the built-in avatar.
    pub model: String,
    /// Rest pose of a model: one of `AVATAR_POSES`.
    pub pose: String,
    /// Size of the desktop avatar window, `MIN_AVATAR_SCALE..=MAX_AVATAR_SCALE`.
    pub scale: f32,
    /// What part of a model the chat column shows: one of `AVATAR_FRAMES`.
    pub frame: String,
    /// A model looks at the cursor.
    pub follow_cursor: bool,
    /// A model does something now and then when nothing goes on.
    pub idle_actions: bool,
    /// Top-left corner of the desktop avatar window after the user moved it,
    /// physical pixels; none: the corner of the screen.
    pub position: Option<(i32, i32)>,
}

impl Default for AvatarConfig {
    fn default() -> Self {
        Self {
            in_island: true,
            on_desktop: false,
            model: String::new(),
            pose: "relaxed".into(),
            scale: 1.0,
            frame: "full".into(),
            follow_cursor: true,
            idle_actions: true,
            position: None,
        }
    }
}

/// Rest poses of a model; `auto` changes them now and then.
pub const AVATAR_POSES: [&str; 6] = ["relaxed", "clasped", "behind", "hip", "crossed", "auto"];
/// Frames of a model in the chat column.
pub const AVATAR_FRAMES: [&str; 3] = ["full", "waist", "close"];
pub const MIN_AVATAR_SCALE: f32 = 0.6;
pub const MAX_AVATAR_SCALE: f32 = 1.6;

/// A size of the desktop avatar within bounds; anything else is the usual size.
pub fn clamp_avatar_scale(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(MIN_AVATAR_SCALE, MAX_AVATAR_SCALE)
    } else {
        1.0
    }
}

/// How a model looks and moves, as the «Персонаж» page sets it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvatarLook {
    pub pose: String,
    pub scale: f32,
    pub frame: String,
    pub follow_cursor: bool,
    pub idle_actions: bool,
}

impl AvatarConfig {
    pub fn look(&self) -> AvatarLook {
        let known = |name: &str, names: &[&str], fallback: &str| {
            if names.contains(&name) {
                name.to_string()
            } else {
                fallback.to_string()
            }
        };
        AvatarLook {
            pose: known(&self.pose, &AVATAR_POSES, "relaxed"),
            scale: clamp_avatar_scale(self.scale),
            frame: known(&self.frame, &AVATAR_FRAMES, "full"),
            follow_cursor: self.follow_cursor,
            idle_actions: self.idle_actions,
        }
    }

    /// Takes a look from the page; an unknown name or a size out of bounds is put right.
    pub fn set_look(&mut self, look: AvatarLook) {
        self.pose = look.pose;
        self.scale = look.scale;
        self.frame = look.frame;
        self.follow_cursor = look.follow_cursor;
        self.idle_actions = look.idle_actions;
        let fixed = self.look();
        self.pose = fixed.pose;
        self.scale = fixed.scale;
        self.frame = fixed.frame;
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
    /// Name of the Windows voice; empty picks one for the language she replies in.
    pub voice: String,
    /// Speech rate of the Windows voice, 0.5..=2.0.
    pub rate: f32,
    /// Who reads replies aloud.
    pub tts: TtsEngine,
    /// Speech model of the provider, for `tts = "provider"`.
    pub tts_model: String,
    /// Voice name the provider model knows, such as `Kore`.
    pub tts_voice: String,
    /// Keep the sound of provider phrases on disk, so a repeated phrase is not paid twice.
    pub tts_cache: bool,
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
    /// After an answer to a spoken question, listen again for a few seconds.
    pub follow_up: bool,
    /// Other programs get quieter while she listens and speaks.
    pub duck_media: bool,
    /// The voice in words for the provider: age, timbre, manner. Sent as
    /// `instructions`, which `gpt-4o-mini-tts` understands.
    pub tts_style: String,
    /// Her tone follows her emotion: pitch and pace of the Windows voice,
    /// a line about the mood for the provider.
    pub tone_by_mood: bool,
    /// A long reply is not read out: she says a short retelling of it.
    pub brief_long: bool,
    /// Fish Audio Drama 3 marks up replies for the voice (decision 0026).
    pub tts_expressive: bool,
    /// A phrase without voice marks of its own gets a direction by her mood (decision 0029).
    pub mood_directions: bool,
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
            tts_cache: true,
            stt_model: String::new(),
            stt_language: "ru".into(),
            speech_base_url: String::new(),
            input_device: String::new(),
            wake_enabled: false,
            follow_up: true,
            duck_media: true,
            tts_style: String::new(),
            tone_by_mood: true,
            brief_long: true,
            tts_expressive: false,
            mood_directions: false,
        }
    }
}

impl VoiceConfig {
    /// Replies are marked up for the voice: the switch is on, replies are
    /// spoken, the provider speaks them and its model performs the marks.
    pub fn expressive(&self) -> bool {
        self.performs_marks() && self.speak_replies
    }

    /// The voice performs marks: the switch of expressive speech is on, the
    /// provider speaks and its model performs them. Whether replies are
    /// spoken on this computer does not matter here.
    pub fn performs_marks(&self) -> bool {
        self.tts_expressive && self.tts == TtsEngine::Provider && expressive_model(&self.tts_model)
    }

    /// The provider speaks and has a model: it can make a voice message.
    pub fn provider_speaks(&self) -> bool {
        self.tts == TtsEngine::Provider && !self.tts_model.trim().is_empty()
    }

    /// The direction a phrase without marks of its own gets for this mood;
    /// `None` when the switch is off, the voice performs no marks, or she is calm.
    pub fn mood_direction(&self, mood: crate::emotion::Emotion) -> Option<&'static str> {
        if self.mood_directions && self.performs_marks() {
            mood.direction()
        } else {
            None
        }
    }
}

/// Whether a speech model performs voice marks, like `fish-audio/drama-3`.
pub fn expressive_model(model: &str) -> bool {
    model.to_lowercase().contains("drama")
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
    /// Voice messages are recognised and run as commands (decision 0029).
    pub voice_in: bool,
    /// A voice message is answered with a voice message too. Works only with `voice_in`.
    pub voice_reply: bool,
}

impl TelegramConfig {
    /// A voice message is answered by voice: both switches are on.
    pub fn voice_replies(&self) -> bool {
        self.voice_in && self.voice_reply
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
    /// ffmpeg.exe or a folder with it; empty = search PATH, then the downloaded copy.
    pub ffmpeg_path: String,
}

pub const DEFAULT_NEWS_FEED: &str = "https://lenta.ru/rss/news";

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            web_enabled: false,
            news_feed: DEFAULT_NEWS_FEED.into(),
            disabled_groups: Vec::new(),
            ffmpeg_path: String::new(),
        }
    }
}

/// Global hotkeys the user chose: action id -> combination as `Ctrl+Alt+D`.
/// An action without an entry uses its defaults (`mika_core::hotkeys::HOTKEYS`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeysConfig {
    pub custom: std::collections::BTreeMap<String, String>,
}

/// Dictation into the window in front. Off until the user turns it on;
/// also needs `privacy.microphone_enabled`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DictationConfig {
    pub enabled: bool,
}

/// A question about a picture of the window in front. Off until the user turns it on;
/// also needs `privacy.screen_enabled`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenAskConfig {
    pub enabled: bool,
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

    /// Names of the sections that differ from `before`, sorted. For the
    /// journal: a record of a save names what changed and never a value.
    pub fn changed_sections(&self, before: &Config) -> Vec<String> {
        let sections = |config: &Config| match serde_json::to_value(config) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        let (now, before) = (sections(self), sections(before));
        let mut changed: Vec<String> = now
            .iter()
            .filter(|(name, value)| before.get(*name) != Some(*value))
            .map(|(name, _)| name.clone())
            .collect();
        changed.sort();
        changed
    }

    /// Names of the switches that are on, for the first records of a run.
    /// Names only: no address, model, phrase or other value of the settings.
    pub fn switches_on(&self) -> Vec<&'static str> {
        let switches = [
            ("web", self.tools.web_enabled),
            ("screen", self.privacy.screen_enabled),
            ("microphone", self.privacy.microphone_enabled),
            ("speak_replies", self.voice.speak_replies),
            ("provider_voice", self.voice.provider_speaks()),
            ("wake_phrase", self.voice.wake_enabled),
            ("dictation", self.dictation.enabled),
            ("screen_ask", self.screen_ask.enabled),
            ("telegram", self.telegram.enabled),
            ("telegram_voice", self.telegram.voice_in),
            ("telegram_voice_reply", self.telegram.voice_replies()),
            ("mcp", self.mcp.servers.iter().any(|s| s.enabled)),
            ("scenarios", self.events.enabled),
            ("notifications", self.notifications.enabled),
            ("briefing", self.briefing.enabled),
            ("evening_summary", self.briefing.evening_enabled),
            ("calendar", self.calendar.enabled),
            ("costs", self.costs.enabled),
            ("app_time", self.tracking.app_time),
            ("clipboard_history", self.tracking.clipboard),
            ("documents", !self.docs.folders.is_empty()),
            ("embeddings", !self.embedding.model.trim().is_empty()),
            ("pictures", !self.image.model.trim().is_empty()),
            ("update_check", self.updates.check_at_start),
            ("journal_texts", self.log.content),
        ];
        switches
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(name, _)| name)
            .collect()
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
    fn a_save_is_told_by_the_names_of_its_sections() {
        let before = Config::default();
        assert!(before.changed_sections(&before).is_empty());
        let mut now = before.clone();
        now.privacy.microphone_enabled = true;
        now.assistant.name = "Тайное имя".into();
        now.log.keep_days = 3;
        let changed = now.changed_sections(&before);
        assert_eq!(changed, vec!["assistant", "log", "privacy"]);
        // Names of sections, never what is in them.
        assert!(!changed.join(" ").contains("Тайное"));
    }

    #[test]
    fn the_switches_of_a_run_are_names_without_values() {
        let mut c = Config::default();
        let off = c.switches_on();
        assert!(!off.contains(&"microphone") && !off.contains(&"screen"));
        assert!(!off.contains(&"journal_texts"));
        c.privacy.microphone_enabled = true;
        c.tools.web_enabled = true;
        c.docs.folders = vec!["C:\\Users\\secret\\Documents".into()];
        c.embedding.model = "embed-model".into();
        let on = c.switches_on();
        for name in ["microphone", "web", "documents", "embeddings"] {
            assert!(on.contains(&name), "{name}");
        }
        let text = on.join(",");
        assert!(!text.contains("secret") && !text.contains("embed-model"));
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
    fn an_old_config_without_avatar_model_loads() {
        let (_d, p) = tmp();
        std::fs::write(&p, "[avatar]\nin_island = false\non_desktop = true\n").unwrap();
        let mut c = Config::load(&p).unwrap();
        assert_eq!(c.avatar.model, "");
        assert!(c.avatar.on_desktop);

        c.avatar.model = "0123456789abcdef".into();
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p).unwrap().avatar.model, "0123456789abcdef");
    }

    #[test]
    fn an_old_config_gets_the_look_of_the_avatar_by_default() {
        let (_d, p) = tmp();
        std::fs::write(
            &p,
            "[avatar]
in_island = true
model = \"\"
",
        )
        .unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.avatar.pose, "relaxed");
        assert_eq!(c.avatar.scale, 1.0);
        assert_eq!(c.avatar.frame, "full");
        assert!(c.avatar.follow_cursor && c.avatar.idle_actions);
        assert_eq!(c.avatar.position, None);
    }

    #[test]
    fn the_scale_of_the_avatar_stays_in_bounds() {
        assert_eq!(clamp_avatar_scale(0.1), 0.6);
        assert_eq!(clamp_avatar_scale(9.0), 1.6);
        assert_eq!(clamp_avatar_scale(f32::NAN), 1.0);
        assert_eq!(clamp_avatar_scale(1.25), 1.25);
    }

    #[test]
    fn a_look_with_unknown_names_gets_the_defaults() {
        let mut avatar = AvatarConfig::default();
        avatar.set_look(AvatarLook {
            pose: "dance".into(),
            scale: 3.0,
            frame: "feet".into(),
            follow_cursor: false,
            idle_actions: false,
        });
        let look = avatar.look();
        assert_eq!(look.pose, "relaxed");
        assert_eq!(look.scale, 1.6);
        assert_eq!(look.frame, "full");
        assert!(!look.follow_cursor && !look.idle_actions);
        avatar.set_look(AvatarLook {
            pose: "auto".into(),
            scale: 0.8,
            frame: "close".into(),
            follow_cursor: true,
            idle_actions: true,
        });
        assert_eq!(avatar.look().pose, "auto");
        assert_eq!(avatar.look().frame, "close");
    }

    #[test]
    fn the_place_of_the_avatar_window_is_kept() {
        let (_d, p) = tmp();
        let mut c = Config::default();
        c.avatar.position = Some((1200, -40));
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p).unwrap().avatar.position, Some((1200, -40)));
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

    #[test]
    fn briefing_comes_once_a_day_after_its_time() {
        let at = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
        let mut b = BriefingConfig {
            enabled: true,
            time: "08:30".into(),
            last_date: "2026-11-12".into(),
            ..Default::default()
        };
        assert!(!b.due(at("2026-11-13 08:00")));
        assert!(b.due(at("2026-11-13 11:00")));
        b.last_date = "2026-11-13".into();
        assert!(!b.due(at("2026-11-13 11:00")));
        b.last_date.clear();
        b.time = "утро".into();
        assert!(!b.due(at("2026-11-13 11:00")));
        assert!(!NotificationsConfig::default().enabled);
        assert!(!BriefingConfig::default().enabled);
    }

    #[test]
    fn evening_summary_and_costs_have_defaults_and_roundtrip() {
        use crate::costs::ManualPrice;
        let at = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
        let mut c = Config::default();
        assert!(c.costs.enabled && c.costs.day_limit == 0.0 && c.costs.month_limit == 0.0);
        assert!(c.costs.currency.is_empty() && c.costs.prices.is_empty());
        assert!(!c.briefing.evening_enabled);
        assert_eq!(c.briefing.evening_time, "21:00");
        assert!(c.briefing.evening_last_date.is_empty());

        // The evening summary keeps its own time and day, apart from the morning one.
        assert!(!c.briefing.evening_due(at("2026-11-14 22:00")), "off");
        c.briefing.evening_enabled = true;
        assert!(!c.briefing.evening_due(at("2026-11-14 20:59")));
        assert!(c.briefing.evening_due(at("2026-11-14 21:00")));
        assert!(!c.briefing.due(at("2026-11-14 21:00")));
        c.briefing.evening_last_date = "2026-11-14".into();
        assert!(!c.briefing.evening_due(at("2026-11-14 23:00")));
        assert!(c.briefing.evening_due(at("2026-11-15 21:30")));
        c.briefing.evening_time = "вечер".into();
        assert!(!c.briefing.evening_due(at("2026-11-15 21:30")));
        c.briefing.evening_time = "20:30".into();

        // An old file without the new fields loads with the defaults.
        let (_d, p) = tmp();
        std::fs::write(
            &p,
            "[briefing]\nenabled = true\ntime = \"07:00\"\nlast_date = \"2026-11-13\"\n",
        )
        .unwrap();
        let old = Config::load(&p).unwrap();
        assert!(old.briefing.enabled && !old.briefing.evening_enabled);
        assert_eq!(old.briefing.evening_time, "21:00");
        assert_eq!(old.costs, crate::costs::CostsConfig::default());

        c.costs.day_limit = 100.0;
        c.costs.month_limit = 2000.0;
        c.costs.currency = "RUB".into();
        c.costs.prices = vec![
            ManualPrice {
                provider: "api.openai.com".into(),
                model: "gpt-4o-mini".into(),
                currency: "USD".into(),
                input_mtok: 0.15,
                output_mtok: 0.6,
                ..Default::default()
            },
            ManualPrice {
                provider: "api.openai.com".into(),
                model: "whisper-1".into(),
                currency: "USD".into(),
                per_minute: 0.006,
                ..Default::default()
            },
        ];
        c.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("[[costs.prices]]"), "{text}");
        assert!(text.contains("evening_time = \"20:30\""), "{text}");
        assert_eq!(Config::load(&p).unwrap(), c);
    }

    #[test]
    fn rhythm_tracking_calendar_and_docs_start_safe() {
        let c = Config::default();
        assert!(!c.rhythm.breaks && c.rhythm.lively && c.rhythm.break_every_min == 50);
        assert!(!c.tracking.app_time && !c.tracking.clipboard);
        assert!(!c.calendar.enabled && c.calendar.remind_min == 10);
        assert!(c.docs.folders.is_empty());
        assert!(c.voice.tone_by_mood && c.voice.tts_style.is_empty());
        assert!(c.voice.brief_long);
        let (_d, p) = tmp();
        std::fs::write(&p, "[voice]\nspeak_replies = false\n").unwrap();
        let old = Config::load(&p).unwrap();
        assert!(old.voice.tone_by_mood && !old.calendar.enabled);
    }

    #[test]
    fn expressive_speech_starts_off_and_old_files_load_without_it() {
        assert!(!VoiceConfig::default().tts_expressive);
        assert!(!VoiceConfig::default().expressive());
        let (_d, p) = tmp();
        std::fs::write(
            &p,
            "[voice]\ntts = \"provider\"\ntts_model = \"fish-audio/drama-3\"\n",
        )
        .unwrap();
        let old = Config::load(&p).unwrap();
        assert!(!old.voice.tts_expressive && !old.voice.expressive());
        let mut c = old.clone();
        c.voice.tts_expressive = true;
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p).unwrap(), c);
    }

    #[test]
    fn expressive_speech_needs_every_condition() {
        assert!(expressive_model("fish-audio/drama-3"));
        assert!(expressive_model("Drama-3"));
        assert!(!expressive_model("fish-audio/s2-pro"));
        assert!(!expressive_model(""));
        let on = VoiceConfig {
            tts_expressive: true,
            speak_replies: true,
            tts: TtsEngine::Provider,
            tts_model: "fish-audio/DRAMA-3".into(),
            ..Default::default()
        };
        assert!(on.expressive());
        let off = [
            VoiceConfig {
                tts_expressive: false,
                ..on.clone()
            },
            VoiceConfig {
                speak_replies: false,
                ..on.clone()
            },
            VoiceConfig {
                tts: TtsEngine::Windows,
                ..on.clone()
            },
            VoiceConfig {
                tts_model: "fish-audio/s2-pro".into(),
                ..on.clone()
            },
        ];
        for v in off {
            assert!(!v.expressive(), "{v:?}");
        }
    }

    #[test]
    fn telegram_voice_and_mood_directions_start_off_and_old_files_load_without_them() {
        let c = Config::default();
        assert!(!c.telegram.voice_in && !c.telegram.voice_reply);
        assert!(!c.telegram.voice_replies());
        assert!(!c.voice.mood_directions);
        let (_d, p) = tmp();
        std::fs::write(
            &p,
            "[telegram]\nenabled = true\nuser_id = 42\n[voice]\ntts_expressive = true\n",
        )
        .unwrap();
        let old = Config::load(&p).unwrap();
        assert!(old.telegram.enabled && old.telegram.user_id == 42);
        assert!(!old.telegram.voice_in && !old.telegram.voice_reply);
        assert!(!old.voice.mood_directions);
        let mut c = old.clone();
        c.telegram.voice_in = true;
        c.telegram.voice_reply = true;
        c.voice.mood_directions = true;
        c.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("voice_in = true"), "{text}");
        assert!(text.contains("voice_reply = true"), "{text}");
        assert!(text.contains("mood_directions = true"), "{text}");
        assert_eq!(Config::load(&p).unwrap(), c);
    }

    #[test]
    fn a_voice_reply_in_telegram_needs_both_switches() {
        let mut t = TelegramConfig {
            voice_reply: true,
            ..Default::default()
        };
        // The second switch alone does nothing: it works only with the first.
        assert!(!t.voice_replies());
        t.voice_in = true;
        assert!(t.voice_replies());
        t.voice_reply = false;
        assert!(!t.voice_replies());
    }

    #[test]
    fn a_voice_performs_marks_whether_or_not_replies_are_spoken_here() {
        let drama = VoiceConfig {
            tts_expressive: true,
            speak_replies: false,
            tts: TtsEngine::Provider,
            tts_model: "fish-audio/drama-3".into(),
            ..Default::default()
        };
        assert!(drama.performs_marks() && !drama.expressive());
        assert!(drama.provider_speaks());
        let off = [
            VoiceConfig {
                tts_expressive: false,
                ..drama.clone()
            },
            VoiceConfig {
                tts: TtsEngine::Windows,
                ..drama.clone()
            },
            VoiceConfig {
                tts_model: "fish-audio/s2-pro".into(),
                ..drama.clone()
            },
        ];
        for v in off {
            assert!(!v.performs_marks(), "{v:?}");
        }
        // A provider voice needs a model to make a voice message.
        assert!(!VoiceConfig::default().provider_speaks());
        let no_model = VoiceConfig {
            tts: TtsEngine::Provider,
            tts_model: "  ".into(),
            ..Default::default()
        };
        assert!(!no_model.provider_speaks());
        let other = VoiceConfig {
            tts: TtsEngine::Provider,
            tts_model: "google/gemini-tts".into(),
            ..Default::default()
        };
        assert!(other.provider_speaks() && !other.performs_marks());
    }

    #[test]
    fn a_mood_direction_needs_the_switch_a_voice_with_marks_and_a_mood() {
        use crate::emotion::Emotion;
        let on = VoiceConfig {
            mood_directions: true,
            tts_expressive: true,
            // Replies need not be spoken on this computer: Telegram hears them.
            speak_replies: false,
            tts: TtsEngine::Provider,
            tts_model: "fish-audio/drama-3".into(),
            ..Default::default()
        };
        assert_eq!(on.mood_direction(Emotion::Sad), Some("sad, quietly"));
        assert_eq!(on.mood_direction(Emotion::Neutral), None);
        assert_eq!(on.mood_direction(Emotion::Think), None);
        let off = [
            VoiceConfig {
                mood_directions: false,
                ..on.clone()
            },
            VoiceConfig {
                tts_expressive: false,
                ..on.clone()
            },
            VoiceConfig {
                tts: TtsEngine::Windows,
                ..on.clone()
            },
            VoiceConfig {
                tts_model: "fish-audio/s2-pro".into(),
                ..on.clone()
            },
        ];
        for v in off {
            assert_eq!(v.mood_direction(Emotion::Sad), None, "{v:?}");
        }
    }

    #[test]
    fn hotkeys_dictation_and_screen_ask_start_off_and_roundtrip() {
        let mut c = Config::default();
        assert!(c.hotkeys.custom.is_empty());
        assert!(!c.dictation.enabled && !c.screen_ask.enabled);
        let (_d, p) = tmp();
        c.hotkeys
            .custom
            .insert("dictation".into(), "Ctrl+Alt+J".into());
        c.dictation.enabled = true;
        c.screen_ask.enabled = true;
        c.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("[hotkeys.custom]"), "{text}");
        assert!(text.contains("dictation = \"Ctrl+Alt+J\""), "{text}");
        assert!(text.contains("[dictation]") && text.contains("[screen_ask]"));
        assert_eq!(Config::load(&p).unwrap(), c);
    }

    #[test]
    fn file_without_hotkey_sections_loads_with_everything_off() {
        let (_d, p) = tmp();
        std::fs::write(&p, "[privacy]\nmicrophone_enabled = true\n").unwrap();
        let old = Config::load(&p).unwrap();
        assert!(old.privacy.microphone_enabled);
        assert!(old.hotkeys.custom.is_empty());
        assert!(!old.dictation.enabled && !old.screen_ask.enabled);
    }

    #[test]
    fn scenarios_by_event_are_on_with_a_limit_and_old_files_load() {
        let c = Config::default();
        assert!(c.events.enabled);
        assert_eq!(c.events.max_per_hour, 12);
        let (_d, p) = tmp();
        std::fs::write(&p, "[events]\nenabled = false\n").unwrap();
        let old = Config::load(&p).unwrap();
        assert!(!old.events.enabled);
        assert_eq!(old.events.max_per_hour, 12);
        old.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("[events]"), "{text}");
        assert_eq!(Config::load(&p).unwrap(), old);
    }

    #[test]
    fn research_defaults_and_old_files_without_the_section() {
        let r = ResearchConfig::default();
        assert!(r.folder.is_empty() && r.model.is_empty());
        assert_eq!(
            (r.max_searches, r.max_pages, r.max_minutes, r.max_tokens),
            (5, 8, 5, 50_000)
        );
        let limits = r.limits();
        assert_eq!(
            limits,
            ResearchLimits {
                searches: 5,
                pages: 8,
                minutes: 5,
                tokens: 50_000
            }
        );
        assert_eq!(limits.llm_calls(), 10);
        assert!(!limits.high());
        let (_d, p) = tmp();
        std::fs::write(&p, "[docs]\nfolders = [\"C:\\\\Docs\"]\n").unwrap();
        let old = Config::load(&p).unwrap();
        assert_eq!(old.research, ResearchConfig::default());
    }

    #[test]
    fn research_limits_stay_inside_their_bounds() {
        let low = ResearchConfig {
            max_searches: 0,
            max_pages: 0,
            max_minutes: 0,
            max_tokens: 0,
            ..Default::default()
        }
        .limits();
        assert_eq!(
            low,
            ResearchLimits {
                searches: 2,
                pages: 2,
                minutes: 2,
                tokens: 20_000
            }
        );
        let high = ResearchConfig {
            max_searches: 10_000_000,
            max_pages: 10_000_000,
            max_minutes: 10_000_000,
            max_tokens: 10_000_000,
            ..Default::default()
        }
        .limits();
        assert_eq!(
            high,
            ResearchLimits {
                searches: 10,
                pages: 16,
                minutes: 15,
                tokens: 200_000
            }
        );
        assert!(high.high());
        assert_eq!(high.llm_calls(), 18);
        let edge = ResearchConfig {
            max_tokens: RESEARCH_HIGH_TOKENS,
            ..Default::default()
        };
        assert!(!edge.limits().high());
    }

    #[test]
    fn the_journal_starts_quiet_about_content_and_old_files_load_without_it() {
        use crate::journal::Level;
        let l = LogConfig::default();
        assert_eq!(l.level, Level::Info);
        assert!(!l.content);
        assert_eq!((l.keep_days, l.keep_files), (14, 5));
        assert_eq!(l.normalized(), l);

        let (_d, p) = tmp();
        std::fs::write(&p, "[voice]\nspeak_replies = false\n").unwrap();
        assert_eq!(Config::load(&p).unwrap().log, LogConfig::default());
        // A section with one key keeps the defaults of the others.
        std::fs::write(&p, "[log]\nlevel = \"debug\"\n").unwrap();
        let partial = Config::load(&p).unwrap().log;
        assert_eq!(partial.level, Level::Debug);
        assert_eq!((partial.keep_days, partial.keep_files), (14, 5));

        let c = Config {
            log: LogConfig {
                level: Level::Warn,
                content: true,
                keep_days: 30,
                keep_files: 3,
            },
            ..Default::default()
        };
        c.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("[log]"), "{text}");
        assert!(text.contains("level = \"warn\""), "{text}");
        assert!(text.contains("content = true"), "{text}");
        assert_eq!(Config::load(&p).unwrap(), c);
    }

    #[test]
    fn a_slip_in_the_journal_section_does_not_cost_the_other_settings() {
        use crate::journal::Level;
        let (_d, p) = tmp();
        let load = |log: &str| {
            std::fs::write(
                &p,
                format!("[voice]\nspeak_replies = false\n\n[log]\n{log}\n\n[llm]\nmodel = \"zq-model\"\n"),
            )
            .unwrap();
            let c = Config::load(&p).unwrap_or_else(|e| panic!("{log}: {e}"));
            // The sections around it are read as they are.
            assert!(!c.voice.speak_replies, "{log}");
            assert_eq!(c.llm.model, "zq-model", "{log}");
            c.log
        };
        // The level as every line of the file spells it, and near misses.
        for (written, level) in [
            ("INFO", Level::Info),
            ("Warn", Level::Warn),
            (" debug ", Level::Debug),
            ("warning", Level::Warn),
            ("trace", Level::Debug),
            ("ERROR", Level::Error),
        ] {
            assert_eq!(
                load(&format!("level = \"{written}\"")).level,
                level,
                "{written}"
            );
        }
        // What cannot be understood gives way to the default of its field.
        let default = LogConfig::default();
        for bad in [
            "level = \"loud\"",
            "level = 3",
            "level = [\"info\"]",
            "keep_days = -1",
            "keep_days = \"many\"",
            "keep_files = true",
            "content = \"maybe\"",
            "content = 1",
        ] {
            assert_eq!(load(bad), default, "{bad}");
        }
        // The fields beside a bad one keep their values.
        let mixed = load("level = \"loud\"\ncontent = true\nkeep_days = \"30\"\nkeep_files = 3.0");
        assert_eq!(
            mixed,
            LogConfig {
                content: true,
                keep_days: 30,
                keep_files: 3,
                ..LogConfig::default()
            }
        );
        // Too much is cut down by the bounds, not thrown away.
        let huge = load("keep_days = 99999999999\nkeep_files = 1000").normalized();
        assert_eq!((huge.keep_days, huge.keep_files), (365, 50));
        assert!(load("content = \"TRUE\"").content);
        // What the page sends is read the same way.
        let sent: LogConfig =
            serde_json::from_str(r#"{"level":"warn","content":true,"keep_days":7,"keep_files":5}"#)
                .unwrap();
        assert_eq!(
            (sent.level, sent.content, sent.keep_days),
            (Level::Warn, true, 7)
        );
    }

    #[test]
    fn the_retention_of_the_journal_stays_inside_its_bounds() {
        let wild = LogConfig {
            keep_days: 0,
            keep_files: 0,
            ..Default::default()
        }
        .normalized();
        assert_eq!((wild.keep_days, wild.keep_files), (1, 1));
        let wild = LogConfig {
            keep_days: 100_000,
            keep_files: 100_000,
            content: true,
            ..Default::default()
        }
        .normalized();
        assert_eq!((wild.keep_days, wild.keep_files), (365, 50));
        assert!(wild.content);
    }

    #[test]
    fn research_section_roundtrips() {
        let (_d, p) = tmp();
        let c = Config {
            research: ResearchConfig {
                folder: "D:\\Reports".into(),
                model: "mistralai/mistral-nemo".into(),
                max_searches: 3,
                max_pages: 4,
                max_minutes: 6,
                max_tokens: 120_000,
            },
            ..Default::default()
        };
        c.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("[research]"), "{text}");
        assert_eq!(Config::load(&p).unwrap(), c);
    }
}
