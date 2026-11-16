# Эпик 26: живая модель VRM. План

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Модель VRM ведёт себя как живой персонаж. Естественные позы, покой, язык тела, губы в такт голосу, взгляд за курсором, реакции на ввод, анимации `.vrma` и инструмент «жест».

**Architecture:**
- Rust:
  - считает огибающую громкости каждой фразы и сообщает начало и конец звучания событиями шины;
  - отдаёт окнам положение курсора;
  - хранит анимации `.vrma`;
  - даёт инструмент `avatar_gesture`.
- Фронтенд:
  - собирает поворот костей из слоёв — чистых функций в `frontend/src/lib/model/`;
  - сцена только применяет результат.

**Tech Stack:** Rust (mika-audio, mika-core, mika-tools, mika-gui на Tauri 2), Svelte 5, three 0.187, @pixiv/three-vrm 3.5.5, @pixiv/three-vrm-animation 3.5.5, vitest.

**Spec:** `docs/superpowers/specs/2026-11-16-living-vrm-avatar-design.md`

## Global Constraints

- Правила `CLAUDE.md`. В частности:
  - каждый инструмент — в реестре с уровнем риска, выполняется через политику;
  - тексты интерфейса — в `ru.json` и `en.json` под одним ключом, в том же порядке;
  - модули `foo.rs` без `mod.rs`;
  - `mika-core` без Tauri;
  - журнал с явной категорией, без записей в горячих циклах;
  - код, имена и сообщения коммитов на английском, документы на русском.
- Новая зависимость только одна: `@pixiv/three-vrm-animation` 3.5.5 (MIT). Грузится только в отдельном куске сцены.
- Встроенные позы и движения пишутся с нуля. Чужие движения не копируются.
- Повороты костей — в нормализованном скелете. Углы задаются для VRM 1.0, у 0.x повороты вокруг X и Z меняют знак (`rigSign`).
- Не больше 30 кадров в секунду. Скрытое окно не рисует.
- `prefers-reduced-motion`: нет покачивания, случайных действий и жестов речи. Губы двигаются.
- Курсор не пишется в журнал, не уходит модели ИИ и не сохраняется.
- Тексты речи в событиях голоса в журнал не пишутся.
- Двусмысленных реакций нет: грудь, бёдра, юбка, ноги — обычный тычок.
- Файлы моделей и анимаций пользователя в git не попадают (`*.vrm`, `*.vrma` в `.gitignore`).
- Огибающая: шаг кадра 20 мс, значения 0–255, нормировка по пику фразы.
- Масштаб окна: 0.6–1.6, по умолчанию 1.0, шаг колеса 0.1.
- Курсор: не чаще 30 раз в секунду. Голова не дальше ±35° по горизонтали и ±20° по вертикали, догоняет примерно за 300 мс. Курсор стоит больше 5 с — интерес пропадает.
- Пики жестов речи — не чаще одного в 600 мс.
- Случайные действия покоя — раз в 20–60 с. `auto` меняет позу раз в 2–5 мин.
- Файл `.vrma` — до 20 МБ.

## Review Focus

1. Модель VRM 0.x: каждая новая поза, жест и взгляд за курсором с `rigSign = -1`. Ожидание: руки не уходят в тело, голова поворачивается к курсору, а не от него.
2. Голос прерван посреди фразы (стоп-слово, новый вопрос). Ожидание: приходит `voice_quiet`, рот закрывается, губы не висят открытыми.
3. Окно аватара на рабочем столе при выключенном взгляде за курсором, со встроенным аватаром и после смены масштаба. Ожидание: щелчки мимо фигуры проходят насквозь, на фигуре ловятся.
4. Удалённый синтез, который не расшифровался, или нечитаемый поток Windows. Ожидание: фраза с пустой огибающей, губы по буквам, затем тишина.
5. Анимация `.vrma` с костями, которых у модели нет, или очень длинная. Ожидание: играет то, что есть, один раз, затем плавный выход.

---

### Task 1: Огибающая и события голоса (Rust)

**Files:**
- Create: `crates/mika-audio/src/lips.rs`
- Modify:
  - `crates/mika-audio/src/lib.rs`
  - `crates/mika-audio/src/lead.rs` (разбор WAV доступен внутри крейта)
  - `crates/mika-audio/src/speech.rs` (трейт `Voice`, обёртка `Retold`)
  - `crates/mika-audio/src/win.rs` (вызов перед `Play()` и после конца)
  - `crates/mika-core/src/events.rs`
  - `crates/mika-gui/src/app.rs` (подключение к шине)
  - `crates/mika-cli/src/repl.rs` (новые события)

**Interfaces:**
- Produces:
  - `mika_audio::lips::FRAME_MS: u32 = 20`;
  - `mika_audio::lips::envelope(wav: &[u8]) -> Option<Vec<u8>>`;
  - `pub enum LipCue { Phrase { text: String, frame_ms: u32, levels: Vec<u8> }, Quiet }`;
  - `pub type LipSink = Arc<dyn Fn(LipCue) + Send + Sync>`;
  - `Voice::set_lips(&self, _sink: Option<LipSink>) {}`;
  - `Event::VoicePhrase { text, frame_ms, levels }` и `Event::VoiceQuiet`, в JSON `voice_phrase` и `voice_quiet`.

- [ ] Тесты `lips.rs`:
  - `silence_gives_zero_levels`;
  - `tone_gives_peak_255_and_frames_by_20ms` (1 с тона 16 бит моно даёт 50 кадров, максимум 255);
  - `stereo_and_8bit_are_read`;
  - `half_loud_frame_is_about_half`;
  - `not_wav_gives_none`;
  - `empty_data_gives_empty_levels`.
- [ ] Посмотреть, что они падают; написать `envelope` (RMS кадра по всем каналам, затем нормировка по пику); посмотреть, что проходят.
- [ ] `win.rs`. Подготовка звука возвращает и звук для игры, и WAV для огибающей: расшифрованную копию, даже если играется исходный звук. Огибающая учитывает тишину, добавленную в начало.
  - Прямо перед `Play()`: `LipCue::Phrase`.
  - После конца или остановки фразы: `LipCue::Quiet`. Всегда, через guard.
  - Нечитаемый поток: `Phrase` с пустыми `levels`.
- [ ] Тест в `speech.rs`: обёртка `Retold` передаёт `set_lips` внутреннему голосу.
- [ ] `app.rs`: `voice.set_lips` публикует события шины.
- [ ] Тест в `events.rs`: `voice_events_serialize_as_snake_case`.
- [ ] `cargo test -p mika-audio -p mika-core`: зелёный.

### Task 2: Губы по звуку (фронтенд)

**Files:**
- Create:
  - `frontend/src/lib/model/lips.ts` + `lips.test.ts`
  - `frontend/src/lib/model/voice.svelte.ts`
- Modify:
  - `frontend/src/lib/model/scene.ts`, `ModelView.svelte`
  - `frontend/src/lib/Avatar.svelte`
  - `frontend/src/App.svelte`, `frontend/src/AvatarWindow.svelte` (запуск слушателя)
  - `frontend/src/dev/mock.ts` (подставные фразы)

**Interfaces:**
- Consumes: события `voice_phrase` и `voice_quiet` из Task 1.
- Produces:
  - `export type Viseme = Vowel | 'closed' | null`;
  - `visemeOf(ch: string): Viseme`;
  - `lipTrack(text: string, levels: number[], frameMs: number): { shape: Vowel | null; open: number }[]` — по кадру на каждый кадр огибающей;
  - `flatTrack(text: string, msPerChar: number, frameMs: number)` — для пустой огибающей, раскрытие 0.6;
  - `mouthAt(track, startedAt: number, now: number, frameMs: number)`;
  - `OPEN_TAU_MS = 30`, `CLOSE_TAU_MS = 80`, `smoothOpen(cur, target, dt)`;
  - стор `voice`: `{ playing: boolean; phrase: { text; frameMs; levels; track; startedAt } | null }`, `startVoice()`.

- [ ] Тесты:
  - `visemeOf` по таблице дизайна (а/я/a → `aa`, о → `oh`, у/ю/u/w → `ou`, и/ы/i/y → `ih`, е/э/ё/e → `ee`, м/б/п/m/b/p → `closed`, прочее → `null`);
  - `lipTrack` кладёт буквы только на звучащие кадры, в порядке текста, тихий кадр даёт `open` 0;
  - `lipTrack` с пустым текстом даёт раскрытие по громкости и форму `aa`;
  - `smoothOpen` открывает быстрее, чем закрывает;
  - `flatTrack` даёт ровное раскрытие и закрывает на `closed`.
- [ ] RED, затем реализация, затем GREEN.
- [ ] Сцена: если звучит фраза (`voice.playing`), губы по треку, сила `lipGain`. Иначе по старой логике, но только когда `talking && !speakReplies`. Сам `speakReplies` узнаётся командой `get_ui_settings` или эквивалентом из настроек голоса.
- [ ] Встроенные картинки: пока звучит фраза, рот открыт при громкости кадра выше 0.25.
- [ ] Подставная фраза в `mock.ts` для `?mock=speak`.
- [ ] `npm --prefix frontend test`, `npm --prefix frontend run check`: зелёные.

### Task 3: Скелет и пять поз

**Files:**
- Create:
  - `frontend/src/lib/model/rig.ts` + `rig.test.ts`
  - `frontend/src/lib/model/poses.ts` + `poses.test.ts`
- Modify:
  - `frontend/src/lib/model/scene.ts` (применение полного скелета вместо `restPose` и `boneAngles`)
  - `layers.ts` (убрать `REST_POSE` и руки из `boneAngles`)

**Interfaces:**
- Produces:
  - `export type Euler3 = [number, number, number]` (градусы, X, Y, Z, порядок XYZ);
  - `export type RigPose = Partial<Record<RigBone, Euler3>>`;
  - `RigBone` — подмножество костей VRM: туловище, шея, голова, ключицы, руки, кисти, 15 костей пальцев на сторону, бёдра, колени;
  - `mirror(pose): RigPose` (левое в правое: X сохраняется, Y и Z меняют знак);
  - `withSign(pose, sign)` (X и Z умножаются на `sign`);
  - `mix(a, b, w)` (у костей из `b` линейно к `b`);
  - `add(a, b, w)` (смещения);
  - `REST_POSES: Record<RestPose, RigPose>`, `type RestPose = 'relaxed' | 'clasped' | 'behind' | 'hip' | 'crossed'`;
  - `FINGERS_SOFT`, `FINGERS_CURL` (наборы пальцев).

- [ ] Тесты:
  - `mirror` дважды даёт исходное;
  - `withSign(-1)` меняет только X и Z;
  - `mix(…, 0)` и `mix(…, 1)` дают края;
  - каждая поза задаёт плечи, локти, кисти и все пальцы обеих рук;
  - `relaxed`, `clasped`, `behind`, `crossed` зеркальны;
  - все углы в пределах ±180;
  - плечевая кость опущена не меньше чем на 60°.
- [ ] RED, затем реализация, затем GREEN.
- [ ] Сцена применяет позу ко всем костям `RigBone` через кватернион. Головой и туловищем по-прежнему правят эмоция и дыхание, поверх.
- [ ] Предпросмотр: обе модели пользователя (0.x и 1.0) во всех пяти позах. Кисть не уходит в тело. Углы подбираются здесь.

### Task 4: Слои тела

**Files:**
- Create:
  - `frontend/src/lib/model/body.ts` + `body.test.ts` (покой, действия, эмоции, состояния, жесты речи, временные линии)
  - `frontend/src/lib/model/live.svelte.ts` (состояние Мики, работа инструмента, набор, курсор, запросы жестов)
- Modify:
  - `scene.ts`, `ModelView.svelte`
  - `App.svelte`, `AvatarWindow.svelte`

**Interfaces:**
- Consumes: `rig.ts`, `poses.ts` (Task 3); стор `voice` (Task 2).
- Produces:
  - `idleOffset(ms: number, seed: number): RigPose` (вес с ноги на ногу, период 6–10 с; покачивание; голова);
  - `IDLE_ACTIONS: Record<IdleAction, Timeline>`, `type IdleAction = 'hair' | 'stretch' | 'look_around' | 'look_hand'`;
  - `idleActionPause(r): number` (20 000–60 000 мс);
  - `autoPosePause(r): number` (120 000–300 000 мс);
  - `EMOTION_BODY: Record<Emotion, RigPose>`;
  - `STATE_BODY: Record<BodyState, RigPose>`, `type BodyState = 'thinking' | 'working' | 'confirm' | 'listening'`;
  - `type Timeline = { ms: number; keys: [number, RigPose][] }`, `sampleTimeline(t, ms): RigPose | null`;
  - `beats(levels, frameMs): number[]` (мс пиков, не чаще 600 мс);
  - `beatOffset(msSinceBeat, side, strength): RigPose`;
  - `endCue(text): 'nod' | 'tilt' | null`;
  - `speechStrength(emotion)` (sad и sleepy 0.5, joy 1.3, прочие 1);
  - `bodyState(state, working): BodyState | null`;
  - стор `live`: `{ state; working; typingAt; cursor; follow; gestures }`.

- [ ] Тесты:
  - `idleOffset` гладкий и ограничен (бёдра ≤ 4°);
  - `idleActionPause` и `autoPosePause` в своих пределах;
  - `sampleTimeline` до начала и после конца даёт `null`, на ключе даёт ключ;
  - `beats` не чаще 600 мс и только на пиках выше 0.5;
  - `endCue('Ура!') === 'nod'`, `endCue('Да?') === 'tilt'`;
  - `bodyState('awaiting_confirm', false) === 'confirm'`, работа инструмента важнее `thinking`;
  - у каждой эмоции и каждого состояния есть поза.
- [ ] RED, затем реализация, затем GREEN.
- [ ] Сцена: слои по разделу 7.1 дизайна. Смешивание с плавностью около 400 мс. Случайное действие не начинается во время речи, набора, состояния или жеста. При `still` покой, действия и жесты речи выключены. Двойное моргание в 10% случаев. Мелкие быстрые движения глаз.
- [ ] Предпросмотр: эмоции, состояния (подставные `state_changed`, `tool_started`), речь.

### Task 5: Настройки вида, размер и кадр

**Files:**
- Modify:
  - `crates/mika-core/src/config.rs` (`AvatarConfig`: `pose`, `scale`, `frame`, `follow_cursor`, `idle_actions`, `position`)
  - `crates/mika-gui/src/avatar_model.rs` (команды `avatar_look`, `avatar_set_look`, `avatar_scale_step`)
  - `crates/mika-gui/src/window.rs` (размер окна по масштабу, сохранённое положение, перетаскивание)
  - `crates/mika-gui/src/main.rs`
  - `frontend/src/lib/model/layers.ts` (`frameFor`: `full`, `waist`, `close`, `head`)
  - `frontend/src/lib/model/state.svelte.ts` (вид модели)
  - `frontend/src/lib/settings/ModelLook.svelte` (новый), `Persona.svelte`
  - `ru.json`, `en.json`

**Interfaces:**
- Produces:
  - `AvatarLook { pose: String, scale: f32, frame: String, follow_cursor: bool, idle_actions: bool }` (serde);
  - `pub fn clamp_scale(v: f32) -> f32` (0.6..=1.6, NaN → 1.0);
  - `pub fn avatar_size(scale) -> (f64, f64)`;
  - `pub fn position_on_screen(saved, monitors) -> Option<(i32, i32)>`;
  - событие `mika://ui {action: "avatar_look_changed"}`;
  - `model.look` во фронтенде.

- [ ] Тесты Rust:
  - старый `config.toml` без новых полей читается со значениями по умолчанию;
  - `clamp_scale` обрезает и чинит NaN;
  - `avatar_size(1.5)` = 840×705;
  - сохранённое положение вне всех мониторов не берётся;
  - неизвестная поза или кадр заменяются значением по умолчанию.
- [ ] Тест фронтенда: `frameFor('waist')` ниже головы и выше бёдер, `frameFor('close')` вмещает голову и плечи.
- [ ] RED, затем реализация, затем GREEN.
- [ ] Страница «Персонаж»: поза (5 и «меняет сама»), ползунок размера, кадр, два переключателя. Тексты в обоих файлах локализации.
- [ ] Окно на рабочем столе меняет размер при смене масштаба. Положение сохраняется после перетаскивания (событие `Moved` с паузой 500 мс).

### Task 6: Курсор, набор, взгляд

**Files:**
- Create: `frontend/src/lib/model/gaze.ts` + `gaze.test.ts`
- Modify:
  - `crates/mika-gui/src/window.rs` (поток курсора окна аватара; курсор острова из потока проверки попаданий; флаг «нужен курсор» у каждого окна)
  - `crates/mika-gui/src/avatar_model.rs` (команды `avatar_cursor_wanted(on)`, `avatar_typing()`)
  - `frontend/src/lib/Chat.svelte` (набор, положение поля ввода)
  - `live.svelte.ts`, `scene.ts`

**Interfaces:**
- Produces:
  - `pub fn cursor_payload(cursor, origin, scale) -> (f64, f64)` (логические пиксели окна);
  - `type GazeSource = 'reaction' | 'typing' | 'state' | 'cursor' | 'emotion' | 'wander'`;
  - `pickGaze(c: {reaction?, typing?, state?, cursor?, emotion?}): GazeSource`;
  - `aimAt(dx, dy): { headYaw, headPitch, eyeYaw, eyePitch, bodyYaw }` (пределы дизайна; корпус при повороте больше 25°);
  - `cursorInterest(lastMoveAt, now, jump): boolean` (5 с; резкое движение возвращает интерес).

- [ ] Тесты:
  - Rust: перевод курсора в координаты окна с масштабом 1.25;
  - `pickGaze` по порядку дизайна;
  - `aimAt` ограничивает голову ±35°/±20°, остаток уходит глазам, корпус включается после 25°;
  - знак: курсор справа на экране — она смотрит вправо от себя-зрителя (`headYaw` < 0 у 1.0);
  - `cursorInterest` гаснет через 5 с.
- [ ] RED, затем реализация, затем GREEN.
- [ ] Rust шлёт `mika://cursor` только окну, которое попросило, не чаще 30 раз в секунду, только при изменении.
- [ ] Остров при наборе зовёт `avatar_typing` не чаще раза в 500 мс. Rust передаёт окну аватара `{action:"typing", x, y}` — середину острова в координатах окна аватара. Кивок на `user_text`.
- [ ] Поза слушания — из `bodyState` при `listening` (Task 4).

### Task 7: Щелчки по частям тела, щелчки сквозь окно, перетаскивание

**Files:**
- Create: `frontend/src/lib/model/touch.ts` + `touch.test.ts`
- Modify:
  - `crates/mika-core/src/emotion.rs` (`Pokes::poke(now, part)`)
  - `crates/mika-gui/src/bridge.rs` (`poke_avatar(part: Option<String>)`)
  - `crates/mika-gui/src/window.rs` и `avatar_model.rs` (`avatar_hover(over)`; окно аватара больше не всегда сквозное)
  - `scene.ts` (`touch(x, y)`, `hits(x, y)`, `push(vx, vy)`)
  - `Avatar.svelte`, `Chat.svelte`, `AvatarWindow.svelte`

**Interfaces:**
- Produces:
  - `pub enum TouchPart { Crown, Face, Hand, Other }` (serde snake_case);
  - `Pokes::poke(&mut self, now, part: Option<TouchPart>) -> Emotion`;
  - `partOf(bone: string, up: number, forward: number): TouchPart`;
  - `REACTION: Record<TouchPart, Timeline | null>`.

- [ ] Тесты Rust:
  - первый щелчок по макушке даёт `Joy`, по лицу `Shy`, по руке `Joy`, `Other` — как раньше;
  - третий и дальше в серии — по серии, какая бы ни была часть;
  - `None` — как раньше.
- [ ] Тесты фронтенда:
  - `partOf('head', up>0.5)` даёт `crown`;
  - `partOf('head', forward>0)` даёт `face`;
  - кости рук дают `hand`;
  - `chest`, `hips`, `upperLeg` дают `other`.
- [ ] RED, затем реализация, затем GREEN.
- [ ] Окно аватара:
  - курсор нужен, пока окно видно;
  - страница проверяет фигуру под курсором: альфа пикселя у модели, сжатый прямоугольник у картинок;
  - при изменении зовёт `avatar_hover`, Rust переключает `set_ignore_cursor_events`;
  - нажатие с движением больше 4 px — `startDragging()`, иначе щелчок — `poke_avatar` с частью;
  - колесо — `avatar_scale_step(±0.1)`.
- [ ] Инерция: скорость окна из `onMoved` даёт наклон корпуса до 8° и сдвиг сцены для пружинных костей, затухание около 600 мс.

### Task 8: Анимации `.vrma`

**Files:**
- Create:
  - `crates/mika-core/src/avatar/motions.rs`
  - `frontend/src/lib/settings/ModelMotions.svelte`
  - `frontend/src/lib/model/clip.ts` (загрузка и игра клипа, внутри куска сцены)
- Modify:
  - `crates/mika-core/src/avatar.rs` (ошибки, реэкспорт), `paths.rs` (`motions_dir()`)
  - `crates/mika-gui/src/avatar_model.rs` (команды `avatar_motions`, `avatar_motion_add`, `avatar_motion_rename`, `avatar_motion_delete`, `avatar_motion_bytes`, `avatar_motion_play`)
  - `crates/mika-gui/src/platform.rs` (стирание папки)
  - `frontend/package.json`
  - `scene.ts`, `Persona.svelte`, локализация

**Interfaces:**
- Produces:
  - `MAX_MOTION_BYTES = 20 MiB`;
  - `MotionEntry { id, name, ms }`;
  - `MotionStore::{new, import, list, rename, delete, bytes, by_name}`;
  - `inspect_motion(&Value) -> Result<u64 /*ms*/, ModelError>`;
  - событие `mika://ui {action:"avatar_motion", name}` для проигрывания из настроек.

- [ ] Тесты Rust:
  - файл без `VRMC_vrm_animation` → `NotVrm`;
  - внешний `uri` → `External`;
  - больше 20 МБ → `TooBig`;
  - импорт даёт имя из имени файла;
  - повтор имени получает суффикс ` 2`;
  - переименование в занятое имя — ошибка;
  - удаление убирает папку;
  - `by_name` без учёта регистра;
  - неправильный id отклоняется до диска;
  - «Удалить все данные» включает папку анимаций.
- [ ] RED, затем реализация, затем GREEN.
- [ ] `npm install @pixiv/three-vrm-animation@3.5.5` в `frontend`.
- [ ] Клип: загрузчик с тем же сторожем адресов, `createVRMAnimationClip`, `AnimationMixer`, игра один раз, вход и выход 300 мс через slerp поверх слоёв. Губы и моргание остаются за слоем лица.

### Task 9: Инструмент «жест»

**Files:**
- Create: `crates/mika-tools/src/avatar.rs`
- Modify:
  - `crates/mika-tools/src/lib.rs` (регистрация, подсказка в `HINTS`)
  - `crates/mika-core/src/events.rs` (`AvatarGesture { name }`)
  - `body.ts` (`GESTURES`: `wave`, `bow`, `nod`, `shake_head`, `shrug`, `clap`, `peace`, `spin`, `dance`)
  - `scene.ts`, `live.svelte.ts`, `Avatar.svelte` (у картинок — простое движение)

**Interfaces:**
- Consumes: `MotionStore::by_name` (Task 8).
- Produces:
  - `AvatarGestureTool { bus, motions: PathBuf }`;
  - инструмент `avatar_gesture`, аргумент `name`, `RiskLevel::Low`;
  - `BUILTIN_GESTURES: [&str; 9]`.

- [ ] Тесты Rust:
  - уровень риска низкий;
  - встроенное имя публикует событие;
  - имя анимации пользователя (любой регистр) публикует её имя;
  - неизвестное имя — `BadArguments` со списком имён;
  - описание перечисляет встроенные жесты и анимации пользователя.
- [ ] Тест фронтенда: у каждого встроенного жеста есть временная линия, она начинается и кончается в покое.
- [ ] RED, затем реализация, затем GREEN.
- [ ] Журнал: имя через `journal::content`.

### Task 10: Документы

**Files:**
- Create: `docs/decisions/0040-living-vrm-avatar.md`
- Modify:
  - `docs/superpowers/specs/2026-11-10-mika-assistant-design.md` (раздел 9)
  - `docs/superpowers/plans/2026-11-10-roadmap.md` (эпик 26)
  - `.gitignore` (`*.vrm`, `*.vrma`)

- [ ] Решение: что сделано, решения автора, что не проверено.
- [ ] Полный прогон:
  - `cargo fmt --all --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test --workspace`;
  - `npm --prefix frontend test`;
  - `npm --prefix frontend run check`;
  - `npm --prefix frontend run build`.
