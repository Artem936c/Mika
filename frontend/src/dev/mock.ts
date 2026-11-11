// Development-only preview without Tauri: open http://localhost:5173/?mock=chat
// Scenarios: island, listen, think, speak, chat, confirm, high, long, settings.
// `&page=providers|permissions|tools|general` picks a settings page, `&narrow` hides the avatar column,
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
            microphone_enabled: false,
            screen_enabled: false,
            web_enabled: true,
            persona,
            custom_prompt: '',
            avatar_in_island: inIsland,
            avatar_on_desktop: onDesktop,
          };
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
            version: '0.1.0-pre.1',
            autostart: false,
            shortcut: 'Ctrl+Alt+M',
            data_dir: String.raw`C:\Users\me\AppData\Roaming\Mika`,
            logs_dir: String.raw`C:\Users\me\AppData\Local\Mika\logs`,
          };
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
          const pages = ['persona', 'providers', 'permissions', 'tools', 'general'];
          click(`.side nav button:nth-child(${pages.indexOf(params.get('page')!) + 1})`);
        }
        break;
      default:
        break;
    }
  }, 400);
}
