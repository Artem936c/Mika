# Эпик 8 «Управление окнами»: пошаговый план

**Цель.** Действия в любой программе: чтение окна, нажатия, ввод текста, клавиши, зрение.

**Дизайн:** раздел 7 «Управление окнами: три уровня», раздел 8 «Слепая зона»; [решение 0009](../../decisions/0009-core-features.md).

## Общие правила

- Инструменты действия имеют средний риск и шаблон правила, равный имени процесса окна.
- «Слепая зона» проверяется до чтения и до действия.
- Снимки экрана делаются только при `privacy.screen_enabled`.
- COM-объекты UI Automation живут в одном рабочем потоке. Инструменты общаются с ним через канал.

## Задачи

### 1. Слепая зона

- `mika-core/src/policy/blind.rs`: `is_blind_title(title) -> bool`. Слова: пароль, вход, логин, банк, оплата, платёж, код подтверждения, кошелёк и их английские пары.
- Тесты: «Вход в банк» закрыт, «Блокнот» открыт.

### 2. Интерфейс `Ui`

- `sys.rs`: `UiNode { n, depth, role, name, value, enabled }`, `Ui::tree(window)`, `invoke(window, n)`, `set_text(window, n, text)`, `keys(combo)`, `type_text(text)`, `click_at(x, y, button)`, `scroll(window, n, amount)`, `element_rect(window, n)`.
- `Screen::capture(window: Option<u64>) -> Png`, `Screen::ocr(png)`.
- `Windows::place(id, Placement)` для раскладки окон.

### 3. Реализация Windows

- `win/uia.rs`: поток с `IUIAutomation`, обход дерева через `ControlViewWalker` с пределом 400 элементов и глубины 12; шаблоны `Invoke`, `Toggle`, `SelectionItem`, `ExpandCollapse`, `Value`.
- `win/input.rs`: `SendInput` для клавиш, текста Unicode и мыши.
- `win/screen.rs`: `BitBlt` и `PrintWindow`, кодирование PNG; `Windows.Media.Ocr`.
- Окно администратора: `OpenProcess` отказал — честное сообщение.

### 4. Инструменты

- `mika-tools/src/ui.rs`: `ui_read`, `ui_click`, `ui_type`, `ui_keys`, `ui_scroll`, `mouse_click`, `arrange_window`.
- `mika-tools/src/screen.rs`: `screen_look` (вопрос о снимке, ответ модели со зрением), `screen_text` (OCR), `screenshot_save`.
- Тесты с подменой `Ui` и `Screen`: слепая зона отклоняет, выключенный «Экран» отклоняет, шаблон правила равен процессу.

### 5. Зрение

- `llm.rs`: `ChatMessage.images` (PNG в base64). Адаптеры OpenAI и Anthropic кодируют картинку.
- `config.rs`: `llm.vision_model`.

### 6. Интерфейс

- Переключатель «Экран» в шапке чата и на странице «Инструменты».
- Рамка-подсветка цели перед кликом.

## Готово, когда

- «Открой Блокнот и напиши привет» выполняется.
- Первое действие в новой программе вызывает вопрос.
- Окно с заголовком «Вход в банк» не читается и не управляется.
- При выключенном «Экране» снимки не делаются.
