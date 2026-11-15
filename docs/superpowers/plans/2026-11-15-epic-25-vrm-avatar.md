# Эпик 25. Аватар из модели VRM: план постройки

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** пользователь выбирает файл `.vrm` на странице «Персонаж», и Мика показывает эту модель живой вместо встроенных картинок.

**Устройство:** Rust хранит модели, проверяет файл, сопоставляет выражения с десятью слотами эмоций и ведёт профиль. Интерфейс рисует модель через three.js за прежним компонентом `Avatar.svelte`: живая сцена для вида `full`, готовые портреты для значков, встроенные картинки как запасной путь. Ядро эмоций и события не меняются.

**Стек:** Rust (`mika-core`, `mika-gui`), Tauri 2, Svelte 5, TypeScript, `three`, `@pixiv/three-vrm`, vitest.

**Дизайн:** [2026-11-15-vrm-avatar-design.md](../specs/2026-11-15-vrm-avatar-design.md). Исполнитель читает его вместе с планом.

## Общие требования

- Коммит и push делаются только по просьбе пользователя. Шагов «закоммитить» в задачах нет.
- Новых крейтов нет. `mika-core` не зависит от Tauri. Модули в виде `foo.rs` рядом с папкой `foo/`, файлов `mod.rs` нет.
- Новые пакеты npm ровно два: `three` и `@pixiv/three-vrm`, оба под MIT. Версии последние стабильные на день установки, записываются в `package.json` с `^`.
- Импортировать `three` и `@pixiv/three-vrm` может только `frontend/src/lib/model/scene.ts`. Этот файл подгружается динамическим `import()`.
- Журнал: категория `app`, цель указана явно (`log::info!(target: "app", …)`). В записи идентификатор модели и вид ошибки. Название, автор и путь исходного файла не пишутся. Из цикла отрисовки записей нет.
- Каждому новому ключу в `ru.json` нужен ключ в `en.json` в том же месте.
- Код, идентификаторы и записи журнала на английском, документы на русском.
- Файл модели не больше 200 МБ. Частота кадров не выше 30. Плотность пикселей не выше 2.
- Интерфейс не передаёт и не получает путей к файлам. Идентификатор модели — 16 шестнадцатеричных знаков в нижнем регистре, всё остальное Rust отклоняет.
- Встроенный аватар (`avatar.ts`, картинки) не меняется по поведению.

## На что смотреть при ревью

Пять случаев, которые дизайн подразумевает, а обычные тесты задач легко пропускают. У каждого есть тест в названной задаче.

1. Идентификатор из webview с `..`, косой чертой или лишней длиной. Ожидание: отказ, файл вне папки моделей не читается и не удаляется (задача 3).
2. Оборванный или лживый файл: заявленная длина описания больше файла или больше 16 МБ. Ожидание: ошибка, без паники и без выделения памяти под заявленную длину (задача 1).
3. Модель без единого выражения. Ожидание: импорт проходит, слоты показываются позой, расчёт слоёв не падает (задачи 2 и 5).
4. Папка выбранной модели пропала (перенос на другой компьютер, ручная чистка). Ожидание: выбран встроенный аватар, настройка не портится, одна запись в журнале (задача 4).
5. Сцена не создалась или потеряна в одном окне. Ожидание: это окно показывает картинки, второе окно работает как работало (задачи 6 и 7).

---

## Файлы

| Файл | За что отвечает |
|---|---|
| `crates/mika-core/src/avatar.rs` | Корень модуля, `ModelError`, повторный экспорт |
| `crates/mika-core/src/avatar/glb.rs` | Чтение описания (JSON) из двоичного glTF |
| `crates/mika-core/src/avatar/vrm.rs` | Проверка, что это допустимая модель VRM; название, автор, условия, выражения |
| `crates/mika-core/src/avatar/slots.rs` | Сопоставление выражений с десятью слотами |
| `crates/mika-core/src/avatar/profile.rs` | Профиль модели: формат файла, чтение и запись |
| `crates/mika-core/src/avatar/store.rs` | Папка моделей: импорт, список, байты, портреты, удаление |
| `crates/mika-core/src/paths.rs` | `models_dir()` |
| `crates/mika-core/src/config.rs` | `avatar.model` |
| `crates/mika-gui/src/avatar_model.rs` | Команды Tauri для моделей |
| `crates/mika-gui/src/window.rs`, `tray.rs` | Сигнал «окно видно / не видно» |
| `frontend/src/lib/model/layers.ts` | Чистый расчёт слоёв |
| `frontend/src/lib/model/show.ts` | Выбор способа показа, имена портретов |
| `frontend/src/lib/model/state.svelte.ts` | Состояние окна: выбранная модель, портреты, видимость окна |
| `frontend/src/lib/model/scene.ts` | Сцена three.js, единственное место с `three` |
| `frontend/src/lib/model/ModelView.svelte` | Живой вид `full` |
| `frontend/src/lib/model/Portrait.svelte` | Значок из портрета |
| `frontend/src/lib/Avatar.svelte` | Переключатель трёх способов показа |
| `frontend/src/lib/settings/ModelTiles.svelte`, `ModelSlots.svelte`, `Persona.svelte` | Страница «Персонаж» |
| `frontend/src/dev/mock.ts` | Подставные команды для предпросмотра |

---

### Задача 1. Разбор файла VRM

**Файлы:**
- Создать: `crates/mika-core/src/avatar.rs`, `crates/mika-core/src/avatar/glb.rs`, `crates/mika-core/src/avatar/vrm.rs`
- Изменить: `crates/mika-core/src/lib.rs` (`pub mod avatar;`)

**Интерфейсы, которые даёт задача:**

```rust
// avatar.rs
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("not a binary glTF 2 file")] NotGlb,
    #[error("no VRM extension")] NotVrm,
    #[error("the file is too big")] TooBig,
    #[error("the model refers to outside files")] External,
    #[error("unsupported extension: {0}")] Unsupported(String),
    #[error("the model file is broken")] Broken,
    #[error("unknown model")] Unknown,
    #[error("bad picture")] BadStill,
    #[error(transparent)] Io(#[from] std::io::Error),
}
impl ModelError { pub fn kind(&self) -> &'static str }   // "not_glb", "not_vrm", "too_big", "external", "unsupported", "broken", "unknown", "bad_still", "io"

pub const MAX_MODEL_BYTES: u64 = 200 * 1024 * 1024;

// avatar/glb.rs
pub const MAX_JSON_BYTES: u32 = 16 * 1024 * 1024;
/// Reads the header and the first chunk only.
pub fn read_json(reader: &mut impl std::io::Read) -> Result<serde_json::Value, ModelError>;

// avatar/vrm.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Spec { #[serde(rename = "0.x")] V0, #[serde(rename = "1.0")] V1 }
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo { pub spec: Spec, pub title: String, pub author: String, pub license: String, pub expressions: Vec<String> }
pub fn inspect(json: &serde_json::Value) -> Result<ModelInfo, ModelError>;
```

Правила, которые тесты и сигнатуры не определяют:

- Заголовок glTF: 12 байт, магия `glTF` (`0x46546C67`, порядок байтов от младшего), версия 2. Затем заголовок куска: длина и тип `JSON` (`0x4E4F534A`). Длина больше `MAX_JSON_BYTES` — `Broken`, до выделения памяти. Нехватка байтов — `Broken`.
- VRM 1.0: `extensions.VRMC_vrm`. Выражения: ключи `expressions.preset` и `expressions.custom`. Название `meta.name`, автор — `meta.authors` через запятую. Условия одной строкой из `meta.licenseUrl`, `avatarPermission`, `commercialUsage`, какие есть.
- VRM 0.x: `extensions.VRM`. Выражения: `blendShapeMaster.blendShapeGroups[]`; если `presetName` известен, имя приводится к 1.0, иначе берётся `name`. Таблица: `a→aa`, `i→ih`, `u→ou`, `e→ee`, `o→oh`, `joy→happy`, `sorrow→sad`, `fun→relaxed`, `angry→angry`, `blink→blink`, `blink_l→blinkLeft`, `blink_r→blinkRight`, `lookup→lookUp`, `lookdown→lookDown`, `lookleft→lookLeft`, `lookright→lookRight`, `neutral→neutral`. Название `meta.title`, автор `meta.author`, условия из `licenseName`, `allowedUserName`, `commercialUssageName` (так в формате).
- `External`: у любого элемента `buffers` или `images` есть `uri`, который не начинается с `data:`.
- `Unsupported(имя)`: в `extensionsRequired` есть имя не из списка `VRMC_vrm`, `VRM`, `VRMC_springBone`, `VRMC_springBone_extended_collider`, `VRMC_node_constraint`, `VRMC_materials_mtoon`, `VRMC_materials_hdr_emissiveMultiplier`, `KHR_materials_unlit`, `KHR_texture_transform`, `KHR_materials_emissive_strength`.
- Список выражений без повторов, в порядке файла. Строки названия, автора и условий обрезаются до 200 знаков.

- [ ] **Шаг 1. Тесты (падают).** В `glb.rs` помощник теста `fn glb(json: &str) -> Vec<u8>` собирает верный файл из описания. Тесты:

```rust
#[test] fn a_json_chunk_is_read_back()              // read_json(&mut &glb(r#"{"asset":{"version":"2.0"}}"#)[..]) == that value
#[test] fn a_file_that_is_not_gltf_is_refused()     // b"PK\x03\x04…" -> NotGlb; version 1 -> NotGlb
#[test] fn a_cut_file_is_broken()                   // glb(..) truncated at 10, 15 and len-1 bytes -> Broken
#[test] fn a_lying_length_is_refused_before_reading() // chunk length u32::MAX and 16 MB + 1 -> Broken
#[test] fn vrm_1_gives_its_meta_and_expressions()   // preset {happy, blink, aa}, custom {Blush} -> spec V1, ["happy","blink","aa","Blush"], title, author
#[test] fn vrm_0_names_become_vrm_1_names()         // groups joy, sorrow, a, blink_l, unknown "Shy" -> ["happy","sad","aa","blinkLeft","Shy"]
#[test] fn a_plain_gltf_is_not_vrm()                // no extension -> NotVrm
#[test] fn an_outside_buffer_or_image_is_refused()  // buffers[1].uri "https://x/y.bin" -> External; images[0].uri "tex.png" -> External; "data:…" passes
#[test] fn a_required_extension_we_cannot_show_is_refused() // extensionsRequired ["KHR_draco_mesh_compression"] -> Unsupported("KHR_draco_mesh_compression")
#[test] fn a_model_without_expressions_is_accepted() // expressions == []
```

- [ ] **Шаг 2.** `cargo test -p mika-core avatar::` — не компилируется или падает.
- [ ] **Шаг 3.** Написать `read_json`, `inspect`, `ModelError::kind`.
- [ ] **Шаг 4.** `cargo test -p mika-core avatar::` — проходит.

---

### Задача 2. Слоты и профиль

**Файлы:**
- Создать: `crates/mika-core/src/avatar/slots.rs`, `crates/mika-core/src/avatar/profile.rs`

**Берёт:** `ModelInfo`, `Spec`, `ModelError` из задачи 1; `mika_core::emotion::Emotion` (`Emotion::ALL`, `as_str`).

**Даёт:**

```rust
// slots.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source { Auto, Manual }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    pub source: Source,
    /// Expression name and weight 0..=1.
    pub mix: Vec<(String, f32)>,
    /// Yaw and pitch of the gaze, degrees; positive is to her left and up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gaze: Option<[f32; 2]>,
}
pub fn auto_slot(emotion: Emotion, expressions: &[String]) -> Slot;
pub fn manual_slot(expression: &str) -> Slot;      // mix [(expression, 1.0)], no gaze

// profile.rs
pub const PROFILE_VERSION: u32 = 1;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub version: u32,
    pub format: String,                 // "vrm"
    pub spec: Spec,
    pub title: String,
    pub author: String,
    pub license: String,
    pub expressions: Vec<String>,
    /// Keyed by `Emotion::as_str()`, all ten present.
    pub slots: std::collections::BTreeMap<String, Slot>,
    /// Lip strength, 0.0..=1.5.
    pub lip: f32,
}
impl Profile {
    pub fn from_info(info: &ModelInfo) -> Profile;                    // lip 1.0, every slot auto
    pub fn set_slot(&mut self, emotion: Emotion, expression: Option<&str>) -> Result<(), ModelError>; // None -> auto; a name not in `expressions` -> Unknown
    pub fn set_lip(&mut self, lip: f32);                              // clamped to 0.0..=1.5, NaN -> 1.0
    pub fn read(path: &Path) -> Result<Profile, ModelError>;          // bad JSON or another version -> Broken
    pub fn write(&self, path: &Path) -> Result<(), ModelError>;       // temp file, then rename
}
```

Порядок `auto_slot`: стандартное выражение, если оно есть в модели; иначе своё выражение, в названии которого без учёта регистра есть слово слота; иначе запасная смесь, из которой выпадают отсутствующие выражения. Своим считается выражение, имени которого нет в наборе 1.0 (`happy`, `angry`, `sad`, `relaxed`, `surprised`, `aa`, `ih`, `ou`, `ee`, `oh`, `blink`, `blinkLeft`, `blinkRight`, `lookUp`, `lookDown`, `lookLeft`, `lookRight`, `neutral`).

| Слот | Стандартное | Слова в названии | Запасная смесь | Взгляд |
|---|---|---|---|---|
| neutral | — | — | пусто | — |
| joy | `happy` | happy, joy, smile, 喜, 笑, рад | пусто | — |
| surprise | `surprised` | surprise, 驚, удив | `oh` 0.5 | — |
| sad | `sad` | sad, sorrow, cry, 悲, груст | пусто | — |
| angry | `angry` | angry, anger, 怒, зл | пусто | — |
| wink | — | wink, ウインク, подмиг | `blinkLeft` 1.0, `happy` 0.5 | — |
| sleepy | — | sleep, 眠, сон | `relaxed` 0.5, `blink` 0.6 | — |
| shy | — | shy, blush, embarrass, 照, смущ | `relaxed` 0.6 | `[15, -15]` |
| think | — | think, 考, дума | пусто | `[-15, 15]` |
| worry | — | worry, anxious, trouble, 困, трев | `sad` 0.5, `surprised` 0.2 | — |

Взгляд ставится только в запасной смеси. Найденное по стандарту или по названию выражение идёт с весом 1.0 без взгляда.

- [ ] **Шаг 1. Тесты (падают).**

```rust
#[test] fn standard_expressions_fill_their_slots()   // ["happy","sad","angry","surprised"] -> joy [("happy",1.0)], … source Auto
#[test] fn a_custom_expression_is_found_by_its_name() // ["happy","Blush_A"] -> shy [("Blush_A",1.0)], gaze None; "照れ" too
#[test] fn a_standard_name_is_never_taken_as_custom() // ["relaxed"] -> sleepy is the fallback [("relaxed",0.5)], not a name match
#[test] fn fallbacks_drop_what_the_model_lacks()      // ["happy"] -> wink [("happy",0.5)]; ["sad"] -> worry [("sad",0.5)]
#[test] fn vrm_0_surprise_falls_back_to_the_mouth()   // ["aa","oh"] -> surprise [("oh",0.5)]
#[test] fn a_model_without_expressions_gets_empty_mixes() // every slot: mix empty; shy and think keep their gaze
#[test] fn a_manual_choice_replaces_the_mix_and_the_gaze() // set_slot(Shy, Some("happy")) -> Manual, [("happy",1.0)], gaze None; set_slot(Shy, None) -> auto again
#[test] fn a_manual_choice_must_be_an_expression_of_the_model() // set_slot(Joy, Some("nope")) -> Err(Unknown)
#[test] fn lip_strength_stays_in_range()               // 9.0 -> 1.5; -1.0 -> 0.0; NaN -> 1.0
#[test] fn a_profile_survives_the_disk()                // write, read -> equal; ten slots present
#[test] fn a_profile_of_another_version_is_broken()     // {"version": 2, …} -> Broken
```

- [ ] **Шаг 2.** `cargo test -p mika-core avatar::` — падает.
- [ ] **Шаг 3.** Написать `slots.rs` и `profile.rs`.
- [ ] **Шаг 4.** `cargo test -p mika-core avatar::` — проходит.

---

### Задача 3. Папка моделей и настройка

**Файлы:**
- Создать: `crates/mika-core/src/avatar/store.rs`
- Изменить: `crates/mika-core/src/paths.rs`, `crates/mika-core/src/config.rs:504-521`

**Берёт:** задачи 1 и 2.

**Даёт:**

```rust
// paths.rs: %LOCALAPPDATA%\Mika\models, built like `vosk_dir()`
pub fn models_dir() -> PathBuf;

// config.rs: AvatarConfig loses `Copy`
pub struct AvatarConfig { pub in_island: bool, pub on_desktop: bool, /// Id of the chosen model; empty is the built-in avatar.
    pub model: String }

// avatar/store.rs
pub const MAX_STILL_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry { pub id: String, pub profile: Profile, pub size_bytes: u64, pub has_stills: bool }
pub struct Store { /* dir */ }
impl Store {
    pub fn new(dir: PathBuf) -> Store;                           // limit MAX_MODEL_BYTES
    pub(crate) fn with_limit(dir: PathBuf, max_bytes: u64) -> Store; // for tests
    pub fn import(&self, file: &Path) -> Result<ModelEntry, ModelError>;
    pub fn list(&self) -> Vec<ModelEntry>;                       // sorted by title, then id; broken folders are skipped
    pub fn entry(&self, id: &str) -> Result<ModelEntry, ModelError>;
    pub fn bytes(&self, id: &str) -> Result<Vec<u8>, ModelError>; // checks the file again before reading it whole
    pub fn set_slot(&self, id: &str, emotion: Emotion, expression: Option<&str>) -> Result<Profile, ModelError>;
    pub fn set_lip(&self, id: &str, lip: f32) -> Result<Profile, ModelError>;
    pub fn save_stills(&self, id: &str, stills: &[(String, Vec<u8>)]) -> Result<(), ModelError>;
    pub fn stills(&self, id: &str) -> Vec<(String, Vec<u8>)>;
    pub fn delete(&self, id: &str) -> Result<(), ModelError>;
}
pub fn still_names() -> Vec<String>;   // "neutral", "neutral-talk", … for the ten emotions in Emotion::ALL order, then "full": 21 names
```

Правила:

- Идентификатор: первые 16 шестнадцатеричных знаков SHA-256 файла, нижний регистр. Любая строка другого вида во всех методах даёт `Unknown` до обращения к диску.
- `import`: размер файла по метаданным больше предела хранилища — `TooBig` до чтения; затем `read_json` и `inspect`; копия в `<dir>/<id>/model.vrm`, `profile.json` из `Profile::from_info`. Если папка с таким идентификатором уже есть, возвращается её запись, профиль не перезаписывается.
- `bytes`: снова `read_json` и `inspect` по файлу в папке; ошибка проверки возвращается как есть.
- `save_stills`: имя должно быть из `still_names()`, байты начинаются с подписи PNG (`89 50 4E 47 0D 0A 1A 0A`), размер не больше `MAX_STILL_BYTES`; иначе `BadStill`, и ничего не записано. Файлы `<dir>/<id>/portraits/<имя>.png`.
- `has_stills`: на диске есть все 21 файл.
- `delete`: стирает только папку `<dir>/<id>`.

- [ ] **Шаг 1. Тесты (падают).** Каталог теста — временная папка, файл модели собирается помощником из задачи 1 (вынести его в `avatar.rs` под `#[cfg(test)] pub(crate)`).

```rust
#[test] fn an_imported_model_is_listed_with_its_profile()  // id is 16 hex chars; list() has it; model.vrm equals the source
#[test] fn the_same_file_twice_is_one_model()               // second import returns the same id; a manual slot set between them is kept
#[test] fn a_bad_file_leaves_no_folder()                    // NotVrm -> dir stays empty
#[test] fn a_file_over_the_limit_is_refused_by_its_size()   // Store::with_limit(dir, 64): a valid model of more than 64 bytes -> TooBig, no folder
#[test] fn an_id_that_is_not_16_hex_chars_is_unknown()      // "../x", "ABCDEF0123456789", "0123", "0123456789abcdef/.." for bytes, entry, delete, set_slot, save_stills, stills
#[test] fn a_file_swapped_after_import_is_checked_again()   // overwrite model.vrm with an External one -> bytes() Err(External)
#[test] fn stills_are_saved_only_when_they_are_png_with_known_names() // "neutral" ok; "evil" -> BadStill; "../x" -> BadStill; not PNG -> BadStill; 1 MB + 1 -> BadStill; nothing written on error
#[test] fn has_stills_needs_all_twenty_one()
#[test] fn delete_removes_the_folder_and_nothing_else()     // a sibling folder and a file in dir stay
#[test] fn models_dir_sits_next_to_the_logs()               // paths test in the style of the existing ones
#[test] fn an_old_config_without_avatar_model_loads()       // c.avatar.model == ""
```

- [ ] **Шаг 2.** `cargo test -p mika-core` — падает.
- [ ] **Шаг 3.** Написать код. Убрать `Copy` у `AvatarConfig`, поправить места, которые перестали собираться.
- [ ] **Шаг 4.** `cargo test -p mika-core` — проходит. `cargo clippy -p mika-core --all-targets -- -D warnings` чисто.

---

### Задача 4. Команды Tauri и сигнал видимости окна

**Файлы:**
- Создать: `crates/mika-gui/src/avatar_model.rs`
- Изменить: `crates/mika-gui/src/main.rs` (модуль, регистрация команд), `crates/mika-gui/src/bridge.rs:236-269` (`UiSettings.avatar_model`), `crates/mika-gui/src/window.rs:567-615` (`bring_front`, `send_back`), `crates/mika-gui/src/tray.rs:58-76`, `frontend/src/locales/ru.json`, `frontend/src/locales/en.json`

**Берёт:** `Store`, `ModelEntry`, `Profile`, `ModelError`, `models_dir()`, `still_names()`; `crate::file_dialog::pick_open(owner, title, (label, pattern))`; `crate::strings::tr`; `crate::tray::UI_CHANNEL`; `ctx.config`, `ctx.save_config()`.

**Даёт (имена полей JSON такие же):**

```rust
#[derive(Serialize)] pub struct ModelView { id: String, title: String, author: String, license: String, spec: Spec, size_bytes: u64, has_stills: bool }
#[derive(Serialize)] pub struct AvatarModels { active: String, models: Vec<ModelView> }
#[derive(Serialize, Deserialize)] pub struct Still { name: String, png_base64: String }

#[tauri::command] pub fn avatar_models(ctx) -> AvatarModels;
#[tauri::command] pub async fn avatar_model_add(window: tauri::WebviewWindow, ctx) -> Result<Option<ModelView>, String>; // None: the dialog was closed
#[tauri::command] pub fn avatar_model_select(app, ctx, id: String) -> Result<(), String>;        // "" is the built-in avatar
#[tauri::command] pub fn avatar_model_delete(app, ctx, id: String) -> Result<(), String>;
#[tauri::command] pub fn avatar_model_profile(id: String) -> Result<Profile, String>;
#[tauri::command] pub async fn avatar_model_bytes(id: String) -> Result<tauri::ipc::Response, String>; // raw bytes, read in spawn_blocking
#[tauri::command] pub fn avatar_model_set_slot(app, id: String, emotion: Emotion, expression: Option<String>) -> Result<Profile, String>;
#[tauri::command] pub fn avatar_model_set_lip(app, id: String, lip: f32) -> Result<Profile, String>;
#[tauri::command] pub fn avatar_model_save_stills(app, id: String, stills: Vec<Still>) -> Result<(), String>;
#[tauri::command] pub fn avatar_model_stills(id: String) -> Vec<Still>;
#[tauri::command] pub fn avatar_model_failed(id: String, kind: String);   // the page could not show the model

/// The id to show: `configured` when its folder is there, else "".
pub fn active_model(configured: &str, present: &[String]) -> String;
pub fn error_text(error: &ModelError) -> String;   // tr("error.model_<kind>")
pub fn window_shown(app: &AppHandle, label: &str, shown: bool); // emits on UI_CHANNEL
```

Правила:

- `avatar_models().active` считается через `active_model`. Настройка при этом не переписывается. Когда настроенная модель пропала, пишется одна запись `WARN` за запуск: `the chosen avatar model is gone, showing the built-in one: id=…`.
- `avatar_model_add`: диалог и импорт идут в `spawn_blocking`; фильтр `("VRM", "*.vrm")`, заголовок `tr("persona.model_pick")`. Запись `INFO`: `avatar model imported: id=… spec=… bytes=… expressions=N`; при отказе `WARN` с `kind`.
- `avatar_model_select`: непустой идентификатор должен быть в хранилище, иначе ошибка. Записывает `avatar.model`, сохраняет настройки, шлёт `{"action":"avatar_model_changed"}` на `UI_CHANNEL`.
- `avatar_model_delete`: если удалена выбранная, `avatar.model` становится пустой; шлёт `avatar_model_changed`.
- `avatar_model_set_slot`, `avatar_model_set_lip`, `avatar_model_save_stills` шлют `avatar_model_changed`, чтобы второе окно перечитало профиль и портреты.
- `avatar_model_failed`: `kind` принимается только из набора `webgl`, `load`, `context_lost`, иное пишется как `other`. Запись `WARN`: `the avatar model is not shown, the built-in one is: id=… kind=…`. Одна запись на пару «модель, вид» за запуск.
- `window_shown` шлёт `{"action":"window_shown","label":"island"|"avatar","shown":bool}`. Вызовы: в `tray.rs` при «скрыть» и «показать» для обоих окон; в `send_back` — `false` для каждого окна, которое действительно ушло вниз; в `bring_front` — `true` для тех же окон.
- Тексты ошибок: ключи `error.model_not_glb`, `error.model_not_vrm`, `error.model_too_big`, `error.model_external`, `error.model_unsupported` (с именем расширения), `error.model_broken`, `error.model_unknown`, `error.model_bad_still`, `error.model_io` в обоих файлах локализации.

- [ ] **Шаг 1. Тесты (падают).**

```rust
#[test] fn a_chosen_model_that_is_gone_shows_the_built_in_one() // active_model("0123456789abcdef", &[]) == ""; present -> itself; "" -> ""
#[test] fn every_model_error_has_a_text_in_both_languages()      // for each kind: key error.model_<kind> exists in ru.json and en.json
#[test] fn the_window_signal_names_the_window_and_the_state()    // payload builder: {"action":"window_shown","label":"avatar","shown":false}
#[test] fn stills_cross_as_base64_png()                          // Still -> (name, bytes) and back; bad base64 -> Err
#[test] fn a_failure_kind_from_the_page_is_one_of_ours()          // "webgl" stays; "x\ny=1" -> "other"
```

- [ ] **Шаг 2.** `cargo test -p mika-gui avatar_model` — падает. Перед этим собрать интерфейс: `npm --prefix frontend run build`.
- [ ] **Шаг 3.** Написать команды, зарегистрировать их в `main.rs`, добавить `avatar_model` в `UiSettings`, вызвать `window_shown` в четырёх местах, добавить ключи локализации.
- [ ] **Шаг 4.** `cargo test -p mika-gui`, `cargo clippy --workspace --all-targets -- -D warnings`, тест `journal_guard` крейта — проходят.

---

### Задача 5. Пакеты и расчёт слоёв

**Файлы:**
- Изменить: `frontend/package.json`, `frontend/package-lock.json` (`npm --prefix frontend install three @pixiv/three-vrm`, плюс `@types/three` в `devDependencies`, если пакет `three` не несёт своих типов)
- Создать: `frontend/src/lib/model/layers.ts`, `frontend/src/lib/model/layers.test.ts`

**Берёт:** `Emotion`, `blinkPause`, `gesturePause`, `MOUTH_STEP_MS` из `frontend/src/lib/avatar.ts`.

**Даёт:**

```ts
export interface Slot { source: 'auto' | 'manual'; mix: [string, number][]; gaze?: [number, number] }
export interface Profile { version: number; format: string; spec: '0.x' | '1.0'; title: string; author: string; license: string; expressions: string[]; slots: Record<Emotion, Slot>; lip: number }

export const FPS = 30;
export const MAX_PIXEL_RATIO = 2;
export const EMOTION_BLEND_MS = 250;
export const VOWELS = ['aa', 'ih', 'ou', 'ee', 'oh'] as const;
export type Vowel = (typeof VOWELS)[number];

/** Weight of the blink expression `ms` after a blink began: up in 50, held 90, down in 50. */
export function blinkWeight(ms: number): number;
/** False when the slot itself closes the eyes: blink, blinkLeft or blinkRight at 0.5 or more. */
export function canModelBlink(slot: Slot): boolean;
/** Next mouth shape; `null` is a closed mouth; never the previous one. */
export function nextVowel(prev: Vowel | null, r: number): Vowel | null;
/** Lip gain: the profile strength, times 0.6 under any emotion but neutral. */
export function lipGain(profile: Profile, emotion: Emotion): number;
/** One step of exponential smoothing towards `target`. */
export function approach(current: number, target: number, dtMs: number, tauMs: number): number;
/** Target weights of a slot, without names the model lacks. */
export function slotWeights(slot: Slot, available: ReadonlySet<string>): Map<string, number>;
/** Head and spine angles of a slot, degrees: pitch (down is positive), yaw, roll, spine pitch. */
export function slotPose(emotion: Emotion): { pitch: number; yaw: number; roll: number; spine: number };
/** Chest pitch of breathing at `ms`, degrees: period 4000 ms, amplitude 1.5. */
export function breath(ms: number): number;
/** Vertical offset (share of her height) and roll (degrees) of a one-off gesture at `ms`; null when it is over. */
export function gestureAt(kind: Emotion | 'poke', ms: number): { y: number; roll: number } | null;
/** The part of her the camera shows: centre height and visible height, metres. `topY` is the top of the head. */
export function frameFor(view: 'full' | 'head', headY: number, hipsY: number, topY: number): { centerY: number; height: number };
/** Whether to draw a frame now. */
export function shouldRender(s: { attached: boolean; windowShown: boolean; documentHidden: boolean }): boolean;
/** Whether enough time has passed for the next frame at FPS. */
export function frameDue(nowMs: number, lastMs: number): boolean;
```

Значения:

- `slotPose`: neutral `0,0,0,0`; joy `-4,0,4,0`; surprise `-6,0,0,-3`; angry `6,0,0,3`; sad `12,0,0,4`; shy `8,12,-6,0`; think `-6,-12,8,0`; wink `0,0,8,0`; sleepy `14,0,4,2`; worry `4,6,0,1`. Это начальные значения, уточняются на живой модели в задаче 7.
- `gestureAt('poke')`: 450 мс, подъём до `y = 0.03` в середине. Для эмоций длительности из `MOTIONS` в `avatar.ts`, высота из пикселей по правилу 1 px = 0.0025 роста, наклон в градусах как там. Neutral — `null` сразу.
- `frameFor('full')`: от `topY + 0.06` сверху до `hipsY - 0.05` снизу. `frameFor('head')`: квадрат с центром `headY + 0.06`, высота `(topY - headY) * 2.6`.
- Сглаживание губ: `tauMs = 60`. Смена гласной раз в `MOUTH_STEP_MS`.

- [ ] **Шаг 1.** Установить пакеты. `npm --prefix frontend run check` проходит.
- [ ] **Шаг 2. Тесты (падают).**

```ts
it('a blink goes up, holds and comes down')            // blinkWeight(0)=0, (25)=0.5, (50)=1, (140)=1, (165)=0.5, (190)=0, (500)=0
it('a slot that closes the eyes does not blink')       // mix [['blink',0.6]] -> false; [['blinkLeft',1]] -> false; [['happy',1]] -> true; [] -> true
it('the mouth never repeats a shape')                  // for 200 random r: nextVowel(p, r) !== p; both null and all five vowels occur
it('lips are quieter under an emotion')                // lip 1 -> neutral 1, joy 0.6; lip 0 -> 0
it('smoothing reaches the target and never overshoots')// approach(0,1,60,60) ≈ 0.632; approach(0,1,1e6,60) = 1; approach(1,1,16,60) = 1
it('weights skip expressions the model lacks')         // mix [['happy',1],['nope',0.5]], available {happy} -> Map {happy: 1}
it('a model without expressions gives empty weights')  // every emotion of a profile with empty mixes -> size 0, and slotPose still answers
it('the neutral slot has no pose and no gesture')      // slotPose('neutral') all zero; gestureAt('neutral', 0) === null
it('a poke is a hop that ends')                        // gestureAt('poke', 225).y ≈ 0.03; gestureAt('poke', 451) === null
it('the full view runs from above the head to the hips') // frameFor('full', 1.4, 0.9, 1.55) -> height 0.76, centerY 1.23
it('the head view is a square around the head')        // frameFor('head', 1.4, 0.9, 1.55) -> centerY 1.46, height 0.39
it('nothing is drawn when the view is not seen')       // any of the three flags off -> false
it('frames come no faster than 30 a second')           // frameDue(1000, 980) false; frameDue(1034, 1000) true
```

- [ ] **Шаг 3.** `npm --prefix frontend test -- layers` — падает.
- [ ] **Шаг 4.** Написать `layers.ts`.
- [ ] **Шаг 5.** `npm --prefix frontend test -- layers` и `npm --prefix frontend run check` — проходят.

---

### Задача 6. Состояние окна, значок и переключатель

**Файлы:**
- Создать: `frontend/src/lib/model/show.ts`, `frontend/src/lib/model/show.test.ts`, `frontend/src/lib/model/state.svelte.ts`, `frontend/src/lib/model/Portrait.svelte`
- Изменить: `frontend/src/lib/Avatar.svelte`, `frontend/src/dev/mock.ts`

**Берёт:** команды задачи 4, `Profile` из задачи 5.

**Даёт:**

```ts
// show.ts
export type Show = 'pictures' | 'live' | 'portrait';
export function pickShow(view: 'head' | 'full', s: { modelId: string; hasStills: boolean; failed: boolean }): Show;
export function stillName(emotion: Emotion, mouthOpen: boolean): string;   // 'joy' | 'joy-talk'
export const FULL_STILL = 'full';
export function windowLabel(search: string): 'island' | 'avatar';           // '?view=avatar' -> 'avatar'

// state.svelte.ts: one per window
export const model: {
  readonly id: string;                       // '' is the built-in avatar
  readonly profile: Profile | null;
  readonly stills: ReadonlyMap<string, string>;   // name -> data URL
  readonly failed: string;                   // '' or the reason the scene is not shown
  readonly windowShown: boolean;
};
export async function startModelState(): Promise<void>;   // reads avatar_models, profile and stills; listens to mika://ui
export function reportFailure(reason: string): void;      // the scene could not be shown in this window
export function clearFailure(): void;                     // called when another model is chosen
```

Правила:

- `pickShow`: без модели или при `failed` — `pictures`. Вид `full` с моделью — `live`. Вид `head` с моделью и портретами — `portrait`, без портретов — `pictures`.
- `startModelState` вызывается один раз из `App.svelte` и из `AvatarWindow.svelte` при монтировании. По `avatar_model_changed` перечитывает всё и снимает `failed`. По `window_shown` со своей меткой окна меняет `windowShown`.
- Вне Tauri и без подставных команд состояние остаётся «встроенный аватар».
- `Portrait.svelte`: свойства `emotion`, `talking`, `size`. Показывает `stills.get(stillName(emotion, open))`; пока `talking`, `open` переключается раз в `MOUTH_STEP_MS * 2`.
- `Avatar.svelte`: свойства не меняются. Нынешняя разметка картинок остаётся веткой `pictures`. Ветка `portrait` рисует `Portrait` в том же круге. Ветка `live` рисует `ModelView` (задача 7); до задачи 7 на её месте ветка `pictures`.
- `mock.ts`: сценарий `?mock=persona&model=1` отдаёт одну подставную модель, профиль и портреты-заглушки (одноцветные PNG из данных в коде), `avatar_model_bytes` отдаёт ошибку. Так значки и страница проверяются без файла модели.

- [ ] **Шаг 1. Тесты (падают).**

```ts
it('without a model she is drawn from pictures')        // pickShow('full'|'head', {modelId:'', …}) === 'pictures'
it('a model is alive in the full view and a portrait in a chip') // 'live'; 'portrait' with stills; 'pictures' without
it('a failed scene falls back to pictures in every view') // failed: true -> 'pictures' for both views, stills or not
it('portraits are named by the emotion and the mouth')  // stillName('joy', false) === 'joy'; ('joy', true) === 'joy-talk'
it('the avatar window knows its label')                 // windowLabel('?view=avatar') === 'avatar'; '' and '?mock=chat' -> 'island'
```

- [ ] **Шаг 2.** `npm --prefix frontend test -- show` — падает.
- [ ] **Шаг 3.** Написать `show.ts`, `state.svelte.ts`, `Portrait.svelte`, ветки в `Avatar.svelte`, подставные команды.
- [ ] **Шаг 4.** `npm --prefix frontend test` и `npm --prefix frontend run check` — проходят. В предпросмотре `http://localhost:5173/?mock=persona&model=1` значки показывают портреты-заглушки, без `model=1` — прежние картинки.

---

### Задача 7. Сцена и живой вид

**Файлы:**
- Создать: `frontend/src/lib/model/scene.ts`, `frontend/src/lib/model/ModelView.svelte`
- Изменить: `frontend/src/lib/Avatar.svelte` (ветка `live`), `frontend/src/App.svelte`, `frontend/src/AvatarWindow.svelte` (вызов `startModelState`)

**Берёт:** всё из `layers.ts`, `model` и `reportFailure` из `state.svelte.ts`, команду `avatar_model_bytes`.

**Даёт:**

```ts
// scene.ts — the only file that imports three and @pixiv/three-vrm
export interface Scene {
  /** Moves the canvas into `host`. The view that had it gets `onLeave` with its last frame as a data URL. */
  attach(host: HTMLElement, onLeave: (lastFrame: string) => void): void;
  /** Gives the canvas up if `host` still has it. */
  detach(host: HTMLElement): void;
  set(input: { emotion: Emotion; talking: boolean; windowShown: boolean; still: boolean }): void;
  gesture(kind: Emotion | 'poke'): void;
  /** Draws the 21 pictures of the model: PNG data URLs by `still_names()` name, 256x256 chips and one 384x512 full frame. */
  stills(): Promise<Map<string, string>>;
  /** Names the loaded model really has. */
  readonly expressions: ReadonlySet<string>;
  dispose(): void;
}
/** The scene of this window for model `id`, loaded once; a second call returns the same promise. */
export function sceneFor(id: string, profile: Profile): Promise<Scene>;
/** Drops the scene of this window: another model was chosen or the profile changed. */
export function dropScene(): void;
```

Правила:

- Одна сцена на окно: один `WebGLRenderer` с `alpha: true`, один холст. `attach` переносит холст в новый узел; прежнему владельцу перед этим отдаётся последний кадр (`render()` и сразу `toDataURL()`), он показывает его картинкой до своего удаления.
- Загрузка: `GLTFLoader` с `VRMLoaderPlugin`, байты из `avatar_model_bytes` как `ArrayBuffer` через `parse`. У `LoadingManager` стоит `setURLModifier`, который пропускает только адреса `data:` и `blob:`, остальные заменяет пустым `data:` и считает отказом загрузки.
- После загрузки: `VRMUtils.removeUnnecessaryVertices`, `combineSkeletons` (или их нынешние имена в установленной версии), для VRM 0.x `VRMUtils.rotateVRM0`. Свет: рассеянный и один направленный спереди сверху.
- Поза покоя ставится на нормализованные кости: плечи опущены на 70°, локти согнуты на 10°. Дыхание — `breath()` на кость груди или верхнюю кость позвоночника.
- Каждый кадр, в порядке слоёв: поза покоя и дыхание, моргание (`blinkWeight`, паузы `blinkPause`, только при `canModelBlink`), взгляд (цель `lookAt`: на камеру, редкий отвод в сторону с паузами `gesturePause`, сдвиг `gaze` слота), губы (`nextVowel`, `lipGain`, `approach`), эмоция (`slotWeights` и `slotPose`, вход и выход через `approach` с `EMOTION_BLEND_MS`), разовый жест (`gestureAt`). Затем `vrm.update(dt)` и отрисовка.
- Цикл на `requestAnimationFrame`, кадр рисуется при `shouldRender` и `frameDue`. При `still: true` (система просит меньше движения) жесты, дыхание и отвод взгляда выключены.
- Камера по `frameFor` от костей головы и бёдер; размер холста следует за узлом через `ResizeObserver`, плотность пикселей `min(devicePixelRatio, MAX_PIXEL_RATIO)`.
- Потеря контекста (`webglcontextlost`), ошибка загрузки, ошибка создания рендерера: `reportFailure(причина)`, сцена освобождается, вызывается `avatar_model_failed(id, kind)` с видом `context_lost`, `load` или `webgl`.
- `ModelView.svelte`: свойства `emotion`, `talking`, `onpoke`. Пока `sceneFor` не готов, показывает портрет `full` из `model.stills`. После готовности вызывает `attach` и плавно проявляет холст. При смене эмоции вызывает `gesture(emotion)`, по щелчку `gesture('poke')` и `onpoke`. При размонтировании `detach`. При смене `model.id` или профиля — `dropScene()` и новая загрузка.
- `Avatar.svelte` в ветке `live` рисует `ModelView` в рамке 3:4 без круга.

- [ ] **Шаг 1.** Написать `scene.ts` и `ModelView.svelte`, подключить ветку `live`, вызвать `startModelState` в двух окнах.
- [ ] **Шаг 2.** `npm --prefix frontend run check`, `npm --prefix frontend test`, `npm --prefix frontend run build` — проходят. В собранном `dist` код `three` лежит в отдельном куске, которого нет среди файлов, загружаемых страницей без модели (проверить по списку запросов в предпросмотре).
- [ ] **Шаг 3. Проверка в предпросмотре с настоящим файлом.** В `mock.ts` добавить сценарий `?mock=persona&vrm=1`: `avatar_model_bytes` читает файл из `frontend/src/dev/sample.vrm` через `fetch`. Файл в репозиторий не входит (добавить строку в `.gitignore`). Файл даёт пользователь, или пробный скачивается с его разрешения; без файла шаги 3 и 4 не выполняются, и это записывается в «Не проверено». Проверить: модель видна на прозрачном фоне, руки опущены, моргает, губы двигаются при `?mock=chat` в состоянии «отвечает», десять эмоций различимы, щелчок даёт подскок, переход чат → настройки не создаёт второй сцены и не мигает, консоль без ошибок. Проверить обе версии формата, если есть оба файла.
- [ ] **Шаг 4.** Уточнить углы `slotPose` и позу покоя по живой модели, поправить значения в `layers.ts` и их тесты.
- [ ] **Шаг 5.** Отказ: в сценарии `?mock=persona&model=1` (байты не отдаются) после отказа и вид `full`, и значки этого окна показывают встроенные картинки, под предпросмотром видна причина; консоль без необработанных ошибок.

---

### Задача 8. Страница «Персонаж»

**Файлы:**
- Создать: `frontend/src/lib/settings/ModelTiles.svelte`, `frontend/src/lib/settings/ModelSlots.svelte`
- Изменить: `frontend/src/lib/settings/Persona.svelte:98-103`, `frontend/src/locales/ru.json:108-112`, `frontend/src/locales/en.json`, `frontend/src/dev/mock.ts`

**Берёт:** команды задачи 4, `model` из задачи 6, `sceneFor` и `Scene.stills()` из задачи 7.

Правила:

- `ModelTiles`: плитка «встроенная», по плитке на модель (название, автор, размер в МБ, крестик), плитка «+ Добавить модель». Щелчок по плитке вызывает `avatar_model_select`. Крестик просит подтверждение вторым нажатием, как «Очистить» на странице «Журнал», затем `avatar_model_delete`.
- Добавление: `avatar_model_add`. При `null` ничего не происходит. При модели без портретов (`has_stills: false`): страница загружает её сцену в предпросмотр, вызывает `stills()`, отдаёт результат в `avatar_model_save_stills`, затем `avatar_model_select`. Пока это идёт, плитка показывает «Готовлю…» и не нажимается. Ошибка любого шага показывается строкой под плитками, модель остаётся в списке невыбранной.
- Та же подготовка портретов запускается, когда выбрана модель с `has_stills: false` и её сцена загрузилась в любом живом виде.
- Под плитками выбранной модели: название, автор, строка условий использования как есть.
- `ModelSlots` виден при выбранной модели: десять строк «слот — список». В списке «Авто» и `profile.expressions`. Выбор вызывает `avatar_model_set_slot` и показывает эту эмоцию в предпросмотре. Ниже ползунок силы губ 0–1.5 с шагом 0.1, `avatar_model_set_lip` по отпусканию.
- Десять значков эмоций слева работают как раньше: щелчок показывает эмоцию в предпросмотре.
- При `model.failed` под предпросмотром строка `persona.model_failed` с причиной.
- Ключи локализации: убрать `persona.model_steam`, `persona.model_upload`, `persona.model_later`; добавить `persona.model_builtin`, `persona.model_add`, `persona.model_pick`, `persona.model_preparing`, `persona.model_delete_confirm`, `persona.model_by`, `persona.model_license`, `persona.model_failed`, `persona.slots`, `persona.slot_auto`, `persona.lip`. Проверить тестом локализации, что наборы ключей в двух файлах совпадают (такой тест уже есть).

- [ ] **Шаг 1.** Написать компоненты, поправить `Persona.svelte`, ключи, подставные команды (`avatar_model_add` в предпросмотре отдаёт вторую подставную модель).
- [ ] **Шаг 2.** `npm --prefix frontend test`, `npm --prefix frontend run check` — проходят.
- [ ] **Шаг 3.** В предпросмотре `?mock=persona&vrm=1`: добавить, выбрать, назначить слот вручную и вернуть «Авто», подвинуть губы, удалить с подтверждением, вернуться к встроенной. На ширине окна настроек плитки не вылезают за край при пяти моделях.

---

### Задача 9. Документы и общая проверка

**Файлы:**
- Создать: `docs/decisions/0039-vrm-avatar.md`
- Изменить: `docs/superpowers/specs/2026-11-10-mika-assistant-design.md` (пункт 8 раздела 2; строки о Live2D в разделах 3 и 4; раздел 9: «Встроенный аватар», «Готовые модели», «Слои на модели»), `docs/superpowers/plans/2026-11-10-roadmap.md` (пункты 7.1, 7.2, 7.7–7.9; абзац про эпик 25), `frontend/src/lib/avatar.ts:1-3` (комментарий в шапке)

Содержание решения 0039: вопрос, решение (VRM первым, общий движок за `Avatar.svelte`, Live2D отложен с двумя путями), четырнадцать решений из раздела 10 дизайна, что не сделано, что не проверено. В основном дизайне формулировки нейтральны к формату: VRM описан как сделанный, Live2D как отложенный, взгляд за курсором и губы по звуку как несделанные.

- [ ] **Шаг 1.** Написать решение, внести правки в дизайн и план.
- [ ] **Шаг 2.** Полная проверка, каждая команда отдельно:

```
npm --prefix frontend test
npm --prefix frontend run check
npm --prefix frontend run build
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Шаг 3.** Записать в решение 0039 раздел «Не проверено»: всё, что не запускалось в настоящем приложении (окно на рабочем столе с прозрачностью, остановка отрисовки под полноэкранной программой, нагрузка в покое, вторая версия формата, если не было файла).
- [ ] **Шаг 4.** Сообщить пользователю итог. Коммит, версия и выпуск — по его слову.

---

## Чего в плане нет намеренно

Live2D, губы по громкости, взгляд за курсором, файлы `.vrma`, поиск по папкам Steam, ручная настройка кадра, перенос файлов моделей в копии, новые инструменты для LLM. Причины в разделе 2 дизайна.
