# Мика

Живой ИИ-помощник для Windows: голос и текст, 2D-персонаж с эмоциями, островок вверху экрана, действия в системе с подтверждением риска, любой LLM-провайдер. Rust, Tauri 2, Svelte.

Состояние: пре-релиз 0.2.0-pre.1 ([что в нём](docs/releases/0.2.0-pre.1.md)). Чат в островке, 60 инструментов Windows с проверкой риска, память с поиском по смыслу, напоминания, управление окнами любых программ, просмотр экрана, MCP, Telegram, озвучка, голосовой ввод и активационная фраза. Модель Live2D впереди.

## Документы

- [Дизайн](docs/superpowers/specs/2026-11-10-mika-assistant-design.md)
- [План по эпикам](docs/superpowers/plans/2026-11-10-roadmap.md)
- Пошаговые планы: [эпик 1](docs/superpowers/plans/2026-11-10-epic-01-foundation.md), [эпик 2](docs/superpowers/plans/2026-11-10-epic-02-chat-llm.md), [эпик 3](docs/superpowers/plans/2026-11-10-epic-03-risk-policy.md), [эпик 4](docs/superpowers/plans/2026-11-10-epic-04-tools.md), [эпик 6](docs/superpowers/plans/2026-11-11-epic-06-island-visuals.md), [эпик 8](docs/superpowers/plans/2026-11-11-epic-08-window-control.md), [эпик 9](docs/superpowers/plans/2026-11-11-epic-09-daily-memory.md), [эпик 10](docs/superpowers/plans/2026-11-11-epic-10-extensions.md)
- [Решения](docs/decisions/): границы MVP [0006](docs/decisions/0006-mvp-scope.md), основной функционал и итоги живой проверки [0009](docs/decisions/0009-core-features.md), речь через провайдеров [0010](docs/decisions/0010-voice-providers.md), режим окна и остатки [0011](docs/decisions/0011-window-mode-and-remainders.md), перетаскивание, активационная фраза, обновления [0012](docs/decisions/0012-drag-wake-updates.md)
- [Макет интерфейса](docs/design/island-mockup.html) (открыть в браузере)

## Что нужно для разработки

| Что | Команда установки |
|---|---|
| Rust stable | `winget install Rustlang.Rustup` |
| Tauri CLI 2 | `cargo install tauri-cli --version "^2" --locked` |
| MSVC Build Tools 2022 | `winget install Microsoft.VisualStudio.2022.BuildTools` |
| WebView2 Runtime | входит в Windows 11 |
| Node.js 24 | `winget install OpenJS.NodeJS` |

## Запуск

```
npm --prefix frontend ci
npm --prefix frontend run build
cd crates/mika-gui
cargo tauri dev
```

Островок появится вверху основного монитора. Щелчок по нему или Ctrl+Alt+M открывает чат. Кнопка настроек в шапке чата открывает разделы: Персонаж, Провайдеры, Разрешения, Инструменты, Память, Расширения, Голос, Островок, Общие.

Установщик: `cargo tauri build` из `crates/mika-gui` (без установленного Tauri CLI: `npx @tauri-apps/cli@2 build`), результат в `target/release/bundle/nsis/`.

Терминальный клиент без интерфейса: `cargo run -p mika-cli`. Настройка из терминала:

```
mika-cli provider openai https://routerai.ru/api/v1 openai/gpt-oss-20b
mika-cli key openai                      # ключ вводится строкой, уходит в Credential Manager
mika-cli model embedding perplexity/pplx-embed-v1-0.6b
mika-cli model vision google/gemma-4-26b-a4b-it
mika-cli model image black-forest-labs/flux.2-klein-4b
mika-cli model tts google/gemini-3.8-flash-lite-tts   # синтез у провайдера; "-" возвращает голос Windows
mika-cli model tts-voice Kore
mika-cli model stt qwen/qwen3-asr-0.6b
mika-cli web on
mika-cli mic on
mika-cli status
mika-cli probe
mika-cli voices
mika-cli say "Привет"
mika-cli roundtrip "Привет"              # синтез и распознавание того же звука, с временем
mika-cli hear                            # записать фразу с микрофона и показать текст
```

На вопрос о действии отвечай «да», «разреши всегда» или «нет».

Предпросмотр интерфейса в браузере без Tauri: `npm --prefix frontend run dev`, затем `http://localhost:5173/?mock=chat` (также `cards`, `reminder`, `confirm`, `high`, `island`, `settings&page=memory`).

Данные лежат в `%APPDATA%\Mika` (переменная `MIKA_DATA_DIR` меняет папку), ключи провайдеров — в «Диспетчере учётных данных» Windows под именем `Mika`.

## Что дальше

Модель Live2D (пробные сборки 0.2 и 0.5; лицензию Cubism SDK и модель выбирает пользователь), подпись установщика (нужен сертификат), сервер выпусков для обновлений.
