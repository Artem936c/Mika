// Development-only preview without Tauri: open http://localhost:5173/?mock=chat
// Scenarios: island, chat, confirm, high. Not bundled into production builds.

import { mockIPC } from '@tauri-apps/api/mocks';
import { emit } from '@tauri-apps/api/event';

const assessment = (level: string) => ({
  level,
  summary: level === 'high' ? 'Перезагрузить компьютер' : 'Запустить программу',
  target: level === 'high' ? 'shutdown /r /t 0' : 'notepad.exe',
  reason: level === 'high' ? 'перезагрузка закроет все программы, отменить нельзя' : 'запуск программы по имени',
  scope: level === 'high' ? '*' : 'notepad',
});

export async function install(scenario: string) {
  document.body.style.background = 'radial-gradient(circle at 30% 20%, #2a2f55, #0b0c14 60%)';
  let state = 'idle';
  mockIPC(
    (cmd, args) => {
      switch (cmd) {
        case 'get_state':
          return state;
        case 'get_history':
          return scenario === 'island'
            ? []
            : [
                { role: 'user', text: 'Привет! Что умеешь?' },
                { role: 'assistant', text: 'Привет! Могу открыть программу, найти файл, выполнить команду и рассказать анекдот.' },
              ];
        case 'get_ui_settings':
          return { name: 'Мика', speak_replies: true, microphone_enabled: false, screen_enabled: false };
        case 'get_pending_confirm':
          return null;
        case 'get_llm_settings':
          return { kind: 'openai', base_url: '', default_base_url: 'https://api.openai.com/v1', model: 'gpt-4o-mini', has_key: true };
        case 'list_rules':
          return [{ id: 1, tool: 'open_app', pattern: 'notepad', decision: 'allow', created_at: '2026-11-10T22:00:00+03:00' }];
        case 'recent_audit':
          return [
            { at: '2026-11-10T22:01:00+03:00', tool: 'open_app', target: 'notepad', risk: 'medium', decision: 'allow_always', decided_by: 'button' },
            { at: '2026-11-10T22:02:00+03:00', tool: 'power', target: 'restart', risk: 'high', decision: 'deny', decided_by: 'chat' },
          ];
        case 'get_general_settings':
          return { version: '0.1.0-pre.1', autostart: false, shortcut: 'Ctrl+Alt+M', data_dir: String.raw`C:\Users\me\AppData\Roaming\Mika`, logs_dir: String.raw`C:\Users\me\AppData\Local\Mika\logs` };
        default:
          console.log('mock invoke', cmd, args);
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

  setTimeout(async () => {
    if (scenario === 'island') return;
    if (scenario === 'chat' || scenario === 'confirm' || scenario === 'high') {
      // Open the chat by clicking the island.
      (document.querySelector('.island') as HTMLElement | null)?.click();
    }
    send({ type: 'user_text', text: scenario === 'high' ? 'Перезагрузи компьютер' : 'Открой блокнот' });
    setState('thinking');
    send({ type: 'tool_started', id: 'c1', name: scenario === 'high' ? 'power' : 'open_app', summary: assessment(scenario === 'high' ? 'high' : 'medium').summary });
    if (scenario === 'chat') {
      send({ type: 'tool_finished', id: 'c1', name: 'open_app', ok: true, output: 'запущено', elapsed_ms: 212 });
      setState('speaking');
      for (const w of 'Открыла Блокнот. Что в нём написать?'.split(' ')) {
        send({ type: 'assistant_delta', text: w + ' ' });
        await new Promise((r) => setTimeout(r, 60));
      }
      send({ type: 'assistant_text', text: 'Открыла Блокнот. Что в нём написать?' });
      setState('idle');
      return;
    }
    setState('awaiting_confirm');
    const level = scenario === 'high' ? 'high' : 'medium';
    send({ type: 'confirm_requested', id: 1, tool: 'open_app', assessment: assessment(level), allow_always: level !== 'high', timeout_s: 60 });
  }, 400);
}
