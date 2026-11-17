//! Backup and transfer to another PC (decision 0034).
//!
//! One file holds the settings, the history, the memory, notes, todos,
//! reminders, the user's commands, the scenarios by event and the
//! permission rules. With a password
//! the whole file is encrypted and carries the keys from the secret store;
//! without one it is plain and holds no key at all.
//!
//! A restore reads the file twice: `preview` changes nothing and says what
//! is inside, `restore` replaces the categories the user ticked. The module
//! is pure: no Windows calls, no environment and no clock. The caller owns
//! the file, picks its place and passes the time, the version and the name
//! of the PC.
//!
//! A file may be somebody else's. What would act without a question once it
//! is restored ("always allow" rules, scheduled tasks) comes in only by a
//! tick of its own, and only the entries the preview listed. Scenarios by
//! event are listed too and come in switched off. Settings that switch on a
//! sensor, the network or a program do not come from a file (`landing`).
//!
//! A password, a key made from it and a secret value never reach the log,
//! an error or a `Debug` print.

mod crypto;
mod format;
mod landing;
mod payload;
mod secrets;
mod tables;

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::num::NonZeroU32;

use crate::config::Config;
use crate::secrets::{Secret, SecretStore};
use crate::storage::Db;

pub use crypto::check_password;
pub use format::{Header, Limits};
pub use landing::{land_config, Landing, SwitchedOff};
pub use secrets::{fixed_secret_keys, known_secrets, SecretKind, SecretRef};
pub use tables::{Category, TableSpec, EXPORTED, SKIPPED};

use rusqlite::Connection;

use crypto::{ChunkReader, ChunkWriter, Keys, Sealing};
use format::CryptoParams;
use payload::{Capped, Line, PayloadReader, PayloadWriter};
use tables::{Schema, TableWalk};

/// Extension of a backup file, without the dot.
pub const EXTENSION: &str = "mikabackup";
pub const MAGIC: [u8; 8] = *b"MIKA-BKP";
/// The format this version writes and the newest it reads.
pub const FORMAT: u16 = 1;
/// Shortest and longest password, characters.
pub const MIN_PASSWORD: usize = 10;
pub const MAX_PASSWORD: usize = 256;
/// PBKDF2 rounds of a new file, and the range a file may name: fewer is not
/// ours, more is an attempt to hang the app.
pub const KDF_ITERATIONS: u32 = 600_000;
pub const KDF_ITERATIONS_MIN: u32 = 100_000;
pub const KDF_ITERATIONS_MAX: u32 = 5_000_000;
/// Plaintext bytes per encrypted chunk of a new file, and the range a file
/// may name.
pub const CHUNK: usize = 1 << 20;
pub const MIN_CHUNK: usize = 4096;
pub const MAX_CHUNK: usize = 4 << 20;
/// "Always allow" rules listed in a preview. The rules after these are not
/// restored: nothing comes in that was not shown.
pub const MAX_RULES_SHOWN: usize = 200;
/// Scheduled tasks listed in a preview, with the same rule.
pub const MAX_TASKS_SHOWN: usize = 50;
/// Scenarios by event listed in a preview, with the same rule: as many as
/// the app keeps.
pub const MAX_SCENARIOS_SHOWN: usize = crate::triggers::MAX_TRIGGERS;
/// Longest secret value taken from a file, characters.
pub const MAX_SECRET_CHARS: usize = 1024;
/// Longest assistant name or wake phrase taken from a file, characters.
pub const MAX_LANDED_NAME: usize = 24;

/// Why a backup was not made or not read. No variant carries user data.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("password is shorter than {0} characters")]
    PasswordShort(usize),
    #[error("password is longer than {0} characters")]
    PasswordLong(usize),
    #[error("the backup is encrypted, a password is needed")]
    PasswordNeeded,
    #[error("wrong password")]
    WrongPassword,
    #[error("not a backup file")]
    NotBackup,
    #[error("backup format {0} is newer than this version reads")]
    Newer(u16),
    #[error("unsupported backup parameters")]
    Unsupported,
    #[error("backup file is damaged")]
    Damaged,
    #[error("backup is over the size limits")]
    TooBig,
    #[error("nothing is selected")]
    NothingSelected,
    #[error("backup io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("backup database error: {0}")]
    Db(#[from] crate::storage::DbError),
    #[error("secret store error: {0}")]
    Secrets(#[from] crate::secrets::SecretError),
}

impl From<rusqlite::Error> for BackupError {
    fn from(e: rusqlite::Error) -> Self {
        BackupError::Db(e.into())
    }
}

/// Where the data of this PC lives.
pub struct Stores<'a> {
    pub config: &'a Config,
    pub db: &'a Db,
    pub secrets: &'a dyn SecretStore,
}

/// What the caller knows and the header shows before a password is asked.
pub struct ExportInfo<'a> {
    pub app_version: &'a str,
    /// RFC 3339, as `storage::now()` gives it.
    pub created_at: &'a str,
    /// Name of this PC.
    pub device: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    pub encrypted: bool,
    /// Keys written into the file.
    pub secrets: usize,
    /// What went in, in `Category::ALL` order.
    pub counts: Vec<(Category, u64)>,
}

/// The right to read the body of a file: nothing for a plain one, the keys
/// made from the password for an encrypted one.
pub struct Access(Option<Keys>);

impl std::fmt::Debug for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Access(key)"
        } else {
            "Access(plain)"
        })
    }
}

/// One category of a backup against the same category of this PC.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CategoryPreview {
    pub id: Category,
    /// Usable entries in the file.
    pub in_file: u64,
    /// Entries on this PC now.
    pub here: u64,
    /// Whether the category can be restored from this file.
    pub available: bool,
    /// The default tick.
    pub selected: bool,
}

/// An "always allow" rule of the file.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RulePreview {
    pub tool: String,
    pub pattern: String,
}

/// A scheduled task of the file: a text the assistant gets as a message
/// when the time comes.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TaskPreview {
    pub text: String,
    /// Unix seconds.
    pub due_at: i64,
    /// `none`, `daily`, `weekly` or `every:<minutes>`.
    pub repeat: String,
}

/// A scenario by event of the file (decision 0031): what happens, and what
/// she then does by herself. It comes in switched off and without its
/// counters.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ScenarioPreview {
    pub name: String,
    pub event: crate::triggers::TriggerEvent,
    /// Part of a device name or a process name; empty when the event takes none.
    pub filter: String,
    /// For the events about the user being away; 0 for the others.
    pub idle_min: u32,
    /// The instruction in words, or the name of a saved command.
    pub action: crate::triggers::TriggerAction,
    pub cooldown_s: u32,
    pub max_per_day: u32,
    /// For a scenario of a saved command: the file holds a command of that
    /// name. With the commands ticked, the scenario comes in only then.
    pub command_in_file: bool,
    /// For a scenario of a saved command: this PC holds a command of that
    /// name. With the commands not ticked, the scenario comes in only then.
    pub command_here: bool,
}

/// A key of the file: its name, never its value.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SecretPreview {
    pub kind: SecretKind,
    pub name: Option<String>,
    /// A value is stored on this PC under the same name.
    pub replaces: bool,
}

/// Where requests of one purpose would go after the settings are restored.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AddressPreview {
    /// `llm`, `speech`, `image` or `embedding`.
    pub purpose: &'static str,
    pub incoming: String,
    pub current: String,
    pub changed: bool,
    /// This PC stores a key that would be sent to the incoming address.
    pub key_here: bool,
    /// With the keys of the file restored, the key sent to the incoming
    /// address is one of the file, not one of this PC.
    pub key_in_file: bool,
}

/// An MCP server of the file's settings.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct McpPreview {
    pub name: String,
    /// The address, or the command with its arguments.
    pub target: String,
    pub was_enabled: bool,
    /// The target of the server of that name on this PC; empty when there
    /// is none.
    pub current: String,
    /// The target differs from the one on this PC, or there is none here.
    pub changed: bool,
    /// This PC stores a token under the name of this server.
    pub token_here: bool,
    /// The file brings the token of this server.
    pub token_in_file: bool,
}

/// The Telegram owner of the file's settings.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TelegramPreview {
    pub was_enabled: bool,
    pub user_id: i64,
    /// The owner on this PC; 0 when none is set.
    pub current: i64,
    pub changed: bool,
    /// This PC stores a bot token: its bot would obey the owner of the file.
    pub token_here: bool,
    /// The file brings a bot token.
    pub token_in_file: bool,
}

/// What a file holds and what restoring it would do. Made without changing
/// anything.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Preview {
    /// Always all of them, in `Category::ALL` order.
    pub categories: Vec<CategoryPreview>,
    /// The first `MAX_RULES_SHOWN` of them: the ones a restore would write.
    pub allow_rules: Vec<RulePreview>,
    pub allow_rules_total: u64,
    /// Refused for a `*` inside the pattern.
    pub allow_rules_dropped: u64,
    /// The first `MAX_TASKS_SHOWN` scheduled tasks: the ones a restore
    /// would write.
    pub tasks: Vec<TaskPreview>,
    pub tasks_total: u64,
    /// The first `MAX_SCENARIOS_SHOWN` scenarios by event: the ones a
    /// restore would write, every one switched off.
    pub scenarios: Vec<ScenarioPreview>,
    pub scenarios_total: u64,
    pub secrets: Vec<SecretPreview>,
    pub addresses: Vec<AddressPreview>,
    pub mcp: Vec<McpPreview>,
    pub telegram: Option<TelegramPreview>,
    pub switched_off: Vec<SwitchedOff>,
    pub docs_folders: Vec<String>,
    /// The file comes from a newer database schema.
    pub schema_newer: bool,
    pub skipped_unknown: u64,
    pub dropped_rows: u64,
}

/// What a restore did.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreReport {
    /// Categories that were replaced and how much went in. Settings count 1,
    /// secrets count the keys written.
    pub restored: Vec<(Category, u64)>,
    /// The settings ready for this PC. The caller saves and installs them.
    pub config: Option<Config>,
    /// What was on in the file and is off now: switches of the settings,
    /// and the scenarios by event, which all land switched off.
    pub switched_off: Vec<SwitchedOff>,
    pub docs_folders: Vec<String>,
    /// Scenarios that were not restored: the saved command they name is
    /// neither restored nor on this PC.
    pub scenarios_without_command: u64,
    pub keys_written: usize,
    /// Keys the secret store refused.
    pub keys_failed: Vec<SecretRef>,
    /// Settings, history or keys were restored: parts of them are read at start.
    pub restart_needed: bool,
}

/// Writes a whole backup file, from its first byte. With a password the file
/// is encrypted and holds the keys; without one it holds none. The caller
/// owns the file, its temporary name and the rename.
pub fn export(
    stores: &Stores<'_>,
    info: &ExportInfo<'_>,
    password: Option<&str>,
    out: &mut dyn Write,
) -> Result<ExportReport, BackupError> {
    export_with(stores, info, password, out, KDF_ITERATIONS)
}

fn export_with(
    stores: &Stores<'_>,
    info: &ExportInfo<'_>,
    password: Option<&str>,
    out: &mut dyn Write,
    iterations: u32,
) -> Result<ExportReport, BackupError> {
    // Everything that can refuse comes before the first byte is written.
    let secrets = match password {
        Some(password) => {
            check_password(password)?;
            secrets::read_secrets(stores.secrets, stores.config)?
        }
        None => Vec::new(),
    };
    let toml = toml::to_string_pretty(stores.config)
        .map_err(|_| std::io::Error::other("settings cannot be written as text"))?;
    let sealed = match password {
        Some(password) => {
            let salt: [u8; crypto::SALT_LEN] = crypto::random()?;
            let nonce: [u8; crypto::NONCE_PREFIX] = crypto::random()?;
            let rounds = NonZeroU32::new(iterations).ok_or(BackupError::Unsupported)?;
            let keys = Keys::derive(password, &salt, rounds);
            let params = CryptoParams {
                iterations,
                salt: salt.to_vec(),
                nonce,
                chunk: CHUNK,
                check: keys.check_value(),
            };
            Some((keys, params))
        }
        None => None,
    };
    let (keys, params) = sealed.unzip();
    let (prefix, header) = format::encode_prefix(
        info.app_version,
        info.created_at,
        info.device,
        secrets.len() as u32,
        params,
    );
    out.write_all(&prefix)?;
    let counts = match (keys, &header.crypto) {
        (Some(keys), Some(params)) => {
            let sealing = Sealing {
                keys,
                nonce: params.nonce,
                aad: header.prefix_sha256,
                chunk: params.chunk,
            };
            let mut payload = PayloadWriter::new(ChunkWriter::new(&mut *out, sealing));
            let counts = write_payload(&mut payload, stores, &toml, &secrets)?;
            payload.finish()?.finish()?;
            counts
        }
        _ => {
            let mut payload = PayloadWriter::new(&mut *out);
            let counts = write_payload(&mut payload, stores, &toml, &[])?;
            payload.finish()?;
            counts
        }
    };
    out.flush()?;
    Ok(ExportReport {
        encrypted: header.encrypted,
        secrets: header.secrets as usize,
        counts,
    })
}

/// The lines between the header and the closing one. The database is read
/// in one transaction, so the tables agree with each other.
fn write_payload<W: Write>(
    out: &mut PayloadWriter<W>,
    stores: &Stores<'_>,
    toml: &str,
    secrets: &[(SecretRef, Secret)],
) -> Result<Vec<(Category, u64)>, BackupError> {
    let rows = stores.db.with_conn(|c| {
        let tx = c.unchecked_transaction()?;
        out.begin(tables::schema_version(&tx)?)?;
        out.config(toml)?;
        for (name, value) in secrets {
            out.secret(&name.key(), value)?;
        }
        tables::export_tables(&tx, out)
    })?;
    Ok(Category::ALL
        .into_iter()
        .map(|category| {
            let count = match category {
                Category::Settings => 1,
                Category::Secrets => secrets.len() as u64,
                table => rows.get(&table).copied().unwrap_or(0),
            };
            (category, count)
        })
        .collect())
}

/// Reads a file up to its body and says what it is. Consumes exactly the
/// prefix: the reader then stands at the body.
pub fn read_header(input: &mut dyn Read) -> Result<Header, BackupError> {
    format::read_header_with(input, Limits::default().header)
}

/// Checks the password against the header. Slow on purpose (PBKDF2): call
/// it off the async threads. A plain file needs no password and ignores one.
pub fn unlock(header: &Header, password: Option<&str>) -> Result<Access, BackupError> {
    let Some(params) = &header.crypto else {
        return Ok(Access(None));
    };
    let password = password.ok_or(BackupError::PasswordNeeded)?;
    // No file of ours was made with a longer one: not worth the hashing.
    if password.chars().count() > MAX_PASSWORD {
        return Err(BackupError::WrongPassword);
    }
    let rounds = NonZeroU32::new(params.iterations).ok_or(BackupError::Unsupported)?;
    let keys = Keys::derive(password, &params.salt, rounds);
    if keys.matches(&params.check) {
        Ok(Access(Some(keys)))
    } else {
        Err(BackupError::WrongPassword)
    }
}

/// The plaintext of the body: the bytes as they are, or the opened chunks.
fn open_body<'a>(
    body: &'a mut dyn Read,
    header: &Header,
    access: &Access,
    limits: &Limits,
) -> Result<Box<dyn BufRead + 'a>, BackupError> {
    let body = Capped::new(body, limits.file);
    match (&header.crypto, &access.0) {
        (None, _) => Ok(Box::new(BufReader::new(body))),
        (Some(_), None) => Err(BackupError::PasswordNeeded),
        (Some(params), Some(keys)) => {
            let sealing = Sealing {
                keys: keys.clone(),
                nonce: params.nonce,
                aad: header.prefix_sha256,
                chunk: params.chunk,
            };
            Ok(Box::new(BufReader::new(ChunkReader::new(body, sealing))))
        }
    }
}

/// Everything one pass over a body gives.
struct Scan {
    /// Schema version of the database the file was made from.
    schema: i64,
    /// The settings, when the file has them and they read.
    config: Option<Config>,
    /// Usable keys of an encrypted file; a repeated name keeps its last value.
    secrets: Vec<(SecretRef, Secret)>,
    valid: std::collections::BTreeMap<Category, u64>,
    written: std::collections::BTreeMap<Category, u64>,
    allow_rules: Vec<RulePreview>,
    allow_rules_dropped: u64,
    tasks: Vec<TaskPreview>,
    scenarios: Vec<ScenarioPreview>,
    scenarios_without_command: u64,
    dropped_rows: u64,
    skipped_unknown: u64,
}

/// Reads the whole body, checking every chunk and every row against the
/// tables of this PC. With a target the selected tables are replaced on its
/// connection, which must be inside a transaction: an error leaves the
/// commit to the caller's choice. Without one nothing is written and the
/// database is not held.
fn scan(
    body: &mut dyn Read,
    header: &Header,
    access: &Access,
    limits: &Limits,
    schema: Schema,
    target: Option<(&Connection, &BTreeSet<Category>)>,
) -> Result<Scan, BackupError> {
    let input = open_body(body, header, access, limits)?;
    let mut reader = PayloadReader::open(input, limits)?;
    let mut walk = TableWalk::new(schema, target);
    let mut config_text: Option<String> = None;
    let mut secrets: Vec<(SecretRef, Secret)> = Vec::new();
    let mut dropped = 0u64;
    let mut skipped = 0u64;
    while let Some(line) = reader.next()? {
        match line {
            Line::Config(text) if config_text.is_none() => config_text = Some(text),
            Line::Config(_) | Line::Unknown => skipped += 1,
            // Keys are taken only from a file that is encrypted as a whole.
            Line::Secret { .. } if !header.encrypted => skipped += 1,
            Line::Secret { key, value, chars } => match SecretRef::parse(&key) {
                Some(name) if (1..=MAX_SECRET_CHARS).contains(&chars) => {
                    secrets.retain(|(known, _)| *known != name);
                    secrets.push((name, value));
                }
                _ => dropped += 1,
            },
            Line::Table { name, columns } => walk.table(&name, &columns),
            Line::Row(cells) => walk.row(&cells)?,
        }
    }
    // The scenarios wait for the end: they need the commands as they are now.
    walk.finish()?;
    let config = config_text.and_then(|text| {
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        // The message only: the full error quotes the line of the settings.
        toml::from_str::<Config>(text)
            .map_err(|e| {
                log::warn!(
                    "the settings of the copy cannot be read, they are left out: {}",
                    e.message()
                )
            })
            .ok()
    });
    Ok(Scan {
        schema: reader.schema(),
        config,
        secrets,
        valid: walk.valid,
        written: walk.written,
        allow_rules: walk.allow_rules,
        allow_rules_dropped: walk.allow_rules_dropped,
        tasks: walk.tasks,
        scenarios: walk.scenarios,
        scenarios_without_command: walk.scenarios_without_command,
        dropped_rows: walk.dropped_rows + dropped,
        skipped_unknown: walk.skipped_unknown + skipped,
    })
}

/// An address as it is compared: no spaces around, no slash at the end.
fn address(own: &str, fallback: &str) -> String {
    let own = own.trim().trim_end_matches('/');
    if own.is_empty() {
        fallback.to_string()
    } else {
        own.to_string()
    }
}

fn addresses(
    file: &Config,
    file_keys: &[(SecretRef, Secret)],
    here: &Stores<'_>,
) -> Vec<AddressPreview> {
    let llm_key = SecretRef::fixed(SecretKind::llm(file.llm.kind));
    let has_here = |name: &SecretRef| secrets::stored(here.secrets, &name.key());
    let in_file = |name: &SecretRef| file_keys.iter().any(|(known, _)| known == name);
    let incoming_llm = file.llm.effective_base_url();
    let current_llm = here.config.llm.effective_base_url();
    let changed = |a: &str, b: &str| a.to_lowercase() != b.to_lowercase();

    let mut list = vec![AddressPreview {
        purpose: "llm",
        changed: changed(&incoming_llm, &current_llm),
        key_here: has_here(&llm_key),
        key_in_file: in_file(&llm_key),
        incoming: incoming_llm.clone(),
        current: current_llm.clone(),
    }];
    // The other three fall back to the model's address and key.
    for (purpose, kind, incoming, current) in [
        (
            "speech",
            SecretKind::Speech,
            &file.voice.speech_base_url,
            &here.config.voice.speech_base_url,
        ),
        (
            "image",
            SecretKind::Image,
            &file.image.base_url,
            &here.config.image.base_url,
        ),
        (
            "embedding",
            SecretKind::Embedding,
            &file.embedding.base_url,
            &here.config.embedding.base_url,
        ),
    ] {
        let incoming = address(incoming, &incoming_llm);
        let current = address(current, &current_llm);
        if !changed(&incoming, &current) {
            continue;
        }
        // The key of the purpose itself goes before the model's. One that
        // this PC stores and the file does not replace is the key that will
        // be sent, whatever model key the file brings.
        let own = SecretRef::fixed(kind);
        list.push(AddressPreview {
            purpose,
            changed: true,
            key_here: has_here(&own) || has_here(&llm_key),
            key_in_file: in_file(&own) || (!has_here(&own) && in_file(&llm_key)),
            incoming,
            current,
        });
    }
    list
}

/// What a server is reached by: its address, or its command with the
/// arguments.
fn mcp_target(server: &crate::config::McpServerConfig) -> String {
    if server.url.trim().is_empty() {
        std::iter::once(&server.command)
            .chain(&server.args)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        server.url.trim().to_string()
    }
}

fn mcp_servers(
    file: &Config,
    file_keys: &[(SecretRef, Secret)],
    here: &Stores<'_>,
) -> Vec<McpPreview> {
    file.mcp
        .servers
        .iter()
        .map(|server| {
            let name = server.name.trim().to_string();
            let target = mcp_target(server);
            let current = here
                .config
                .mcp
                .servers
                .iter()
                .find(|s| s.name.trim() == name)
                .map(mcp_target)
                .unwrap_or_default();
            let token = SecretRef {
                kind: SecretKind::Mcp,
                name: Some(name.clone()),
            };
            McpPreview {
                changed: current != target,
                token_here: secrets::stored(here.secrets, &token.key()),
                token_in_file: file_keys.iter().any(|(known, _)| *known == token),
                name,
                target,
                was_enabled: server.enabled,
                current,
            }
        })
        .collect()
}

fn telegram_owner(
    file: &Config,
    file_keys: &[(SecretRef, Secret)],
    here: &Stores<'_>,
) -> Option<TelegramPreview> {
    let t = &file.telegram;
    if t.user_id == 0 && !t.enabled {
        return None;
    }
    let token = SecretRef::fixed(SecretKind::Telegram);
    let current = here.config.telegram.user_id;
    Some(TelegramPreview {
        was_enabled: t.enabled,
        user_id: t.user_id,
        current,
        changed: current != t.user_id,
        token_here: secrets::stored(here.secrets, &token.key()),
        token_in_file: file_keys.iter().any(|(known, _)| *known == token),
    })
}

/// Reads a whole file, checks every chunk and every row, and says what a
/// restore would do. Changes nothing. `body` is the reader right after the
/// header.
///
/// The database is asked once, before the file is read, for its columns and
/// its counts: a large file does not keep everything else waiting.
pub fn preview(
    body: &mut dyn Read,
    header: &Header,
    access: &Access,
    here: &Stores<'_>,
    limits: &Limits,
) -> Result<Preview, BackupError> {
    let (schema, counts_here, schema_here) = here.db.with_conn(|c| {
        let mut counts = Vec::with_capacity(Category::ALL.len());
        for category in Category::ALL {
            counts.push(tables::count_here(c, category)?);
        }
        Ok::<_, BackupError>((Schema::read(c)?, counts, tables::schema_version(c)?))
    })?;
    let scan = scan(body, header, access, limits, schema, None)?;

    let keys_here = known_secrets(here.config)
        .iter()
        .filter(|name| secrets::stored(here.secrets, &name.key()))
        .count() as u64;
    let categories = Category::ALL
        .into_iter()
        .zip(counts_here)
        .map(|(id, rows_here)| {
            let (in_file, here) = match id {
                Category::Settings => (u64::from(scan.config.is_some()), 1),
                Category::Secrets => (scan.secrets.len() as u64, keys_here),
                table => (scan.valid.get(&table).copied().unwrap_or(0), rows_here),
            };
            let available = in_file > 0;
            CategoryPreview {
                id,
                in_file,
                here,
                available,
                // What acts without a question comes back only by the
                // user's own tick.
                selected: available && !matches!(id, Category::RulesAllow | Category::Tasks),
            }
        })
        .collect();

    let landing = scan
        .config
        .as_ref()
        .map(|file| land_config(file.clone(), here.config));
    let (switched_off, docs_folders) =
        landing.map_or_else(Default::default, |l| (l.switched_off, l.docs_folders));
    Ok(Preview {
        categories,
        allow_rules_total: scan.valid.get(&Category::RulesAllow).copied().unwrap_or(0),
        allow_rules: scan.allow_rules,
        allow_rules_dropped: scan.allow_rules_dropped,
        tasks_total: scan.valid.get(&Category::Tasks).copied().unwrap_or(0),
        tasks: scan.tasks,
        scenarios_total: scan.valid.get(&Category::Scenarios).copied().unwrap_or(0),
        scenarios: scan.scenarios,
        secrets: scan
            .secrets
            .iter()
            .map(|(name, _)| SecretPreview {
                kind: name.kind,
                name: name.name.clone(),
                replaces: secrets::stored(here.secrets, &name.key()),
            })
            .collect(),
        addresses: scan
            .config
            .as_ref()
            .map_or_else(Vec::new, |file| addresses(file, &scan.secrets, here)),
        mcp: scan
            .config
            .as_ref()
            .map_or_else(Vec::new, |file| mcp_servers(file, &scan.secrets, here)),
        telegram: scan
            .config
            .as_ref()
            .and_then(|file| telegram_owner(file, &scan.secrets, here)),
        switched_off,
        docs_folders,
        schema_newer: scan.schema > schema_here,
        skipped_unknown: scan.skipped_unknown,
        dropped_rows: scan.dropped_rows,
    })
}

/// Replaces the selected categories of this PC by those of the file.
///
/// The database changes in one transaction that is committed only after the
/// whole file was read and checked: any error leaves the database, the keys
/// and the settings as they were. Keys are written after the commit. The
/// settings are only returned: the caller saves and installs them.
pub fn restore(
    body: &mut dyn Read,
    header: &Header,
    access: &Access,
    here: &Stores<'_>,
    selected: &BTreeSet<Category>,
    limits: &Limits,
) -> Result<RestoreReport, BackupError> {
    if selected.is_empty() {
        return Err(BackupError::NothingSelected);
    }
    let scan = here.db.with_conn(|c| {
        let tx = c.unchecked_transaction()?;
        let schema = Schema::read(&tx)?;
        let scan = scan(body, header, access, limits, schema, Some((&tx, selected)))?;
        tx.commit()?;
        Ok::<_, BackupError>(scan)
    })?;
    // What the file held and this restore did not take: counts only.
    if scan.dropped_rows > 0 || scan.allow_rules_dropped > 0 || scan.skipped_unknown > 0 {
        log::warn!(
            "parts of the copy were left out: unusable_rows={} allow_rules={} unknown_lines={}",
            scan.dropped_rows,
            scan.allow_rules_dropped,
            scan.skipped_unknown
        );
    }

    let landing = scan
        .config
        .filter(|_| selected.contains(&Category::Settings))
        .map(|file| land_config(file, here.config));

    let mut keys_written = 0;
    let mut keys_failed = Vec::new();
    if selected.contains(&Category::Secrets) {
        // The settings that will be in force decide which MCP tokens belong.
        let in_force = landing.as_ref().map_or(here.config, |l| &l.config);
        let known = known_secrets(in_force);
        for (name, value) in scan.secrets {
            if name.kind == SecretKind::Mcp && !known.contains(&name) {
                continue;
            }
            match here.secrets.set(&name.key(), &value) {
                Ok(()) => keys_written += 1,
                Err(e) => {
                    // The name of the entry, never what it holds.
                    log::warn!(
                        "a key of the copy was not written: entry={} error={e}",
                        name.key()
                    );
                    keys_failed.push(name);
                }
            }
        }
    }

    let restored: Vec<(Category, u64)> = Category::ALL
        .into_iter()
        .filter_map(|category| {
            let count = match category {
                Category::Settings => u64::from(landing.is_some()),
                Category::Secrets => keys_written as u64,
                table => scan.written.get(&table).copied().unwrap_or(0),
            };
            (count > 0).then_some((category, count))
        })
        .collect();
    let restart_needed = restored.iter().any(|(category, _)| {
        matches!(
            category,
            Category::Settings | Category::History | Category::Secrets
        )
    });
    let (config, mut switched_off, docs_folders) = match landing {
        Some(l) => (Some(l.config), l.switched_off, l.docs_folders),
        None => (None, Vec::new(), Vec::new()),
    };
    // Every scenario that came in is off: the user is told, as for a switch.
    let scenarios = scan.written.get(&Category::Scenarios).copied().unwrap_or(0);
    if scenarios > 0 {
        switched_off.push(SwitchedOff::Scenarios {
            count: scenarios as usize,
        });
    }
    Ok(RestoreReport {
        restored,
        config,
        switched_off,
        docs_folders,
        scenarios_without_command: scan.scenarios_without_command,
        keys_written,
        keys_failed,
        restart_needed,
    })
}

#[cfg(test)]
mod tests {
    use super::crypto::tests::test_keys;
    use super::format::tests::test_params;
    use super::payload::tests::{gunzip, gz};
    use super::tables::tests::{filled_db, rows_of, snapshot};
    use super::*;
    use crate::config::{McpServerConfig, ProviderKind};
    use crate::secrets::{MemorySecretStore, SecretError};

    const PASSWORD: &str = "correct horse battery";
    const LLM_KEY: &str = "sk-llm-VALUE-0001";
    const SPEECH_KEY: &str = "sk-speech-VALUE-0002";
    const TELEGRAM_TOKEN: &str = "123456:telegram-VALUE-0003";
    const CALENDAR_LINK: &str = "https://calendar.example/private-VALUE-0004.ics";
    const MCP_TOKEN: &str = "mcp-VALUE-0005";
    /// Every secret value the old PC holds.
    const VALUES: [&str; 5] = [
        LLM_KEY,
        SPEECH_KEY,
        TELEGRAM_TOKEN,
        CALENDAR_LINK,
        MCP_TOKEN,
    ];

    const INFO: ExportInfo<'static> = ExportInfo {
        app_version: "0.3.3",
        created_at: "2026-11-14T15:30:00+05:00",
        device: "OLD-PC",
    };

    /// A secret store that can refuse to read, or to write one key.
    #[derive(Default)]
    struct TestStore {
        inner: MemorySecretStore,
        fail_get: bool,
        fail_set: Option<&'static str>,
    }

    impl SecretStore for TestStore {
        fn get(&self, key: &str) -> Result<Option<Secret>, SecretError> {
            if self.fail_get {
                return Err(SecretError::Backend("store is locked".into()));
            }
            self.inner.get(key)
        }

        fn set(&self, key: &str, value: &Secret) -> Result<(), SecretError> {
            if self.fail_set == Some(key) {
                return Err(SecretError::Backend("value refused".into()));
            }
            self.inner.set(key, value)
        }

        fn delete(&self, key: &str) -> Result<(), SecretError> {
            self.inner.delete(key)
        }
    }

    /// The three stores of one PC.
    struct Pc {
        config: Config,
        db: Db,
        secrets: TestStore,
    }

    impl Pc {
        fn empty() -> Pc {
            Pc {
                config: Config::default(),
                db: Db::open_in_memory().unwrap(),
                secrets: TestStore::default(),
            }
        }

        /// The PC a backup is made on: data in every table, keys of every
        /// kind, and everything that must land switched off turned on.
        fn old() -> Pc {
            let mut config = Config::default();
            config.assistant.name = "Айри".into();
            config.llm.base_url = "https://routerai.ru/api/v1".into();
            config.voice.speech_base_url = "https://speech.example/v1/".into();
            config.privacy.microphone_enabled = true;
            config.privacy.screen_enabled = true;
            config.telegram.enabled = true;
            config.telegram.user_id = 123_456_789;
            config.tools.ffmpeg_path = "D:\\Tools\\ffmpeg".into();
            config.docs.folders = vec!["C:\\Users\\old\\Documents\\Работа".into()];
            config.mcp.servers = vec![
                McpServerConfig {
                    name: "files".into(),
                    command: "npx".into(),
                    args: vec!["-y".into(), "server-filesystem".into(), "C:\\data".into()],
                    enabled: true,
                    url: String::new(),
                },
                McpServerConfig {
                    name: "remote".into(),
                    url: "https://mcp.example.com/mcp".into(),
                    enabled: false,
                    ..Default::default()
                },
            ];
            let pc = Pc {
                config,
                db: filled_db(),
                secrets: TestStore::default(),
            };
            pc.put("llm.openai.api_key", LLM_KEY);
            pc.put("speech.api_key", SPEECH_KEY);
            pc.put("telegram.bot_token", TELEGRAM_TOKEN);
            pc.put("calendar.ics_url", CALENDAR_LINK);
            pc.put("mcp.remote.token", MCP_TOKEN);
            // The token of a server that is no longer in the settings: not ours to carry.
            pc.put("mcp.gone.token", "mcp-VALUE-gone");
            pc
        }

        fn stores(&self) -> Stores<'_> {
            Stores {
                config: &self.config,
                db: &self.db,
                secrets: &self.secrets,
            }
        }

        fn put(&self, key: &str, value: &str) {
            self.secrets.set(key, &Secret::new(value)).unwrap();
        }

        fn has(&self, key: &str, value: &str) -> bool {
            self.secrets.get(key).unwrap() == Some(Secret::new(value))
        }

        fn lacks(&self, key: &str) -> bool {
            self.secrets.get(key).unwrap().is_none()
        }

        /// Which of the names a backup may write are stored.
        fn stored_keys(&self) -> Vec<String> {
            let mut names = fixed_secret_keys();
            for server in ["files", "remote", "gone"] {
                names.push(format!("mcp.{server}.token"));
            }
            names.retain(|key| !self.lacks(key));
            names
        }
    }

    /// A backup of a PC, with the lowest KDF cost a file may name.
    fn export_bytes(pc: &Pc, password: Option<&str>) -> (Vec<u8>, ExportReport) {
        let mut out = Vec::new();
        let report =
            export_with(&pc.stores(), &INFO, password, &mut out, KDF_ITERATIONS_MIN).unwrap();
        (out, report)
    }

    fn preview_of(file: &[u8], password: Option<&str>, here: &Pc) -> Result<Preview, BackupError> {
        let mut input = file;
        let header = read_header(&mut input)?;
        let access = unlock(&header, password)?;
        preview(
            &mut input,
            &header,
            &access,
            &here.stores(),
            &Limits::default(),
        )
    }

    fn restore_of(
        file: &[u8],
        password: Option<&str>,
        here: &Pc,
        selected: &[Category],
    ) -> Result<RestoreReport, BackupError> {
        let mut input = file;
        let header = read_header(&mut input)?;
        let access = unlock(&header, password)?;
        let selected: BTreeSet<Category> = selected.iter().copied().collect();
        restore(
            &mut input,
            &header,
            &access,
            &here.stores(),
            &selected,
            &Limits::default(),
        )
    }

    /// A plain file made by hand from payload lines.
    fn plain_file(lines: &[&str]) -> Vec<u8> {
        let (mut file, _) = format::encode_prefix("0.3.3", "2026-11-14", "HAND", 0, None);
        file.extend_from_slice(&gz(lines));
        file
    }

    /// An encrypted file made by hand from payload lines.
    fn sealed_file(lines: &[&str], password: &str) -> Vec<u8> {
        let params = test_params(password);
        let keys = test_keys(password, &params.salt);
        let (mut file, header) =
            format::encode_prefix("0.3.3", "2026-11-14", "HAND", 1, Some(params.clone()));
        let sealing = Sealing {
            keys,
            nonce: params.nonce,
            aad: header.prefix_sha256,
            chunk: params.chunk,
        };
        let mut body = ChunkWriter::new(Vec::new(), sealing);
        body.write_all(&gz(lines)).unwrap();
        file.extend_from_slice(&body.finish().unwrap());
        file
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }

    fn category(p: &Preview, id: Category) -> &CategoryPreview {
        p.categories.iter().find(|c| c.id == id).unwrap()
    }

    const BEGIN: &str = r#"{"t":"begin","schema":7}"#;
    const NOTES: &str =
        r#"{"t":"table","name":"notes","columns":["id","created_at","title","body"]}"#;
    const NOTE: &str = r#"{"t":"row","v":[1,"2026-11-10T10:00:00+05:00","Из копии","текст"]}"#;

    #[test]
    fn a_plain_backup_previews_and_restores_into_an_empty_pc() {
        let old = Pc::old();
        let source_before = snapshot(&old.db);
        let (file, report) = export_bytes(&old, None);
        assert!(!report.encrypted);
        assert_eq!(report.secrets, 0);
        let counts = vec![
            (Category::Settings, 1),
            (Category::History, 3),
            (Category::Memory, 2),
            (Category::Notes, 1),
            (Category::Todos, 2),
            (Category::Reminders, 1),
            (Category::Tasks, 1),
            (Category::Commands, 1),
            (Category::Scenarios, 2),
            (Category::RulesDeny, 1),
            (Category::RulesAllow, 2),
            (Category::Secrets, 0),
        ];
        assert_eq!(report.counts, counts);

        let header = read_header(&mut &file[..]).unwrap();
        assert!(!header.encrypted);
        assert_eq!(header.secrets, 0);
        assert_eq!(header.device, "OLD-PC");
        assert_eq!(header.app_version, "0.3.3");
        assert_eq!(header.created_at, "2026-11-14T15:30:00+05:00");
        // No password is needed, and one is ignored.
        assert!(unlock(&header, None).is_ok());
        assert!(unlock(&header, Some("whatever")).is_ok());

        let new = Pc::empty();
        let seen = preview_of(&file, None, &new).unwrap();
        assert_eq!(seen.categories.len(), Category::ALL.len());
        for (row, (id, in_file)) in seen.categories.iter().zip(&counts) {
            assert_eq!(row.id, *id);
            assert_eq!(row.in_file, *in_file, "{id:?}");
            assert_eq!(row.here, u64::from(*id == Category::Settings), "{id:?}");
            assert_eq!(row.available, *in_file > 0, "{id:?}");
            // The default tick: all that is there, but never what acts
            // without a question: the "always" rules and the scheduled tasks.
            assert_eq!(
                row.selected,
                row.available && !matches!(id, Category::RulesAllow | Category::Tasks),
                "{id:?}"
            );
        }
        assert!(category(&seen, Category::RulesAllow).available);
        assert!(category(&seen, Category::Tasks).available);
        assert_eq!(seen.tasks_total, 1);
        assert_eq!(
            seen.tasks,
            vec![TaskPreview {
                text: "Погода".into(),
                due_at: 1_794_300_600,
                repeat: "every:30".into(),
            }]
        );
        // The scenarios by event are ticked: they land switched off, so
        // nothing of them acts until the user turns it on here. Each one is
        // listed, with whether the saved command it names is at hand.
        assert!(category(&seen, Category::Scenarios).selected);
        assert_eq!(seen.scenarios_total, 2);
        let listed: Vec<(&str, bool, bool)> = seen
            .scenarios
            .iter()
            .map(|s| (s.name.as_str(), s.command_in_file, s.command_here))
            .collect();
        assert_eq!(
            listed,
            [("Наушники", false, false), ("Работа", true, false)]
        );
        assert!(!category(&seen, Category::Secrets).available);
        assert!(seen.secrets.is_empty());
        assert_eq!(seen.allow_rules_total, 2);
        assert_eq!(seen.allow_rules.len(), 2);
        assert_eq!(seen.allow_rules[0].tool, "open_app");
        assert_eq!(seen.allow_rules_dropped, 0);
        assert_eq!((seen.dropped_rows, seen.skipped_unknown), (0, 0));
        assert!(!seen.schema_newer);

        let ticked: Vec<Category> = seen
            .categories
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.id)
            .collect();
        let done = restore_of(&file, None, &new, &ticked).unwrap();
        assert_eq!(
            done.restored,
            vec![
                (Category::Settings, 1),
                (Category::History, 3),
                (Category::Memory, 2),
                (Category::Notes, 1),
                (Category::Todos, 2),
                (Category::Reminders, 1),
                (Category::Commands, 1),
                (Category::Scenarios, 2),
                (Category::RulesDeny, 1),
            ]
        );
        assert_eq!(done.scenarios_without_command, 0);
        assert_eq!(done.keys_written, 0);
        assert!(done.keys_failed.is_empty());
        assert!(new.stored_keys().is_empty());
        assert!(done.restart_needed);
        let landed = done.config.unwrap();
        assert_eq!(landed.assistant.name, "Айри");
        assert_eq!(landed.llm.base_url, "https://routerai.ru/api/v1");
        // The path to a program is the one of this PC, which has none.
        assert_eq!(landed.tools.ffmpeg_path, "");
        assert!(!landed.privacy.microphone_enabled && !landed.telegram.enabled);
        assert!(landed.assistant.onboarded);
        assert_eq!(
            done.switched_off,
            vec![
                SwitchedOff::Microphone,
                SwitchedOff::Screen,
                SwitchedOff::Telegram,
                SwitchedOff::Mcp { count: 1 },
                SwitchedOff::Docs { count: 1 },
                // Both scenarios of the old PC were on there.
                SwitchedOff::Scenarios { count: 2 },
            ]
        );
        assert_eq!(
            rows_of(
                &new.db,
                "SELECT name, enabled, fired_count FROM triggers ORDER BY name"
            ),
            [
                r#"Ok(Text("Наушники"))|Ok(Int(0))|Ok(Int(0))"#,
                r#"Ok(Text("Работа"))|Ok(Int(0))|Ok(Int(0))"#
            ]
        );
        assert_eq!(done.docs_folders, ["C:\\Users\\old\\Documents\\Работа"]);
        for table in ["messages", "notes", "todos", "commands"] {
            let sql = format!("SELECT * FROM {table} ORDER BY id");
            assert_eq!(rows_of(&new.db, &sql), rows_of(&old.db, &sql), "{table}");
        }
        // The scheduled task stayed in the file: nothing starts by itself.
        assert_eq!(
            rows_of(&new.db, "SELECT text, kind FROM reminders"),
            [r#"Ok(Text("Чайник"))|Ok(Text("remind"))"#]
        );
        // The allow rules stayed in the file.
        assert_eq!(
            rows_of(&new.db, "SELECT tool, decision FROM rules"),
            [r#"Ok(Text("kill_process"))|Ok(Text("deny"))"#]
        );
        // The source is as it was.
        assert_eq!(snapshot(&old.db), source_before);
    }

    #[test]
    fn an_encrypted_backup_brings_every_key_back() {
        let old = Pc::old();
        let (file, report) = export_bytes(&old, Some(PASSWORD));
        assert!(report.encrypted);
        assert_eq!(report.secrets, 5);
        assert_eq!(report.counts.last(), Some(&(Category::Secrets, 5)));

        let header = read_header(&mut &file[..]).unwrap();
        assert!(header.encrypted);
        assert_eq!(header.secrets, 5);
        assert!(matches!(
            unlock(&header, None),
            Err(BackupError::PasswordNeeded)
        ));
        assert!(matches!(
            unlock(&header, Some("correct horse batterz")),
            Err(BackupError::WrongPassword)
        ));
        assert!(matches!(
            unlock(&header, Some("")),
            Err(BackupError::WrongPassword)
        ));
        // Longer than any password a file was made with: refused unhashed.
        assert!(matches!(
            unlock(&header, Some(&"я".repeat(MAX_PASSWORD + 1))),
            Err(BackupError::WrongPassword)
        ));
        // A wrong password is told apart from a damaged file.
        assert!(matches!(
            preview_of(&file, Some("another password"), &Pc::empty()),
            Err(BackupError::WrongPassword)
        ));

        let new = Pc::empty();
        let seen = preview_of(&file, Some(PASSWORD), &new).unwrap();
        let secrets = category(&seen, Category::Secrets);
        assert_eq!((secrets.in_file, secrets.here), (5, 0));
        assert!(secrets.available && secrets.selected);
        let names: Vec<(SecretKind, Option<&str>)> = seen
            .secrets
            .iter()
            .map(|s| (s.kind, s.name.as_deref()))
            .collect();
        assert_eq!(
            names,
            [
                (SecretKind::LlmOpenai, None),
                (SecretKind::Speech, None),
                (SecretKind::Telegram, None),
                (SecretKind::Calendar, None),
                (SecretKind::Mcp, Some("remote")),
            ]
        );
        assert!(seen.secrets.iter().all(|s| !s.replaces));

        let done = restore_of(&file, Some(PASSWORD), &new, &Category::ALL).unwrap();
        assert_eq!(done.keys_written, 5);
        assert!(done.keys_failed.is_empty());
        assert_eq!(done.restored.last(), Some(&(Category::Secrets, 5)));
        assert_eq!(done.restored.len(), Category::ALL.len());
        assert!(new.has("llm.openai.api_key", LLM_KEY));
        assert!(new.has("speech.api_key", SPEECH_KEY));
        assert!(new.has("telegram.bot_token", TELEGRAM_TOKEN));
        assert!(new.has("calendar.ics_url", CALENDAR_LINK));
        // The restored settings have the server, so its token belongs.
        assert!(new.has("mcp.remote.token", MCP_TOKEN));
        assert_eq!(new.stored_keys().len(), 5);
    }

    #[test]
    fn an_mcp_token_needs_its_server_in_the_settings_in_force() {
        let (file, _) = export_bytes(&Pc::old(), Some(PASSWORD));
        // Keys without the settings: this PC has no such server.
        let new = Pc::empty();
        let done = restore_of(&file, Some(PASSWORD), &new, &[Category::Secrets]).unwrap();
        assert_eq!(done.keys_written, 4);
        assert!(done.keys_failed.is_empty());
        assert!(new.lacks("mcp.remote.token"));
        assert!(new.has("llm.openai.api_key", LLM_KEY));
        assert_eq!(done.restored, vec![(Category::Secrets, 4)]);
        assert!(done.config.is_none());

        // This PC has a server of that name: the token is written.
        let mut new = Pc::empty();
        new.config.mcp.servers = vec![McpServerConfig {
            name: " remote ".into(),
            ..Default::default()
        }];
        let done = restore_of(&file, Some(PASSWORD), &new, &[Category::Secrets]).unwrap();
        assert_eq!(done.keys_written, 5);
        assert!(new.has("mcp.remote.token", MCP_TOKEN));
    }

    #[test]
    fn a_key_value_is_nowhere_but_inside_the_encrypted_body() {
        let old = Pc::old();
        let (plain, _) = export_bytes(&old, None);
        let (sealed, _) = export_bytes(&old, Some(PASSWORD));

        // The plain file: neither as it is nor unpacked.
        let mut body = &plain[..];
        read_header(&mut body).unwrap();
        let unpacked = gunzip(body);
        assert!(contains(&unpacked, "Кота зовут Барсик"));
        for value in VALUES {
            assert!(!contains(&plain, value));
            assert!(!contains(&unpacked, value));
        }
        assert!(!contains(&unpacked, r#""t":"secret""#));
        // The encrypted file: nothing readable at all, the password included.
        for value in VALUES.iter().chain(&[PASSWORD, "Барсик", "Айри"]) {
            assert!(!contains(&sealed, value), "{value}");
        }

        // What the operations say about themselves.
        let new = Pc::empty();
        new.put("llm.openai.api_key", "sk-here-VALUE-0006");
        let header = read_header(&mut &sealed[..]).unwrap();
        let access = unlock(&header, Some(PASSWORD)).unwrap();
        assert_eq!(format!("{access:?}"), "Access(key)");
        assert_eq!(
            format!(
                "{:?}",
                unlock(&read_header(&mut &plain[..]).unwrap(), None).unwrap()
            ),
            "Access(plain)"
        );
        let seen = preview_of(&sealed, Some(PASSWORD), &new).unwrap();
        let done = restore_of(&sealed, Some(PASSWORD), &new, &Category::ALL).unwrap();
        assert_eq!(done.keys_written, 5);
        let mut said = vec![
            format!("{header:?}"),
            format!("{seen:?}"),
            serde_json::to_string(&seen).unwrap(),
            format!("{done:?}"),
        ];
        // Every way of failing on a file that holds keys.
        let mut flipped = sealed.clone();
        let last = flipped.len() - 20;
        flipped[last] ^= 1;
        let failures = [
            preview_of(&sealed, None, &new).unwrap_err(),
            preview_of(&sealed, Some("not the password"), &new).unwrap_err(),
            preview_of(&flipped, Some(PASSWORD), &new).unwrap_err(),
            preview_of(&sealed[..sealed.len() - 1], Some(PASSWORD), &new).unwrap_err(),
            restore_of(&sealed, Some(PASSWORD), &new, &[]).unwrap_err(),
            BackupError::PasswordShort(MIN_PASSWORD),
            BackupError::PasswordLong(MAX_PASSWORD),
            BackupError::NotBackup,
            BackupError::Newer(2),
            BackupError::Unsupported,
            BackupError::TooBig,
            BackupError::Io(std::io::Error::other("disk is full")),
            BackupError::Db(rusqlite::Error::InvalidQuery.into()),
            BackupError::Secrets(SecretError::Backend("store is locked".into())),
        ];
        for e in &failures {
            said.push(format!("{e:?}"));
            said.push(e.to_string());
        }
        for text in &said {
            for value in VALUES.iter().chain(&[PASSWORD, "sk-here-VALUE-0006"]) {
                assert!(!text.contains(value), "{value} in {text}");
            }
        }
        // The preview does name the keys.
        assert!(said[1].contains("Telegram"));
    }

    #[test]
    fn secret_lines_of_a_plain_file_are_ignored() {
        let file = plain_file(&[
            BEGIN,
            r#"{"t":"config","toml":"[assistant]\nname = \"Юки\"\n"}"#,
            r#"{"t":"secret","key":"llm.openai.api_key","value":"sk-planted"}"#,
            r#"{"t":"secret","key":"telegram.bot_token","value":"planted-token"}"#,
            NOTES,
            NOTE,
            r#"{"t":"end","lines":6}"#,
        ]);
        let here = Pc::empty();
        here.put("llm.openai.api_key", "sk-mine");
        let seen = preview_of(&file, None, &here).unwrap();
        let secrets = category(&seen, Category::Secrets);
        assert_eq!(secrets.in_file, 0);
        assert!(!secrets.available && !secrets.selected);
        assert!(seen.secrets.is_empty());
        assert_eq!(seen.skipped_unknown, 2);
        // Nothing in the file brings the key: an address change would warn.
        assert!(seen.addresses.iter().all(|a| !a.key_in_file));

        let done = restore_of(&file, None, &here, &Category::ALL).unwrap();
        assert_eq!(done.keys_written, 0);
        assert!(here.has("llm.openai.api_key", "sk-mine"));
        assert!(here.lacks("telegram.bot_token"));
        assert_eq!(
            done.restored,
            vec![(Category::Settings, 1), (Category::Notes, 1)]
        );
    }

    #[test]
    fn only_known_and_sane_keys_are_taken_from_an_encrypted_file() {
        let long = format!(
            r#"{{"t":"secret","key":"image.api_key","value":"{}"}}"#,
            "k".repeat(MAX_SECRET_CHARS + 1)
        );
        let longest = format!(
            r#"{{"t":"secret","key":"embedding.api_key","value":"{}"}}"#,
            "я".repeat(MAX_SECRET_CHARS)
        );
        let file = sealed_file(
            &[
                BEGIN,
                r#"{"t":"secret","key":"speech.api_key","value":"first"}"#,
                r#"{"t":"secret","key":"speech.api_key","value":"second"}"#,
                r#"{"t":"secret","key":"telegram.bot_token","value":""}"#,
                r#"{"t":"secret","key":"windows.login","value":"x"}"#,
                r#"{"t":"secret","key":"mcp..token","value":"x"}"#,
                &long,
                &longest,
                r#"{"t":"end","lines":8}"#,
            ],
            PASSWORD,
        );
        let here = Pc::empty();
        let seen = preview_of(&file, Some(PASSWORD), &here).unwrap();
        assert_eq!(category(&seen, Category::Secrets).in_file, 2);
        assert_eq!(seen.dropped_rows, 4);
        let kinds: Vec<SecretKind> = seen.secrets.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [SecretKind::Speech, SecretKind::Embedding]);
        // No settings in the file: that part cannot be restored.
        assert!(!category(&seen, Category::Settings).available);

        let done = restore_of(&file, Some(PASSWORD), &here, &Category::ALL).unwrap();
        assert_eq!(done.keys_written, 2);
        // A repeated name keeps its last value.
        assert!(here.has("speech.api_key", "second"));
        assert!(here.has("embedding.api_key", &"я".repeat(MAX_SECRET_CHARS)));
        assert_eq!(here.stored_keys().len(), 2);
    }

    #[test]
    fn keys_stay_as_they_are_when_secrets_are_not_selected() {
        let (file, _) = export_bytes(&Pc::old(), Some(PASSWORD));
        let here = Pc::empty();
        here.put("llm.openai.api_key", "sk-mine");
        let done = restore_of(
            &file,
            Some(PASSWORD),
            &here,
            &[Category::Settings, Category::Notes],
        )
        .unwrap();
        assert_eq!(done.keys_written, 0);
        assert_eq!(
            done.restored,
            vec![(Category::Settings, 1), (Category::Notes, 1)]
        );
        assert!(here.has("llm.openai.api_key", "sk-mine"));
        assert_eq!(here.stored_keys(), ["llm.openai.api_key"]);
    }

    #[test]
    fn a_key_the_store_refuses_is_named_and_the_rest_go_on() {
        let (file, _) = export_bytes(&Pc::old(), Some(PASSWORD));
        let mut here = Pc::empty();
        here.secrets.fail_set = Some("calendar.ics_url");
        let done = restore_of(&file, Some(PASSWORD), &here, &Category::ALL).unwrap();
        assert_eq!(done.keys_written, 4);
        assert_eq!(
            done.keys_failed,
            vec![SecretRef::fixed(SecretKind::Calendar)]
        );
        assert!(here.has("telegram.bot_token", TELEGRAM_TOKEN));
        assert!(here.lacks("calendar.ics_url"));
        assert!(done.restart_needed);
        // The data came in all the same.
        assert_eq!(
            rows_of(&here.db, "SELECT COUNT(*) FROM messages"),
            ["Ok(Int(3))"]
        );
    }

    #[test]
    fn a_preview_changes_nothing() {
        let (file, _) = export_bytes(&Pc::old(), Some(PASSWORD));
        let here = Pc::empty();
        here.put("speech.api_key", "sk-mine");
        here.db
            .with(|c| {
                c.execute_batch(
                    "INSERT INTO notes (created_at, title, body) VALUES ('d', 'Моя', 'здесь');
                     INSERT INTO rules (tool, pattern, decision, created_at) VALUES ('open_app', 'x', 'allow', 'd');",
                )
            })
            .unwrap();
        let (db_before, config_before) = (snapshot(&here.db), here.config.clone());

        let seen = preview_of(&file, Some(PASSWORD), &here).unwrap();
        assert_eq!(category(&seen, Category::Notes).here, 1);
        assert_eq!(category(&seen, Category::RulesAllow).here, 1);
        assert_eq!(category(&seen, Category::RulesDeny).here, 0);
        assert_eq!(category(&seen, Category::Secrets).here, 1);
        // The speech key of this PC would be replaced, and the preview says so.
        let speech = seen
            .secrets
            .iter()
            .find(|s| s.kind == SecretKind::Speech)
            .unwrap();
        assert!(speech.replaces);

        assert_eq!(snapshot(&here.db), db_before);
        assert_eq!(here.config, config_before);
        assert_eq!(here.stored_keys(), ["speech.api_key"]);
        assert!(here.has("speech.api_key", "sk-mine"));
    }

    #[test]
    fn a_restore_that_fails_late_leaves_everything_as_it_was() {
        let here = Pc::old();
        let (db_before, config_before, keys_before) =
            (snapshot(&here.db), here.config.clone(), here.stored_keys());
        let config = r#"{"t":"config","toml":"[assistant]\nname = \"Юки\"\n"}"#;
        let secret = r#"{"t":"secret","key":"llm.openai.api_key","value":"sk-from-file"}"#;
        let failing: Vec<Vec<&str>> = vec![
            // A row of the wrong width in the last table.
            vec![
                BEGIN,
                config,
                secret,
                NOTES,
                NOTE,
                r#"{"t":"table","name":"todos","columns":["id","created_at","text"]}"#,
                r#"{"t":"row","v":[1,"d","Дело"]}"#,
                r#"{"t":"row","v":[2,"d"]}"#,
                r#"{"t":"end","lines":8}"#,
            ],
            // The closing line counts wrong, or is not there.
            vec![
                BEGIN,
                config,
                secret,
                NOTES,
                NOTE,
                r#"{"t":"end","lines":4}"#,
            ],
            vec![BEGIN, config, secret, NOTES, NOTE],
        ];
        for lines in failing {
            let file = sealed_file(&lines, PASSWORD);
            assert!(
                matches!(
                    restore_of(&file, Some(PASSWORD), &here, &Category::ALL),
                    Err(BackupError::Damaged)
                ),
                "{lines:?}"
            );
            assert_eq!(snapshot(&here.db), db_before);
            assert_eq!(here.config, config_before);
            assert_eq!(here.stored_keys(), keys_before);
            assert!(here.has("llm.openai.api_key", LLM_KEY));
        }
        // The same lines with a proper end do restore: the rollback was the difference.
        let file = sealed_file(
            &[
                BEGIN,
                config,
                secret,
                NOTES,
                NOTE,
                r#"{"t":"end","lines":5}"#,
            ],
            PASSWORD,
        );
        let done = restore_of(&file, Some(PASSWORD), &here, &Category::ALL).unwrap();
        assert_eq!(
            done.restored,
            vec![
                (Category::Settings, 1),
                (Category::Notes, 1),
                (Category::Secrets, 1)
            ]
        );
        assert!(here.has("llm.openai.api_key", "sk-from-file"));
        assert_eq!(done.config.unwrap().assistant.name, "Юки");
    }

    #[test]
    fn a_damaged_or_oversized_file_changes_nothing() {
        let (sealed, _) = export_bytes(&Pc::old(), Some(PASSWORD));
        let (plain, _) = export_bytes(&Pc::old(), None);
        let here = Pc::empty();
        here.db
            .with(|c| {
                c.execute(
                    "INSERT INTO notes (created_at, title, body) VALUES ('d', 'Моя', 'здесь')",
                    [],
                )
            })
            .unwrap();
        let before = snapshot(&here.db);
        let prefix_len = {
            let mut rest = &sealed[..];
            read_header(&mut rest).unwrap();
            sealed.len() - rest.len()
        };

        let mut body_flipped = sealed.clone();
        body_flipped[prefix_len + 40] ^= 4;
        let mut cut = sealed.clone();
        cut.truncate(sealed.len() - 7);
        let mut padded = sealed.clone();
        padded.extend_from_slice(b"more");
        let mut plain_cut = plain.clone();
        plain_cut.truncate(plain.len() - 3);
        for (file, password) in [
            (&body_flipped, Some(PASSWORD)),
            (&cut, Some(PASSWORD)),
            (&padded, Some(PASSWORD)),
            (&sealed[..prefix_len].to_vec(), Some(PASSWORD)),
            (&plain_cut, None),
        ] {
            assert!(matches!(
                preview_of(file, password, &here),
                Err(BackupError::Damaged)
            ));
            assert!(matches!(
                restore_of(file, password, &here, &Category::ALL),
                Err(BackupError::Damaged)
            ));
            assert_eq!(snapshot(&here.db), before);
        }
        assert!(here.stored_keys().is_empty());

        // A body over the file limit is refused as too big, not as damaged.
        let mut input = &plain[..];
        let header = read_header(&mut input).unwrap();
        let access = unlock(&header, None).unwrap();
        let small = Limits {
            file: 100,
            ..Limits::default()
        };
        assert!(matches!(
            preview(&mut input, &header, &access, &here.stores(), &small),
            Err(BackupError::TooBig)
        ));
        let mut input = &plain[..];
        read_header(&mut input).unwrap();
        let rows = Limits {
            rows: 5,
            ..Limits::default()
        };
        let all: BTreeSet<Category> = Category::ALL.into_iter().collect();
        assert!(matches!(
            restore(&mut input, &header, &access, &here.stores(), &all, &rows),
            Err(BackupError::TooBig)
        ));
        assert_eq!(snapshot(&here.db), before);

        // An encrypted body opened without its key.
        let mut input = &sealed[..];
        let header = read_header(&mut input).unwrap();
        assert!(matches!(
            preview(
                &mut input,
                &header,
                &access,
                &here.stores(),
                &Limits::default()
            ),
            Err(BackupError::PasswordNeeded)
        ));
    }

    #[test]
    fn settings_that_do_not_read_leave_the_rest_restorable() {
        let file = plain_file(&[
            BEGIN,
            r#"{"t":"config","toml":"this is [not toml"}"#,
            NOTES,
            NOTE,
            r#"{"t":"later","x":1}"#,
            r#"{"t":"end","lines":5}"#,
        ]);
        let here = Pc::empty();
        let seen = preview_of(&file, None, &here).unwrap();
        let settings = category(&seen, Category::Settings);
        assert_eq!(settings.in_file, 0);
        assert!(!settings.available && !settings.selected);
        assert!(category(&seen, Category::Notes).selected);
        assert!(seen.addresses.is_empty() && seen.mcp.is_empty());
        assert!(seen.telegram.is_none() && seen.switched_off.is_empty());
        assert_eq!(seen.skipped_unknown, 1);

        let done = restore_of(&file, None, &here, &[Category::Settings, Category::Notes]).unwrap();
        assert_eq!(done.restored, vec![(Category::Notes, 1)]);
        assert!(done.config.is_none());
        assert!(!done.restart_needed);
        assert_eq!(
            rows_of(&here.db, "SELECT title FROM notes"),
            [r#"Ok(Text("Из копии"))"#]
        );

        // Settings of the wrong shape do not read either.
        let file = plain_file(&[
            BEGIN,
            r#"{"t":"config","toml":"[assistant]\nname = 5\n"}"#,
            r#"{"t":"end","lines":2}"#,
        ]);
        assert!(!category(&preview_of(&file, None, &here).unwrap(), Category::Settings).available);
        // A second settings line is passed over: the first one counts.
        let file = plain_file(&[
            BEGIN,
            r#"{"t":"config","toml":"[assistant]\nname = \"Первая\"\n"}"#,
            r#"{"t":"config","toml":"[assistant]\nname = \"Вторая\"\n"}"#,
            r#"{"t":"end","lines":3}"#,
        ]);
        let done = restore_of(&file, None, &here, &[Category::Settings]).unwrap();
        assert_eq!(done.config.unwrap().assistant.name, "Первая");
    }

    #[test]
    fn the_preview_shows_what_a_foreign_file_could_change() {
        let old = Pc::old();
        let (sealed, _) = export_bytes(&old, Some(PASSWORD));
        let (plain, _) = export_bytes(&old, None);
        let here = Pc::empty();
        here.put("llm.openai.api_key", "sk-mine");

        let seen = preview_of(&sealed, Some(PASSWORD), &here).unwrap();
        let llm = &seen.addresses[0];
        assert_eq!(llm.purpose, "llm");
        assert_eq!(llm.incoming, "https://routerai.ru/api/v1");
        assert_eq!(llm.current, "https://api.openai.com/v1");
        assert!(llm.changed && llm.key_here && llm.key_in_file);
        // Speech has an address of its own in the file; the slash at its end is dropped.
        let speech = seen
            .addresses
            .iter()
            .find(|a| a.purpose == "speech")
            .unwrap();
        assert_eq!(speech.incoming, "https://speech.example/v1");
        assert_eq!(speech.current, "https://api.openai.com/v1");
        // No speech key here, but the model key it falls back to is.
        assert!(speech.changed && speech.key_here && speech.key_in_file);
        // Pictures and embeddings follow the model's address.
        let purposes: Vec<&str> = seen.addresses.iter().map(|a| a.purpose).collect();
        assert_eq!(purposes, ["llm", "speech", "image", "embedding"]);
        assert_eq!(seen.addresses[2].incoming, "https://routerai.ru/api/v1");

        assert_eq!(
            seen.mcp,
            vec![
                McpPreview {
                    name: "files".into(),
                    target: "npx -y server-filesystem C:\\data".into(),
                    was_enabled: true,
                    current: String::new(),
                    changed: true,
                    token_here: false,
                    token_in_file: false,
                },
                McpPreview {
                    name: "remote".into(),
                    target: "https://mcp.example.com/mcp".into(),
                    was_enabled: false,
                    current: String::new(),
                    changed: true,
                    token_here: false,
                    token_in_file: true,
                },
            ]
        );
        assert_eq!(
            seen.telegram,
            Some(TelegramPreview {
                was_enabled: true,
                user_id: 123_456_789,
                current: 0,
                changed: true,
                token_here: false,
                token_in_file: true,
            })
        );
        assert_eq!(
            seen.switched_off,
            vec![
                SwitchedOff::Microphone,
                SwitchedOff::Screen,
                SwitchedOff::Telegram,
                SwitchedOff::Mcp { count: 1 },
                SwitchedOff::Docs { count: 1 },
            ]
        );
        assert_eq!(seen.docs_folders, ["C:\\Users\\old\\Documents\\Работа"]);

        // The same settings without keys: the key of this PC would go to the new address.
        let seen = preview_of(&plain, None, &here).unwrap();
        let llm = &seen.addresses[0];
        assert!(llm.changed && llm.key_here && !llm.key_in_file);

        // A PC without a key has nothing to leak; one with the same address has nothing to warn of.
        let seen = preview_of(&plain, None, &Pc::empty()).unwrap();
        assert!(seen.addresses[0].changed && !seen.addresses[0].key_here);
        let mut same = Pc::empty();
        same.config.llm.base_url = "HTTPS://RouterAI.ru/api/v1/".into();
        same.config.voice.speech_base_url = "https://speech.example/v1".into();
        let seen = preview_of(&plain, None, &same).unwrap();
        assert_eq!(seen.addresses.len(), 1);
        assert!(!seen.addresses[0].changed);
        // The key is looked up for the provider kind of the file.
        let mut other_kind = Pc::old();
        other_kind.config.llm.kind = ProviderKind::Anthropic;
        other_kind.config.llm.base_url.clear();
        let (file, _) = export_bytes(&other_kind, None);
        let seen = preview_of(&file, None, &here).unwrap();
        assert_eq!(seen.addresses[0].incoming, "https://api.anthropic.com");
        assert!(!seen.addresses[0].key_here);

        // The shape the interface reads.
        let json = serde_json::to_value(&seen).unwrap();
        assert_eq!(json["categories"][0]["id"], "settings");
        assert_eq!(json["categories"][6]["id"], "tasks");
        assert_eq!(json["categories"][6]["selected"], false);
        assert_eq!(json["categories"][8]["id"], "scenarios");
        assert_eq!(json["categories"][8]["selected"], true);
        assert_eq!(json["categories"][10]["id"], "rules_allow");
        assert_eq!(json["categories"][10]["selected"], false);
        assert_eq!(json["tasks"][0]["repeat"], "every:30");
        assert_eq!(json["scenarios_total"], 2);
        assert_eq!(json["scenarios"][0]["event"], "audio_connected");
        assert_eq!(json["scenarios"][0]["action"]["kind"], "instruction");
        assert_eq!(json["scenarios"][1]["action"]["text"], "Рабочий режим");
        assert_eq!(json["mcp"][0]["changed"], true);
        assert_eq!(json["telegram"]["token_here"], false);
        assert_eq!(json["addresses"][0]["purpose"], "llm");
        assert_eq!(json["switched_off"][0]["id"], "microphone");
        assert_eq!(json["telegram"]["user_id"], 123_456_789);
    }

    #[test]
    fn a_key_of_this_pc_that_stays_is_warned_about() {
        // The file brings the model key and addresses of its own, but no
        // speech key. This PC has a speech key: it is the one that would
        // be sent to the speech address of the file.
        let mut old = Pc::empty();
        old.config.llm.base_url = "https://routerai.ru/api/v1".into();
        old.config.voice.speech_base_url = "https://other.example/v1".into();
        old.put("llm.openai.api_key", LLM_KEY);
        let (file, _) = export_bytes(&old, Some(PASSWORD));
        let address = |seen: &Preview, purpose: &str| {
            seen.addresses
                .iter()
                .find(|a| a.purpose == purpose)
                .cloned()
                .unwrap()
        };

        let here = Pc::empty();
        here.put("speech.api_key", "sk-mine-speech");
        let seen = preview_of(&file, Some(PASSWORD), &here).unwrap();
        let speech = address(&seen, "speech");
        assert!(speech.changed && speech.key_here);
        assert!(!speech.key_in_file);
        // The same for pictures and embeddings, which follow the model's address.
        here.put("image.api_key", "sk-mine-image");
        here.put("embedding.api_key", "sk-mine-embedding");
        let seen = preview_of(&file, Some(PASSWORD), &here).unwrap();
        assert!(!address(&seen, "image").key_in_file);
        assert!(!address(&seen, "embedding").key_in_file);
        // The model itself has no key here: the file's key goes to the file's address.
        assert!(address(&seen, "llm").key_in_file && !address(&seen, "llm").key_here);

        // No key of that purpose here: the model key of the file is the one sent.
        let here = Pc::empty();
        here.put("llm.openai.api_key", "sk-mine");
        let speech = address(&preview_of(&file, Some(PASSWORD), &here).unwrap(), "speech");
        assert!(speech.key_here && speech.key_in_file);

        // The file brings the speech key itself: it replaces the one here.
        old.put("speech.api_key", SPEECH_KEY);
        let (file, _) = export_bytes(&old, Some(PASSWORD));
        let here = Pc::empty();
        here.put("speech.api_key", "sk-mine-speech");
        let speech = address(&preview_of(&file, Some(PASSWORD), &here).unwrap(), "speech");
        assert!(speech.key_here && speech.key_in_file);
    }

    #[test]
    fn a_token_of_this_pc_that_would_follow_the_file_is_pointed_out() {
        // A file without keys: a server named like one of this PC, at
        // another address, and another Telegram owner.
        let mut foreign = Pc::empty();
        foreign.config.mcp.servers = vec![
            McpServerConfig {
                name: "files".into(),
                url: "https://other.example/mcp".into(),
                ..Default::default()
            },
            McpServerConfig {
                name: "same".into(),
                url: "https://same.example/mcp".into(),
                ..Default::default()
            },
        ];
        foreign.config.telegram.user_id = 999;
        let (file, _) = export_bytes(&foreign, None);

        let mut here = Pc::empty();
        here.config.mcp.servers = vec![
            McpServerConfig {
                name: " files ".into(),
                url: "https://mine.example/mcp".into(),
                enabled: true,
                ..Default::default()
            },
            McpServerConfig {
                name: "same".into(),
                url: "https://same.example/mcp".into(),
                ..Default::default()
            },
        ];
        here.config.telegram.user_id = 111;
        here.put("mcp.files.token", "mcp-mine");
        here.put("mcp.same.token", "mcp-same");
        here.put("telegram.bot_token", "bot-mine");

        let seen = preview_of(&file, None, &here).unwrap();
        let files = &seen.mcp[0];
        assert_eq!(files.current, "https://mine.example/mcp");
        assert!(files.changed && files.token_here && !files.token_in_file);
        // The same target: the token goes where it went before.
        let same = &seen.mcp[1];
        assert!(!same.changed && same.token_here);
        let telegram = seen.telegram.unwrap();
        assert_eq!((telegram.user_id, telegram.current), (999, 111));
        assert!(telegram.changed && telegram.token_here && !telegram.token_in_file);

        // The same owner here: nothing changes hands.
        here.config.telegram.user_id = 999;
        let telegram = preview_of(&file, None, &here).unwrap().telegram.unwrap();
        assert!(!telegram.changed);
        // No token here: nothing of this PC follows the file.
        let seen = preview_of(&file, None, &Pc::empty()).unwrap();
        assert!(seen.mcp[0].changed && !seen.mcp[0].token_here);
        assert!(!seen.telegram.unwrap().token_here);
    }

    #[test]
    fn a_scheduled_task_comes_in_only_by_its_own_tick() {
        let (file, _) = export_bytes(&Pc::old(), None);
        // Everything but the tasks: the reminder comes, the task does not.
        let new = Pc::empty();
        let most: Vec<Category> = Category::ALL
            .into_iter()
            .filter(|c| *c != Category::Tasks)
            .collect();
        let done = restore_of(&file, None, &new, &most).unwrap();
        assert!(!done.restored.iter().any(|(c, _)| *c == Category::Tasks));
        assert_eq!(
            rows_of(
                &new.db,
                "SELECT COUNT(*) FROM reminders WHERE kind = 'task'"
            ),
            ["Ok(Int(0))"]
        );
        // Ticked: it is restored, as the preview listed it.
        let done = restore_of(&file, None, &new, &[Category::Tasks]).unwrap();
        assert_eq!(done.restored, vec![(Category::Tasks, 1)]);
        assert!(!done.restart_needed);
        assert_eq!(
            rows_of(
                &new.db,
                "SELECT text, repeat, due_at FROM reminders WHERE kind = 'task'"
            ),
            [r#"Ok(Text("Погода"))|Ok(Text("every:30"))|Ok(Int(1794300600))"#]
        );
        // The reminder restored before is still there.
        assert_eq!(
            rows_of(
                &new.db,
                "SELECT COUNT(*) FROM reminders WHERE kind = 'remind'"
            ),
            ["Ok(Int(1))"]
        );
    }

    /// A reader that asks the database a question at its first read.
    struct AsksTheDatabase<'a> {
        inner: &'a [u8],
        db: &'a Db,
        asked: bool,
    }

    impl Read for AsksTheDatabase<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.asked {
                self.asked = true;
                // Waits for ever if the preview holds the database.
                self.db
                    .with(|c| c.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get::<_, i64>(0)))
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
            }
            self.inner.read(buf)
        }
    }

    #[test]
    fn a_preview_does_not_hold_the_database_while_it_reads_the_file() {
        let (file, _) = export_bytes(&Pc::old(), None);
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let here = Pc::empty();
            let mut input = &file[..];
            let header = read_header(&mut input).unwrap();
            let access = unlock(&header, None).unwrap();
            let mut body = AsksTheDatabase {
                inner: input,
                db: &here.db,
                asked: false,
            };
            let seen = preview(
                &mut body,
                &header,
                &access,
                &here.stores(),
                &Limits::default(),
            );
            assert!(body.asked);
            let _ = done.send(seen.map(|p| p.categories.len()).ok());
        });
        let seen = result
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the preview kept the database locked while reading the file");
        assert_eq!(seen, Some(Category::ALL.len()));
    }

    #[test]
    fn scenarios_alone_need_no_restart_and_are_told_as_switched_off() {
        let old = Pc::old();
        let (file, _) = export_bytes(&old, None);
        let new = Pc::empty();
        // Without the commands the scenario of the saved command stays out.
        let done = restore_of(&file, None, &new, &[Category::Scenarios]).unwrap();
        assert_eq!(done.restored, vec![(Category::Scenarios, 1)]);
        assert_eq!(done.scenarios_without_command, 1);
        assert_eq!(done.switched_off, vec![SwitchedOff::Scenarios { count: 1 }]);
        assert!(!done.restart_needed);
        assert!(done.config.is_none());
        // With them both come, and nothing is left out.
        let done = restore_of(
            &file,
            None,
            &new,
            &[Category::Commands, Category::Scenarios],
        )
        .unwrap();
        assert_eq!(
            done.restored,
            vec![(Category::Commands, 1), (Category::Scenarios, 2)]
        );
        assert_eq!(done.scenarios_without_command, 0);
        assert_eq!(done.switched_off, vec![SwitchedOff::Scenarios { count: 2 }]);
        // The watches of the old PC lived in memory: no file holds them.
        assert!(!contains(&file, "wait_for"));
    }

    #[test]
    fn a_restart_is_asked_only_when_something_is_read_at_start() {
        let (file, _) = export_bytes(&Pc::old(), Some(PASSWORD));
        let restart = |selected: &[Category]| {
            restore_of(&file, Some(PASSWORD), &Pc::empty(), selected)
                .unwrap()
                .restart_needed
        };
        assert!(!restart(&[Category::Notes]));
        assert!(!restart(&[
            Category::Notes,
            Category::Todos,
            Category::Memory,
            Category::Reminders,
            Category::Tasks,
            Category::Commands,
            Category::RulesDeny,
            Category::RulesAllow,
        ]));
        assert!(restart(&[Category::Settings]));
        assert!(restart(&[Category::History]));
        assert!(restart(&[Category::Secrets]));
        assert!(restart(&[Category::Notes, Category::History]));
        assert!(matches!(
            restore_of(&file, Some(PASSWORD), &Pc::empty(), &[]),
            Err(BackupError::NothingSelected)
        ));
    }

    #[test]
    fn an_export_refuses_before_the_first_byte() {
        let old = Pc::old();
        let mut out = Vec::new();
        assert!(matches!(
            export(&old.stores(), &INFO, Some("short"), &mut out),
            Err(BackupError::PasswordShort(MIN_PASSWORD))
        ));
        assert!(matches!(
            export(
                &old.stores(),
                &INFO,
                Some(&"я".repeat(MAX_PASSWORD + 1)),
                &mut out
            ),
            Err(BackupError::PasswordLong(MAX_PASSWORD))
        ));
        assert!(out.is_empty());
        // The secret store does not answer: no file without the keys it was asked for.
        let mut locked = Pc::old();
        locked.secrets.fail_get = true;
        assert!(matches!(
            export(&locked.stores(), &INFO, Some(PASSWORD), &mut out),
            Err(BackupError::Secrets(_))
        ));
        assert!(out.is_empty());
        // Without a password the store is not even asked.
        let report = export(&locked.stores(), &INFO, None, &mut out).unwrap();
        assert!(!report.encrypted && !out.is_empty());
    }

    #[test]
    fn two_backups_of_the_same_data_share_no_salt_and_no_nonce() {
        let old = Pc::old();
        let (a, _) = export_bytes(&old, Some(PASSWORD));
        let (b, _) = export_bytes(&old, Some(PASSWORD));
        let (ha, hb) = (
            read_header(&mut &a[..]).unwrap(),
            read_header(&mut &b[..]).unwrap(),
        );
        let (ca, cb) = (ha.crypto.unwrap(), hb.crypto.unwrap());
        assert_ne!(ca.salt, cb.salt);
        assert_ne!(ca.nonce, cb.nonce);
        assert_ne!(ca.check, cb.check);
        assert_eq!(ca.salt.len(), 16);
        assert_eq!(ca.chunk, CHUNK);
        assert_eq!(a.len(), b.len());
        assert_ne!(a[a.len() - 64..], b[b.len() - 64..]);
        // Plain backups of the same data are the same bytes.
        assert_eq!(export_bytes(&old, None).0, export_bytes(&old, None).0);
    }

    #[test]
    fn a_real_backup_uses_the_full_kdf_cost() {
        let mut out = Vec::new();
        let report = export(&Pc::empty().stores(), &INFO, Some(PASSWORD), &mut out).unwrap();
        assert!(report.encrypted);
        let header = read_header(&mut &out[..]).unwrap();
        assert_eq!(header.crypto.as_ref().unwrap().iterations, KDF_ITERATIONS);
        assert_eq!(KDF_ITERATIONS, 600_000);
        assert!(unlock(&header, Some(PASSWORD)).is_ok());
    }

    /// The PC the embedded format 1 file was made on.
    fn fixture_pc() -> Pc {
        let mut pc = Pc::empty();
        pc.config.assistant.name = "Айри".into();
        pc.db
            .with(|c| {
                c.execute_batch(
                    "INSERT INTO notes (id, created_at, title, body)
                         VALUES (1, '2026-11-10T10:00:00+05:00', 'Заметка', 'Текст заметки');
                     INSERT INTO facts (id, created_at, text)
                         VALUES (1, '2026-11-10T10:00:00+05:00', 'Кота зовут Барсик');
                     INSERT INTO rules (tool, pattern, decision, created_at)
                         VALUES ('open_app', 'проводник', 'allow', '2026-11-10T22:00:00+03:00'),
                                ('kill_process', 'explorer.exe', 'deny', '2026-11-10T22:07:00+03:00');",
                )
            })
            .unwrap();
        pc.put("speech.api_key", "sk-fixture-speech");
        pc
    }

    /// A format 1 file made once by `export_with` from `fixture_pc()`: password
    /// "correct horse", the lowest KDF cost a file may name. Hex of its bytes.
    /// Never made again: a later version must keep opening exactly these bytes.
    const FORMAT_1_FILE: &str = "\
         4d494b412d424b500100350100007b226170705f76657273696f6e223a22302e332e33222c226368\
         65636b223a22335a486d346b4d476f4578455a384465487341384c71586879705755515933457a53\
         53543267726f5648673d222c226368756e6b223a313034383537362c22636970686572223a226165\
         732d3235362d67636d222c22637265617465645f6174223a22323032362d31312d31345431353a33\
         303a30302b30353a3030222c22646576696365223a224f4c442d5043222c22656e63727970746564\
         223a747275652c22697465726174696f6e73223a3130303030302c226b6466223a2270626b646632\
         2d686d61632d736861323536222c226e6f6e6365223a2258414d507453755a49673d3d222c227361\
         6c74223a226e48726851555262524f614e6356416b537a355172773d3d222c227365637265747322\
         3a317d2e04000040d7016d531c64f445c2a8082f93339a717b0950104d734d30aa55b821d9f4075f\
         a90fc5ba02cb64742f86efab36bf66563cac6a9b9d6bf2932e2e90da38c08629a091fa178f765a57\
         1109dfb6d73765872a3b935baded3929a97e9c30b104a8fbe40d73352862429e7a241887023cd41f\
         e608402f73d0c0a86a0165a8afe81cb52548708b9543f2510c33697f8513f68dda68368789e46f83\
         748461002c7f655a6db88de162260208406b0092ff631b6cd85c4491d5fc4402cf01e3296744ed46\
         f7b4d70eacea9451c08283d9ab1cf1b85c6dea59f22ac1110396b1d2268295adb3bb4ae1dd7f6380\
         838de8958ff0773954c3b5e8502476b49cd94aad3907fdf6120ce36e77498dee2bad0aeae9801209\
         09be74730c9a6d494c7670119ef303b0e7d6c95e9e1e4b4a230499d1bf7f09080adfb6157a99fa2e\
         53f151dd5caffd3918c7bbc563941fecfc6adb1818c9332295fdf2e5d5cb2ace79c57a2ec66b35b5\
         4c2147bdbcfd747c4f3831707ea47137e4317abb6581a0be79574af5a5cf1b82b26e79626bd588cc\
         5dcddccc2b449c873b4545900161327ac2494753347766562082c01a177f0d5af0e8de808b20dbdf\
         842a445c86118135e04d015ec67c7de1eca737addc2690727d82fa90a4833ba404c32dce80a7d8b2\
         e7fc13e2a0dfd9905bedc140d2cffcc624920d0d69143b0aacf355377d24baf48c1cf264d52679fc\
         07ae1faae59edcadb92bf7496f3de2a342c8b20e83b49c6b590f6505d60f77ed24b0a1804b12acf7\
         f623d9f5c168bbd316b9d90ca7ef1cbac9fa15fa662d80f2a480d4ac61e111845fffac9da089ef07\
         e5d7d654d83c1c8f9cb495ad6f590fe3834dc0e76fec51346af9ec5db018aa02dcb81eebda30efcd\
         7bc949d2f028ded33fae11845da9afe3a868fbaa5ca6345e8512db67b98f5dcde726ab0be97722ea\
         c5746a87314d717f06ae8ada637b4a28d245b801c192eb458227388e69bac9419d72fffe8768c14d\
         a37fb2c27041a70ce3c22a4dc5a60e439745f86e435d7c81aa0b35f23b42e74c3e538aa324292b74\
         1dcc64be35e74f2ba69821aa1950e0e2077edee0ba0e8672d01c53495a0ef97a7aea5b81ce7113ca\
         01bfafb2962e2a3f9961533aca27dbe99d7c53768b295bb10fe72f59fd55efa5c70f422a927cd9bf\
         b6fe7f70a3dee14e0d50ca42ed221be0fe0021523635563bc01f212dea5c2f2188147bb6b0b3a8de\
         d6e8001b0f399009467e3fabbaa882f4f09b8867fc8898c1245c2dac2a95fecd52b3e149dc7b77ef\
         f30f3d5e005070bdb684a464cf73d65c28126943af28c5f91c073ac6dd22fc0be0b5c2bbb0dd8720\
         7a3e8ff909e4111a76c922a6d12356b9a86ef0b763d6708ee42c8ea9853819dda5cedcb563a2b9ad\
         c0148049bd916f49e66ba89ce864a28f694d6f2cc9419e725bdb42508041b60a0e6ada06f20392f4\
         1e3f40a53d8a27ca5350c1fcbacff95a1537fc5d7d822ee7dd76634d5df7bee88cca5a080b\
         ";

    #[test]
    fn a_format_1_file_keeps_opening() {
        let file: Vec<u8> = (0..FORMAT_1_FILE.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&FORMAT_1_FILE[i..i + 2], 16).unwrap())
            .collect();
        let made_on = fixture_pc();

        let header = read_header(&mut &file[..]).unwrap();
        assert!(header.encrypted);
        assert_eq!(header.secrets, 1);
        assert_eq!(header.app_version, "0.3.3");
        assert_eq!(header.created_at, "2026-11-14T15:30:00+05:00");
        assert_eq!(header.device, "OLD-PC");
        let params = header.crypto.as_ref().unwrap();
        assert_eq!(params.iterations, KDF_ITERATIONS_MIN);
        assert_eq!(params.chunk, 1 << 20);
        assert!(matches!(
            unlock(&header, Some("correct horsf")),
            Err(BackupError::WrongPassword)
        ));

        let new = Pc::empty();
        let seen = preview_of(&file, Some("correct horse"), &new).unwrap();
        let in_file: Vec<u64> = seen.categories.iter().map(|c| c.in_file).collect();
        // Settings, history, memory, notes, todos, reminders, tasks, commands,
        // scenarios (a file of that time has none), denies, allows, keys.
        assert_eq!(in_file, [1, 0, 1, 1, 0, 0, 0, 0, 0, 1, 1, 1]);
        assert_eq!(
            seen.allow_rules,
            vec![RulePreview {
                tool: "open_app".into(),
                pattern: "проводник".into()
            }]
        );
        assert_eq!(seen.secrets[0].kind, SecretKind::Speech);
        assert_eq!((seen.dropped_rows, seen.skipped_unknown), (0, 0));
        assert!(!seen.schema_newer);

        let done = restore_of(&file, Some("correct horse"), &new, &Category::ALL).unwrap();
        assert_eq!(
            done.restored,
            vec![
                (Category::Settings, 1),
                (Category::Memory, 1),
                (Category::Notes, 1),
                (Category::RulesDeny, 1),
                (Category::RulesAllow, 1),
                (Category::Secrets, 1),
            ]
        );
        assert_eq!(done.config.unwrap().assistant.name, "Айри");
        assert!(new.has("speech.api_key", "sk-fixture-speech"));
        for sql in [
            "SELECT id, created_at, title, body FROM notes",
            "SELECT id, created_at, text FROM facts",
            "SELECT tool, pattern, decision, created_at FROM rules ORDER BY tool",
        ] {
            assert_eq!(rows_of(&new.db, sql), rows_of(&made_on.db, sql), "{sql}");
        }
    }

    #[test]
    fn a_class_rule_is_shown_and_comes_back_only_when_ticked() {
        let old = Pc::empty();
        old.db
            .with(|c| {
                c.execute(
                    "INSERT INTO rules (tool, pattern, decision, created_at)
                     VALUES ('run_shell', ?1, 'allow', '2026-11-17T10:00:00+05:00')",
                    [crate::policy::RuleClass::SystemInfo.pattern()],
                )
            })
            .unwrap();
        let (file, _) = export_bytes(&old, None);
        let new = Pc::empty();
        let seen = preview_of(&file, None, &new).unwrap();
        assert_eq!(
            seen.allow_rules,
            vec![RulePreview {
                tool: "run_shell".into(),
                pattern: "class:system-info".into()
            }]
        );
        let allows = category(&seen, Category::RulesAllow);
        assert!(allows.available && !allows.selected);
        let sql = "SELECT tool, pattern, decision FROM rules";
        restore_of(&file, None, &new, &[Category::Notes]).unwrap();
        assert!(rows_of(&new.db, sql).is_empty());
        restore_of(&file, None, &new, &[Category::RulesAllow]).unwrap();
        assert_eq!(rows_of(&new.db, sql), rows_of(&old.db, sql));
    }

    #[test]
    fn a_newer_schema_and_unknown_parts_are_reported() {
        let file = plain_file(&[
            r#"{"t":"begin","schema":99}"#,
            r#"{"t":"avatar","model":"x"}"#,
            r#"{"t":"table","name":"dreams","columns":["id"]}"#,
            r#"{"t":"row","v":[1]}"#,
            r#"{"t":"row","v":[2]}"#,
            NOTES,
            NOTE,
            r#"{"t":"row","v":[2,"d","без текста",null]}"#,
            r#"{"t":"end","lines":8}"#,
        ]);
        let seen = preview_of(&file, None, &Pc::empty()).unwrap();
        assert!(seen.schema_newer);
        // The unknown line and the unknown table; its rows go with it.
        assert_eq!(seen.skipped_unknown, 2);
        assert_eq!(seen.dropped_rows, 1);
        assert_eq!(category(&seen, Category::Notes).in_file, 1);
    }
}
