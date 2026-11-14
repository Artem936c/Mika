//! The single list of what Mika keeps in Windows Credential Manager.
//!
//! A backup collects the keys by this list and "delete all data" wipes them
//! by it. A name read from a backup file is only compared with the list.

use crate::config::{
    mcp_token_key, Config, ProviderKind, CALENDAR_SECRET_KEY, EMBEDDING_SECRET_KEY,
    IMAGE_SECRET_KEY, SPEECH_SECRET_KEY, TELEGRAM_SECRET_KEY,
};
use crate::secrets::{Secret, SecretError, SecretStore};

/// Longest MCP server name whose token travels in a backup, characters.
const MAX_MCP_NAME: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    LlmOpenai,
    LlmAnthropic,
    LlmOllama,
    Speech,
    Image,
    Embedding,
    Telegram,
    Calendar,
    Mcp,
}

/// The kinds whose entry name does not depend on the settings.
const FIXED: [SecretKind; 8] = [
    SecretKind::LlmOpenai,
    SecretKind::LlmAnthropic,
    SecretKind::LlmOllama,
    SecretKind::Speech,
    SecretKind::Image,
    SecretKind::Embedding,
    SecretKind::Telegram,
    SecretKind::Calendar,
];

impl SecretKind {
    /// The kind that holds the model key of a provider.
    pub fn llm(kind: ProviderKind) -> SecretKind {
        match kind {
            ProviderKind::Openai => SecretKind::LlmOpenai,
            ProviderKind::Anthropic => SecretKind::LlmAnthropic,
            ProviderKind::Ollama => SecretKind::LlmOllama,
        }
    }

    /// Name of the entry of a fixed kind; `None` for an MCP token.
    fn fixed_key(self) -> Option<String> {
        Some(match self {
            SecretKind::LlmOpenai => ProviderKind::Openai.secret_key(),
            SecretKind::LlmAnthropic => ProviderKind::Anthropic.secret_key(),
            SecretKind::LlmOllama => ProviderKind::Ollama.secret_key(),
            SecretKind::Speech => SPEECH_SECRET_KEY.into(),
            SecretKind::Image => IMAGE_SECRET_KEY.into(),
            SecretKind::Embedding => EMBEDDING_SECRET_KEY.into(),
            SecretKind::Telegram => TELEGRAM_SECRET_KEY.into(),
            SecretKind::Calendar => CALENDAR_SECRET_KEY.into(),
            SecretKind::Mcp => return None,
        })
    }
}

/// One entry of the secret store: its kind and, for an MCP token, the server name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SecretRef {
    pub kind: SecretKind,
    pub name: Option<String>,
}

impl SecretRef {
    pub fn fixed(kind: SecretKind) -> SecretRef {
        SecretRef { kind, name: None }
    }

    /// The token of an MCP server; `None` for a name a backup does not carry.
    pub fn mcp(server: &str) -> Option<SecretRef> {
        let name = server.trim();
        (!name.is_empty() && name.chars().count() <= MAX_MCP_NAME).then(|| SecretRef {
            kind: SecretKind::Mcp,
            name: Some(name.to_string()),
        })
    }

    /// Name of the entry in the secret store.
    pub fn key(&self) -> String {
        self.kind
            .fixed_key()
            .unwrap_or_else(|| mcp_token_key(self.name.as_deref().unwrap_or_default()))
    }

    /// The entry a key names. Only the known names are accepted.
    pub fn parse(key: &str) -> Option<SecretRef> {
        if let Some(kind) = FIXED
            .into_iter()
            .find(|k| k.fixed_key().as_deref() == Some(key))
        {
            return Some(SecretRef::fixed(kind));
        }
        let name = key.strip_prefix("mcp.")?.strip_suffix(".token")?;
        // The key is built from a trimmed name: anything else is not ours.
        SecretRef::mcp(name).filter(|r| r.name.as_deref() == Some(name))
    }
}

/// The eight keys that do not depend on the settings.
pub fn fixed_secret_keys() -> Vec<String> {
    FIXED.into_iter().filter_map(|k| k.fixed_key()).collect()
}

/// The eight fixed entries plus one per MCP server of the settings.
pub fn known_secrets(config: &Config) -> Vec<SecretRef> {
    let mut all: Vec<SecretRef> = FIXED.into_iter().map(SecretRef::fixed).collect();
    for server in &config.mcp.servers {
        if let Some(r) = SecretRef::mcp(&server.name) {
            if !all.contains(&r) {
                all.push(r);
            }
        }
    }
    all
}

/// Whether the store holds a non-empty value under the key. A store that
/// cannot answer counts as "no".
pub(crate) fn stored(store: &dyn SecretStore, key: &str) -> bool {
    matches!(store.get(key), Ok(Some(s)) if !s.is_empty())
}

/// Every known entry that holds a value.
pub(crate) fn read_secrets(
    store: &dyn SecretStore,
    config: &Config,
) -> Result<Vec<(SecretRef, Secret)>, SecretError> {
    let mut found = Vec::new();
    for r in known_secrets(config) {
        if let Some(value) = store.get(&r.key())?.filter(|v| !v.is_empty()) {
            found.push((r, value));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpServerConfig;
    use crate::secrets::MemorySecretStore;

    fn server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn key_and_parse_agree_for_every_kind() {
        let mut all: Vec<SecretRef> = FIXED.into_iter().map(SecretRef::fixed).collect();
        all.push(SecretRef::mcp("files").unwrap());
        all.push(SecretRef::mcp("мой сервер").unwrap());
        all.push(SecretRef::mcp("a.b").unwrap());
        for r in all {
            assert_eq!(SecretRef::parse(&r.key()), Some(r.clone()), "{r:?}");
        }
        assert_eq!(SecretRef::mcp("files").unwrap().key(), "mcp.files.token");
        assert_eq!(
            SecretRef::fixed(SecretKind::LlmAnthropic).key(),
            "llm.anthropic.api_key"
        );
    }

    #[test]
    fn parse_refuses_unknown_names() {
        let long = format!("mcp.{}.token", "я".repeat(MAX_MCP_NAME + 1));
        for junk in [
            "",
            "x",
            "llm.api_key",
            "llm.gemini.api_key",
            "mcp..token",
            "mcp. .token",
            "mcp. files.token",
            "mcp.files .token",
            "mcp.a.b",
            "mcp.token",
            "mcp.files.token ",
            "MCP.files.token",
            "speech.api_key.old",
            long.as_str(),
        ] {
            assert_eq!(SecretRef::parse(junk), None, "{junk:?}");
        }
        let longest = format!("mcp.{}.token", "я".repeat(MAX_MCP_NAME));
        assert!(SecretRef::parse(&longest).is_some());
    }

    #[test]
    fn fixed_keys_are_the_constants_of_the_settings() {
        assert_eq!(
            fixed_secret_keys(),
            vec![
                ProviderKind::Openai.secret_key(),
                ProviderKind::Anthropic.secret_key(),
                ProviderKind::Ollama.secret_key(),
                SPEECH_SECRET_KEY.to_string(),
                IMAGE_SECRET_KEY.to_string(),
                EMBEDDING_SECRET_KEY.to_string(),
                TELEGRAM_SECRET_KEY.to_string(),
                CALENDAR_SECRET_KEY.to_string(),
            ]
        );
        assert!(fixed_secret_keys().contains(&"calendar.ics_url".to_string()));
        for kind in [
            ProviderKind::Openai,
            ProviderKind::Anthropic,
            ProviderKind::Ollama,
        ] {
            assert_eq!(
                SecretRef::fixed(SecretKind::llm(kind)).key(),
                kind.secret_key()
            );
        }
    }

    #[test]
    fn known_secrets_follow_the_mcp_servers() {
        let mut config = Config::default();
        assert_eq!(known_secrets(&config).len(), 8);
        config.mcp.servers = vec![
            server(" files "),
            server("files"),
            server(""),
            server("remote"),
            server(&"я".repeat(MAX_MCP_NAME + 1)),
        ];
        let known = known_secrets(&config);
        let names: Vec<&str> = known.iter().filter_map(|r| r.name.as_deref()).collect();
        assert_eq!(names, ["files", "remote"]);
        assert_eq!(known.len(), 10);
    }

    #[test]
    fn read_secrets_returns_only_the_stored_ones() {
        let store = MemorySecretStore::default();
        let mut config = Config::default();
        config.mcp.servers = vec![server("files"), server("remote")];
        store
            .set("speech.api_key", &Secret::new("speech-value"))
            .unwrap();
        store
            .set("mcp.files.token", &Secret::new("files-value"))
            .unwrap();
        // Not in the list: never read.
        store.set("mcp.gone.token", &Secret::new("x")).unwrap();
        store.set("other.key", &Secret::new("y")).unwrap();
        let found = read_secrets(&store, &config).unwrap();
        let keys: Vec<String> = found.iter().map(|(r, _)| r.key()).collect();
        assert_eq!(keys, ["speech.api_key", "mcp.files.token"]);
        assert!(stored(&store, "speech.api_key"));
        assert!(!stored(&store, "image.api_key"));
    }
}
