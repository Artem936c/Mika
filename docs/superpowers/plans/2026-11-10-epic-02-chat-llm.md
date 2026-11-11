# Эпик 2 «Текстовый чат и LLM» Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Мика отвечает в чате островка через любого из трёх провайдеров и вызывает инструменты.

**Architecture:** Общие типы LLM, реестр инструментов, шлюз и цикл агента живут в `mika-core`. HTTP-адаптеры живут в `mika-providers`. Инструмент выполняется только с жетоном `Approval`, который создаёт шлюз. Так эпик 3 подключает политику рисков без переделки цикла. Интерфейс получает поток текста событиями шины.

**Tech Stack:** эпик 1 плюс `reqwest 0.12` (rustls, stream), `futures 0.3`, `async-trait`, `tokio-util` (`CancellationToken`), `rusqlite 0.37` (bundled), `chrono`.

**Spec:** [docs/superpowers/specs/2026-11-10-mika-assistant-design.md](../specs/2026-11-10-mika-assistant-design.md), разделы 5, 6, 11.

## Global Constraints

- Всё из эпика 1.
- `mika-core` не зависит от `reqwest`, Tauri и Windows. HTTP только в `mika-providers`.
- Ключ провайдера берётся из `SecretStore` по имени `llm.<kind>.api_key`. Ключ не попадает в `Debug`, журнал и текст ошибок. Текст ответа сервера при ошибке обрезается до 300 символов, а строка ключа в нём заменяется на `***`.
- Инструмент выполняется только через `Gate`. `Approval` не реализует `Clone` и `Default`, его поле закрыто.
- Предел шагов цикла агента: 8 вызовов LLM на одну реплику.
- Первый фрагмент ответа уходит в шину без буферизации (бюджет 30 мс, раздел 6 дизайна).

## Мои решения

- **Ollama через OpenAI-совместимый адрес.** Ollama отдаёт `/v1/chat/completions` с потоком и вызовами инструментов. Адаптер Ollama — это адаптер OpenAI с адресом `http://localhost:11434/v1` и без ключа. Отдельный разбор NDJSON не нужен.
- **Чат островка 600 × 500.** Размер «без аватара» из таблицы раздела 10. Аватар появится в эпике 7, тогда чат станет 860 × 500.
- **Настройки провайдера внутри чата**, отдельным экраном по кнопке «Настройки». Отдельное окно настроек появится в эпике 6.
- **Временный шлюз эпика 2** разрешает низкий риск и запрещает остальное. Эпик 3 заменяет его политикой.

## Review Focus

1. Сеть оборвалась посреди потока. Ожидание: в чате понятная ошибка, уже пришедший текст остаётся, следующая реплика работает. Тест в задаче 2.
2. Сервер вернул 401 с ключом в теле ответа. Ожидание: в ошибке нет ключа. Тест в задаче 2.
3. Модель зациклилась на вызовах инструментов. Ожидание: после 8 шагов цикл останавливается с сообщением. Тест в задаче 3.
4. LLM вызвала несуществующий инструмент или прислала битый JSON аргументов. Ожидание: LLM получает текст ошибки как результат инструмента, цикл продолжается. Тест в задаче 3.
5. Пользователь нажал «Стоп» во время потока. Ожидание: поток прерывается за время меньше 100 мс, состояние возвращается в `idle`. Тест в задаче 3.

---

### Task 1: Типы LLM, инструменты, шлюз

**Files:** Create `crates/mika-core/src/llm.rs`, `crates/mika-core/src/tools.rs`, `crates/mika-core/src/gate.rs`.

**Interfaces (Produces):**

```rust
// llm.rs
#[serde(rename_all = "snake_case")] pub enum Role { System, User, Assistant, Tool }
pub struct ChatMessage { pub role: Role, pub content: String, pub tool_calls: Vec<ToolCall>, pub tool_call_id: Option<String> }
impl ChatMessage { pub fn system(..)/user(..)/assistant(..)/tool_result(id, content) -> Self }
pub struct ToolCall { pub id: String, pub name: String, pub arguments: serde_json::Value }
pub struct ToolSpec { pub name: String, pub description: String, pub parameters: serde_json::Value }
pub struct LlmRequest { pub messages: Vec<ChatMessage>, pub tools: Vec<ToolSpec> }
pub enum LlmChunk { Text(String), ToolCall(ToolCall) }
pub enum LlmError { Network(String), Http { status: u16, body: String }, Protocol(String), Cancelled, NotConfigured(String) }
pub type LlmStream = BoxStream<'static, Result<LlmChunk, LlmError>>;
#[async_trait] pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError>;
}

// tools.rs
#[serde(rename_all = "snake_case")] pub enum RiskLevel { Low, Medium, High }   // Ord: Low < Medium < High
pub struct Assessment { pub level: RiskLevel, pub summary: String, pub target: String, pub reason: String }
pub struct ToolOutput { pub text: String }
pub enum ToolError { BadArguments(String), Failed(String), Cancelled }
#[async_trait] pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn assess(&self, args: &Value) -> Result<Assessment, ToolError>;
    async fn run(&self, args: Value, approval: Approval, cancel: CancellationToken) -> Result<ToolOutput, ToolError>;
}
pub struct ToolRegistry { .. }
impl ToolRegistry { pub fn new() -> Self; pub fn register(&mut self, tool: Arc<dyn Tool>); pub fn specs(&self) -> Vec<ToolSpec>; pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>>; }
pub struct TimeTool; pub struct EchoTool;   // проверочные, риск Low

// gate.rs
pub struct Approval { _private: () }        // pub(crate) fn new()
#[cfg(any(test, feature = "testing"))] impl Approval { pub fn for_tests() -> Self }
pub enum GateDecision { Approved(Approval), Denied(String) }
#[async_trait] pub trait Gate: Send + Sync {
    async fn check(&self, tool: &str, args: &Value, assessment: &Assessment) -> GateDecision;
}
pub struct LowRiskOnlyGate;                 // временный, эпик 3 удаляет
```

- [ ] Тесты: `registry_lists_specs_sorted_by_name`, `registry_get_unknown_is_none`, `time_tool_returns_rfc3339_like_text`, `echo_tool_returns_text_argument`, `echo_tool_rejects_missing_text`, `low_risk_gate_approves_low_and_denies_medium`, `chat_message_serde_roundtrip`.
- [ ] Реализация, `cargo test -p mika-core` зелёный.

### Task 2: Адаптеры провайдеров

**Files:** Create `crates/mika-providers/{Cargo.toml, src/lib.rs, src/sse.rs, src/openai.rs, src/anthropic.rs, src/factory.rs}`, `crates/mika-providers/tests/mock_server.rs`.

**Interfaces (Produces):**

```rust
// sse.rs
pub struct SseParser { .. }   // push(&[u8]) -> Vec<SseEvent>; события по пустой строке, "data:" склеиваются через \n, ":" комментарии
pub struct SseEvent { pub event: Option<String>, pub data: String }

// openai.rs — /chat/completions, stream: true, tools в формате function
pub struct OpenAiProvider { .. }
impl OpenAiProvider { pub fn new(base_url: String, model: String, api_key: Option<Secret>) -> Self }
// вызовы инструментов копятся по index, отдаются после finish_reason или [DONE]

// anthropic.rs — /v1/messages, stream: true, x-api-key, anthropic-version: 2023-06-01, max_tokens 4096
pub struct AnthropicProvider { .. }
impl AnthropicProvider { pub fn new(base_url: String, model: String, api_key: Option<Secret>) -> Self }
// system отдельно; tool_use копится из input_json_delta; результаты инструмента — блоки tool_result в сообщении user

// factory.rs
pub fn build_provider(cfg: &LlmConfig, secrets: &dyn SecretStore) -> Result<Arc<dyn LlmProvider>, LlmError>;
pub async fn probe(provider: &dyn LlmProvider) -> Result<Duration, LlmError>; // время до первого фрагмента
pub fn redact(body: &str, secret: Option<&Secret>) -> String;
```

`LlmConfig` в `mika-core::config`: `kind: ProviderKind (openai | anthropic | ollama)`, `base_url: String`, `model: String`. Значения по умолчанию: `openai`, `https://api.openai.com/v1`, `gpt-4o-mini`. Для `ollama` пустой адрес означает `http://localhost:11434/v1`, для `anthropic` — `https://api.anthropic.com`.

- [ ] Тесты разбора: `sse_splits_events_across_chunks`, `sse_joins_multiline_data`, `sse_ignores_comments`.
- [ ] Тесты на поддельном сервере (TCP-слушатель tokio с заготовленным ответом): `openai_streams_text_deltas`, `openai_assembles_tool_call_from_fragments`, `openai_http_401_is_redacted`, `openai_disconnect_mid_stream_yields_network_error`, `anthropic_streams_text`, `anthropic_assembles_tool_use`, `anthropic_sends_system_separately_and_tool_results_as_user_blocks`.
- [ ] Реализация, `cargo test -p mika-providers` зелёный.

### Task 3: Цикл агента и история

**Files:** Create `crates/mika-core/src/agent.rs`, `crates/mika-core/src/storage.rs`. Modify `events.rs`.

**Interfaces (Produces):**

```rust
// events.rs, новые варианты
AssistantDelta { text: String },                         // фрагмент потока
ToolStarted { id: String, name: String, summary: String },
ToolFinished { id: String, name: String, ok: bool, output: String, elapsed_ms: u64 },
// AssistantText { text } — итоговый текст реплики

// agent.rs
pub const MAX_STEPS: usize = 8;
pub struct Agent { .. }
impl Agent {
    pub fn new(provider: Arc<dyn LlmProvider>, registry: Arc<ToolRegistry>, gate: Arc<dyn Gate>, bus: EventBus, state: StateHandle, system_prompt: String) -> Self;
    pub fn set_provider(&self, provider: Arc<dyn LlmProvider>);
    pub fn history(&self) -> Vec<ChatMessage>;
    pub fn with_history(self, messages: Vec<ChatMessage>) -> Self;
    pub async fn handle(&self, text: &str, cancel: CancellationToken) -> Result<String, AgentError>;
}
pub fn system_prompt(name: &str) -> String;

// storage.rs
pub struct Db { .. }   // rusqlite::Connection под parking_lot::Mutex
impl Db {
    pub fn open(path: &Path) -> Result<Db, DbError>;
    pub fn open_in_memory() -> Result<Db, DbError>;
    pub fn append_message(&self, conversation: &str, msg: &ChatMessage) -> Result<(), DbError>;
    pub fn load_messages(&self, conversation: &str) -> Result<Vec<ChatMessage>, DbError>;
}
```

Порядок в `handle`: `UserText`, состояние `thinking`; цикл до `MAX_STEPS`: поток LLM, каждый `Text` уходит в `AssistantDelta` и копится; вызовы инструментов идут по одному через `assess`, `gate.check`, `run`, с событиями `ToolStarted`/`ToolFinished`; результат добавляется в историю как сообщение `tool`. Ответ без вызовов завершает цикл: `AssistantText`, состояние `idle`. Ошибка: `Error`, состояние `idle`, история без незаконченной реплики ассистента. Отмена: `Error { "остановлено" }` не публикуется, состояние `idle`.

- [ ] Тесты с поддельным провайдером (очередь заготовленных ответов): `plain_answer_streams_deltas_and_final_text`, `tool_call_runs_tool_and_feeds_result_back`, `unknown_tool_returns_error_to_llm`, `bad_arguments_return_error_to_llm`, `denied_tool_is_reported_to_llm`, `stops_after_max_steps`, `cancel_stops_stream_and_returns_to_idle`, `provider_error_publishes_error_and_keeps_chat_usable`.
- [ ] Тесты хранилища: `messages_roundtrip_in_order`, `conversations_are_separate`.

### Task 4: Чат в интерфейсе и страница провайдеров

**Files:** Modify `crates/mika-gui/src/{main.rs, bridge.rs}`, `crates/mika-cli/src/*`. Create `crates/mika-gui/src/app.rs`, `frontend/src/lib/chat.ts`, `frontend/src/lib/chat.test.ts`, `frontend/src/lib/Chat.svelte`, `frontend/src/lib/Settings.svelte`.

**Interfaces (Produces):**

```rust
// bridge.rs, команды
send_message(text: String)            // запускает Agent::handle в фоне, отменяет прошлый токен
stop()                                // отменяет текущий токен
get_llm_settings() -> LlmSettingsView // kind, base_url, model, has_key; ключ не возвращается
save_llm_settings(kind, base_url, model, api_key: Option<String>) // пустая строка удаляет ключ, None не трогает
test_provider() -> Result<u64, String> // мс до первого фрагмента
```

```ts
// chat.ts
export interface ChatItem { kind: 'user' | 'assistant' | 'tool' | 'error'; text: string; id?: string; ok?: boolean; ms?: number; streaming?: boolean }
export function reduceChat(items: ChatItem[], event: unknown): ChatItem[];
export const CHAT_SHAPE: IslandShape;   // 600 × 500, radius 28
```

- [ ] Тесты `chat.test.ts`: `user_text appends user item`, `deltas join into one streaming assistant item`, `assistant_text finalizes the streaming item`, `tool_started then tool_finished updates one tool row with ms`, `error appends error and finalizes streaming`, `malformed events are ignored`.
- [ ] CLI: строка идёт в `Agent::handle`, вывод `[mika] ` печатает фрагменты без перевода строки, `[tool] name ok 12ms`.
- [ ] Ручная проверка с поддельным сервером OpenAI (`MIKA_DATA_DIR` во временную папку).

## Готово, когда

- Тесты всех крейтов и интерфейса проходят, `clippy -D warnings` чист.
- `mika-cli` с поддельным сервером показывает поток текста и вызов `get_time`.
- Живая проверка трёх провайдеров требует ключей пользователя: она записывается как непроверенная.
