// Development-only preview without Tauri: open http://localhost:5173/?mock=chat
// Scenarios: island, listen, think, speak, chat, cards, av, reminder, confirm, high, long, settings.
// `&page=providers|permissions|tools|memory|extensions|voice|island|hotkeys|costs|journal|general` picks a settings page, `&timer` shows a countdown, `&narrow` hides the avatar column,
// `&cloud` switches the voice page to the provider engine, `&drama` to Drama 3 with expressive speech on,
// FFmpeg on the tools page: not found by default, `&ffmpeg` found in PATH, `&ffmpeg_dl` downloaded by Mika,
// `&ffmpeg_gpl` a GPL build, `&ffmpeg_bad` a saved path without ffmpeg.exe, `&ffmpeg_fail` a download that fails its checksum,
// `?view=avatar&mock=speak` shows the desktop avatar window. Not bundled into production builds.
// Day summary and spending: scenario `day` is the card of the day and the evening summary, scenario `costs` the card of spending and a budget warning;
// `&page=costs` is the settings page of spending: `&costs_empty` nothing counted and no limits, `&costs_over` the day limit passed,
// `&costs_noprice` a model without a price, requests without token data and a second currency; `&evening` turns the evening summary on (tools page);
// `&costs_free` only a local model was used: nothing spent, which is not «the price is unknown».
// With `&costs_over` the page also says that her own paid work is on hold (the switch under the budget turns that off).
// Scenario `tgvoice`: a command that came as a Telegram voice message. Telegram on the extensions page:
// `&tgvoice` voice messages in and voice replies on, `&tgvoice_in` voice messages in only, `&nostt` no recognition model.
// `&moodvoice` turns on the mood direction switch of the voice page.
// Epic 18: scenario `dictation` walks the island through a dictation into another program (`&blind` shows the refusal instead),
// scenario `screen_ask` opens the chat with the card of a window picture (`&mic` enables its voice button, `&screen_off` makes the send fail),
// `?mock=settings&page=hotkeys` is the hotkeys page: `&dictation` and `&screen_ask` turn the two actions on, `&hk_taken` leaves dictation
// without a free combination, `&mic` and `&screen` are the privacy switches, `&nostt` means no speech recognition model is set.
// `&needs` on the hotkeys page makes an action whose master switch (`&mic`, `&screen`) is off wait for it; `&gone` in scenario
// `screen_ask` lets the app throw the picture away after three seconds.
// Events and watching: scenario `events` (a scenario is made and runs, a watch is set and tells), `event_confirm` (the question
// before a scenario is saved; `&long` with an instruction of 300 characters, `&inchat` inside the chat). On the extensions page:
// `&noevents` no scenarios and no watches, `&events_off` scenarios on pause.
// Background jobs: `bgjob` a compression that went to the background and a second job waiting in the queue (`&fail`: the first one fails),
// `folder` a whole folder as one job, `folderask` its confirm card, `recask` the confirm card of a screen recording
// (`&ofwindow`: of one window with a long title), `rec&window` the ordinary window with the red dot of a recording,
// `rec` the island with the red dot of a recording (`&inchat`: the chat with its card, `&done`: it ends with a file).
// Deep search: `research` runs one in the chat (`&partial` a report cut by the time limit, `&fail` a search that found nothing),
// `research_ask` shows its confirm card (`&high` with a token limit above 100 thousand, `&inchat` inside the chat);
// `&research_high` shows the settings block on the tools page with such a limit.
// Backup block on the general page (`?mock=settings&page=general`): `&backup_plain` a copy without a password,
// `&backup_wrong` every password is refused, `&backup_newer` a copy of a newer format, `&backup_fail` the export fails,
// `&backup_cancel` the file dialog is cancelled, `&backup_keysfail` one key is not written. The copy holds three scenarios
// by event: one of an instruction, one of a saved command the copy holds, one of a command that is nowhere.
// `?mock=chat&welcome` shows the restore link on the first-run screen.
// Journal: scenario `journal` opens the journal page of the settings with rows of every level and category, a long row,
// a row of several lines and the line about a run that was cut short; a new row is written every three seconds, so «follow»
// has something to show. `&notice` stops in the chat with the notice of that run (its link opens the page), `&journal_empty`
// is an empty journal, `&journal_fail` makes reading the next page and saving the settings fail, `&journal_cancel` closes
// the save dialog without a file. `?mock=settings&page=journal` is the same page without the unclean run.
// `&lang=en` starts the preview in English; the language picker on `&page=general` and on `&welcome` switches it in place.

import { mockIPC } from '@tauri-apps/api/mocks';
import { emit } from '@tauri-apps/api/event';
import { clock } from '../lib/chat';

const LONG_COMMAND =
  'Get-ChildItem "$env:USERPROFILE\\Downloads" -Recurse | Where-Object { $_.Length -gt 100MB } | Sort-Object Length -Descending | ' +
  'Select-Object FullName, @{n="GB";e={[math]::Round($_.Length/1GB,2)}} | Export-Csv "$env:TEMP\\big-files.csv" -NoTypeInformation; '.repeat(4);

const ASSESSMENTS = {
  confirm: {
    level: 'medium',
    summary: 'Выполнить команду PowerShell',
    target: 'Get-ChildItem "$env:USERPROFILE\\Downloads" | Sort-Object Length -Descending | Select-Object -First 10',
    reason: 'Только читает список файлов. Ничего не меняет.',
    scope: 'get-childitem',
  },
  high: {
    level: 'high',
    summary: 'Удалить всё из папки «Загрузки»',
    target: 'Remove-Item "$env:USERPROFILE\\Downloads\\*" -Recurse -Force',
    reason: 'Удалит 214 файлов (3,8 ГБ) мимо корзины. Восстановить не получится.',
    scope: '*',
  },
  long: {
    level: 'medium',
    summary: 'Выполнить команду PowerShell',
    target: LONG_COMMAND,
    reason: 'Читает список файлов и пишет отчёт во временную папку. Причина тоже бывает длинной и тогда переносится на вторую строку.',
    scope: 'get-childitem',
  },
} as const;

const WEATHER = {
  kind: 'weather',
  city: 'Пермь',
  temp: -10,
  feels: -16,
  wind: 6,
  text: 'ясно',
  days: [
    { date: '2026-11-11', min: -12, max: -9, text: 'ясно' },
    { date: '2026-11-12', min: -7, max: -4, text: 'снег' },
    { date: '2026-11-13', min: -9, max: -6, text: 'ясно' },
    { date: '2026-11-14', min: -5, max: -2, text: 'облачно' },
  ],
};

const REPLY = 'Изучила информацию. Сегодня −10 и ветер, оденься потеплее. Завтра потеплеет до −4, пойдёт снег.';

/** The card of the tool `day_summary`. */
const DAY_CARD = {
  kind: 'day_summary',
  date: '2026-11-14',
  title: 'пятница, 14 ноября',
  app_total: '6 ч 20 мин',
  apps: [
    { name: 'VS Code', seconds: 11100, text: '3 ч 05 мин' },
    { name: 'Chrome', seconds: 6000, text: '1 ч 40 мин' },
    { name: 'Telegram', seconds: 2100, text: '35 мин' },
  ],
  counts: [
    { id: 'focus', value: '50 мин' },
    { id: 'meetings', value: '2' },
    { id: 'todos', value: '3' },
    { id: 'reminders', value: '2' },
    { id: 'tasks', value: '2' },
  ],
  spent: '12,40 ₽',
};

/** The card of the tool `cost_report`. */
const COST_CARD = {
  kind: 'cost_report',
  label: 'сегодня',
  totals: [{ currency: 'RUB', cost: 12.4, text: '12,40 ₽' }],
  budget: {
    currency: 'RUB',
    day_spent: 12.4,
    day_limit: 100,
    day_share: 12,
    month_spent: 310,
    month_limit: 2000,
    month_share: 15,
    day_text: '12,40 ₽ из 100 ₽',
    month_text: '310 ₽ из 2 000 ₽',
  },
  kinds: [
    { kind: 'chat', name: 'ответы модели', money: '9,10 ₽', units: '310 тыс. токенов', requests: 84, free: false },
    { kind: 'speech', name: 'озвучка', money: '3,10 ₽', units: '2,9 тыс. знаков', requests: 41, free: false },
    { kind: 'transcription', name: 'распознавание речи', money: '0,20 ₽', units: '4 мин 10 с', requests: 6, free: false },
  ],
  unpriced: 3,
  blind: 2,
  free: false,
};

const EVENING_REMARK = {
  type: 'remark',
  kind: 'day',
  text: 'Итог дня:\nСегодня ты шесть часов двадцать минут провёл за компьютером, больше всего в VS Code. Закрыл три дела и был на двух встречах. Завтра в 9:30 планёрка — составить план на день?',
};

const SIGNS: Record<string, string> = { RUB: '₽', USD: '$', EUR: '€' };

/** Money as the core writes it; enough for a preview. */
function moneyText(cost: number, code: string): string {
  const digits = Number.isInteger(cost) ? 0 : 2;
  const n = new Intl.NumberFormat('ru-RU', { minimumFractionDigits: digits, maximumFractionDigits: digits }).format(cost);
  return SIGNS[code] ? `${n} ${SIGNS[code]}` : n;
}

interface MockPrice {
  provider: string;
  model: string;
  currency: string;
  input_mtok: number;
  output_mtok: number;
  per_kchar: number;
  per_minute: number;
  per_image: number;
}

/** One model of one kind in a report of spending. `cost` in roubles, `null` when there is no money. */
function usageLine(kind: string, model: string, requests: number, units: string, cost: number | null, extra: Record<string, unknown> = {}) {
  return {
    kind,
    provider: 'routerai.ru',
    model,
    requests,
    blind: 0,
    unpriced: 0,
    tokens_in: 0,
    tokens_cached: 0,
    tokens_out: 0,
    chars: 0,
    seconds: 0,
    images: 0,
    units,
    cost: cost === null ? [] : [{ currency: 'RUB', cost, text: moneyText(cost, 'RUB') }],
    free: false,
    price: 'list',
    ...extra,
  };
}

type MockLine = ReturnType<typeof usageLine>;

const COST_ORDER = ['chat', 'vision', 'speech', 'transcription', 'image', 'embedding'];

/** A report as the core builds it from the lines of models. */
function costReport(models: MockLine[], from: string, to: string, main: string) {
  const sum = (lines: MockLine[]) => {
    const by = new Map<string, number>();
    for (const l of lines) for (const m of l.cost) by.set(m.currency, (by.get(m.currency) ?? 0) + m.cost);
    return [...by.entries()]
      .filter(([, cost]) => cost > 0)
      .sort(([a], [b]) => Number(b === main) - Number(a === main))
      .map(([currency, cost]) => ({ currency, cost: Math.round(cost * 100) / 100, text: moneyText(Math.round(cost * 100) / 100, currency) }));
  };
  const count = (lines: MockLine[], field: 'requests' | 'unpriced' | 'blind') => lines.reduce((n, l) => n + l[field], 0);
  const kinds = COST_ORDER.map((kind) => models.filter((l) => l.kind === kind))
    .filter((lines) => lines.length > 0)
    .map((lines) => ({
      ...lines[0],
      provider: '',
      model: '',
      requests: count(lines, 'requests'),
      unpriced: count(lines, 'unpriced'),
      blind: count(lines, 'blind'),
      cost: sum(lines),
      price: 'none',
    }));
  const totals = sum(models);
  const requests = count(models, 'requests');
  // Every request has its money and the money is zero: a local model.
  const free = requests > 0 && totals.length === 0 && models.every((l) => l.free);
  return { from, to, totals, kinds, models, requests, unpriced: count(models, 'unpriced'), blind: count(models, 'blind'), free };
}
/** A background job as the job board sends it. */
interface MockJob {
  id: number;
  kind: string;
  title: string;
  state: 'queued' | 'running' | 'stopping' | 'done' | 'failed' | 'stopped';
  live: boolean;
  share: number;
  note?: string;
  text?: string;
  data?: unknown;
}

const REC_TITLE = 'Записать экран';
const REC_FILE = String.raw`C:\Users\me\Videos\Mika\Запись экрана 2026-11-14 15-30-05.mp4`;
const FOLDER_TITLE = 'Сжать 12 видео из папки «Отпуск» до 720p (1,8 ГБ, 47:10)';
const FOLDER_OUT = String.raw`C:\Users\me\Videos\Отпуск\720p`;

/** Confirm cards of the folder tool and of screen recording, the target in lines as the tools write it. */
const JOB_ASKS = {
  folderask: {
    tool: 'av_folder',
    user: 'Сожми все видео из папки «Отпуск» до 720p',
    assessment: {
      level: 'medium',
      summary: FOLDER_TITLE,
      target: [
        String.raw`C:\Users\me\Videos\Отпуск → ` + FOLDER_OUT,
        'день1.mp4, день2.mp4, день3.mp4, день4.mp4, день5.mp4 и ещё 7',
        'Не подходят 2: титры.mp4 — уже меньше нужного размера; обрывок.avi — файл не читается',
      ].join('\n'),
      reason: 'исходные файлы не меняются, результаты — новые файлы; работа пойдёт в фоне',
      scope: String.raw`compress: c:\users\me\videos\отпуск -> c:\users\me\videos\отпуск\720p`,
    },
  },
  recask: {
    tool: 'screen_record',
    user: 'Запиши экран вместе с моим голосом',
    assessment: {
      level: 'high',
      summary: REC_TITLE,
      target: ['Микрофон: записывается', 'Звук компьютера: записывается', 'Длина: до 30 минут', 'Экран: монитор 1, 2560×1440', `Файл: ${REC_FILE}`].join('\n'),
      reason: 'запись экрана и звука включается только с твоего подтверждения, каждый раз заново; на окнах с паролями и оплатой запись встаёт на паузу',
      scope: 'экран',
    },
  },
} as const;

/** The same card for one window: its title is the page's own text, sixty characters of it. */
const REC_WINDOW_ASK = {
  tool: 'screen_record',
  user: 'Запиши окно браузера вместе с моим голосом',
  assessment: {
    level: 'high',
    summary: 'Записать окно chrome',
    target: [
      'Микрофон: записывается',
      'Звук компьютера: записывается',
      'Длина: до 30 минут',
      'Окно: «Отчёт за третий квартал — сводная таблица продаж по региона…» (chrome.exe), 1920×1040',
      `Файл: ${REC_FILE}`,
    ].join('\n'),
    reason:
      'запись экрана и звука включается только с твоего подтверждения, каждый раз заново; на окнах с паролями и оплатой запись встаёт на паузу; окно пишется, пока его программа на переднем плане',
    scope: 'chrome.exe',
  },
} as const;

const RESEARCH_QUESTION = 'Чем тепловой насос «воздух-вода» отличается от «грунт-вода» для дома 120 м², и что выгоднее';
const RESEARCH_FOLDER = String.raw`C:\Users\me\Documents\Mika\Research`;
const RESEARCH_MODEL = 'mistralai/mistral-nemo';

const RESEARCH_REPORT = {
  kind: 'research_report',
  path: `${RESEARCH_FOLDER}\\2026-11-14 12-30 Чем тепловой насос воздух вода отличается от грунт.md`,
  title: RESEARCH_QUESTION,
  sources: 6,
  unread: 3,
  unsourced: 1,
  llm_calls: 9,
  tokens: 38200,
  seconds: 143,
  limits: [] as string[],
};

/** The confirm card of a deep search: medium by default, high above 100 thousand tokens. */
const researchAssessment = (high: boolean) => ({
  level: high ? 'high' : 'medium',
  summary: `Глубокий поиск: «${RESEARCH_QUESTION}»`,
  target: `Лимиты: поисков до 5, страниц до 8, минут до 5, токенов до ${high ? 150 : 50} тыс. по оценке, без скрытых рассуждений модели.\nОбращений к модели ${RESEARCH_MODEL}: до 10, каждое платное.\nОтчёт: ${RESEARCH_FOLDER}`,
  reason: high ? 'лимит больше 100 тыс. токенов: такой поиск спрашивается каждый раз' : 'запросы уйдут в DuckDuckGo, тексты страниц — модели; запишет один файл .md',
  scope: `5/8/5/${high ? 150000 : 50000} ${RESEARCH_MODEL} -> ${RESEARCH_FOLDER.toLowerCase()}`,
});

// A copy made on another PC, as the backup commands describe it.
const BACKUP_SAFETY = String.raw`C:\Users\me\AppData\Roaming\Mika\backups\before-restore.mikabackup`;

const BACKUP_FILE = {
  path: String.raw`D:\Копии\mika-backup-2026-11-14.mikabackup`,
  name: 'mika-backup-2026-11-14.mikabackup',
  bytes: 4_812_300,
  created_at: '2026-11-14T15:30:00+05:00',
  device: 'OLD-PC',
  app_version: '0.3.3',
  encrypted: true,
  keys: 5,
  newer_app: false,
};

const BACKUP_OFF = [
  { id: 'microphone' },
  { id: 'screen' },
  { id: 'dictation' },
  { id: 'telegram' },
  { id: 'telegram_voice' },
  { id: 'web' },
  { id: 'briefing' },
  { id: 'evening' },
  { id: 'mcp', count: 1 },
  { id: 'docs', count: 1 },
];
// Scenarios by event of the copy: the second runs a saved command the copy holds, the third one it does not.
const BACKUP_SCENARIOS = [
  {
    name: 'Наушники',
    event: 'audio_connected',
    filter: 'sony',
    idle_min: 0,
    action: { kind: 'instruction', text: 'Включи музыку и сделай громкость 30' },
    cooldown_s: 60,
    max_per_day: 20,
    command_in_file: false,
    command_here: false,
  },
  {
    name: 'Работа',
    event: 'app_started',
    filter: 'code.exe',
    idle_min: 0,
    action: { kind: 'command', text: 'Рабочий режим' },
    cooldown_s: 300,
    max_per_day: 5,
    command_in_file: true,
    command_here: false,
  },
  {
    name: 'Ушёл',
    event: 'user_away',
    filter: '',
    idle_min: 15,
    action: { kind: 'command', text: 'Тишина' },
    cooldown_s: 3600,
    max_per_day: 10,
    command_in_file: false,
    command_here: false,
  },
];
const BACKUP_DOCS = [String.raw`C:\Users\old\Documents\Работа`];

const BACKUP_PREVIEW = {
  file: BACKUP_FILE,
  categories: [
    { id: 'settings', in_file: 1, here: 1, available: true, selected: true },
    { id: 'history', in_file: 1520, here: 12, available: true, selected: true },
    { id: 'memory', in_file: 34, here: 0, available: true, selected: true },
    { id: 'notes', in_file: 9, here: 2, available: true, selected: true },
    { id: 'todos', in_file: 4, here: 0, available: true, selected: true },
    { id: 'reminders', in_file: 3, here: 1, available: true, selected: true },
    { id: 'tasks', in_file: 2, here: 0, available: true, selected: false },
    { id: 'commands', in_file: 2, here: 0, available: true, selected: true },
    { id: 'scenarios', in_file: 3, here: 1, available: true, selected: true },
    { id: 'rules_deny', in_file: 1, here: 0, available: true, selected: true },
    { id: 'rules_allow', in_file: 3, here: 1, available: true, selected: false },
    { id: 'secrets', in_file: 5, here: 2, available: true, selected: true },
  ],
  allow_rules: [
    { tool: 'open_app', pattern: 'проводник' },
    { tool: 'run_shell', pattern: 'get-childitem' },
    { tool: 'av_convert', pattern: String.raw`c:\видео -> c:\видео` },
  ],
  allow_rules_total: 3,
  allow_rules_dropped: 0,
  tasks: [
    { text: 'Расскажи погоду и что в календаре', due_at: 1794366000, repeat: 'daily' },
    { text: 'Проверь, ответили ли в рабочем чате', due_at: 1794300600, repeat: 'every:30' },
  ],
  tasks_total: 2,
  scenarios: BACKUP_SCENARIOS,
  scenarios_total: 3,
  secrets: [
    { kind: 'llm_openai', name: null, replaces: true },
    { kind: 'speech', name: null, replaces: false },
    { kind: 'telegram', name: null, replaces: false },
    { kind: 'calendar', name: null, replaces: false },
    { kind: 'mcp', name: 'files', replaces: false },
  ],
  addresses: [
    { purpose: 'llm', incoming: 'https://routerai.ru/api/v1', current: 'https://api.openai.com/v1', changed: true, key_here: true, key_in_file: true },
  ],
  mcp: [
    {
      name: 'files',
      target: String.raw`npx -y @modelcontextprotocol/server-filesystem C:\data`,
      was_enabled: true,
      current: '',
      changed: true,
      token_here: false,
      token_in_file: true,
    },
  ],
  telegram: { was_enabled: true, user_id: 123456789, current: 0, changed: true, token_here: true, token_in_file: true },
  switched_off: BACKUP_OFF,
  docs_folders: BACKUP_DOCS,
  schema_newer: false,
  skipped_unknown: 0,
  dropped_rows: 0,
  safety_copy: BACKUP_SAFETY,
};

const BACKUP_RESULT = {
  restored: BACKUP_PREVIEW.categories.map((c) => ({ id: c.id, count: c.in_file })),
  keys_written: 5,
  keys_failed: [] as { kind: string; name: string | null }[],
  switched_off: BACKUP_OFF,
  docs_folders: BACKUP_DOCS,
  scenarios_without_command: 0,
  restart_needed: true,
  config_error: null,
  safety_copy: BACKUP_SAFETY,
};

/** A row of the journal as the journal commands send it. */
interface MockRow {
  seq: number | null;
  t: string;
  level: 'error' | 'warn' | 'info' | 'debug';
  category: string;
  message: string;
}

const JOURNAL_LONG =
  'ffmpeg exited with code 1 after 41.3 s: job 7 (video_compress), input 245 MB, 12:31, h264 1920x1080; ' +
  'last line: [libx264 @ 000001d5c0a4f3c0] Error while opening encoder - maybe incorrect parameters such as bit_rate, rate, width or height. ' +
  'The unfinished file was removed. The queue goes on with job 8 (av_convert). ' +
  'Arguments: <hidden 212 chars>. Free space on the target disk: 18.4 GB. Hardware encoder: none, software x264 with preset medium. ' +
  'The same input failed 2 times today.';

const JOURNAL_LINES = [
  'request failed: provider=routerai.ru model=mistralai/mistral-nemo 2210 ms, http=500, retries=3',
  'body: {"error":{"message":"upstream timeout","type":"server_error"}}',
  'the turn ended with an error shown in the chat',
].join('\n');

/** One row of every level and of every category; the same texts are written again and again further back. */
const JOURNAL_SAMPLES: [MockRow['level'], string, string][] = [
  ['info', 'turn', 'turn finished: turn=12 outcome=ok ms=4800 steps=2 tools=1 reply_chars=148'],
  ['info', 'voice', 'synthesis done: 148 chars, 620 ms, engine=provider'],
  ['info', 'llm', 'request done: provider=routerai.ru model=mistralai/mistral-nemo 1840 ms, tokens in=2140 out=96, stop=stop, http=200, retries=0'],
  ['info', 'tool', 'tool call finished: turn=12 tool=open_app risk=low decision=rule ms=212 outcome=ok'],
  ['debug', 'llm', 'retry 1 of 3 after http=429, wait 800 ms'],
  ['info', 'llm', 'request done: provider=routerai.ru model=mistralai/mistral-nemo 960 ms, tokens in=1980 out=41, stop=tool_calls, http=200, retries=1'],
  ['info', 'turn', 'turn started: turn=12 source=window chars=42 expanded=false waited_ms=0'],
  ['error', 'jobs', JOURNAL_LONG],
  ['warn', 'costs', 'day budget at 80%: 80.40 of 100 RUB'],
  ['info', 'policy', 'question asked in the island: run_shell, risk=medium; answer=once'],
  ['warn', 'tool', 'tool call finished: turn=11 tool=run_shell risk=medium decision=asked_yes ms=30012 outcome=error error=<hidden 38 chars>'],
  ['error', 'llm', JOURNAL_LINES],
  ['warn', 'telegram', 'polling failed: connection reset by peer; next try in 5 s'],
  ['info', 'telegram', 'incoming: voice, 7 s'],
  ['error', 'mcp', 'server "files" exited with code 1'],
  ['info', 'mcp', 'server "remote" started: 4 tools'],
  ['info', 'events', 'scenario 3 fired: audio_connected'],
  ['debug', 'events', 'scenario 5 skipped: pause of 2 h is not over'],
  ['info', 'jobs', 'job 6 finished: av_convert, 19.8 s'],
  ['info', 'backup', 'copy created: 12 categories, 4.6 MB, encrypted'],
  ['info', 'update', 'check done: no newer version'],
  ['warn', 'hotkeys', 'registration refused: Ctrl+Alt+D is taken by another program'],
  ['info', 'web', 'deep search finished: searches=5 pages=8 sources=6, 143 s'],
  ['warn', 'web', 'guarded client refused https://example.com/v1/items?key=***: private address'],
  ['warn', 'lib', 'reqwest: connection closed before message completed'],
  ['debug', 'voice', 'microphone opened: 16000 Hz, mono'],
  ['info', 'voice', 'recognition done: 1840 ms, 4.2 s of audio, ok'],
  ['info', 'app', 'settings saved: log, voice'],
  ['info', 'app', 'Mika 0.4.0 started: Windows build 26100, lang=ru, switches: speak, web, costs'],
  ['warn', 'app', 'the previous run did not exit cleanly'],
  ['error', 'app', 'panic at crates/mika-gui/src/window.rs:276: called `Option::unwrap()` on a `None` value'],
];

/** A time as the journal writes it: RFC 3339 with milliseconds and the local offset. */
function journalStamp(ms: number): string {
  const d = new Date(ms);
  const two = (n: number) => String(n).padStart(2, '0');
  const offset = -d.getTimezoneOffset();
  const zone = `${offset < 0 ? '-' : '+'}${two(Math.floor(Math.abs(offset) / 60))}:${two(Math.abs(offset) % 60)}`;
  return (
    `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())}T${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}` +
    `.${String(d.getMilliseconds()).padStart(3, '0')}${zone}`
  );
}

/** 260 rows, newest first, the oldest from days ago. Only the newest 120 are «in memory» and have a number. */
function journalRows(): MockRow[] {
  const now = Date.now();
  const rows: MockRow[] = [];
  for (let i = 0; i < 260; i += 1) {
    const [level, category, message] = JOURNAL_SAMPLES[i % JOURNAL_SAMPLES.length];
    // Every row a little further back, with a jump of a day after each hundred.
    const at = now - 4000 - i * 97_531 - Math.floor(i / 100) * 24 * 3_600_000;
    rows.push({ seq: i < 120 ? 120 - i : null, t: journalStamp(at), level, category, message });
  }
  return rows;
}

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

// ---- avatar models ----
// `&model` gives one model with stand-in portraits; `&vrm` also serves the file
// `src/dev/sample.vrm` as that model (the file is not in the repository);
// `&nostills` starts it without portraits, so the page draws them itself.

const MOCK_MODEL_ID = '0123456789abcdef';
const MOCK_MODEL_2 = 'fedcba9876543210';
const MOCK_EMOTIONS = ['neutral', 'joy', 'shy', 'surprise', 'sad', 'angry', 'think', 'wink', 'sleepy', 'worry'];
const MOCK_EXPRESSIONS = ['happy', 'angry', 'sad', 'relaxed', 'surprised', 'aa', 'ih', 'ou', 'ee', 'oh', 'blink', 'blinkLeft', 'blinkRight', 'Blush'];

interface MockSlot {
  source: 'auto' | 'manual';
  mix: [string, number][];
  gaze?: [number, number];
}

/** The slot as the program fills it; the same table as `auto_slot` in mika-core, for the expressions above. */
function mockAutoSlot(emotion: string): MockSlot {
  const mixes: Record<string, MockSlot> = {
    neutral: { source: 'auto', mix: [] },
    joy: { source: 'auto', mix: [['happy', 1]] },
    shy: { source: 'auto', mix: [['Blush', 1]] },
    surprise: { source: 'auto', mix: [['surprised', 1]] },
    sad: { source: 'auto', mix: [['sad', 1]] },
    angry: { source: 'auto', mix: [['angry', 1]] },
    think: { source: 'auto', mix: [], gaze: [-15, 15] },
    wink: { source: 'auto', mix: [['blinkLeft', 1], ['happy', 0.5]] },
    sleepy: { source: 'auto', mix: [['relaxed', 0.5], ['blink', 0.6]] },
    worry: { source: 'auto', mix: [['sad', 0.5], ['surprised', 0.2]] },
  };
  return mixes[emotion];
}

function mockProfile(title: string) {
  return {
    version: 1,
    format: 'vrm',
    spec: '1.0',
    title,
    author: 'Автор модели',
    license: 'https://vrm.dev/licenses/1.0/; everyone; personalNonProfit',
    expressions: MOCK_EXPRESSIONS,
    slots: Object.fromEntries(MOCK_EMOTIONS.map((e) => [e, mockAutoSlot(e)])) as Record<string, MockSlot>,
    lip: 1,
  };
}

/** A stand-in portrait: a flat face of its own colour for each emotion, base64 PNG. */
function mockStill(name: string): string {
  const full = name === 'full';
  const canvas = document.createElement('canvas');
  canvas.width = full ? 192 : 96;
  canvas.height = full ? 256 : 96;
  const g = canvas.getContext('2d')!;
  const emotion = name.replace('-talk', '');
  const hue = (MOCK_EMOTIONS.indexOf(emotion) + 1) * 36;
  if (!full) {
    g.fillStyle = `hsl(${hue} 55% 30%)`;
    g.fillRect(0, 0, 96, 96);
  }
  const cx = canvas.width / 2;
  const cy = full ? 70 : 50;
  if (full) {
    g.fillStyle = 'hsl(260 40% 40%)';
    g.beginPath();
    g.roundRect(cx - 55, 110, 110, 160, 30);
    g.fill();
  }
  g.fillStyle = '#ffe3d2';
  g.beginPath();
  g.arc(cx, cy, 30, 0, Math.PI * 2);
  g.fill();
  g.fillStyle = '#2a2140';
  g.fillRect(cx - 14, cy - 8, 6, 6);
  g.fillRect(cx + 8, cy - 8, 6, 6);
  g.beginPath();
  if (name.endsWith('-talk')) g.ellipse(cx, cy + 13, 6, 5, 0, 0, Math.PI * 2);
  else g.rect(cx - 7, cy + 12, 14, 2);
  g.fill();
  if (!full) {
    g.fillStyle = '#fff';
    g.font = '10px sans-serif';
    g.textAlign = 'center';
    g.fillText(emotion, cx, 92);
  }
  return canvas.toDataURL('image/png').split(',')[1];
}

const MOCK_STILL_NAMES = [...MOCK_EMOTIONS.flatMap((e) => [e, `${e}-talk`]), 'full'];

export async function install(scenario: string) {
  const params = new URLSearchParams(location.search);
  document.body.style.background =
    'radial-gradient(760px 520px at 16% 108%,#51308f 0,transparent 62%),radial-gradient(700px 520px at 88% 104%,#b0437f 0,transparent 60%),radial-gradient(900px 500px at 60% -10%,#16436e 0,transparent 62%),linear-gradient(#0c0f1c,#131022)';
  let state = 'idle';
  let lang = params.get('lang') === 'en' ? 'en' : 'ru';
  // Before the first-run screen is passed the name follows the language.
  let name = lang === 'en' && params.has('welcome') ? 'Mika' : 'Мика';
  let persona = 'caring';
  let inIsland = !params.has('narrow');
  let onDesktop = false;
  const hasModel = params.has('model') || params.has('vrm');
  const avatarModels = hasModel
    ? [{ id: MOCK_MODEL_ID, title: 'Пробная модель', author: 'Автор модели', license: mockProfile('').license, spec: '1.0', size_bytes: 12_400_000, has_stills: !params.has('nostills') }]
    : [];
  const avatarProfiles = new Map(avatarModels.map((m) => [m.id, mockProfile(m.title)]));
  const avatarStills = new Map<string, { name: string; png_base64: string }[]>();
  let activeModel = hasModel ? MOCK_MODEL_ID : '';
  const modelChanged = () => emit('mika://ui', { action: 'avatar_model_changed' });
  let look = { pose: params.get('pose') ?? 'relaxed', scale: 1, frame: params.get('frame') ?? 'full', follow_cursor: true, idle_actions: true, idle_motion: '' };
  // `&window` starts the preview as an ordinary window.
  let windowed = params.has('window');
  const ffmpeg = {
    downloaded: params.has('ffmpeg_dl'),
    inPath: params.has('ffmpeg'),
    setting: params.has('ffmpeg_bad') ? String.raw`D:\Tools\ffmpeg` : '',
    downloading: false,
  };
  const research = { folder: '', model: '', searches: 5, pages: 8, minutes: 5, tokens: params.has('research_high') ? 150_000 : 50_000 };
  const researchView = () => ({
    folder: research.folder,
    folder_shown: research.folder || RESEARCH_FOLDER,
    folder_default: RESEARCH_FOLDER,
    model: research.model,
    model_shown: research.model || RESEARCH_MODEL,
    max_searches: research.searches,
    max_pages: research.pages,
    max_minutes: research.minutes,
    max_tokens: research.tokens,
    llm_calls: research.pages + 2,
    high_tokens: research.tokens > 100_000,
    high_folder: /^c:\\(windows|program files)/i.test(research.folder),
  });
  const ffmpegView = () => {
    const source = ffmpeg.inPath ? 'path' : ffmpeg.downloaded ? 'downloaded' : null;
    return {
      found: source !== null,
      source,
      path:
        source === 'path'
          ? String.raw`C:\Tools\ffmpeg\bin\ffmpeg.exe`
          : source === 'downloaded'
            ? String.raw`C:\Users\me\AppData\Local\Mika\ffmpeg\ffmpeg.exe`
            : '',
      version: source === 'path' ? '7.1-full_build-www.gyan.dev' : source === 'downloaded' ? 'n9.0.2-39-g7adfbe40dc-20261031' : '',
      gpl: source !== null && params.has('ffmpeg_gpl'),
      setting: ffmpeg.setting,
      setting_invalid: ffmpeg.setting !== '',
      downloaded: ffmpeg.downloaded,
      downloading: ffmpeg.downloading,
      bytes: ffmpeg.downloaded ? 205_000_000 : 0,
      download_mb: 161,
    };
  };
  const evening = { enabled: params.has('evening'), time: '21:00' };
  const costs = {
    enabled: true,
    empty: params.has('costs_empty'),
    dayLimit: params.has('costs_empty') ? 0 : 100,
    monthLimit: params.has('costs_empty') ? 0 : 2000,
    currency: 'RUB',
    currencySaved: false,
    pauseUnattended: true,
    manual: [] as MockPrice[],
    pricesAt: params.has('costs_empty') ? null : '14.11 09:12',
  };
  const costsView = () => {
    const over = params.has('costs_over');
    const local = params.has('costs_free');
    const ollama = (requests: number, units: string, blind: number) =>
      usageLine('chat', 'qwen3:8b', requests, units, null, { provider: 'localhost:11434', free: true, blind, price: 'none' });
    const today = costs.empty
      ? []
      : local
        ? [ollama(64, '212 тыс. токенов', 1)]
        : over
        ? [
            usageLine('chat', 'google/gemma-4-26b-a4b-it', 612, '2,4 млн токенов', 91.4),
            usageLine('speech', 'fish-audio/drama-3', 150, '10,8 тыс. знаков', 11.6),
            usageLine('transcription', 'qwen/qwen3-asr-0.6b', 31, '24 мин 40 с', 1.2),
          ]
        : [
            usageLine('chat', 'google/gemma-4-26b-a4b-it', 84, '310 тыс. токенов', 9.1),
            usageLine('speech', 'fish-audio/drama-3', 41, '2,9 тыс. знаков', 3.1),
            usageLine('transcription', 'qwen/qwen3-asr-0.6b', 6, '4 мин 10 с', 0.2),
          ];
    const month = costs.empty
      ? []
      : local
        ? [ollama(1480, '5,1 млн токенов', 3)]
        : over
        ? [
            usageLine('chat', 'google/gemma-4-26b-a4b-it', 9480, '37,2 млн токенов', 1398),
            usageLine('speech', 'fish-audio/drama-3', 2540, '183 тыс. знаков', 196.4),
            usageLine('transcription', 'qwen/qwen3-asr-0.6b', 610, '8 ч 05 мин', 24.24),
            usageLine('image', 'openai/gpt-image-1', 5, '5 шт.', 21.36),
          ]
        : [
            usageLine('chat', 'google/gemma-4-26b-a4b-it', 1955, '7,2 млн токенов', 211.82),
            usageLine('speech', 'fish-audio/drama-3', 954, '67,5 тыс. знаков', 72.16),
            usageLine('image', 'openai/gpt-image-1', 5, '5 шт.', 21.36),
            usageLine('transcription', 'qwen/qwen3-asr-0.6b', 140, '1 ч 37 мин', 4.66),
          ];
    if (params.has('costs_noprice') && !costs.empty) {
      for (const lines of [today, month]) {
        lines.push(
          usageLine('vision', 'openai/gpt-4o', 9, '148 тыс. токенов', null, {
            provider: 'api.openai.com',
            cost: [{ currency: 'USD', cost: 0.42, text: moneyText(0.42, 'USD') }],
          }),
          usageLine('transcription', 'whisper-1', 3, '3 мин 05 с', null, { provider: 'api.openai.com', unpriced: 3, seconds: 185, price: 'none' }),
          usageLine('chat', 'mistralai/mistral-nemo', 2, 'токены не сообщены', null, { blind: 2 }),
        );
      }
    }
    // A price of the user's own: the calls that waited for it get their money, as after a repricing.
    for (const l of [...today, ...month]) {
      const own = costs.manual.find((p) => p.provider === l.provider && p.model === l.model);
      if (!own) continue;
      l.price = 'manual';
      if (l.unpriced > 0 && own.per_minute > 0) {
        const cost = Math.round((l.seconds / 60) * own.per_minute * 100) / 100;
        l.cost = [{ currency: own.currency, cost, text: moneyText(cost, own.currency) }];
        l.unpriced = 0;
      }
    }
    const day = '2026-11-14';
    const reports = { today: costReport(today, day, day, costs.currency), month: costReport(month, '2026-11-01', day, costs.currency) };
    // Only money in the main currency counts against the limits.
    const spent = (r: { totals: { currency: string; cost: number }[] }) => r.totals.find((m) => m.currency === costs.currency)?.cost ?? 0;
    const share = (value: number, limit: number) => (limit > 0 ? Math.round((value / limit) * 100) : 0);
    const text = (value: number, limit: number) =>
      limit > 0 ? `${moneyText(value, costs.currency)} из ${moneyText(limit, costs.currency)}` : moneyText(value, costs.currency);
    const [daySpent, monthSpent] = [spent(reports.today), spent(reports.month)];
    return {
      enabled: costs.enabled,
      currency: costs.currency,
      currency_saved: costs.currencySaved,
      currency_assumed: false,
      day_limit: costs.dayLimit,
      month_limit: costs.monthLimit,
      pause_unattended: costs.pauseUnattended,
      // The month goes before the day: it holds longer.
      paused:
        costs.enabled && costs.pauseUnattended
          ? costs.monthLimit > 0 && monthSpent >= costs.monthLimit
            ? 'month'
            : costs.dayLimit > 0 && daySpent >= costs.dayLimit
              ? 'day'
              : null
          : null,
      budget: {
        currency: costs.currency,
        day_spent: daySpent,
        day_limit: costs.dayLimit,
        day_share: share(daySpent, costs.dayLimit),
        month_spent: monthSpent,
        month_limit: costs.monthLimit,
        month_share: share(monthSpent, costs.monthLimit),
        day_text: text(daySpent, costs.dayLimit),
        month_text: text(monthSpent, costs.monthLimit),
      },
      ...reports,
      manual: costs.manual,
      prices_at: costs.pricesAt,
    };
  };
  const tg = { voice_in: params.has('tgvoice') || params.has('tgvoice_in'), voice_reply: params.has('tgvoice') };
  const tgView = () => ({
    enabled: true,
    user_id: 123456789,
    has_token: true,
    status: 'on',
    voice_in: tg.voice_in,
    voice_reply: tg.voice_reply,
    stt_ready: !params.has('nostt'),
    tts_ready: params.has('cloud') || params.has('drama'),
    ffmpeg_found: ffmpeg.inPath || ffmpeg.downloaded,
    voice_max_s: 120,
  });
  const hk = { custom: {} as Record<string, string>, dictation: params.has('dictation'), screenAsk: params.has('screen_ask') };
  const HOTKEY_DEFAULTS: [string, string][] = [
    ['chat', 'Ctrl+Alt+M'],
    ['listen', 'Ctrl+Alt+K'],
    ['selection', 'Ctrl+Alt+X'],
    ['dictation', 'Ctrl+Alt+D'],
    ['screen_ask', 'Ctrl+Alt+Q'],
  ];
  const hotkeyStatus = (id: string) => {
    // With `&needs` the master switches «Микрофон» and «Экран» count: a hotkey waits for its own.
    const waits = (master: string) => params.has('needs') && !params.has(master);
    if (id === 'dictation') return !hk.dictation ? 'off' : waits('mic') ? 'needs' : params.has('hk_taken') ? 'taken' : 'on';
    if (id === 'screen_ask') return !hk.screenAsk ? 'off' : waits('screen') ? 'needs' : 'on';
    return 'on';
  };
  const hotkeysView = () => ({
    hotkeys: HOTKEY_DEFAULTS.map(([id, combo]) => ({ id, combo: hk.custom[id] ?? combo, custom: id in hk.custom, status: hotkeyStatus(id) })),
    dictation: hk.dictation,
    screen_ask: hk.screenAsk,
    microphone: params.has('mic'),
    screen: params.has('screen'),
    stt_model: !params.has('nostt'),
  });
  const SCREEN_ASK_WINDOW = { app: 'excel.exe', title: 'Отчёт за октябрь.xlsx — Excel', width: 1600, height: 900 };
  // A fake window: a title bar and a table with one red cell.
  const SCREEN_ASK_PICTURE =
    '<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180"><rect width="320" height="180" fill="#f4f5f7"/>' +
    '<rect width="320" height="22" fill="#1d6f42"/><circle cx="304" cy="11" r="4" fill="#fff"/>' +
    [0, 1, 2, 3, 4]
      .flatMap((r) =>
        [0, 1, 2, 3].map((c) => {
          const fill = r === 0 ? '#d9e2ec' : r === 3 && c === 2 ? '#f28b82' : '#fff';
          return `<rect x="${10 + c * 76}" y="${34 + r * 28}" width="72" height="24" fill="${fill}" stroke="#c5cbd3"/>`;
        }),
      )
      .join('') +
    '</svg>';
  /** The question about the picture reaches the chat, then her answer. */
  const screenAskReply = async (question: string) => {
    send({ type: 'user_text', text: `Вопрос про окно программы ${SCREEN_ASK_WINDOW.app}: ${question.trim() || 'объясни, что на экране'}` });
    setState('thinking');
    await wait(900);
    await say(
      'Это таблица Excel с отчётом за октябрь: продажи по неделям и итог внизу. Красная ячейка в третьей строке значит, что неделя не добрала до плана. Остальные недели в норме, итог за месяц выше плана.',
    );
    setState('idle');
  };
  // Scenarios by event and what she is watching for, as the extensions page gets them.
  const nowS = Math.floor(Date.now() / 1000);
  const today = new Date().toISOString().slice(0, 10);
  const events = {
    enabled: !params.has('events_off'),
    max_per_hour: 12,
    triggers: params.has('noevents')
      ? []
      : [
          {
            id: 3,
            name: 'Наушники',
            event: 'audio_connected',
            filter: 'sony',
            idle_min: 0,
            action: { kind: 'command', text: 'музыка' },
            enabled: true,
            cooldown_s: 60,
            max_per_day: 20,
            fired_at: nowS - 1500,
            fired_day: today,
            fired_count: 2,
            fired_today: 2,
            command_missing: false,
            command_changed: false,
            command_text: 'Включи плеер и поставь громкость на 30.\nЕсли плеер уже играет, ничего не трогай.',
          },
          {
            id: 4,
            name: 'После игры',
            event: 'app_closed',
            filter: 'game.exe',
            idle_min: 0,
            action: { kind: 'instruction', text: 'Верни громкость на 40 и включи уведомления' },
            enabled: false,
            cooldown_s: 300,
            max_per_day: 20,
            fired_at: 0,
            fired_day: '',
            fired_count: 0,
            fired_today: 0,
            command_missing: false,
            command_changed: false,
          },
          {
            id: 5,
            name: 'Вернулся',
            event: 'user_back',
            filter: '',
            idle_min: 15,
            action: { kind: 'command', text: 'сводка' },
            enabled: true,
            cooldown_s: 7200,
            max_per_day: 5,
            fired_at: 0,
            fired_day: '',
            fired_count: 0,
            fired_today: 0,
            command_missing: true,
            command_changed: false,
          },
        ],
    waits: params.has('noevents')
      ? []
      : [
          { id: 1, kind: 'file_ready', what: 'ubuntu.iso', since: nowS - 600, until: nowS + 3000 },
          { id: 2, kind: 'process_exits', what: 'cargo.exe', since: nowS - 120, until: nowS + 3470 },
        ],
  };
  // Background jobs, as the job board keeps them. Every change goes to the page as `bg_job_updated`.
  const jobs = new Map<number, MockJob>();
  /** What a folder job has done so far: shown when it is stopped halfway. */
  const partial = new Map<number, { text: string; data: unknown }>();
  const putJob = (job: MockJob) => {
    jobs.set(job.id, job);
    emit('mika://event', { type: 'bg_job_updated', job });
    return job;
  };
  /** The job in its last state: the note of the work goes away, the outcome comes. */
  const endJob = (job: MockJob, state: MockJob['state'], outcome: { text?: string; data?: unknown }) => {
    const next: MockJob = { ...job, ...outcome, state };
    delete next.note;
    return putJob(next);
  };
  /** A recording ends well when it is stopped: the file is saved and she says so. */
  const saveRecording = async (id: number) => {
    const job = jobs.get(id);
    if (!job || job.state !== 'running') return;
    putJob({ ...job, state: 'stopping', note: 'сохраняю запись…' });
    await wait(600);
    endJob({ ...job, share: 100 }, 'done', {
      text: `Запись сохранена: ${REC_FILE}, 17,5 МБ, 2:15. Запись 1 раз вставала на паузу из-за окна из слепой зоны: эти места в файл не попали.`,
      data: { kind: 'av_files', files: [{ path: REC_FILE, bytes: 18_400_000, duration_s: 135 }] },
    });
    emit('mika://event', { type: 'remark', kind: 'job', text: `Готово: ${job.title}` });
  };
  // The journal: rows newest first, and what the journal commands answer with.
  const journal = {
    rows: scenario === 'journal' && params.has('journal_empty') ? [] : journalRows(),
    lastSeq: 120,
    level: 'info',
    content: false,
    keepDays: 14,
    keepFiles: 5,
    crashed: scenario === 'journal' && !params.has('journal_empty'),
  };
  const journalWrite = (level: MockRow['level'], category: string, message: string) => {
    journal.lastSeq += 1;
    journal.rows.unshift({ seq: journal.lastSeq, t: journalStamp(Date.now()), level, category, message });
  };
  const journalView = () => {
    const bytes = journal.rows.reduce((n, r) => n + r.message.length + 48, 0);
    return {
      level: journal.level,
      content: journal.content,
      keep_days: journal.keepDays,
      keep_files: journal.keepFiles,
      dir: String.raw`C:\Users\me\AppData\Local\Mika\logs`,
      // Two earlier files of five megabytes each, unless the journal was cleared.
      size_bytes: journal.rows.length > 200 ? bytes + 2 * 5_242_880 : bytes,
      files: journal.rows.length > 200 ? 3 : 1,
      oldest: journal.rows.length > 0 ? journal.rows[journal.rows.length - 1].t : null,
      crashed: journal.crashed,
    };
  };
  /** The rows a query asks for, newest first; the page mark is the place in the list. */
  const journalRead = (query: Record<string, unknown> | null) => {
    const levels = (query?.levels ?? []) as string[];
    const categories = (query?.categories ?? []) as string[];
    const text = String(query?.text ?? '').toLowerCase();
    const found = journal.rows.filter(
      (r) => (levels.length === 0 || levels.includes(r.level)) && (categories.length === 0 || categories.includes(r.category)) && r.message.toLowerCase().includes(text),
    );
    const from = query?.before ? Number(query.before) : 0;
    const limit = Math.min(500, Number(query?.limit) || 100);
    return { rows: structuredClone(found.slice(from, from + limit)), next: from + limit < found.length ? String(from + limit) : null };
  };
  // The app goes on working while the page is looked at.
  if (scenario === 'journal' && !params.has('journal_empty')) {
    let beat = 0;
    setInterval(() => {
      const [level, category, message] = JOURNAL_SAMPLES[beat % 7];
      journalWrite(level, category, message);
      beat += 1;
    }, 3000);
  }
  // The recording of scenario `rec` started before the page came up: the page learns of it from `bg_jobs_list`.
  if (scenario === 'rec') {
    jobs.set(5, { id: 5, kind: 'screen_record', title: REC_TITLE, state: 'running', live: true, share: 0, note: '0:02 из 30:00' });
  }
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case 'get_state':
          return state;
        case 'get_emotion':
          return 'neutral';
        case 'get_history':
          return [];
        case 'get_ui_settings':
          return {
            name,
            speak_replies: true,
            microphone_enabled: params.has('mic'),
            screen_enabled: false,
            web_enabled: true,
            persona,
            custom_prompt: '',
            avatar_in_island: inIsland,
            avatar_on_desktop: onDesktop,
            windowed,
            onboarded: !params.has('welcome'),
          };
        case 'get_update_info':
          return {
            current: '0.2.0-pre.1',
            manifest_url: 'https://example.com/mika-update.json',
            check_at_start: true,
            available: params.has('update') ? { version: '0.3.0', notes: 'Перетаскивание островка\nАктивационная фраза' } : null,
          };
        case 'check_update':
          return params.has('update') ? { version: '0.3.0', notes: '' } : null;
        case 'set_wake':
          return 'on';
        case 'set_window_mode':
          windowed = a.windowed === true;
          return null;
        case 'set_assistant_name':
          name = String(a.value).trim();
          emit('mika://ui', { action: 'settings_changed' });
          return null;
        case 'set_persona':
          persona = String(a.persona);
          return null;
        case 'set_avatar_placement':
          inIsland = a.inIsland === true;
          onDesktop = a.onDesktop === true;
          emit('mika://ui', { action: 'settings_changed' });
          return null;
        case 'get_pending_confirm':
          return null;
        case 'get_llm_settings':
          return {
            kind: 'openai',
            base_url: 'https://routerai.ru/api/v1',
            default_base_url: 'https://api.openai.com/v1',
            model: 'mistralai/mistral-nemo',
            has_key: !params.has('nokey'),
          };
        case 'test_provider':
          return 420;
        case 'list_rules':
          return [
            { id: 1, tool: 'open_app', pattern: 'проводник', decision: 'allow', created_at: '2026-11-10T22:00:00+03:00' },
            { id: 2, tool: 'run_shell', pattern: 'get-childitem', decision: 'allow', created_at: '2026-11-10T22:05:00+03:00' },
            { id: 3, tool: 'kill_process', pattern: 'explorer.exe', decision: 'deny', created_at: '2026-11-10T22:07:00+03:00' },
          ];
        case 'recent_audit':
          return [
            { at: '2026-11-11T16:59:00+03:00', tool: 'open_app', target: 'explorer.exe', risk: 'low', decision: 'allow_once', decided_by: 'button' },
            { at: '2026-11-11T17:02:00+03:00', tool: 'run_shell', target: 'Get-ChildItem "$env:USERPROFILE\\Downloads"', risk: 'medium', decision: 'allow_always', decided_by: 'chat' },
            { at: '2026-11-11T17:04:00+03:00', tool: 'power', target: 'restart', risk: 'high', decision: 'deny', decided_by: 'button' },
            { at: '2026-11-11T17:06:00+03:00', tool: 'delete_to_trash', target: 'C:\\Users\\me\\Downloads\\old.zip', risk: 'high', decision: 'timeout', decided_by: 'timeout' },
            { at: '2026-11-11T17:09:00+03:00', tool: 'dictation', target: '42 знаков → winword.exe', risk: 'medium', decision: 'direct', decided_by: 'hotkey' },
            { at: '2026-11-11T17:11:00+03:00', tool: 'screen_ask', target: 'окно «Отчёт за октябрь.xlsx — Excel» (excel.exe)', risk: 'medium', decision: 'direct', decided_by: 'hotkey' },
          ];
        case 'get_general_settings':
          return {
            version: '0.2.0-pre.1',
            autostart: false,
            shortcut: 'Ctrl+Alt+M',
            selection_shortcut: 'Ctrl+Alt+X',
            data_dir: String.raw`C:\Users\me\AppData\Roaming\Mika`,
            logs_dir: String.raw`C:\Users\me\AppData\Local\Mika\logs`,
          };
        case 'get_language':
          return lang;
        case 'set_language':
          if (a.value !== 'ru' && a.value !== 'en') throw 'No such interface language: ru or en';
          lang = a.value;
          if (params.has('welcome')) name = lang === 'en' ? 'Mika' : 'Мика';
          emit('mika://ui', { action: 'language_changed', lang });
          emit('mika://ui', { action: 'settings_changed' });
          return null;
        case 'get_tool_settings':
          return {
            web_enabled: true,
            screen_enabled: false,
            news_feed: 'https://lenta.ru/rss/news',
            groups: ['system', 'apps', 'files', 'shell', 'web', 'notes', 'daily', 'memory', 'media', 'av', 'windows', 'screen', 'images', 'commands', 'events', 'rdp'].map(
              (id) => ({ id, enabled: id !== 'rdp' }),
            ),
            tool_count: 70,
            notifications: { enabled: true, only_messengers: true, read_aloud: true },
            briefing_enabled: false,
            briefing_time: '08:30',
          };
        case 'get_ffmpeg_status':
          return ffmpegView();
        case 'set_ffmpeg_path': {
          ffmpeg.setting = String(a.value ?? '').trim();
          // Any non-empty path counts as one without ffmpeg.exe, unless it is the one from PATH.
          if (ffmpeg.setting === String.raw`C:\Tools\ffmpeg`) {
            ffmpeg.setting = '';
            ffmpeg.inPath = true;
          }
          return ffmpegView();
        }
        case 'install_ffmpeg':
          return (async () => {
            ffmpeg.downloading = true;
            for (const share of [0, 12, 31, 55, 78, 96, 100]) {
              await emit('ffmpeg-progress', share);
              await wait(350);
            }
            ffmpeg.downloading = false;
            if (params.has('ffmpeg_fail')) throw 'Контрольная сумма скачанного файла не совпала, файл удалён';
            ffmpeg.downloaded = true;
            return ffmpegView();
          })();
        case 'remove_ffmpeg':
          ffmpeg.downloaded = false;
          return ffmpegView();
        case 'open_av_file':
          return null;
        case 'get_evening_summary':
          return { ...evening };
        case 'set_evening_summary':
          evening.enabled = a.enabled === true;
          evening.time = String(a.time ?? '');
          return { ...evening };
        case 'evening_summary_now':
          return (async () => {
            await wait(600);
            await emit('mika://event', EVENING_REMARK);
            return null;
          })();
        case 'get_costs':
          return costsView();
        case 'set_costs_enabled':
          costs.enabled = a.value === true;
          return costsView();
        case 'set_cost_pause':
          costs.pauseUnattended = a.value === true;
          return costsView();
        case 'set_cost_budget':
          costs.dayLimit = Number(a.dayLimit) || 0;
          costs.monthLimit = Number(a.monthLimit) || 0;
          costs.currency = String(a.currency);
          costs.currencySaved = true;
          return costsView();
        case 'set_cost_price': {
          const price = a.price as MockPrice;
          if (![price.input_mtok, price.output_mtok, price.per_kchar, price.per_minute, price.per_image].some((n) => n > 0)) {
            throw 'Укажи хотя бы одну цену больше нуля';
          }
          costs.manual = [...costs.manual.filter((p) => !(p.provider === price.provider && p.model === price.model)), price];
          return costsView();
        }
        case 'remove_cost_price':
          costs.manual = costs.manual.filter((p) => !(p.provider === a.provider && p.model === a.model));
          return costsView();
        case 'refresh_cost_prices':
          return (async () => {
            await wait(500);
            const now = new Date();
            const two = (n: number) => String(n).padStart(2, '0');
            costs.pricesAt = `${two(now.getDate())}.${two(now.getMonth() + 1)} ${two(now.getHours())}:${two(now.getMinutes())}`;
            return costsView();
          })();
        case 'clear_costs':
          costs.empty = true;
          return costsView();
        case 'bg_jobs_list':
          return [...jobs.values()].filter((j) => j.state === 'queued' || j.state === 'running' || j.state === 'stopping');
        case 'bg_job_stop': {
          const job = jobs.get(Number(a.id));
          if (job?.state === 'stopping') return null;
          if (!job || (job.state !== 'queued' && job.state !== 'running')) throw 'Этого задания уже нет';
          if (job.live) {
            saveRecording(job.id);
          } else if (job.state === 'queued') {
            // A waiting job is taken off the queue at once.
            endJob(job, 'stopped', { text: 'Остановлено.' });
          } else {
            putJob({ ...job, state: 'stopping' });
            setTimeout(() => endJob(job, 'stopped', partial.get(job.id) ?? { text: 'Остановлено.' }), 600);
          }
          return null;
        }
        case 'get_research_settings':
          return researchView();
        case 'set_research_settings': {
          const folder = String(a.folder ?? '').trim();
          if (/[*?"<>|]/.test(folder)) throw 'Папка отчётов задана неверно: нужен обычный путь к папке на этом компьютере';
          research.folder = folder;
          research.model = String(a.model ?? '').trim();
          research.searches = Number(a.maxSearches);
          research.pages = Number(a.maxPages);
          research.minutes = Number(a.maxMinutes);
          research.tokens = Number(a.maxTokens);
          return researchView();
        }
        case 'get_rhythm_settings':
          return {
            rhythm: { breaks: true, break_every_min: 50, lively: true },
            tracking: { app_time: true, clipboard: true },
            clip_count: 7,
            focus_left_s: params.has('focus') ? 1260 : null,
            calendar_enabled: true,
            remind_min: 10,
            has_calendar_url: true,
            calendar_events: 42,
            calendar_read_at: '10:45',
            calendar_error: null,
            docs_folders: [String.raw`~\Documents\Работа`, String.raw`D:\Договоры`],
            docs: { files: 128, chunks: 2310, embedded: 2310, running: false, last_run: 0, error: null, progress: null },
          };
        case 'get_island_settings':
          return { city: 'Пермь', hide_in_games: true, monitor: '', monitors: [String.raw`\\.\DISPLAY1`, String.raw`\\.\DISPLAY2`] };
        case 'idle_info':
          return {
            weather: { temp: -10, text: 'ясно' },
            reminder: params.has('timer') ? { due_at: Math.floor(Date.now() / 1000) + 581, text: 'Выключить чайник' } : null,
            track: params.has('track') ? { title: 'Кукушка', artist: 'Кино' } : null,
          };
        case 'get_memory':
          return {
            facts: [
              { id: 1, created_at: '2026-11-10T22:00:00+03:00', text: 'Кота зовут Барсик' },
              { id: 2, created_at: '2026-11-11T09:00:00+03:00', text: 'Любит чай без сахара' },
            ],
            archived_messages: 42,
            embedding_base_url: '',
            embedding_model: params.has('embed') ? 'openai/text-embedding-3-small' : '',
            has_embedding_key: false,
            llm_base_url: 'https://routerai.ru/api/v1',
          };
        case 'test_embedding':
          return 1536;
        case 'list_commands':
          return [{ id: 1, name: 'Рабочий режим', prompt: 'Открой Блокнот, калькулятор и сайт почты' }];
        case 'list_reminders':
          return [
            { id: 1, due_at: Math.floor(Date.now() / 1000) + 581, text: 'Выключить чайник', kind: 'remind', repeat: 'none' },
            { id: 2, due_at: Math.floor(Date.now() / 1000) + 40000, text: 'Расскажи погоду и курс доллара', kind: 'task', repeat: 'daily' },
          ];
        case 'get_events':
          // A copy, as over the real bridge: the page must not hold the mock's own objects.
          return structuredClone(events);
        case 'set_events_enabled':
          events.enabled = a.value === true;
          return null;
        case 'set_trigger_enabled': {
          const found = events.triggers.find((tr) => tr.id === a.id);
          if (found) found.enabled = a.enabled === true;
          return found !== undefined;
        }
        case 'delete_trigger': {
          const before = events.triggers.length;
          events.triggers = events.triggers.filter((tr) => tr.id !== a.id);
          return events.triggers.length < before;
        }
        case 'stop_wait': {
          const before = events.waits.length;
          events.waits = events.waits.filter((w) => w.id !== a.id);
          return events.waits.length < before;
        }
        case 'list_mcp':
          return [
            { name: 'files', command: 'npx', args: ['-y', '@modelcontextprotocol/server-filesystem', String.raw`C:\Data`], url: '', has_token: false, enabled: true, tools: 11, error: null },
            { name: 'remote', command: '', args: [], url: 'https://mcp.example.com/mcp', has_token: true, enabled: true, tools: 4, error: null },
            { name: 'broken', command: 'nope', args: [], url: '', has_token: false, enabled: true, tools: null, error: 'сервер «broken» закрылся' },
          ];
        case 'get_telegram':
          return tgView();
        case 'set_telegram_voice':
          tg.voice_in = a.voiceIn === true;
          tg.voice_reply = a.voiceReply === true;
          return tgView();
        case 'test_telegram':
          return 'mika_home_bot';
        case 'get_media_providers':
          return { vision_model: '', image_base_url: '', image_model: '', has_image_key: false };
        case 'get_voice_settings':
          return {
            speak_replies: true,
            voice: '',
            rate: 1,
            voices: [
              { name: 'Microsoft Irina', language: 'ru-RU' },
              { name: 'Microsoft David', language: 'en-US' },
            ],
            has_russian: true,
            tts: params.has('cloud') || params.has('drama') ? 'provider' : 'windows',
            tts_model: params.has('drama') ? 'fish-audio/drama-3' : 'google/gemini-3.8-flash-lite-tts',
            tts_voice: 'Kore',
            tts_cache: true,
            tts_cache_bytes: 3_400_000,
            stt_model: 'qwen/qwen3-asr-0.6b',
            stt_language: 'ru',
            speech_base_url: '',
            has_speech_key: false,
            llm_base_url: 'https://routerai.ru/api/v1',
            microphone_enabled: true,
            microphone_found: !params.has('nomic'),
            listen_shortcut: 'Ctrl+Alt+K',
            input_device: '',
            input_devices: params.has('nomic') ? [] : ['Микрофон (USB Audio)', 'Гарнитура (Bluetooth)'],
            wake_enabled: true,
            wake_phrase: 'Мика',
            wake_status: 'on',
            vosk_installed: params.has('vosk'),
            vosk_downloading: false,
            vosk_bytes: 141_000_000,
            follow_up: true,
            duck_media: true,
            tts_style: '',
            tone_by_mood: true,
            brief_long: true,
            tts_expressive: params.has('drama'),
            expressive_model: params.has('drama'),
            mood_directions: params.has('moodvoice'),
          };
        case 'set_mood_directions':
          return null;
        case 'save_speech_settings':
          // Whether the saved model understands speech marks, as the app decides it.
          return /drama/i.test(String(a.ttsModel ?? ''));
        case 'test_microphone':
          return 'какая сегодня погода';
        case 'problem_report':
          return 'Mika 0.2.0-pre.1\nProvider: openai · https://routerai.ru/api/v1 · mistralai/mistral-nemo\n\n--- log ---\n[INFO] Mika starting';
        case 'load_picture':
          return 'data:image/svg+xml;base64,' + btoa('<svg xmlns="http://www.w3.org/2000/svg" width="320" height="200"><rect width="320" height="200" fill="#51308f"/><circle cx="160" cy="100" r="60" fill="#ff7eb6"/></svg>');
        case 'hotkeys_get':
          return hotkeysView();
        case 'hotkeys_set': {
          const combo = String(a.combo ?? '');
          if (combo === 'Ctrl+Alt+KeyM') throw 'Это сочетание уже у действия «Открыть чат»';
          if (combo === 'Shift+Win+KeyS') throw 'Сочетание Win+Shift+S занято другой программой, прежнее оставлено';
          // The app stores a tidy form: `KeyJ` becomes `J`.
          hk.custom[String(a.action)] = combo.replace(/(?:Key|Digit)(\w)$/, '$1');
          return hotkeysView();
        }
        case 'hotkeys_reset':
          delete hk.custom[String(a.action)];
          return hotkeysView();
        case 'hotkeys_pause':
          return null;
        case 'dictation_set_enabled':
          hk.dictation = a.value === true;
          return hotkeysView();
        case 'screen_ask_set_enabled':
          hk.screenAsk = a.value === true;
          return hotkeysView();
        case 'screen_ask_preview':
          return 'data:image/svg+xml;base64,' + btoa(SCREEN_ASK_PICTURE);
        case 'screen_ask_send':
          if (params.has('screen_off')) throw 'Просмотр экрана выключен. Включи переключатель «Экран» в шапке чата.';
          // The command answers at once; the reply comes by events.
          screenAskReply(String(a.question ?? ''));
          return null;
        case 'screen_ask_listen':
          return (async () => {
            setState('listening');
            await wait(1500);
            screenAskReply('что значит красная ячейка?');
            return true;
          })();
        case 'screen_ask_cancel':
          return null;
        case 'backup_export':
          return (async () => {
            // The two checks Rust makes before the save dialog opens.
            if (a.protect === true) {
              if ([...String(a.password ?? '')].length < 10) throw 'Пароль короче 10 знаков';
              if (a.password !== a.repeat) throw 'Пароли не совпадают';
            }
            if (params.has('backup_cancel')) return null;
            await wait(900);
            if (params.has('backup_fail')) throw 'Файл копии не записался: на диске нет места';
            return { path: BACKUP_FILE.path, bytes: BACKUP_FILE.bytes, encrypted: a.protect === true, keys: a.protect === true ? 5 : 0 };
          })();
        case 'backup_reveal':
          return null;
        case 'backup_import_pick': {
          if (params.has('backup_cancel')) return null;
          if (params.has('backup_newer')) throw 'Копия сделана более новой версией (формат 2). Обнови приложение и повтори';
          const plain = params.has('backup_plain');
          return { ...BACKUP_FILE, encrypted: !plain, keys: plain ? 0 : BACKUP_FILE.keys };
        }
        case 'backup_import_preview':
          return (async () => {
            await wait(900);
            const plain = params.has('backup_plain');
            if (params.has('backup_wrong') || (!plain && !a.password)) throw 'Пароль не подошёл';
            if (!plain) return BACKUP_PREVIEW;
            // A copy without a password: no keys inside, so the key of this PC would go to the address from the copy.
            return {
              ...BACKUP_PREVIEW,
              file: { ...BACKUP_FILE, encrypted: false, keys: 0 },
              categories: BACKUP_PREVIEW.categories.map((c) => (c.id === 'secrets' ? { ...c, in_file: 0, available: false, selected: false } : c)),
              secrets: [],
              addresses: BACKUP_PREVIEW.addresses.map((x) => ({ ...x, key_in_file: false })),
              // The bot token of this PC would obey the owner from the copy.
              mcp: BACKUP_PREVIEW.mcp.map((x) => ({ ...x, token_in_file: false })),
              telegram: { ...BACKUP_PREVIEW.telegram, token_in_file: false },
            };
          })();
        case 'backup_import_apply':
          return (async () => {
            await wait(900);
            const ticked = (a.categories ?? []) as string[];
            const plain = params.has('backup_plain');
            const settings = ticked.includes('settings');
            const failed = ticked.includes('secrets') && params.has('backup_keysfail') ? [{ kind: 'telegram', name: null }] : [];
            const keys = ticked.includes('secrets') && !plain ? BACKUP_RESULT.keys_written - failed.length : 0;
            // A scenario of a saved command comes back only with its command; every one that comes is off.
            const comes = BACKUP_SCENARIOS.filter((s) => s.action.kind !== 'command' || (ticked.includes('commands') ? s.command_in_file : s.command_here));
            const scenarios = ticked.includes('scenarios') ? comes.length : 0;
            return {
              ...BACKUP_RESULT,
              restored: BACKUP_RESULT.restored
                .filter((r) => ticked.includes(r.id) && !(r.id === 'secrets' && keys === 0) && !(r.id === 'scenarios' && scenarios === 0))
                .map((r) => (r.id === 'secrets' ? { ...r, count: keys } : r.id === 'scenarios' ? { ...r, count: scenarios } : r)),
              keys_written: keys,
              keys_failed: failed,
              switched_off: [...(settings ? BACKUP_RESULT.switched_off : []), ...(scenarios > 0 ? [{ id: 'scenarios', count: scenarios }] : [])],
              scenarios_without_command: ticked.includes('scenarios') ? BACKUP_SCENARIOS.length - comes.length : 0,
              docs_folders: settings ? BACKUP_RESULT.docs_folders : [],
              restart_needed: settings || ticked.includes('history') || keys > 0,
            };
          })();
        case 'backup_import_cancel':
        case 'backup_restart':
          return null;
        case 'journal_view':
          return journalView();
        case 'journal_read': {
          const query = (a.query ?? null) as Record<string, unknown> | null;
          if (params.has('journal_fail') && query?.before) throw 'The log file could not be read: access is denied';
          return journalRead(query);
        }
        case 'journal_tail': {
          const after = typeof a.after === 'number' ? a.after : 0;
          // Oldest first, as the app sends them.
          return { rows: structuredClone(journal.rows.filter((r) => r.seq !== null && r.seq > after).reverse()), last: journal.lastSeq };
        }
        case 'set_journal': {
          const s = (a.settings ?? {}) as { level?: string; content?: boolean; keep_days?: number; keep_files?: number };
          if (params.has('journal_fail')) throw 'The settings were not saved: config.toml is read-only';
          if (!Number.isInteger(s.keep_days) || Number(s.keep_days) < 1 || Number(s.keep_days) > 365) throw 'Keep days: a whole number from 1 to 365';
          journal.level = String(s.level);
          journal.content = s.content === true;
          journal.keepDays = Number(s.keep_days);
          journal.keepFiles = Number(s.keep_files);
          journalWrite('info', 'app', 'settings saved: log');
          return journalView();
        }
        case 'journal_export':
          return (async () => {
            await wait(400);
            if (params.has('journal_cancel')) return null;
            return String.raw`C:\Users\me\Documents\mika-journal-2026-11-14.txt`;
          })();
        case 'journal_clear':
          journal.rows = [];
          journalWrite('info', 'app', 'journal cleared');
          return journalView();
        case 'journal_crash_seen':
          journal.crashed = false;
          return null;
        case 'avatar_motions':
          return [{ id: 'aaaaaaaaaaaaaaaa', name: 'Победный танец', ms: 4200 }];
        case 'avatar_motion_add':
        case 'avatar_motion_rename':
        case 'avatar_motion_delete':
        case 'avatar_motion_play':
        case 'avatar_hover':
        case 'avatar_dragging':
        case 'avatar_cursor_wanted':
        case 'avatar_typing':
          return null;
        case 'avatar_look':
          return look;
        case 'avatar_set_look':
          look = { ...(args as { look: typeof look }).look };
          emit('mika://ui', { action: 'avatar_look_changed' });
          return look;
        case 'avatar_scale_step':
          look = { ...look, scale: Math.min(1.6, Math.max(0.6, Math.round((look.scale + ((args as { delta: number }).delta > 0 ? 0.1 : -0.1)) * 10) / 10)) };
          emit('mika://ui', { action: 'avatar_look_changed' });
          return look.scale;
        case 'avatar_models':
          return { active: activeModel, models: avatarModels };
        case 'avatar_model_profile': {
          const found = avatarProfiles.get(String(a.id));
          if (!found) throw 'Такой модели нет';
          return found;
        }
        case 'avatar_model_stills': {
          const id = String(a.id);
          const saved = avatarStills.get(id);
          if (saved) return saved;
          if (!avatarModels.find((m) => m.id === id)?.has_stills) return [];
          const drawn = MOCK_STILL_NAMES.map((name) => ({ name, png_base64: mockStill(name) }));
          avatarStills.set(id, drawn);
          return drawn;
        }
        case 'avatar_model_bytes':
          // `&vrm`: the file a developer put beside this module.
          if (!params.has('vrm')) throw 'Файл модели повреждён';
          // `&vrm=name` serves `src/dev/name.vrm`; a bare `&vrm=1` serves `sample.vrm`.
          const file = /^[a-z0-9_-]+$/i.test(params.get('vrm') ?? '') && params.get('vrm') !== '1' ? params.get('vrm') : 'sample';
          return fetch(`/src/dev/${file}.vrm`).then((r) => {
            if (!r.ok) throw `Нет файла frontend/src/dev/${file}.vrm`;
            return r.arrayBuffer();
          });
        case 'avatar_model_add': {
          if (avatarModels.some((m) => m.id === MOCK_MODEL_2)) return avatarModels.find((m) => m.id === MOCK_MODEL_2);
          const added = { id: MOCK_MODEL_2, title: 'Вторая модель', author: 'Другой автор', license: 'CC_BY; Everyone; Allow', spec: '0.x', size_bytes: 38_200_000, has_stills: false };
          avatarModels.push(added);
          avatarProfiles.set(added.id, mockProfile(added.title));
          return added;
        }
        case 'avatar_model_select':
          activeModel = String(a.id);
          modelChanged();
          return null;
        case 'avatar_model_delete': {
          const at = avatarModels.findIndex((m) => m.id === a.id);
          if (at < 0) throw 'Такой модели нет';
          avatarModels.splice(at, 1);
          if (activeModel === a.id) activeModel = '';
          modelChanged();
          return null;
        }
        case 'avatar_model_set_slot': {
          const found = avatarProfiles.get(String(a.id));
          if (!found) throw 'Такой модели нет';
          const emotion = String(a.emotion);
          found.slots[emotion] = typeof a.expression === 'string' ? { source: 'manual', mix: [[a.expression, 1]] } : mockAutoSlot(emotion);
          modelChanged();
          return found;
        }
        case 'avatar_model_set_lip': {
          const found = avatarProfiles.get(String(a.id));
          if (!found) throw 'Такой модели нет';
          found.lip = Math.min(1.5, Math.max(0, Number(a.lip)));
          modelChanged();
          return found;
        }
        case 'avatar_model_save_stills': {
          const id = String(a.id);
          const given = (a.stills as { name: string; png_base64: string }[]).map((s) => ({ name: s.name, png_base64: s.png_base64.split(',').pop() ?? '' }));
          const kept = new Map((avatarStills.get(id) ?? []).map((s) => [s.name, s]));
          for (const s of given) kept.set(s.name, s);
          avatarStills.set(id, MOCK_STILL_NAMES.filter((n) => kept.has(n)).map((n) => kept.get(n)!));
          const listed = avatarModels.find((m) => m.id === id);
          if (listed) listed.has_stills = kept.size >= MOCK_STILL_NAMES.length;
          modelChanged();
          return null;
        }
        case 'avatar_model_failed':
          console.info('[mock] avatar_model_failed', a.id, a.kind);
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );

  const send = (payload: unknown) => emit('mika://event', payload);
  const setState = (to: string) => {
    send({ type: 'state_changed', from: state, to });
    state = to;
  };
  const feel = (emotion: string) => send({ type: 'emotion_changed', emotion });
  const say = async (text: string) => {
    setState('speaking');
    for (const w of text.split(' ')) {
      send({ type: 'assistant_delta', text: w + ' ' });
      await wait(110);
    }
    send({ type: 'assistant_text', text });
    // The voice reads it phrase by phrase, a little behind the text.
    void (async () => {
      for (const phrase of text.split(/(?<=[.!?])\s+/)) {
        const steps = Math.max(1, Math.round((phrase.length * 65) / 20));
        const levels = Array.from({ length: steps }, (_, i) => Math.round(255 * Math.abs(Math.sin(i / 2.3)) * (i % 17 === 16 ? 0 : 1)));
        send({ type: 'voice_phrase', text: phrase, frame_ms: 20, levels });
        await wait(steps * 20);
        send({ type: 'voice_quiet' });
        await wait(250);
      }
    })();
  };
  const click = (selector: string) => (document.querySelector(selector) as HTMLElement | null)?.click();

  setTimeout(async () => {
    // The page may still be loading modules: wait until it is mounted and listening.
    while (!document.querySelector('.iw, .stage, .win')) await wait(50);
    await wait(300);
    switch (scenario) {
      case 'listen':
        setState('listening');
        break;
      case 'think':
        setState('thinking');
        feel('think');
        send({ type: 'tool_started', id: 'c1', name: 'get_weather', summary: 'Смотрю погоду…' });
        break;
      case 'speak':
        send({ type: 'user_text', text: 'Какая на улице погода?' });
        await say(REPLY);
        break;
      case 'chat':
        click('.iw');
        await wait(700);
        send({ type: 'user_text', text: 'Какая на улице погода?' });
        setState('thinking');
        feel('think');
        send({ type: 'tool_started', id: 'c1', name: 'get_weather', summary: 'Поиск погоды' });
        await wait(400);
        send({ type: 'tool_finished', id: 'c1', name: 'get_weather', ok: true, output: '', elapsed_ms: 400, data: WEATHER });
        feel('neutral');
        await say(REPLY);
        setState('idle');
        send({ type: 'user_text', text: 'Убавь громкость' });
        setState('thinking');
        send({ type: 'tool_started', id: 'c2', name: 'volume', summary: 'Громкость 40%' });
        await wait(200);
        send({ type: 'tool_finished', id: 'c2', name: 'volume', ok: true, output: '', elapsed_ms: 60, data: { kind: 'volume', level: 40, was: 60 } });
        await say('Готово, сделала **тише**. Файл `заметки.txt` не трогала.');
        feel('joy');
        setState('idle');
        break;
      case 'reminder':
        send({ type: 'reminder_fired', id: 1, text: 'Выключить чайник', kind: 'remind', late_s: 0 });
        break;
      case 'tgvoice':
        // A voice message from Telegram looks like any message; the recognition time stands under the reply.
        click('.iw');
        await wait(700);
        send({ type: 'timing', stage: 'stt', ms: 1840 });
        send({ type: 'user_text', text: 'Какая завтра погода?' });
        setState('thinking');
        feel('think');
        send({ type: 'tool_started', id: 'c1', name: 'get_weather', summary: 'Поиск погоды' });
        await wait(400);
        send({ type: 'tool_finished', id: 'c1', name: 'get_weather', ok: true, output: '', elapsed_ms: 400, data: WEATHER });
        feel('neutral');
        await say(REPLY);
        feel('joy');
        setState('idle');
        break;
      case 'selection':
        emit('mika://ui', { action: 'selection', text: 'Добрый день! Прошу прислать акт сверки за октябрь до пятницы.', app: 'outlook.exe' });
        await wait(700);
        send({ type: 'notification', app: 'Telegram', title: 'Аня Смирнова', text: 'Ты будешь на созвоне в шесть?' });
        break;
      case 'task': {
        click('.iw');
        await wait(700);
        send({ type: 'user_text', text: 'Разбери загрузки по папкам и найди в интернете, чем открыть .heic' });
        setState('thinking');
        feel('think');
        const steps = ['Посмотреть, что лежит в Загрузках', 'Разложить файлы по папкам по типу', 'Проверить, что всё на месте', 'Найти, чем открыть .heic'];
        const board = (marks: string[], finished = false, ok = false) => ({
          type: 'task_updated',
          task: { title: 'Разобрать загрузки', steps: steps.map((text, i) => ({ text, status: marks[i], ...(i === 0 && marks[0] === 'done' ? { note: '47 файлов' } : {}) })), finished, ok },
        });
        send(board(['active', 'pending', 'pending', 'pending']));
        send({ type: 'tool_started', id: 'c1', name: 'list_folder', summary: 'Посмотреть папку' });
        await wait(500);
        send({ type: 'tool_finished', id: 'c1', name: 'list_folder', ok: true, output: '', elapsed_ms: 120 });
        send(board(['done', 'active', 'pending', 'pending']));
        send({ type: 'tool_started', id: 'c2', name: 'file_batch', summary: 'Файлы пакетом: перенести 44, создать папок 5' });
        await wait(500);
        send({ type: 'tool_finished', id: 'c2', name: 'file_batch', ok: true, output: '', elapsed_ms: 900 });
        send(board(['done', 'done', 'active', 'pending']));
        if (params.has('done')) {
          send(board(['done', 'done', 'done', 'done'], true, true));
          await say('Готово: разложила 44 файла по пяти папкам, а .heic открывает приложение «Фотографии» с расширением HEIF.');
          feel('joy');
          setState('idle');
        }
        break;
      }
      case 'cards':
        click('.iw');
        await wait(700);
        send({ type: 'user_text', text: 'Нарисуй кота и напомни про чайник' });
        send({ type: 'tool_started', id: 'c1', name: 'generate_image', summary: 'Нарисовать картинку' });
        await wait(300);
        send({ type: 'tool_finished', id: 'c1', name: 'generate_image', ok: true, output: '', elapsed_ms: 8200, data: { kind: 'image', path: String.raw`C:\Users\me\Pictures\Mika\cat.png` } });
        await say('Готово, вот твой кот.');
        setState('idle');
        send({ type: 'reminder_fired', id: 1, text: 'Выключить чайник', kind: 'remind', late_s: 0 });
        break;
      case 'events':
        click('.iw');
        await wait(700);
        send({ type: 'user_text', text: 'Когда подключаю наушники — включи музыку' });
        setState('thinking');
        feel('think');
        send({ type: 'tool_started', id: 'c1', name: 'trigger_create', summary: 'Создать сценарий «Наушники»' });
        await wait(300);
        send({ type: 'tool_finished', id: 'c1', name: 'trigger_create', ok: true, output: '', elapsed_ms: 12 });
        feel('neutral');
        await say('Готово. Когда подключишь наушники, включу музыку.');
        setState('idle');
        await wait(600);
        // A scenario run opens like a message of the user, then goes on as any turn.
        send({ type: 'user_text', text: '⚡ Сценарий «Наушники»: подключено звуковое устройство' });
        setState('thinking');
        send({ type: 'tool_started', id: 'c2', name: 'media_control', summary: 'Пауза или продолжить' });
        await wait(300);
        send({ type: 'tool_finished', id: 'c2', name: 'media_control', ok: true, output: '', elapsed_ms: 40 });
        await say('Наушники на месте, включила музыку.');
        setState('idle');
        await wait(600);
        send({ type: 'user_text', text: 'Скажи, когда докачается ubuntu.iso' });
        setState('thinking');
        send({ type: 'tool_started', id: 'c3', name: 'wait_for', summary: 'Сказать, когда «ubuntu.iso» перестанет расти' });
        await wait(200);
        send({ type: 'tool_finished', id: 'c3', name: 'wait_for', ok: true, output: '', elapsed_ms: 8 });
        await say('Хорошо, слежу за файлом. Скажу, когда он докачается.');
        setState('idle');
        await wait(900);
        send({ type: 'remark', kind: 'watch', text: 'Файл «ubuntu.iso» готов: больше не растёт, 4,2 ГБ.' });
        break;
      case 'event_confirm': {
        if (params.has('inchat')) {
          click('.iw');
          await wait(700);
        }
        // `&long`: an instruction of the full 300 characters, to see that none of it is hidden.
        const long = params.has('long');
        const instruction =
          'Поставь музыку на паузу, убавь громкость до 20, сверни все окна, включи режим «Не беспокоить», закрой вкладки с видео в браузере, ' +
          'запиши в заметку «Перерыв» время, когда я отошёл, проверь, идёт ли сборка проекта, и скажи о ней, когда я вернусь, ' +
          'а потом заблокируй компьютер и ничего не трогай до утра';
        const assessment = {
          level: 'medium',
          summary: long ? 'Создать сценарий «Ушёл»' : 'Создать сценарий «Наушники»',
          target: long
            ? `Пауза 1 ч, не больше 3 раз в день. Когда: тебя нет за компьютером 15 мин. Что делать: ${instruction}.`
            : 'Пауза 1 мин, не больше 20 раз в день. Когда: подключено звуковое устройство «sony». Что делать: команда «музыка»: Открой плеер и включи мой плейлист «Работа».',
          reason: 'сценарий запускается сам, когда тебя может не быть рядом; его действия спросят разрешение, как обычно',
          scope: long
            ? 'ушел|user_away||15|instruction:поставь музыку на паузу, убавь громкость до 20, сверни все окна, включи режим «не #5d0c1f7a92b3e641|3600|3'
            : 'наушники|audio_connected|sony|0|command:музыка#1a07cb8d7f493660|60|20',
        };
        send({ type: 'user_text', text: long ? 'Когда я отхожу — приведи всё в порядок' : 'Когда подключаю наушники — включи музыку' });
        send({ type: 'tool_started', id: 'c1', name: 'trigger_create', summary: assessment.summary });
        setState('awaiting_confirm');
        send({ type: 'confirm_requested', id: 1, tool: 'trigger_create', assessment, allow_always: true, timeout_s: 60 });
        break;
      }
      case 'av': {
        click('.iw');
        await wait(700);
        send({ type: 'user_text', text: 'Сожми отпуск.mp4 до 50 МБ и расшифруй интервью' });
        setState('thinking');
        feel('think');
        send({ type: 'tool_started', id: 'c1', name: 'av_info', summary: 'Сведения о «отпуск.mp4»' });
        await wait(200);
        send({
          type: 'tool_finished',
          id: 'c1',
          name: 'av_info',
          ok: true,
          output: 'mp4, 12:31, 245 МБ, H.264 1920×1080, 30 к/с; AAC 48 кГц, стерео',
          elapsed_ms: 180,
        });
        send({ type: 'tool_started', id: 'c2', name: 'video_compress', summary: 'Сжать «отпуск.mp4» (12:31, 245 МБ) до 50 МБ → «отпуск (сжато).mp4»' });
        for (const share of [3, 12, 27, 42]) {
          await wait(250);
          send({ type: 'tool_progress', id: 'c2', share });
        }
        send({ type: 'tool_started', id: 'c3', name: 'av_transcribe', summary: 'Распознать речь из «интервью.m4a» (8:05, 7,4 МБ)' });
        await wait(300);
        send({
          type: 'tool_finished',
          id: 'c3',
          name: 'av_transcribe',
          ok: true,
          output: '',
          elapsed_ms: 41_300,
          data: {
            kind: 'av_files',
            files: [
              { path: String.raw`C:\Users\me\Music\интервью.txt`, bytes: 9_830 },
              { path: String.raw`C:\Users\me\Music\интервью.srt`, bytes: 14_250 },
            ],
          },
        });
        send({ type: 'tool_started', id: 'c5', name: 'av_convert', summary: 'Звук из «концерт.webm» (1:02:03, 412 МБ) → «концерт.mp3»' });
        await wait(200);
        send({
          type: 'tool_finished',
          id: 'c5',
          name: 'av_convert',
          ok: true,
          output: '',
          elapsed_ms: 19_800,
          data: { kind: 'av_files', files: [{ path: String.raw`C:\Users\me\Videos\концерт.mp3`, bytes: 148_900_000, duration_s: 3723.4 }] },
        });
        send({ type: 'tool_started', id: 'c4', name: 'video_gif', summary: 'GIF из «котик.mov», 5 с' });
        await wait(150);
        send({
          type: 'tool_finished',
          id: 'c4',
          name: 'video_gif',
          ok: false,
          output: 'Ошибка: FFmpeg не найден, поэтому с файлом ничего не сделано.',
          elapsed_ms: 4,
          data: { kind: 'ffmpeg_missing' },
        });
        break;
      }
      case 'day':
        click('.iw');
        await wait(700);
        send({ type: 'user_text', text: 'Как прошёл день?' });
        setState('thinking');
        feel('think');
        send({ type: 'tool_started', id: 'c1', name: 'day_summary', summary: 'Собрать итог дня' });
        await wait(300);
        send({ type: 'tool_finished', id: 'c1', name: 'day_summary', ok: true, output: '', elapsed_ms: 40, data: DAY_CARD });
        feel('neutral');
        await say('За компьютером ты сегодня шесть часов двадцать минут, больше всего в VS Code. Закрыл три дела, был на двух встречах. Завтра в 9:30 планёрка — составить план?');
        setState('idle');
        await wait(600);
        send(EVENING_REMARK);
        break;
      case 'costs':
        click('.iw');
        await wait(700);
        send({ type: 'user_text', text: 'Сколько я сегодня потратил?' });
        setState('thinking');
        feel('think');
        send({ type: 'tool_started', id: 'c1', name: 'cost_report', summary: 'Посмотреть расходы на провайдеров' });
        await wait(300);
        send({ type: 'tool_finished', id: 'c1', name: 'cost_report', ok: true, output: '', elapsed_ms: 12, data: COST_CARD });
        feel('neutral');
        await say('Сегодня ушло 12,40 ₽: почти всё на ответы модели, ещё немного на озвучку. До дневного лимита далеко.');
        setState('idle');
        await wait(600);
        send({ type: 'remark', kind: 'cost', text: 'За сегодня потрачено 80,40 ₽ из 100 ₽: это 80% дневного лимита.' });
        break;
      case 'dictation':
        if (params.has('blind')) {
          send({ type: 'remark', kind: 'dictation', text: 'Диктовка: окно впереди в слепой зоне (пароли, вход, банк, оплата). Ничего не записываю и не печатаю.' });
          break;
        }
        emit('mika://ui', { action: 'dictation', phase: 'listening', app: 'winword.exe' });
        setState('listening');
        await wait(2500);
        emit('mika://ui', { action: 'dictation', phase: 'recognising', app: 'winword.exe' });
        setState('thinking');
        await wait(1200);
        emit('mika://ui', { action: 'dictation', phase: 'typing', app: 'winword.exe' });
        await wait(400);
        emit('mika://ui', { action: 'dictation', phase: 'idle', app: '' });
        setState('idle');
        break;
      case 'screen_ask':
        // The page opens the chat itself.
        emit('mika://ui', { action: 'screen_ask', ...SCREEN_ASK_WINDOW });
        // `&gone`: the app throws the picture away after a while, the card leaves by itself.
        if (params.has('gone')) setTimeout(() => emit('mika://ui', { action: 'screen_ask', gone: true }), 3000);
        break;
      case 'bgjob': {
        click('.iw');
        await wait(700);
        send({ type: 'user_text', text: 'Сожми отпуск.mp4 до 50 МБ, а из концерта достань звук' });
        setState('thinking');
        feel('think');
        const compress = 'Сжать «отпуск.mp4» (12:31, 245 МБ) до 50 МБ → «отпуск (сжато).mp4»';
        const sound = 'Звук из «концерт.webm» (1:02:03, 412 МБ) → «концерт.mp3»';
        send({ type: 'tool_started', id: 'c1', name: 'video_compress', summary: compress });
        await wait(200);
        // As in the app: the tool answers with the job as it was submitted, and the job has moved on by then.
        const submitted = putJob({ id: 3, kind: 'video_compress', title: compress, state: 'running', live: false, share: 0 });
        putJob({ ...submitted, share: 2 });
        send({ type: 'tool_finished', id: 'c1', name: 'video_compress', ok: true, output: 'Запущено в фоне: задание №3', elapsed_ms: 40, data: { kind: 'bg_job', job: submitted } });
        send({ type: 'tool_started', id: 'c2', name: 'av_convert', summary: sound });
        await wait(200);
        const waiting = putJob({ id: 4, kind: 'av_convert', title: sound, state: 'queued', live: false, share: 0 });
        send({ type: 'tool_finished', id: 'c2', name: 'av_convert', ok: true, output: 'Запущено в фоне: задание №4', elapsed_ms: 35, data: { kind: 'bg_job', job: waiting } });
        feel('neutral');
        await say('Запустила в фоне, скажу, когда будет готово.');
        setState('idle');
        for (const share of [5, 12, 20, 29, 38, 47, 56, 65, 74, 83, 92, 100]) {
          await wait(400);
          const job = jobs.get(3);
          if (job?.state !== 'running') break;
          if (params.has('fail') && share > 38) {
            const why = 'FFmpeg завершился с ошибкой: Conversion failed! Недописанный файл удалён';
            endJob(job, 'failed', { text: why });
            send({ type: 'remark', kind: 'job', text: `Не получилось: ${compress}. ${why}` });
            break;
          }
          putJob({ ...job, share });
        }
        const first = jobs.get(3);
        if (first?.state === 'running') {
          endJob(first, 'done', {
            text: String.raw`Готово: C:\Users\me\Videos\отпуск (сжато).mp4, 48,7 МБ (было 245 МБ), 12:31, H.264 1280×720.`,
            data: { kind: 'av_files', files: [{ path: String.raw`C:\Users\me\Videos\отпуск (сжато).mp4`, bytes: 51_066_000, duration_s: 751.2 }] },
          });
          send({ type: 'remark', kind: 'job', text: `Готово: ${compress}` });
        }
        // The queue moves on: the second job starts and stays at work, so «Стоп» can be tried on it.
        const next = jobs.get(4);
        if (next?.state === 'queued') putJob({ ...next, state: 'running' });
        for (const share of [6, 15, 24, 33]) {
          await wait(400);
          const job = jobs.get(4);
          if (job?.state !== 'running') break;
          putJob({ ...job, share });
        }
        break;
      }
      case 'folder': {
        click('.iw');
        await wait(700);
        send({ type: 'user_text', text: JOB_ASKS.folderask.user });
        setState('thinking');
        feel('think');
        send({ type: 'tool_started', id: 'c1', name: 'av_folder', summary: FOLDER_TITLE });
        await wait(200);
        const submitted = putJob({ id: 4, kind: 'av_folder', title: FOLDER_TITLE, state: 'running', live: false, share: 0, note: '1 из 12: день1.mp4' });
        send({ type: 'tool_finished', id: 'c1', name: 'av_folder', ok: true, output: 'Запущено в фоне: задание №4', elapsed_ms: 60, data: { kind: 'bg_job', job: submitted } });
        feel('neutral');
        await say('Запустила в фоне: двенадцать видео, скажу, когда закончу.');
        setState('idle');
        // The seventh file fails: it is skipped and named at the end.
        const made: { path: string; bytes: number; duration_s: number }[] = [];
        for (let i = 1; i <= 12; i += 1) {
          await wait(500);
          const job = jobs.get(4);
          if (job?.state !== 'running') break;
          if (i !== 7) made.push({ path: `${FOLDER_OUT}\\день${i}.mp4`, bytes: 38_000_000 + i * 1_300_000, duration_s: 205 + i * 6 });
          partial.set(4, { text: `Готово ${made.length} из 12: результаты в ${FOLDER_OUT}.`, data: { kind: 'av_files', files: [...made] } });
          if (i < 12) putJob({ ...job, share: Math.floor((i * 100) / 12), note: `${i + 1} из 12: день${i + 1}.mp4` });
        }
        const last = jobs.get(4);
        if (last?.state === 'running') {
          endJob({ ...last, share: 100 }, 'done', {
            text: `Готово 11 из 12: результаты в ${FOLDER_OUT}. Не получилось: день7.mp4 — FFmpeg завершился с ошибкой: Invalid data found when processing input`,
            data: { kind: 'av_files', files: made },
          });
          send({ type: 'remark', kind: 'job', text: `Готово: ${FOLDER_TITLE}` });
        }
        break;
      }
      case 'rec':
        // The recording runs already (see `bg_jobs_list`): the red dot on the island, the card in the chat.
        if (params.has('inchat')) {
          click('.iw');
          await wait(700);
        }
        // Rust sends the time once a second, or why the recording stands still; the card itself counts nothing.
        for (let tick = 1, recorded = 2; jobs.get(5)?.state === 'running'; tick += 1) {
          await wait(1000);
          const job = jobs.get(5);
          if (job?.state !== 'running') break;
          // A window with a password came to the front for two seconds.
          const paused = tick === 4 || tick === 5;
          if (!paused) recorded += 1;
          if (params.has('done') && recorded > 7) {
            await saveRecording(5);
            break;
          }
          putJob({ ...job, share: Math.floor(recorded / 18), note: paused ? 'пауза: на экране окно из слепой зоны' : `${clock(recorded)} из 30:00` });
        }
        break;
      case 'folderask':
      case 'recask': {
        if (params.has('inchat')) {
          click('.iw');
          await wait(700);
        }
        const ask = scenario === 'recask' && params.has('ofwindow') ? REC_WINDOW_ASK : JOB_ASKS[scenario];
        send({ type: 'user_text', text: ask.user });
        send({ type: 'tool_started', id: 'c1', name: ask.tool, summary: ask.assessment.summary });
        setState('awaiting_confirm');
        if (ask.assessment.level === 'high') feel('worry');
        send({ type: 'confirm_requested', id: 1, tool: ask.tool, assessment: ask.assessment, allow_always: ask.assessment.level !== 'high', timeout_s: 60 });
        break;
      }
      case 'research': {
        click('.iw');
        await wait(700);
        send({ type: 'user_text', text: `Разберись, ${RESEARCH_QUESTION.charAt(0).toLowerCase()}${RESEARCH_QUESTION.slice(1)}` });
        setState('thinking');
        feel('think');
        send({ type: 'tool_started', id: 'r1', name: 'deep_search', summary: `Глубокий поиск: «${RESEARCH_QUESTION}»` });
        for (const share of [5, 13, 25, 40, 62, 85, 96]) {
          await wait(350);
          send({ type: 'tool_progress', id: 'r1', share });
        }
        // `&fail` is read only here.
        if (params.has('fail')) {
          send({
            type: 'tool_finished',
            id: 'r1',
            name: 'deep_search',
            ok: false,
            output: 'Ошибка: поиск ничего не нашёл: поисковик не вернул результатов',
            elapsed_ms: 9100,
          });
          await say('Поиск ничего не вернул: похоже, поисковик просит проверку. Попробуем чуть позже?');
          feel('worry');
          setState('idle');
          break;
        }
        const report = params.has('partial') ? { ...RESEARCH_REPORT, limits: ['time'], sources: 3 } : RESEARCH_REPORT;
        send({ type: 'tool_finished', id: 'r1', name: 'deep_search', ok: true, output: '', elapsed_ms: 143000, data: report });
        await say('Готово. Коротко: для дома 120 м² «воздух-вода» дешевле в установке, а «грунт-вода» экономнее в морозы. Отчёт со ссылками лежит в «Документы\\Mika\\Research».');
        feel('joy');
        setState('idle');
        break;
      }
      case 'research_ask': {
        if (params.has('inchat')) {
          click('.iw');
          await wait(700);
        }
        const assessment = researchAssessment(params.has('high'));
        send({ type: 'user_text', text: `Разберись, ${RESEARCH_QUESTION.charAt(0).toLowerCase()}${RESEARCH_QUESTION.slice(1)}` });
        send({ type: 'tool_started', id: 'r1', name: 'deep_search', summary: assessment.summary });
        setState('awaiting_confirm');
        if (assessment.level === 'high') feel('worry');
        send({ type: 'confirm_requested', id: 1, tool: 'deep_search', assessment, allow_always: assessment.level !== 'high', timeout_s: 60 });
        break;
      }
      case 'confirm':
      case 'high':
      case 'long': {
        if (params.has('inchat')) {
          click('.iw');
          await wait(700);
        }
        const assessment = ASSESSMENTS[scenario];
        send({ type: 'user_text', text: 'Почисти загрузки' });
        send({ type: 'tool_started', id: 'c1', name: 'run_shell', summary: assessment.summary });
        setState('awaiting_confirm');
        if (assessment.level === 'high') feel('worry');
        send({ type: 'confirm_requested', id: 1, tool: 'run_shell', assessment, allow_always: assessment.level !== 'high', timeout_s: 60 });
        break;
      }
      case 'settings':
      case 'journal': {
        click('.iw');
        await wait(500);
        // `&notice`: the chat stays, with the notice of a run that was cut short.
        if (scenario === 'journal' && params.has('notice')) break;
        // The settings button: the last one of an ordinary window, the one before «close» in the island.
        click(windowed ? '.cbar .ib:last-child' : '.cbar .ib:nth-last-child(2)');
        await wait(500);
        const page =scenario === 'journal' ? 'journal' : params.get('page');
        if (page) {
          const pages = ['persona', 'providers', 'permissions', 'tools', 'memory', 'extensions', 'voice', 'island', 'hotkeys', 'costs', 'journal', 'general'];
          click(`.side nav button:nth-child(${pages.indexOf(page) + 1})`);
        }
        break;
      }
      default:
        break;
    }
  }, 400);
}
