// Development-only preview without Tauri: open http://localhost:5173/?mock=chat
// Scenarios: island, listen, think, speak, chat, cards, reminder, confirm, high, long, settings.
// `&page=providers|permissions|tools|memory|extensions|voice|island|general` picks a settings page, `&timer` shows a countdown, `&narrow` hides the avatar column,
// `?view=avatar&mock=speak` shows the desktop avatar window. Not bundled into production builds.

import { mockIPC } from '@tauri-apps/api/mocks';
import { emit } from '@tauri-apps/api/event';

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

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

export async function install(scenario: string) {
  const params = new URLSearchParams(location.search);
  document.body.style.background =
    'radial-gradient(760px 520px at 16% 108%,#51308f 0,transparent 62%),radial-gradient(700px 520px at 88% 104%,#b0437f 0,transparent 60%),radial-gradient(900px 500px at 60% -10%,#16436e 0,transparent 62%),linear-gradient(#0c0f1c,#131022)';
  let state = 'idle';
  let name = 'Мика';
  let persona = 'caring';
  let inIsland = !params.has('narrow');
  let onDesktop = false;
  // `&window` starts the preview as an ordinary window.
  let windowed = params.has('window');
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
          ];
        case 'get_general_settings':
          return {
            version: '0.2.0-pre.1',
            autostart: false,
            shortcut: 'Ctrl+Alt+M',
            data_dir: String.raw`C:\Users\me\AppData\Roaming\Mika`,
            logs_dir: String.raw`C:\Users\me\AppData\Local\Mika\logs`,
          };
        case 'get_tool_settings':
          return {
            web_enabled: true,
            screen_enabled: false,
            news_feed: 'https://lenta.ru/rss/news',
            groups: ['system', 'apps', 'files', 'shell', 'web', 'notes', 'daily', 'memory', 'media', 'windows', 'screen', 'images', 'commands', 'rdp'].map(
              (id) => ({ id, enabled: id !== 'rdp' }),
            ),
            tool_count: 58,
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
        case 'list_mcp':
          return [
            { name: 'files', command: 'npx', args: ['-y', '@modelcontextprotocol/server-filesystem', String.raw`C:\Data`], url: '', has_token: false, enabled: true, tools: 11, error: null },
            { name: 'remote', command: '', args: [], url: 'https://mcp.example.com/mcp', has_token: true, enabled: true, tools: 4, error: null },
            { name: 'broken', command: 'nope', args: [], url: '', has_token: false, enabled: true, tools: null, error: 'сервер «broken» закрылся' },
          ];
        case 'get_telegram':
          return { enabled: true, user_id: 123456789, has_token: true, status: 'on' };
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
            tts: params.has('cloud') ? 'provider' : 'windows',
            tts_model: 'google/gemini-3.8-flash-lite-tts',
            tts_voice: 'Kore',
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
          };
        case 'test_microphone':
          return 'какая сегодня погода';
        case 'problem_report':
          return 'Mika 0.2.0-pre.1\nProvider: openai · https://routerai.ru/api/v1 · mistralai/mistral-nemo\n\n--- log ---\n[INFO] Mika starting';
        case 'load_picture':
          return 'data:image/svg+xml;base64,' + btoa('<svg xmlns="http://www.w3.org/2000/svg" width="320" height="200"><rect width="320" height="200" fill="#51308f"/><circle cx="160" cy="100" r="60" fill="#ff7eb6"/></svg>');
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
  };
  const click = (selector: string) => (document.querySelector(selector) as HTMLElement | null)?.click();

  setTimeout(async () => {
    // The page may still be loading modules: wait until it is mounted and listening.
    while (!document.querySelector('.iw, .stage')) await wait(50);
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
        click('.iw');
        await wait(500);
        click('.cbar .ib:nth-last-child(2)');
        await wait(500);
        if (params.get('page')) {
          const pages = ['persona', 'providers', 'permissions', 'tools', 'memory', 'extensions', 'voice', 'island', 'general'];
          click(`.side nav button:nth-child(${pages.indexOf(params.get('page')!) + 1})`);
        }
        break;
      default:
        break;
    }
  }, 400);
}
