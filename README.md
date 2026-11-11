# Мика

Живой ИИ-помощник для Windows: голос и текст, 2D-персонаж с эмоциями, островок вверху экрана, действия в системе с подтверждением риска, любой LLM-провайдер. Rust, Tauri 2, Svelte.

Состояние: пре-релиз 0.1.0-pre.1 ([что в нём](docs/releases/0.1.0-pre.1.md)), веха В1. Текстовый чат в островке, 28 инструментов Windows, проверка риска и подтверждения. Голос, аватар и управление чужими окнами впереди.

## Документы

- [Дизайн](docs/superpowers/specs/2026-11-10-mika-assistant-design.md)
- [План по эпикам](docs/superpowers/plans/2026-11-10-roadmap.md)
- Пошаговые планы: [эпик 1](docs/superpowers/plans/2026-11-10-epic-01-foundation.md), [эпик 2](docs/superpowers/plans/2026-11-10-epic-02-chat-llm.md), [эпик 3](docs/superpowers/plans/2026-11-10-epic-03-risk-policy.md), [эпик 4](docs/superpowers/plans/2026-11-10-epic-04-tools.md)
- [Решения](docs/decisions/), границы MVP: [0006](docs/decisions/0006-mvp-scope.md)
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

Островок появится вверху основного монитора. Щелчок по нему или Ctrl+Alt+M открывает чат. Кнопка ⚙ в шапке чата — настройки: провайдер (OpenAI-совместимый, Anthropic, Ollama), разрешения, интернет-инструменты.

Установщик: `cargo tauri build --bundles nsis` из `crates/mika-gui`, результат в `target/release/bundle/nsis/`.

Терминальный клиент без интерфейса: `cargo run -p mika-cli`. Настройка из терминала: `mika-cli provider openai https://routerai.ru/api/v1 mistralai/mistral-nemo`, затем `mika-cli key openai` (ключ вводится строкой, уходит в Credential Manager), `mika-cli status`, `mika-cli probe`. На вопрос о действии отвечай «да», «разреши всегда» или «нет».

Предпросмотр интерфейса в браузере без Tauri: `npm --prefix frontend run dev`, затем `http://localhost:5173/?mock=chat` (также `card`, `high`, `island`).

Данные лежат в `%APPDATA%\Mika` (переменная `MIKA_DATA_DIR` меняет папку), ключи провайдеров — в «Диспетчере учётных данных» Windows под именем `Mika`.

## Что дальше

Пробные сборки 0.2–0.5 (аватар, голос, UI Automation, лицензии), затем эпики 5–7: голос, островок по макету, аватар.
