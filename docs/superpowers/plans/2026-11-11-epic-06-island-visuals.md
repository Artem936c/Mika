# Эпик 6 «Островок» и встроенный аватар Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Интерфейс приложения выглядит и ведёт себя как макет `docs/design/island-mockup.html`: шесть форм островка, аватар с десятью эмоциями, чат с карточками, карточка подтверждения, настройки с боковой панелью.

**Architecture:** Svelte только рисует. Эмоцию выбирает ядро в `mika-core` и присылает событием. Аватар — рисунок SVG из макета, собирается чистой функцией из таблицы эмоций. Данные для карточек погоды и громкости приходят от инструмента в поле `data` события `tool_finished`. Все тексты интерфейса лежат в `frontend/src/locales/ru.json`.

**Tech Stack:** эпики 1–4, Svelte 5, шрифт Manrope (`@fontsource-variable/manrope`, лицензия OFL), значки Lucide (ISC, контуры взяты из макета), второе окно Tauri для аватара на рабочем столе.

**Spec:** [дизайн](../specs/2026-11-10-mika-assistant-design.md), разделы 9 и 10; [макет](../../design/island-mockup.html); [решение 0008](../../decisions/0008-visual-epic.md).

## Global Constraints

- Размеры форм островка, px: покой 236 × 44, слушает 340 × 52, думает 380 × 52, отвечает 620 × 78, чат 860 × 500 с аватаром и 600 × 500 без него, настройки 860 × 500, подтверждение 600 × 268. Радиусы: 22, 26, 26, 30, 34, 34, 30.
- Переход между формами: 620 мс, `cubic-bezier(.32,1.28,.54,1)`. Слои содержимого: проявление 220 мс с размытием 10 px, задержка 160 мс.
- Цвета только из раздела 10 дизайна: `#8f7dff`, `#ff7eb6`, `#6fe3ff`, красный `#ff6b7d` только для опасности. Островок `#06070b`.
- Интерфейс только рисует состояние и отправляет команды. Выбор эмоции, вырезание меток, текст характера живут в Rust.
- Ни одной русской буквы в `.svelte` и `.ts` вне `locales/`, `dev/` и тестов. Это проверяет тест.
- Микрофон и экран остаются выключенными: переключатели видны, но неактивны.
- Действие высокого риска нельзя разрешить навсегда: кнопка «Разрешать всегда» неактивна.
- Щелчки мимо островка проходят в окна под ним. Окно аватара на рабочем столе пропускает все щелчки.
- Окно показывается только через `window::show_without_activation` (решение 0007).

## Мои решения

- **Порядок.** Эпик 6 идёт раньше эпика 5: пользователь перенёс голос в конец. Формы «слушает» и «отвечает голосом» нарисованы, но источник у «слушает» появится только с голосом.
- **Встроенный аватар — рисунок SVG из макета.** Он оригинальный и уже показывает десять эмоций. Live2D (7.1–7.3, 7.7–7.9) и источник настоящей модели (7.12) остаются на эпик 7.
- **Ядро эмоций без силы и затухания.** Слоты, удержание 4 с, приоритеты и возврат к спокойной есть. Сила нужна только Live2D.
- **Настройки остаются в островке**, размер 860 × 500, вид как в макете: панель слева, страница справа. Отдельного окна нет.
- **Страницы настроек:** Персонаж, Провайдеры, Разрешения, Инструменты, Общие. «Голос», «Островок» и «Память» показаны неактивными.
- **Правило в «Разрешениях» не меняет решение.** Переключатель в строке показывает текущее решение и «Спрашивать»; «Спрашивать» удаляет правило. Смена «запретить → разрешить» потребовала бы уровень риска, которого в таблице правил нет.
- **Погода в покое не показывается.** Для неё нужен город и фоновые запросы в сеть; интернет-инструменты по умолчанию выключены.
- **Ползунок «Движение губ»** появится с голосом.
- **Отложено:** режимы окна 6.4 (другой монитор нельзя проверить на машине разработки с одним монитором), поведение в играх 6.5. Кнопка «Открепить» в шапке чата видна, но неактивна.
- **Тексты инструментов и оценок риска остаются в Rust.** В `ru.json` переезжают тексты интерфейса и меню значка.

## Review Focus

1. Метка эмоции разорвана между двумя фрагментами потока (`[jo` + `y] Привет`). Ожидание: в чат попадает «Привет», эмоция `joy`. Тест в задаче 2.
2. Квадратные скобки без эмоции (`[1]`, `[файл.txt]`, незакрытая `[`). Ожидание: текст доходит без потерь. Тест в задаче 2.
3. Высокий риск ждёт подтверждения, в это время приходит метка `[joy]`. Ожидание: остаётся «тревога». Тест в задаче 1.
4. Инструмент не прислал `data` или прислал другой `kind`. Ожидание: обычная строка шага, без карточки. Тест в задаче 6.
5. Длинная команда в карточке подтверждения. Ожидание: блок с командой прокручивается, кнопки остаются видны. Проверка в задаче 7.

---

### Task 1: Ядро эмоций

**Files:**
- Create: `crates/mika-core/src/emotion.rs`
- Modify: `crates/mika-core/src/events.rs`, `crates/mika-core/src/lib.rs`

**Interfaces:**
- Produces: `Emotion` (`Neutral, Joy, Shy, Surprise, Sad, Angry, Think, Wink, Sleepy, Worry`, serde `snake_case`), `Emotion::ALL`, `Emotion::from_tag(&str) -> Option<Emotion>` (английское имя слота или русское: «радость», «смущение», «удивление», «грусть», «злость», «думает», «подмигивает», «сонная», «тревога»; без учёта регистра).
- Produces: `Event::EmotionCue { emotion }` (метка из ответа), `Event::EmotionChanged { emotion }`.
- Produces: `EmotionCore::new(now)`, `on_event(&mut self, &Event, now: Instant)`, `current(&self, now) -> Emotion`, `next_change(&self, now) -> Option<Instant>`; `spawn_emotions(bus) -> EmotionHandle`; `EmotionHandle::get() -> Emotion`.

Правила ядра. Константы `PULSE_HOLD = 4 с`, `SLEEPY_AFTER = 5 мин`.

| Событие | Эмоция | Приоритет | Держится |
|---|---|---|---|
| `ConfirmRequested` с высоким риском | Worry | 3 | до `ConfirmResolved` |
| `Error`, `ToolFinished { ok: false }` | Sad | 2 | 4 с |
| `EmotionCue` | из метки | 1 | 4 с |
| `AssistantText` после успешных инструментов без ошибок в этом ходе | Joy | 1 | 4 с |
| состояние `Thinking` | Think | 0 | пока состояние |
| 5 минут без событий в состоянии `Idle` | Sleepy | 0 | до любого события |

Импульс с меньшим приоритетом не заменяет действующий с большим. `UserText` сбрасывает счётчики хода.

- [ ] **Step 1:** Тесты: `thinking_state_shows_think`, `high_risk_confirm_beats_cue` (Review Focus 3), `pulse_expires_to_neutral_after_hold`, `sad_on_failed_tool`, `joy_after_turn_with_ok_tools_only`, `sleepy_after_idle_and_wakes_on_event`, `from_tag_accepts_russian_and_case`, `changed_event_serializes` (`{"type":"emotion_changed","emotion":"joy"}`).
- [ ] **Step 2:** `cargo test -p mika-core emotion` — падают.
- [ ] **Step 3:** Реализовать `EmotionCore` чистой структурой, `spawn_emotions` — задача tokio: подписка на шину, `sleep_until(next_change)`, публикация `EmotionChanged` при смене. Свои `EmotionChanged` она пропускает.
- [ ] **Step 4:** Тесты проходят.

### Task 2: Метки эмоций и характер

**Files:**
- Modify: `crates/mika-core/src/agent.rs`, `crates/mika-core/src/config.rs`, `crates/mika-gui/src/app.rs`, `crates/mika-cli/src/main.rs`

**Interfaces:**
- Produces: `TagFilter::default()`, `push(&mut self, chunk: &str) -> (String, Vec<Emotion>)`, `finish(&mut self) -> String`. Метка — `[слово]`, где слово узнаёт `Emotion::from_tag`. Метка и один пробел после неё вырезаются. Буфер ждёт `]` не дольше 16 символов.
- Produces: `config::Persona` (`Caring` по умолчанию, `Cheerful`, `Calm`, `Bold`, `Custom`), поля `AssistantConfig::persona`, `AssistantConfig::custom_prompt`; `AvatarConfig { in_island: true, on_desktop: false }` в `Config::avatar`.
- Changes: `system_prompt(cfg: &AssistantConfig) -> String`. Добавляет абзац характера и правило меток: одна метка в начале ответа, только когда эмоция уместна, список из девяти слотов.

- [ ] **Step 1:** Тесты `TagFilter`: `strips_tag_at_start`, `tag_split_across_chunks` (Review Focus 1), `keeps_ordinary_brackets` и `flushes_unclosed_bracket_on_finish` (Review Focus 2), `tag_in_the_middle_is_stripped`. Тест агента `reply_tag_becomes_cue_and_is_not_stored`: в истории и в `AssistantText` метки нет, на шине есть `EmotionCue`. Тесты `system_prompt_mentions_persona_and_tags`, `old_config_without_persona_loads`.
- [ ] **Step 2:** Падают.
- [ ] **Step 3:** Реализовать. Фильтр стоит в `one_step` перед публикацией `AssistantDelta`; пустые фрагменты не публикуются.
- [ ] **Step 4:** `cargo test -p mika-core` проходит.

### Task 3: Данные для карточек

**Files:**
- Modify: `crates/mika-core/src/tools.rs`, `crates/mika-core/src/events.rs`, `crates/mika-core/src/agent.rs`, `crates/mika-tools/src/web.rs`, `crates/mika-tools/src/system.rs`

**Interfaces:**
- Produces: `ToolOutput::data: Option<serde_json::Value>`, `ToolOutput::with_data(self, Value) -> Self`; `Event::ToolFinished::data` (в JSON поля нет, когда `None`).
- Погода: `{"kind":"weather","city":"Пермь","temp":1,"feels":-3,"wind":6,"text":"дождь","days":[{"date":"2026-11-12","min":-4,"max":0,"text":"снег"}]}`, числа округлены до целых.
- Громкость (действия, которые меняют уровень): `{"kind":"volume","level":40,"was":60}`.

- [ ] **Step 1:** Тесты: `weather_returns_card_data`, `volume_set_returns_levels`, `tool_finished_without_data_has_no_field`, `agent_forwards_tool_data`.
- [ ] **Step 2–4:** падают, реализация, проходят.

### Task 4: Команды и окно аватара в `mika-gui`

**Files:**
- Modify: `crates/mika-gui/src/bridge.rs`, `main.rs`, `app.rs`, `tray.rs`, `window.rs`, `tauri.conf.json`, `capabilities/default.json`
- Create: `crates/mika-gui/src/strings.rs`

**Interfaces:**
- `get_ui_settings` дополнительно отдаёт `persona`, `custom_prompt`, `avatar_in_island`, `avatar_on_desktop`.
- Новые команды: `set_assistant_name(value)` (после обрезки 1–24 символа, иначе ошибка), `set_persona(persona, custom_prompt)`, `set_avatar_placement(in_island, on_desktop)`, `get_emotion() -> Emotion`. Первые три сохраняют настройки, обновляют системный промпт и шлют `mika://ui` `{ "action": "settings_changed" }`.
- Окно `avatar`: адрес `index.html?view=avatar`, 560 × 470, прозрачное, без рамки, поверх окон, вне панели задач, без фокуса, скрыто при запуске. `window::avatar_position(monitor_x, monitor_y, monitor_w, monitor_h, scale) -> (i32, i32)`: правый нижний угол с отступами 24 px справа и 64 px снизу (панель задач). Окно всегда пропускает щелчки.
- `strings::tr(key) -> String` читает `frontend/src/locales/ru.json`, вшитый `include_str!`. Меню значка берёт тексты оттуда. Пункт «Скрыть островок» прячет и окно аватара.

- [ ] **Step 1:** Тесты: `avatar_window_sits_in_bottom_right_corner` (1920 × 1080, масштаб 1 → (1336, 546)), `name_is_trimmed_and_limited`, `tr_returns_tray_strings`, `tr_unknown_key_returns_key`.
- [ ] **Step 2–4:** падают, реализация, `cargo test -p mika-gui` проходит.

### Task 5: Основа интерфейса

**Files:**
- Create: `frontend/src/locales/ru.json`, `frontend/src/lib/i18n.ts`, `frontend/src/lib/i18n.test.ts`, `frontend/src/lib/icons.ts`, `frontend/src/lib/Icon.svelte`, `frontend/src/lib/avatar.ts`, `frontend/src/lib/avatar.test.ts`, `frontend/src/lib/Avatar.svelte`, `frontend/src/lib/AvatarDefs.svelte`, `frontend/src/avatar.css`
- Modify: `frontend/src/app.css`, `frontend/package.json`, все компоненты (перенос строк)

**Interfaces:**
- `t(key: MessageKey, params?: Record<string, string | number>) -> string`; подстановка `{name}`. `MessageKey = keyof typeof ru`, ключи плоские: `chat.placeholder`.
- `ICONS`: `vol, mic, mon, out, set, up, check, search, term, shield, star, plug, x, plus, back, stop, globe, folder`. `<Icon name size? />`.
- `EMOTIONS: Record<Emotion, EmotionLook>`, `avatarSvg(view: 'head' | 'full', emotion: Emotion) -> string`, `isEmotion(v) -> v is Emotion`. `<Avatar view emotion talking? />`.

- [ ] **Step 1:** Тесты: `t_interpolates_params`, `every_message_is_non_empty`, `no_cyrillic_outside_locales` (читает `src/**/*.{svelte,ts}` кроме `locales/`, `dev/`, `*.test.ts`), `avatar_svg_has_view_box_for_head_and_full`, `every_emotion_renders_distinct_face`, `unknown_emotion_falls_back_to_neutral`.
- [ ] **Step 2–4:** падают, реализация, `npm --prefix frontend test` проходит. Шрифт подключается в `main.ts`, основной кегль 15 px как в макете.

### Task 6: Островок и чат

**Files:**
- Modify: `frontend/src/lib/island.ts`, `chat.ts`, `Island.svelte`, `Face.svelte`, `Chat.svelte`, `App.svelte`, тесты рядом
- Create: `frontend/src/lib/WeatherCard.svelte`, `frontend/src/lib/Bars.svelte`

**Interfaces:**
- `View = 'island' | 'chat' | 'settings'`; `viewShape(view, state, wide: boolean) -> IslandShape` в `island.ts`.
- `ChatItem.data?: CardData`; `cardData(v: unknown) -> CardData | undefined` принимает только `kind` `weather` и `volume` с числами на местах.
- `formatDuration(ms) -> string`: 400 → «0,4 с», 19090 → «19 с».
- `speakWords(text, maxChars) -> { i: number; word: string }[]`: хвост ответа, `i` — номер слова от начала ответа.
- `toolLabel(name) -> MessageKey` для плашки инструмента в форме «думает».

Содержимое форм по макету: кружок с лицом аватара во всех четырёх малых формах; волна из 26 полос; переливающийся текст, плашка инструмента и вращающееся кольцо; ответ по словам и кнопка озвучки. Светящийся контур: `listening` и `thinking` — трёхцветный, высокий риск — красный. Чат: колонка аватара 290 px с ореолом и плашкой «имя · состояние», шапка с переключателями и значками, шаги с галочкой и временем, карточка погоды, полоска громкости, поле ввода 46 px.

- [ ] **Step 1:** Тесты: `view_shape_sizes_match_design`, `card_data_rejects_unknown_kind` (Review Focus 4), `tool_finished_keeps_card_data`, `format_duration_uses_comma`, `speak_words_numbers_from_reply_start`.
- [ ] **Step 2–4:** падают, реализация, проходят. Сверка с макетом в предпросмотре: `?mock=island|listen|think|speak|chat`.

### Task 7: Карточка подтверждения

**Files:**
- Modify: `frontend/src/lib/ConfirmCard.svelte`, `frontend/src/dev/mock.ts`

Заголовок: значок и уровень риска («Нужно подтверждение · риск низкий» зелёный, «… риск средний» жёлтый, «Опасно · риск высокий» красный), справа отсчёт. Дальше название, блок с точной целью (не выше 72 px, прокрутка, сжимается первым), причина не длиннее двух строк, три кнопки, строка «Можно ответить в чате». Высота 268 px соблюдается при цели в одну и в десять строк (Review Focus 5).

- [ ] **Step 1:** Сценарии предпросмотра `?mock=confirm`, `?mock=high`, `?mock=long` (команда в 600 символов).
- [ ] **Step 2:** Реализовать, сверить снимки с макетом, кнопки видны во всех трёх.

### Task 8: Настройки

**Files:**
- Modify: `frontend/src/lib/Settings.svelte`, `Permissions.svelte`, `General.svelte`
- Create: `frontend/src/lib/settings/Persona.svelte`, `Providers.svelte`, `Tools.svelte`

Панель слева 200 px: кнопка «назад», кружок и имя, пять страниц и три неактивные. «Персонаж»: превью аватара с ореолом, десять кнопок эмоций (нажатие показывает эмоцию на превью), имя, три плитки модели (активна «Мика по умолчанию»), пять вариантов характера и поле своего промпта, «Где показывать». «Провайдеры»: карточка «Ответы» с плашкой задержки и формой под ней, три карточки «не подключено». «Разрешения»: строки правил с переключателем и журнал.

- [ ] **Step 1:** Сценарий `?mock=settings`.
- [ ] **Step 2:** Реализовать, сверить с разделом 05 макета. Смена имени видна в островке без перезапуска.

### Task 9: Аватар на рабочем столе

**Files:**
- Create: `frontend/src/AvatarWindow.svelte`, `frontend/src/lib/bubble.ts`, `frontend/src/lib/bubble.test.ts`
- Modify: `frontend/src/main.ts`

**Interfaces:**
- `bubbleText(state, reply, work, pending) -> MessageKey | { text: string } | null`: слушает → «Слушаю…», думает → текст шага или «Секунду…», отвечает → хвост ответа до 140 символов, подтверждение → название действия, покой → `null`.

- [ ] **Step 1:** Тесты `bubble_is_empty_when_idle`, `bubble_shows_reply_tail_when_speaking`, `bubble_shows_action_when_confirming`.
- [ ] **Step 2–4:** падают, реализация, проходят. Сценарий `?view=avatar&mock=speak`.

### Task 10: Проверка и документы

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `npm --prefix frontend test`, `npm --prefix frontend run check`.
- [ ] Сборка `cargo build -p mika-gui --release --features tauri/custom-protocol`, проверка в настоящем окне: щелчок по островку, чат, настройки, смена характера, аватар на рабочем столе, щелчки мимо островка проходят насквозь.
- [ ] Живой ответ модели с меткой эмоции на `mistral-nemo`.
- [ ] Обновить дизайн (разделы 9, 10, 15), дорожную карту, `CLAUDE.md`, заметки о выпуске.
