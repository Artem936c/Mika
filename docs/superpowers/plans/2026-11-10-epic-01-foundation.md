# Эпик 1 «Каркас» Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Рабочее пространство Cargo с ядром, терминальным клиентом и окном-островком Tauri, в которое следующие эпики добавляют функции.

**Architecture:** Ядро `mika-core` не зависит от Windows и Tauri и содержит шину событий, состояние помощника, настройки и хранилище секретов. `mika-cli` и `mika-gui` подписываются на шину. Интерфейс на Svelte получает события через мост Tauri и только рисует.

**Tech Stack:** Rust stable (редакция 2021), tokio 1, serde 1, toml 0.9, thiserror 2, log 0.4, parking_lot 0.12, keyring 3 (`windows-native`), Tauri 2, Svelte 5, Vite, Vitest.

**Spec:** [docs/superpowers/specs/2026-11-10-mika-assistant-design.md](../specs/2026-11-10-mika-assistant-design.md)

## Global Constraints

- Платформа: Windows 10 19041+ и Windows 11, x64.
- Rust: редакция 2021, стандартный `rustfmt` без своих настроек.
- Версии зависимостей задаются один раз в `[workspace.dependencies]` корневого `Cargo.toml`. Крейты пишут `dep.workspace = true`.
- Модули в виде `foo.rs` рядом с папкой `foo/`. Файлов `mod.rs` нет.
- `mika-core` не зависит от `tauri`, `windows` и других платформенных крейтов, кроме `keyring`.
- Настройки: `%APPDATA%\Mika\config.toml`.
- Ключи: Windows Credential Manager, служба `Mika`. Ключи никогда не пишутся в `config.toml` и в журнал.
- Микрофон и просмотр экрана по умолчанию выключены.
- Журнал через `log`, блокировки через `parking_lot`.
- Код Priler/jarvis не копируется.
- Формы островка, px: покой 236 × 44, слушает 340 × 52, думает 380 × 52, отвечает 620 × 78, подтверждение 600 × 268.
- Цвет островка `#06070b`, переход формы `620ms cubic-bezier(.32,1.28,.54,1)`.

## Review Focus

1. Файл настроек сохранён Блокнотом с BOM. Ожидание: он загружается как обычный. Тест в задаче 2.
2. Файл настроек повреждён. Ожидание: приложение стартует с настройками по умолчанию, исходный файл сохранён рядом. Тест в задаче 2.
3. Монитор уже холста окна или стоит слева от основного (отрицательные координаты). Ожидание: окно остаётся в пределах монитора. Тест в задаче 5.
4. Ключ провайдера попадает в журнал через `{:?}`. Ожидание: вместо значения печатается `***`. Тест в задаче 3.
5. В терминальный клиент приходит строка не в UTF-8 (перенаправление ввода в старой кодировке консоли). Ожидание: клиент не падает и продолжает читать. Тест в задаче 4.

## Перед началом

Проверено 2026-11-10 на машине разработки:

| Что | Состояние |
|---|---|
| Rust 1.99.1 stable, `x86_64-pc-windows-msvc` | установлен |
| Tauri CLI 2 | установлен |
| MSVC Build Tools 2022 (14.44) | установлен |
| WebView2 Runtime (157) | установлен |
| Node.js 24.11, npm 11.6 | установлены |
| Git 2.53 | установлен |

- [ ] Установить Rust: `winget install Rustlang.Rustup`, затем открыть новый терминал.
- [ ] Проверить: `rustc --version` печатает версию stable для `x86_64-pc-windows-msvc`.
- [ ] Установить Tauri CLI: `cargo install tauri-cli --version "^2" --locked`.
- [ ] Прочитать решение `docs/decisions/0001-island-window.md`. Задача 5 построена на нём: прозрачный холст 1000 × 600, островок меняет форму внутри, мышь проходит насквозь везде, кроме островка.

## Файлы

```
Cargo.toml                         рабочее пространство, общие зависимости
rust-toolchain.toml                канал stable
crates/mika-core/src/lib.rs        объявление модулей
crates/mika-core/src/state.rs      AssistantState, StateHandle
crates/mika-core/src/events.rs     Event, EventBus
crates/mika-core/src/paths.rs      папки данных
crates/mika-core/src/config.rs     Config и его части
crates/mika-core/src/secrets.rs    Secret, SecretStore, две реализации
crates/mika-cli/src/main.rs        цикл чтения stdin
crates/mika-cli/src/repl.rs        разбор строки, формат вывода
crates/mika-gui/src/main.rs        запуск Tauri
crates/mika-gui/src/bridge.rs      мост шины в интерфейс, команды
crates/mika-gui/src/window.rs      положение окна
crates/mika-gui/tauri.conf.json    настройки окна
frontend/src/lib/island.ts         формы островка, свёртка событий
frontend/src/lib/Island.svelte     островок
frontend/src/App.svelte            подписка на события
.github/workflows/ci.yml           проверки
```

---

### Task 1: Рабочее пространство, состояние и шина событий

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`
- Create: `crates/mika-core/Cargo.toml`, `crates/mika-core/src/lib.rs`
- Create: `crates/mika-core/src/state.rs`, `crates/mika-core/src/events.rs`
- Test: в тех же файлах, `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: ничего.
- Produces:

```rust
// state.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantState { Idle, Listening, Thinking, Speaking, AwaitingConfirm }

#[derive(Clone)]
pub struct StateHandle { /* Arc<Mutex<AssistantState>> + EventBus */ }
impl StateHandle {
    pub fn new(bus: EventBus) -> Self;            // начальное состояние Idle
    pub fn get(&self) -> AssistantState;
    pub fn set(&self, next: AssistantState) -> bool; // false, если состояние не изменилось
}

// events.rs
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    StateChanged { from: AssistantState, to: AssistantState },
    UserText { text: String },
    AssistantText { text: String },
    Error { message: String },
}

#[derive(Clone)]
pub struct EventBus { /* tokio::sync::broadcast::Sender<Event> */ }
impl EventBus {
    pub fn new(capacity: usize) -> Self;
    pub fn publish(&self, event: Event) -> usize;  // число получателей, 0 без подписчиков
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event>;
}
```

- [ ] **Step 1: Создать рабочее пространство**

Корневой `Cargo.toml`: `[workspace]` с `resolver = "2"` и `members = ["crates/*"]`; `[workspace.package]` с `version = "0.1.0"`, `edition = "2021"`; `[workspace.dependencies]` со всеми зависимостями из раздела Tech Stack. `rust-toolchain.toml`: `channel = "stable"`, `components = ["rustfmt", "clippy"]`. `mika-core` берёт `serde` (с `derive`), `serde_json`, `tokio` (с `sync`, `rt`, `macros`), `parking_lot`, `log`.

- [ ] **Step 2: Написать падающие тесты шины и состояния**

```rust
// events.rs
#[test]
fn publish_without_subscribers_returns_zero() {
    let bus = EventBus::new(8);
    assert_eq!(bus.publish(Event::UserText { text: "a".into() }), 0);
}

#[tokio::test]
async fn two_subscribers_both_receive() {
    let bus = EventBus::new(8);
    let (mut a, mut b) = (bus.subscribe(), bus.subscribe());
    assert_eq!(bus.publish(Event::UserText { text: "привет".into() }), 2);
    assert_eq!(a.recv().await.unwrap(), Event::UserText { text: "привет".into() });
    assert_eq!(b.recv().await.unwrap(), Event::UserText { text: "привет".into() });
}

#[tokio::test]
async fn lagged_subscriber_gets_lagged_error_and_recovers() {
    use tokio::sync::broadcast::error::RecvError;
    let bus = EventBus::new(2);
    let mut rx = bus.subscribe();
    for i in 0..3 { bus.publish(Event::UserText { text: i.to_string() }); }
    assert!(matches!(rx.recv().await, Err(RecvError::Lagged(1))));
    assert_eq!(rx.recv().await.unwrap(), Event::UserText { text: "1".into() });
}

#[test]
fn event_serializes_with_snake_case_tag() {
    let e = Event::StateChanged { from: AssistantState::Idle, to: AssistantState::AwaitingConfirm };
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        serde_json::json!({ "type": "state_changed", "from": "idle", "to": "awaiting_confirm" })
    );
}

// state.rs
#[tokio::test]
async fn set_publishes_state_changed() {
    let bus = EventBus::new(8);
    let mut rx = bus.subscribe();
    let state = StateHandle::new(bus);
    assert_eq!(state.get(), AssistantState::Idle);
    assert!(state.set(AssistantState::Listening));
    assert_eq!(state.get(), AssistantState::Listening);
    assert_eq!(
        rx.recv().await.unwrap(),
        Event::StateChanged { from: AssistantState::Idle, to: AssistantState::Listening }
    );
}

#[test]
fn set_same_value_returns_false_and_publishes_nothing() {
    let bus = EventBus::new(8);
    let mut rx = bus.subscribe();
    let state = StateHandle::new(bus);
    assert!(!state.set(AssistantState::Idle));
    assert!(rx.try_recv().is_err());
}
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p mika-core`
Expected: ошибка компиляции, типы не определены.

- [ ] **Step 4: Реализовать `AssistantState`, `StateHandle`, `Event`, `EventBus` по блоку Interfaces**

Переходы между состояниями не ограничиваются: перебивание голосом может перевести из любого состояния в любое. `StateHandle::set` меняет значение и публикует событие под одной блокировкой, чтобы порядок событий совпадал с порядком смен.

- [ ] **Step 5: Убедиться, что тесты проходят**

Run: `cargo test -p mika-core`
Expected: 6 passed.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml rust-toolchain.toml crates/mika-core
git commit -m "feat(core): add workspace, assistant state and event bus"
```

---

### Task 2: Настройки

**Files:**
- Create: `crates/mika-core/src/paths.rs`, `crates/mika-core/src/config.rs`
- Modify: `crates/mika-core/src/lib.rs`, `crates/mika-core/Cargo.toml` (добавить `toml`, `thiserror`, dev-зависимость `tempfile`)

**Interfaces:**
- Consumes: ничего.
- Produces:

```rust
// paths.rs
pub fn data_dir_from(override_dir: Option<OsString>, appdata: Option<OsString>) -> PathBuf;
pub fn data_dir() -> PathBuf;     // data_dir_from(env MIKA_DATA_DIR, env APPDATA)
pub fn config_path() -> PathBuf;  // data_dir().join("config.toml")

// config.rs
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config { pub assistant: AssistantConfig, pub window: WindowConfig, pub voice: VoiceConfig, pub privacy: PrivacyConfig }

pub struct AssistantConfig { pub name: String, pub wake_phrase: String }   // "Мика", "Мика"
pub struct WindowConfig { pub mode: WindowMode, pub monitor: Option<String> } // Pinned, None
pub enum WindowMode { Pinned, Island, Window }                              // snake_case в TOML
pub struct VoiceConfig { pub speak_replies: bool }                          // true
pub struct PrivacyConfig { pub microphone_enabled: bool, pub screen_enabled: bool } // false, false

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config io error: {0}")] Io(#[from] std::io::Error),
    #[error("config parse error in {path}: {source}")] Parse { path: PathBuf, source: toml::de::Error },
    #[error("config serialize error: {0}")] Serialize(#[from] toml::ser::Error),
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError>;
    pub fn load_or_default(path: &Path) -> (Config, Option<ConfigError>);
    pub fn save(&self, path: &Path) -> Result<(), ConfigError>;
}
```

Все вложенные структуры имеют `#[serde(default)]` и те же derive, что `Config`.

- [ ] **Step 1: Написать падающие тесты**

```rust
// paths.rs
#[test]
fn data_dir_prefers_override() {
    assert_eq!(data_dir_from(Some("D:\\x".into()), Some("C:\\A".into())), PathBuf::from("D:\\x"));
}
#[test]
fn data_dir_uses_appdata_mika() {
    assert_eq!(data_dir_from(None, Some("C:\\A".into())), PathBuf::from("C:\\A").join("Mika"));
}
#[test]
fn data_dir_falls_back_to_relative_mika() {
    assert_eq!(data_dir_from(None, None), PathBuf::from("Mika"));
}

// config.rs
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
    std::fs::write(&p, "future_key = 1\n[window]\nmode = \"island\"\nextra = true\n").unwrap();
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
    assert_eq!(std::fs::read_to_string(d.path().join("config.broken.toml")).unwrap(), "[assistant\nname = ");
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p mika-core config paths`
Expected: ошибка компиляции.

- [ ] **Step 3: Реализовать `paths.rs` и `config.rs` по блоку Interfaces**

`save` пишет во временный файл `config.toml.tmp` в той же папке и переименовывает его в `config.toml`, чтобы обрыв записи не испортил настройки. `load_or_default` при ошибке разбора копирует исходный файл в `config.broken.toml` рядом и возвращает настройки по умолчанию вместе с ошибкой. При ошибке чтения, отличной от «файл не найден», она возвращает настройки по умолчанию и ошибку без копирования.

- [ ] **Step 4: Убедиться, что тесты проходят**

Run: `cargo test -p mika-core`
Expected: 18 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/mika-core
git commit -m "feat(core): add TOML config with defaults and safe save"
```

---

### Task 3: Хранилище секретов

**Files:**
- Create: `crates/mika-core/src/secrets.rs`
- Modify: `crates/mika-core/src/lib.rs`, `crates/mika-core/Cargo.toml` (добавить `keyring` с признаком `windows-native`)

**Interfaces:**
- Consumes: ничего.
- Produces:

```rust
pub const SERVICE: &str = "Mika";

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);          // Debug печатает "Secret(***)". Serialize и Display не реализуются.
impl Secret {
    pub fn new(value: impl Into<String>) -> Self;
    pub fn expose(&self) -> &str;
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret store unavailable: {0}")] Backend(String),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Secret>, SecretError>;
    fn set(&self, key: &str, value: &Secret) -> Result<(), SecretError>; // пустое значение удаляет запись
    fn delete(&self, key: &str) -> Result<(), SecretError>;              // отсутствие записи не ошибка
}

#[derive(Default)]
pub struct MemorySecretStore { /* Mutex<HashMap<String, String>> */ }
pub struct KeyringSecretStore;      // keyring::Entry::new(SERVICE, key)
impl KeyringSecretStore { pub fn new() -> Self; }
```

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn secret_debug_is_redacted() {
    let s = Secret::new("sk-12345");
    assert_eq!(format!("{:?}", s), "Secret(***)");
    assert_eq!(format!("{:?}", Some(s.clone())), "Some(Secret(***))");
    assert_eq!(s.expose(), "sk-12345");
}

fn check_store(store: &dyn SecretStore, key: &str) {
    assert_eq!(store.get(key).unwrap(), None);
    store.delete(key).unwrap();                          // удаление отсутствующей записи
    store.set(key, &Secret::new("one")).unwrap();
    assert_eq!(store.get(key).unwrap(), Some(Secret::new("one")));
    store.set(key, &Secret::new("two")).unwrap();        // перезапись
    assert_eq!(store.get(key).unwrap(), Some(Secret::new("two")));
    store.set(key, &Secret::new("")).unwrap();           // пустое значение удаляет
    assert_eq!(store.get(key).unwrap(), None);
    store.set(key, &Secret::new("three")).unwrap();
    store.delete(key).unwrap();
    assert_eq!(store.get(key).unwrap(), None);
}

#[test]
fn memory_store_contract() {
    check_store(&MemorySecretStore::default(), "llm.api_key");
}

#[test]
#[ignore = "пишет в Windows Credential Manager"]
fn keyring_store_contract() {
    let key = format!("test.contract.{}", std::process::id());
    check_store(&KeyringSecretStore::new(), &key);
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p mika-core secrets`
Expected: ошибка компиляции.

- [ ] **Step 3: Реализовать `Secret`, обе реализации `SecretStore`**

В `KeyringSecretStore` ошибка `keyring::Error::NoEntry` превращается в `Ok(None)` для `get` и в `Ok(())` для `delete`. Остальные ошибки превращаются в `SecretError::Backend` с текстом ошибки. Значение секрета в текст ошибки не попадает.

- [ ] **Step 4: Убедиться, что тесты проходят**

Run: `cargo test -p mika-core secrets`
Expected: 2 passed, 1 ignored.

Run: `cargo test -p mika-core secrets -- --ignored`
Expected: 1 passed. После теста в «Диспетчере учётных данных» Windows нет записей `test.contract.*` со службой `Mika`.

- [ ] **Step 5: Commit**

```bash
git add crates/mika-core
git commit -m "feat(core): add secret store backed by Windows Credential Manager"
```

---

### Task 4: Терминальный клиент

**Files:**
- Create: `crates/mika-cli/Cargo.toml`, `crates/mika-cli/src/main.rs`, `crates/mika-cli/src/repl.rs`

**Interfaces:**
- Consumes: `EventBus::new`, `EventBus::publish`, `EventBus::subscribe`, `StateHandle::new`, `StateHandle::set`, `Event`, `AssistantState` из задачи 1.
- Produces:

```rust
// repl.rs
#[derive(Debug, PartialEq, Eq)]
pub enum LineOutcome { Continue, Quit }

pub fn decode_line(bytes: &[u8]) -> String;                 // UTF-8 с заменой битых байтов, без \r\n в конце
pub fn format_event(event: &Event) -> String;
pub fn handle_line(line: &str, bus: &EventBus, state: &StateHandle) -> LineOutcome;
```

Форматы вывода:

| Событие | Строка |
|---|---|
| `StateChanged { from: Idle, to: Thinking }` | `[state] idle -> thinking` |
| `UserText { text: "привет" }` | `[user] привет` |
| `AssistantText { text: "эхо: привет" }` | `[mika] эхо: привет` |
| `Error { message: "x" }` | `[error] x` |

Имена состояний в выводе совпадают с их видом в JSON: `idle`, `listening`, `thinking`, `speaking`, `awaiting_confirm`.

- [ ] **Step 1: Написать падающие тесты**

```rust
fn setup() -> (EventBus, StateHandle, tokio::sync::broadcast::Receiver<Event>) {
    let bus = EventBus::new(32);
    let rx = bus.subscribe();
    (bus.clone(), StateHandle::new(bus), rx)
}
fn drain(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<Event> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

#[test]
fn text_line_produces_echo_sequence() {
    let (bus, state, mut rx) = setup();
    assert_eq!(handle_line("привет", &bus, &state), LineOutcome::Continue);
    assert_eq!(drain(&mut rx), vec![
        Event::UserText { text: "привет".into() },
        Event::StateChanged { from: AssistantState::Idle, to: AssistantState::Thinking },
        Event::AssistantText { text: "эхо: привет".into() },
        Event::StateChanged { from: AssistantState::Thinking, to: AssistantState::Idle },
    ]);
}

#[test]
fn blank_line_produces_nothing() {
    let (bus, state, mut rx) = setup();
    assert_eq!(handle_line("   ", &bus, &state), LineOutcome::Continue);
    assert!(drain(&mut rx).is_empty());
}

#[test]
fn quit_command_stops() {
    let (bus, state, mut rx) = setup();
    assert_eq!(handle_line("/quit", &bus, &state), LineOutcome::Quit);
    assert!(drain(&mut rx).is_empty());
}

#[test]
fn line_is_trimmed_before_echo() {
    let (bus, state, mut rx) = setup();
    handle_line("  привет  ", &bus, &state);
    assert_eq!(drain(&mut rx)[0], Event::UserText { text: "привет".into() });
}

#[test]
fn decode_line_survives_invalid_utf8() {
    assert_eq!(decode_line(b"ok\r\n"), "ok");
    assert_eq!(decode_line(&[0xFF, 0xFE, b'o', b'k', b'\n']), "\u{fffd}\u{fffd}ok");
}

#[test]
fn format_event_matches_table() {
    assert_eq!(format_event(&Event::StateChanged { from: AssistantState::Idle, to: AssistantState::AwaitingConfirm }), "[state] idle -> awaiting_confirm");
    assert_eq!(format_event(&Event::UserText { text: "привет".into() }), "[user] привет");
    assert_eq!(format_event(&Event::AssistantText { text: "эхо: привет".into() }), "[mika] эхо: привет");
    assert_eq!(format_event(&Event::Error { message: "x".into() }), "[error] x");
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p mika-cli`
Expected: ошибка компиляции.

- [ ] **Step 3: Реализовать `repl.rs` и `main.rs`**

`main.rs` читает stdin побайтно по строкам (`BufRead::read_until(b'\n')`), передаёт строку через `decode_line` в `handle_line` и после каждой строки печатает накопленные события через `format_event`. Конец ввода завершает программу с кодом 0.

- [ ] **Step 4: Убедиться, что тесты проходят**

Run: `cargo test -p mika-cli`
Expected: 6 passed.

- [ ] **Step 5: Проверить запуск**

Run (PowerShell): `"hello" | cargo run -q -p mika-cli`
Expected:

```
[user] hello
[state] idle -> thinking
[mika] эхо: hello
[state] thinking -> idle
```

- [ ] **Step 6: Commit**

```bash
git add crates/mika-cli
git commit -m "feat(cli): add echo REPL over the event bus"
```

---

### Task 5: Окно-островок

**Files:**
- Create: `crates/mika-gui/Cargo.toml`, `crates/mika-gui/build.rs`, `crates/mika-gui/tauri.conf.json`, `crates/mika-gui/capabilities/default.json`
- Create: `crates/mika-gui/src/main.rs`, `crates/mika-gui/src/bridge.rs`, `crates/mika-gui/src/window.rs`
- Create: `frontend/` шаблоном `npm create vite@latest frontend -- --template svelte-ts`, затем добавить `@tauri-apps/api` и `vitest`
- Create: `frontend/src/lib/island.ts`, `frontend/src/lib/island.test.ts`, `frontend/src/lib/Island.svelte`
- Modify: `frontend/src/App.svelte`, `frontend/package.json` (скрипт `"test": "vitest run"`)

**Interfaces:**
- Consumes: `EventBus`, `StateHandle`, `Event`, `AssistantState` из задачи 1; `Config::load_or_default`, `config_path` из задачи 2.
- Produces:

```rust
// window.rs
pub const CANVAS_WIDTH: f64 = 1000.0;  // логические пиксели, решение 0001
pub const CANVAS_HEIGHT: f64 = 600.0;
pub const HIT_PAD: f64 = 4.0;          // расширение прямоугольника островка, логические пиксели
/// Левый верхний угол окна в физических пикселях: по центру верхнего края монитора.
pub fn canvas_position(monitor_x: i32, monitor_y: i32, monitor_width: u32, scale: f64) -> (i32, i32);

/// Прямоугольник островка в логических пикселях относительно левого верхнего угла холста.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Deserialize)]
pub struct IslandRect { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }

/// Курсор (физические пиксели экрана) внутри островка, расширенного на HIT_PAD.
/// origin — левый верхний угол холста в физических пикселях. Левая и верхняя границы включены, правая и нижняя нет.
pub fn island_hit(cursor: (i32, i32), origin: (i32, i32), scale: f64, rect: IslandRect) -> bool;

/// Запускает поток опроса курсора (GetCursorPos, каждые 16 мс). При смене результата island_hit
/// вызывает set_ignore_cursor_events(!inside). Положение и масштаб окна кэшируются и обновляются
/// по событиям Moved и ScaleFactorChanged, а не читаются на каждом шаге.
pub fn spawn_click_through(window: tauri::WebviewWindow, rect: std::sync::Arc<parking_lot::Mutex<IslandRect>>);

// bridge.rs, дополнительно
#[tauri::command] pub fn set_island_rect(rect: tauri::State<'_, std::sync::Arc<parking_lot::Mutex<IslandRect>>>, value: IslandRect);

// bridge.rs
pub const EVENT_CHANNEL: &str = "mika://event";
pub fn spawn_bridge(app: tauri::AppHandle, bus: EventBus);   // каждое Event уходит в интерфейс как JSON
#[tauri::command] pub fn get_state(state: tauri::State<'_, StateHandle>) -> AssistantState;
#[tauri::command] pub fn debug_set_state(state: tauri::State<'_, StateHandle>, next: AssistantState) -> bool; // только в отладочной сборке
```

```ts
// frontend/src/lib/island.ts
export type AssistantState = 'idle' | 'listening' | 'thinking' | 'speaking' | 'awaiting_confirm';
export interface IslandShape { width: number; height: number; radius: number }
export const ISLAND_SHAPES: Record<AssistantState, IslandShape>;
export const STATE_LABELS: Record<AssistantState, string>;
export type MikaEvent =
  | { type: 'state_changed'; from: AssistantState; to: AssistantState }
  | { type: 'user_text'; text: string }
  | { type: 'assistant_text'; text: string }
  | { type: 'error'; message: string };
export function isAssistantState(value: unknown): value is AssistantState;
export function shapeFor(state: unknown): IslandShape;               // неизвестное значение даёт форму idle
export function reduceState(current: AssistantState, event: unknown): AssistantState;
export interface Rect { x: number; y: number; w: number; h: number }
export const ISLAND_TOP = 14;
export function islandRect(state: unknown, canvasWidth: number): Rect;  // по центру, y = ISLAND_TOP
export function unionRect(a: Rect, b: Rect): Rect;
```

Порядок отправки прямоугольника при смене состояния: сразу `unionRect(старый, новый)`, через 700 мс `islandRect(новый)`. Без объединения клик в середине перехода проходит насквозь (решение 0001).

Значения:

| Состояние | width | height | radius | Подпись |
|---|---|---|---|---|
| `idle` | 236 | 44 | 22 | `Жду` |
| `listening` | 340 | 52 | 26 | `Слушаю` |
| `thinking` | 380 | 52 | 26 | `Думаю` |
| `speaking` | 620 | 78 | 30 | `Отвечаю` |
| `awaiting_confirm` | 600 | 268 | 30 | `Нужно подтверждение` |

Окно в `tauri.conf.json`: `label` `island`, `width` 1000, `height` 600, `decorations` false, `transparent` true, `alwaysOnTop` true, `skipTaskbar` true, `resizable` false, `shadow` false, `focus` false, `visible` false. Окно показывается из `setup` после того, как поставлено на место и включён пропуск мыши. `build.frontendDist` указывает на `../../frontend/dist`, `build.devUrl` на `http://localhost:5173`, `build.beforeDevCommand` запускает `npm --prefix ../../frontend run dev`, `build.beforeBuildCommand` запускает `npm --prefix ../../frontend run build`.

- [ ] **Step 1: Написать падающие тесты положения окна**

```rust
// window.rs
#[test]
fn centres_on_full_hd() {
    assert_eq!(canvas_position(0, 0, 1920, 1.0), (460, 0));
}
#[test]
fn respects_scale_and_monitor_origin() {
    // холст 1000 * 1.5 = 1500 физических пикселей
    assert_eq!(canvas_position(1920, 0, 2560, 1.5), (2450, 0));
}
#[test]
fn monitor_left_of_primary_has_negative_x() {
    assert_eq!(canvas_position(-1920, 120, 1920, 1.0), (-1460, 120));
}
#[test]
fn monitor_narrower_than_canvas_clamps_to_left_edge() {
    assert_eq!(canvas_position(0, 0, 800, 1.0), (0, 0));
    assert_eq!(canvas_position(-800, 0, 800, 1.0), (-800, 0));
}

// Островок в покое на холсте у (460, 0): x = (1000 - 236) / 2 = 382, y = 14.
const IDLE: IslandRect = IslandRect { x: 382.0, y: 14.0, w: 236.0, h: 44.0 };

#[test]
fn hit_inside_island() {
    assert!(island_hit((960, 36), (460, 0), 1.0, IDLE));
}
#[test]
fn hit_includes_pad_and_excludes_beyond_it() {
    // левый край островка 460 + 382 = 842, с запасом 4 px — 838
    assert!(island_hit((838, 36), (460, 0), 1.0, IDLE));
    assert!(!island_hit((837, 36), (460, 0), 1.0, IDLE));
    // нижний край 14 + 44 = 58, с запасом 62, граница не включена
    assert!(island_hit((960, 61), (460, 0), 1.0, IDLE));
    assert!(!island_hit((960, 62), (460, 0), 1.0, IDLE));
}
#[test]
fn hit_transparent_canvas_area_is_false() {
    assert!(!island_hit((700, 300), (460, 0), 1.0, IDLE));
}
#[test]
fn hit_respects_scale_and_negative_origin() {
    // холст на мониторе слева, масштаб 1.5: левый край островка -1460 + (382 - 4) * 1.5 = -893
    assert!(island_hit((-893, 30), (-1460, 0), 1.5, IDLE));
    assert!(!island_hit((-894, 30), (-1460, 0), 1.5, IDLE));
}
#[test]
fn hit_with_empty_rect_is_false_outside_pad() {
    assert!(!island_hit((960, 36), (460, 0), 1.0, IslandRect::default()));
}
```

- [ ] **Step 2: Написать падающие тесты интерфейса**

```ts
// frontend/src/lib/island.test.ts
import { describe, expect, it } from 'vitest';
import { ISLAND_SHAPES, STATE_LABELS, islandRect, reduceState, shapeFor, unionRect } from './island';

describe('islandRect', () => {
  it('centres the island on the canvas', () => {
    expect(islandRect('idle', 1000)).toEqual({ x: 382, y: 14, w: 236, h: 44 });
    expect(islandRect('awaiting_confirm', 1000)).toEqual({ x: 200, y: 14, w: 600, h: 268 });
  });
  it('falls back to idle for unknown state', () => {
    expect(islandRect('bogus', 1000)).toEqual({ x: 382, y: 14, w: 236, h: 44 });
  });
});

describe('unionRect', () => {
  it('covers both rectangles', () => {
    expect(unionRect({ x: 382, y: 14, w: 236, h: 44 }, { x: 200, y: 14, w: 600, h: 268 }))
      .toEqual({ x: 200, y: 14, w: 600, h: 268 });
    expect(unionRect({ x: 0, y: 0, w: 10, h: 10 }, { x: 20, y: 5, w: 10, h: 10 }))
      .toEqual({ x: 0, y: 0, w: 30, h: 15 });
  });
});

describe('shapeFor', () => {
  it('returns the spec sizes', () => {
    expect(shapeFor('idle')).toEqual({ width: 236, height: 44, radius: 22 });
    expect(shapeFor('listening')).toEqual({ width: 340, height: 52, radius: 26 });
    expect(shapeFor('thinking')).toEqual({ width: 380, height: 52, radius: 26 });
    expect(shapeFor('speaking')).toEqual({ width: 620, height: 78, radius: 30 });
    expect(shapeFor('awaiting_confirm')).toEqual({ width: 600, height: 268, radius: 30 });
  });
  it('falls back to idle for unknown values', () => {
    expect(shapeFor('bogus')).toEqual(ISLAND_SHAPES.idle);
    expect(shapeFor(undefined)).toEqual(ISLAND_SHAPES.idle);
  });
});

describe('STATE_LABELS', () => {
  it('has a Russian label for every state', () => {
    expect(STATE_LABELS).toEqual({
      idle: 'Жду', listening: 'Слушаю', thinking: 'Думаю',
      speaking: 'Отвечаю', awaiting_confirm: 'Нужно подтверждение',
    });
  });
});

describe('reduceState', () => {
  it('follows state_changed', () => {
    expect(reduceState('idle', { type: 'state_changed', from: 'idle', to: 'listening' })).toBe('listening');
  });
  it('ignores other events', () => {
    expect(reduceState('thinking', { type: 'assistant_text', text: 'x' })).toBe('thinking');
  });
  it('ignores malformed payloads', () => {
    expect(reduceState('thinking', { type: 'state_changed', to: 'bogus' })).toBe('thinking');
    expect(reduceState('thinking', null)).toBe('thinking');
    expect(reduceState('thinking', 'state_changed')).toBe('thinking');
  });
});
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p mika-gui window`
Expected: ошибка компиляции.

Run: `npm --prefix frontend test`
Expected: FAIL, модуль `./island` не найден.

- [ ] **Step 4: Реализовать `window.rs` и `island.ts`**

`canvas_position`: ширина холста в физических пикселях равна `round(CANVAS_WIDTH * scale)`. Координата `x` равна `monitor_x + (monitor_width - ширина) / 2` и не меньше `monitor_x`. Координата `y` равна `monitor_y`.

`island_hit`: границы в физических пикселях равны `origin + (rect.x - HIT_PAD) * scale` слева и `origin + (rect.x + rect.w + HIT_PAD) * scale` справа, так же по вертикали. Сравнение в `f64`.

- [ ] **Step 5: Убедиться, что тесты проходят**

Run: `cargo test -p mika-gui window`
Expected: 9 passed.

Run: `npm --prefix frontend test`
Expected: 9 passed.

- [ ] **Step 6: Реализовать `bridge.rs`, `main.rs`, `Island.svelte`, `App.svelte`**

`main.rs`: загружает настройки через `Config::load_or_default(&config_path())` и пишет ошибку в журнал, если она есть; создаёт `EventBus::new(256)` и `StateHandle`; кладёт `StateHandle` и `Arc<Mutex<IslandRect>>` в состояние Tauri; регистрирует команды; в `setup` ставит окно в `canvas_position` основного монитора, вызывает `set_ignore_cursor_events(true)`, `spawn_click_through`, `spawn_bridge` и только потом показывает окно без активации: `ShowWindow(hwnd, SW_SHOWNOACTIVATE)` из `windows-sys` (признак `Win32_UI_WindowsAndMessaging`) по `window.hwnd()`, а не `show()`. `debug_set_state` регистрируется только при `cfg!(debug_assertions)`.

`spawn_bridge`: задача tokio читает подписку шины и вызывает `app.emit(EVENT_CHANNEL, &event)`. Ошибку `Lagged` она пишет в журнал и продолжает работу. Ошибка `Closed` завершает задачу.

`App.svelte`: при запуске вызывает `get_state`, подписывается на `mika://event` и обновляет состояние через `reduceState`. При каждой смене состояния отправляет прямоугольник через `set_island_rect` в порядке, описанном под блоком Interfaces. В отладочной сборке клавиши 1–5 вызывают `debug_set_state` с состояниями в порядке таблицы. Клавиши работают, когда окно в фокусе, то есть после клика по островку.

`Island.svelte`: блок по центру верхнего края холста с отступом 14 px. Ширина, высота и скругление берутся из `shapeFor(state)`. Фон `#06070b`, рамка `1px solid rgba(255,255,255,.1)`, переход `620ms cubic-bezier(.32,1.28,.54,1)` для ширины, высоты и скругления. Внутри подпись из `STATE_LABELS`. Фон страницы прозрачный.

- [ ] **Step 7: Проверить вручную**

Run: `cargo tauri dev` из папки `crates/mika-gui`
Expected:
- вверху по центру основного монитора виден чёрный островок 236 × 44 с подписью «Жду»;
- у окна нет рамки, заголовка и значка на панели задач;
- островок остаётся поверх других окон;
- окно, которое было активным до запуска, остаётся активным: запустить сборку, сразу щёлкнуть в Блокнот и подождать появления островка;
- клик по прозрачной области рядом с островком попадает в окно под ним;
- после клика по островку клавиши 1–5 меняют форму и подпись по таблице, переход плавный;
- в состоянии «Нужно подтверждение» клик внутри карточки не проходит насквозь;
- в консоли нет ошибок.

Скрипт автоматической проверки клик-сквозь из пробной сборки описан в решении 0001.

- [ ] **Step 8: Commit**

```bash
git add crates/mika-gui frontend
git commit -m "feat(gui): add island window with state-driven shape"
```

---

### Task 6: Проверки в GitHub Actions

**Files:**
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: команды проверки из задач 1–5.
- Produces: задание `check` на `windows-latest`.

- [ ] **Step 1: Написать `ci.yml`**

Запуск на `push` и `pull_request`. Шаги по порядку: checkout; установка Rust stable с `rustfmt` и `clippy`; установка Node.js 24; `npm --prefix frontend ci`; `npm --prefix frontend test`; `npm --prefix frontend run build`; `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.

Сборка интерфейса идёт раньше команд Cargo, потому что `mika-gui` при компиляции требует готовую папку `frontend/dist`.

- [ ] **Step 2: Проверить те же команды локально**

Run по порядку из корня: `npm --prefix frontend ci`, `npm --prefix frontend test`, `npm --prefix frontend run build`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
Expected: каждая команда завершается с кодом 0. Итог тестов: 35 passed, 1 ignored в Rust и 9 passed в интерфейсе.

- [ ] **Step 3: Commit**

```bash
git add .github
git commit -m "ci: add fmt, clippy and test checks"
```

---

## Готово, когда

- `cargo test --workspace` проходит.
- `"hello" | cargo run -q -p mika-cli` печатает четыре строки из задачи 4.
- `cargo tauri dev` показывает островок, который меняет форму клавишами 1–5.
- `cargo test -p mika-core secrets -- --ignored` проходит и не оставляет записей в Credential Manager.
