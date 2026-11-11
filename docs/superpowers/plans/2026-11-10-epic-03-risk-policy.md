# Эпик 3 «Политика рисков и подтверждения» Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ни один инструмент не выполняется без решения политики. Пользователь отвечает кнопкой или фразой в чате, разрешение бывает разовым или постоянным.

**Architecture:** `PolicyGate` в `mika-core` заменяет временный шлюз эпика 2. Он оценивает вызов, ищет правило, при необходимости открывает запрос подтверждения и ждёт ответа до 60 секунд. Запрос живёт в `Confirmations`, ответ приходит кнопкой из интерфейса или фразой из чата через `Session`. Классификаторы команд и путей уточняют риск. Правила и журнал лежат в SQLite.

**Tech Stack:** эпики 1–2, `regex`.

**Spec:** [дизайн](../specs/2026-11-10-mika-assistant-design.md), раздел 8.

## Global Constraints

- Высокий риск нельзя разрешить навсегда: ни кнопкой, ни фразой, ни записью правила через `RuleStore::add`.
- Без правила политика спрашивает даже для низкого риска (решение пользователя «проверять всё»).
- Правило «запретить» сильнее правила «разрешить».
- Без ответа за 60 секунд действие отклоняется, LLM получает отказ.
- `Approval` создаёт только шлюз в `mika-core`.
- Каждое решение пишется в журнал: время, инструмент, цель, риск, решение, кто решил (`rule`, `button`, `chat`, `timeout`, `policy`).

## Мои решения

- **Шаблон правила** — поле `scope` в `Assessment`. Его задаёт инструмент: `*` для инструментов без опасных аргументов, нормализованная команда для командной строки, имя программы для запуска, адрес сайта для ссылок. Шаблон поддерживает `*` в любом месте.
- **Неизвестная команда оболочки — высокий риск.** Средний только у команд из списка чтения. Так «разрешать всегда» доступно для `Get-Process`, но не для произвольного скрипта.
- **Съёмные диски** определяет инструмент через Windows API и передаёт классификатору признак. Ядро не зависит от Windows.
- **Фраза в чате во время вопроса** — это ответ, если она распознана. Иначе Мика просит ответить «да», «разреши всегда» или «нет», а сообщение не уходит в LLM.

## Review Focus

1. Команда спрятана: `Re`+`move-Item`, ``Re`move-Item``, `de^l`, `iex`, `-enc`. Ожидание: высокий риск. Тест в задаче 2.
2. Цепочка `Get-Date; Remove-Item C:\x -Recurse`. Ожидание: риск по самой опасной части. Тест в задаче 2.
3. Путь `C:\Users\me\..\..\Windows\System32`. Ожидание: системная папка. Тест в задаче 2.
4. Пользователь ответил на запрос после таймаута. Ожидание: ответ отклонён, действие не выполняется. Тест в задаче 3.
5. Фраза «разреши всегда» для высокого риска. Ожидание: правило не создаётся, запрос остаётся открытым. Тест в задаче 3.

---

### Task 1: Правила и журнал

**Files:** Create `crates/mika-core/src/policy.rs`, `crates/mika-core/src/policy/rules.rs`. Modify `tools.rs` (`Assessment.scope`).

```rust
pub enum RuleDecision { Allow, Deny }                  // snake_case
pub struct Rule { pub id: i64, pub tool: String, pub pattern: String, pub decision: RuleDecision, pub created_at: String }
pub struct RuleStore { db: Arc<Db> }
impl RuleStore {
    pub fn add(&self, tool: &str, pattern: &str, decision: RuleDecision, level: RiskLevel) -> Result<Rule, PolicyError>; // High + Allow -> PolicyError::HighRiskForever
    pub fn list(&self) -> Result<Vec<Rule>, PolicyError>;
    pub fn remove(&self, id: i64) -> Result<bool, PolicyError>;
    pub fn find(&self, tool: &str, scope: &str) -> Result<Option<RuleDecision>, PolicyError>; // Deny сильнее Allow
}
pub fn pattern_matches(pattern: &str, value: &str) -> bool;   // '*' — любая последовательность, без учёта регистра
pub struct AuditEntry { pub at, tool, target, risk, decision, decided_by: String }
pub struct AuditLog { db: Arc<Db> }  // record(..), recent(limit) -> Vec<AuditEntry>
```

- [ ] Тесты: `add_and_find_allow_rule`, `deny_beats_allow`, `high_risk_cannot_be_allowed_forever`, `high_risk_can_be_denied_forever`, `remove_rule`, `pattern_star_matches_any`, `pattern_is_case_insensitive`, `audit_records_newest_first`.

### Task 2: Классификаторы

**Files:** Create `crates/mika-core/src/policy/command.rs`, `crates/mika-core/src/policy/paths.rs`.

```rust
pub enum Shell { PowerShell, Cmd }
pub struct CommandRisk { pub level: RiskLevel, pub reasons: Vec<String>, pub normalized: String }
pub fn classify_command(shell: Shell, command: &str) -> CommandRisk;

pub enum PathClass { System, ProgramFiles, UserProfile, Network, Removable, Other }
pub enum PathAccess { Read, Write, Delete }
pub fn normalize_path(path: &str) -> String;                    // лексически убирает `.` и `..`, слэши в `\`
pub fn classify_path(path: &str, user_profile: &str, removable: bool) -> PathClass;
pub fn path_risk(class: PathClass, access: PathAccess) -> RiskLevel;
```

Нормализация команды: убрать `` ` `` (PowerShell) и `^` (cmd), нижний регистр, схлопнуть пробелы. Разбор на части по `;`, `|`, `&&`, `||`, `&` (cmd), переводам строки. Риск цепочки — максимум частей. Признаки обфускации дают высокий риск: склейка строк `"a"+"b"`, вызов `&(` или `.(`, `[char]`, `-join`, `frombase64string`, `-enc`/`-encodedcommand`.

`path_risk`: чтение — Low везде, кроме System (Medium); запись — Medium, в System и ProgramFiles High; удаление — High везде.

- [ ] Табличный тест `command_table` не меньше чем на 40 строк: чтение (`Get-Process`, `dir`, `ipconfig /all`, `whoami`, `Get-ChildItem C:\`) — Medium; изменения и опасные операции (`Remove-Item -Recurse`, `rm -rf`, `del /s /q`, `rd /s`, `format c:`, `diskpart`, `bcdedit`, `reg delete`, `Set-ExecutionPolicy`, `shutdown /r`, `Restart-Computer`, `Stop-Process`, `taskkill /f`, `net user x /add`, `schtasks /create`, `sc delete`, `vssadmin delete shadows`, `cipher /w`, `takeown`, `icacls`, `Start-Process -Verb RunAs`, `Invoke-WebRequest ... | iex`, `certutil -urlcache`, `bitsadmin /transfer`, `Add-MpPreference -ExclusionPath`, `Set-MpPreference -DisableRealtimeMonitoring`, `netsh advfirewall set allprofiles state off`, `wmic process call create`, `powershell -enc ...`) — High; обфускация (``Re`move-Item``, `de^l`, `"Remo"+"ve-Item"`, `&("i"+"ex")`, `[char]105+[char]101+[char]120`, `IEX(New-Object Net.WebClient).DownloadString(..)`) — High; неизвестная программа — High; цепочки — максимум.
- [ ] Тесты путей: `dotdot_into_windows_is_system`, `program_files_x86`, `user_profile_documents`, `unc_is_network`, `drive_root_is_system`, `forward_slashes_normalized`, `delete_is_always_high`, `write_to_system_is_high`.

### Task 3: Шлюз, подтверждения, сессия

**Files:** Create `crates/mika-core/src/policy/gate.rs`, `crates/mika-core/src/confirm.rs`, `crates/mika-core/src/session.rs`. Modify `events.rs`. Remove `LowRiskOnlyGate` из продукта (остаётся под `cfg(test)`).

```rust
// events.rs
ConfirmRequested { id: u64, tool: String, assessment: Assessment, allow_always: bool, timeout_s: u64 },
ConfirmResolved { id: u64, answer: Answer, by: DecidedBy },

// confirm.rs
pub enum Answer { Once, Always, Deny, Timeout }               // snake_case
pub enum DecidedBy { Rule, Button, Chat, Timeout, Policy }    // snake_case
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);
pub struct Confirmations { .. }
impl Confirmations {
    pub fn new(bus: EventBus, state: StateHandle) -> Arc<Self>;
    pub async fn ask(&self, tool: &str, assessment: &Assessment, timeout: Duration) -> (Answer, DecidedBy);
    pub fn answer(&self, id: u64, answer: Answer, by: DecidedBy) -> Result<(), ConfirmError>; // NotPending, HighRiskForever
    pub fn pending(&self) -> Option<PendingView>;               // id, tool, assessment
}
pub fn parse_answer(text: &str) -> Option<Answer>;

// policy/gate.rs
pub struct PolicyGate { rules: RuleStore, audit: AuditLog, confirms: Arc<Confirmations>, timeout: Duration }
impl Gate for PolicyGate { .. }   // правило -> выполнить/запретить; иначе ask; Always создаёт правило

// session.rs
pub struct Session { agent: Arc<Agent>, confirms: Arc<Confirmations>, bus: EventBus, cancel: Mutex<CancellationToken> }
impl Session {
    pub fn submit(self: &Arc<Self>, text: &str) -> Submitted;   // Answered | Started | Hinted | Empty
    pub fn stop(&self);                                         // отменяет и закрывает открытый запрос отказом
}
```

- [ ] Тесты `parse_answer`: «да», «Да!», «подтверждаю», «разреши», «ок» → Once; «разреши всегда», «всегда» → Always; «нет», «отмена», «не надо», «отклони» → Deny; «да нет наверное», «привет» → None.
- [ ] Тесты шлюза: `no_rule_asks_even_for_low_risk`, `once_runs_without_creating_rule`, `always_creates_rule_and_next_call_skips_question`, `high_risk_always_is_rejected_and_stays_pending`, `deny_rule_blocks_without_question`, `timeout_denies_and_late_answer_is_rejected`, `every_decision_is_audited`, `awaiting_confirm_state_during_question`.
- [ ] Тесты сессии: `text_during_question_answers_it`, `unrelated_text_during_question_gets_hint`, `stop_denies_open_question`.

### Task 4: Интерфейс

**Files:** Modify `crates/mika-gui/src/{app.rs, bridge.rs}`, `crates/mika-cli/src/main.rs`. Create `frontend/src/lib/confirm.ts`, `confirm.test.ts`, `ConfirmCard.svelte`, `Permissions.svelte`.

Команды: `answer_confirm(id, answer)`, `get_pending_confirm()`, `list_rules()`, `remove_rule(id)`, `recent_audit(limit)`.

```ts
// confirm.ts
export interface Pending { id: number; tool: string; assessment: Assessment; allowAlways: boolean; deadline: number }
export function reducePending(p: Pending | null, event: unknown, now: number): Pending | null;
export function secondsLeft(p: Pending, now: number): number;   // не меньше 0, округление вверх
export const RISK_LABELS: Record<RiskLevel, string>;           // «низкий», «средний», «высокий»
```

- [ ] Тесты `confirm.test.ts`: `confirm_requested opens`, `confirm_resolved with same id closes`, `other id is ignored`, `secondsLeft counts down and stops at 0`, `malformed ignored`.
- [ ] Карточка: свёрнутый островок принимает форму 600 × 268 с карточкой; в открытом чате карточка над полем ввода. Кнопка «Разрешать всегда» отключена для высокого риска, с подсказкой. Отсчёт секунд.
- [ ] Настройки: вкладки «Провайдер» и «Разрешения». Во второй — правила с кнопкой «Отозвать» и последние 30 записей журнала.
- [ ] CLI: строка во время вопроса идёт в `Session::submit`, вопрос печатается как `[confirm] <summary>: <target> (риск ...) — да / разреши всегда / нет`.

## Готово, когда

- Карточка появляется на каждый вызов без правила.
- «Разрешать всегда» создаёт правило, повторный вызов проходит без вопроса.
- Для высокого риска правило создать нельзя.
- Через 60 секунд без ответа LLM получает отказ.
- Табличный тест классификатора покрывает не меньше 40 команд.
