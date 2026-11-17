# Безопасность

[English below](#security-policy)

Мика читает окна, нажимает кнопки, работает с файлами и хранит ключи провайдеров. Поэтому ошибки, которые ослабляют защиту, важнее обычных.

## Как сообщить об уязвимости

Не создавай открытую задачу (issue): её увидят все, пока ошибка не исправлена.

Сообщи приватно через GitHub: [Report a vulnerability](https://github.com/Artem936c/Mika/security/advisories/new). Сообщение увидит только автор.

Опиши:

- версию Мики (Настройки → «Общие») и Windows;
- что нужно сделать, чтобы повторить;
- что происходит и чем это опасно.

Если прикладываешь журнал (Настройки → «Журнал» → «Сохранить в файл»), просмотри его перед отправкой. Ключи из журнала вычищаются, но тексты разговоров в нём есть, если ты включил их запись.

## Что считается уязвимостью

- Действие выполняется в обход политики рисков или без подтверждения, которое она требует.
- Действие высокого риска разрешается навсегда.
- Мика читает или нажимает что-то в окне из «слепой зоны»: пароли, вход, банк, оплата.
- Ключ провайдера или токен Telegram попадает в `config.toml`, журнал, текст ошибки или незашифрованную копию для переноса.
- Инструменты серверов MCP или команды из Telegram обходят политику рисков.
- Обновление ставится без проверки контрольной суммы.
- Микрофон или просмотр экрана включаются без явного согласия.

Ошибки модели (неверный ответ, выдуманный факт) уязвимостью не считаются. О них можно написать в обычной задаче.

## Какие версии поддерживаются

Исправления выходят только для последней версии из [релизов](https://github.com/Artem936c/Mika/releases).

---

# Security policy

Mika reads windows, clicks buttons, works with files and stores provider keys, so bugs that weaken its protections matter more than ordinary ones.

## Reporting a vulnerability

Please do not open a public issue. Report privately through GitHub: [Report a vulnerability](https://github.com/Artem936c/Mika/security/advisories/new). Only the maintainer sees it.

Include the Mika version (Settings → General), the Windows version, steps to reproduce, and what goes wrong. If you attach the log (Settings → Log → Save to a file), review it first: keys are scrubbed, but conversation texts are kept when you turned their logging on.

## What counts

- An action runs around the risk policy or without the confirmation it requires.
- A high-risk action can be allowed forever.
- Mika reads or clicks inside a "blind zone" window: passwords, sign-in, banking, payment.
- A provider key or Telegram token ends up in `config.toml`, the journal, an error text or an unencrypted transfer backup.
- MCP server tools or Telegram commands bypass the risk policy.
- An update installs without a checksum match.
- The microphone or screen viewing turns on without explicit consent.

Wrong or made-up model answers are not vulnerabilities; a regular issue is fine for those.

## Supported versions

Only the latest [release](https://github.com/Artem936c/Mika/releases) receives fixes.
