import { describe, expect, it } from 'vitest';
import { showsWhole } from './confirm';
import { actionLabel, minutesLeft, pauseLabel, waitLabel, whenLabel } from './events';
import { isMessageKey } from './i18n';
import { CONFIRM_LINE_PX, CONFIRM_MAX_LINES, ISLAND_SHAPES, confirmShape, targetLines } from './island';

const EVENTS = [
  'audio_connected',
  'audio_disconnected',
  'app_started',
  'app_closed',
  'rdp_connected',
  'rdp_disconnected',
  'session_locked',
  'session_unlocked',
  'user_away',
  'user_back',
];

describe('whenLabel', () => {
  it('has a text for each of the ten events', () => {
    for (const event of EVENTS) {
      expect(isMessageKey(`events.when.${event}`), event).toBe(true);
      const label = whenLabel({ event, filter: '', idle_min: 10 });
      expect(label, event).not.toBe(event);
      expect(label, event).not.toContain('{');
    }
  });
  it('puts the filter in quotes and leaves them out without one', () => {
    expect(whenLabel({ event: 'audio_connected', filter: 'sony', idle_min: 0 })).toBe('подключено звуковое устройство «sony»');
    expect(whenLabel({ event: 'audio_connected', filter: '', idle_min: 0 })).toBe('подключено звуковое устройство');
    expect(whenLabel({ event: 'app_closed', filter: 'game.exe', idle_min: 0 })).toBe('закрыта программа «game.exe»');
  });
  it('names the minutes of an absence', () => {
    expect(whenLabel({ event: 'user_away', filter: '', idle_min: 10 })).toBe('тебя нет 10 мин');
    expect(whenLabel({ event: 'user_back', filter: '', idle_min: 15 })).toBe('ты снова здесь после 15 мин');
  });
  it('shows the id of an event it does not know', () => {
    expect(whenLabel({ event: 'usb_plugged', filter: 'x', idle_min: 0 })).toBe('usb_plugged');
  });
});

describe('actionLabel', () => {
  it('tells a saved command from an instruction', () => {
    expect(actionLabel({ kind: 'command', text: 'музыка' })).toBe('команда «музыка»');
    expect(actionLabel({ kind: 'instruction', text: 'Включи музыку' })).toBe('поручение: Включи музыку');
  });
});

describe('pauseLabel', () => {
  it('picks seconds, minutes or hours and keeps whole numbers', () => {
    expect(pauseLabel(10)).toBe('10 с');
    expect(pauseLabel(60)).toBe('60 с');
    expect(pauseLabel(119)).toBe('119 с');
    expect(pauseLabel(120)).toBe('2 мин');
    expect(pauseLabel(300)).toBe('5 мин');
    expect(pauseLabel(5400)).toBe('90 мин');
    expect(pauseLabel(7200)).toBe('2 ч');
    expect(pauseLabel(86_400)).toBe('24 ч');
  });
});

describe('waitLabel', () => {
  const wait = (kind: string, what: string) => ({ id: 1, kind, what, since: 0, until: 60 });

  it('has a text for each of the five kinds', () => {
    expect(waitLabel(wait('file_appears', 'отчёт.pdf'))).toBe('появится «отчёт.pdf»');
    expect(waitLabel(wait('file_ready', 'ubuntu.iso'))).toBe('«ubuntu.iso» перестанет расти');
    expect(waitLabel(wait('process_exits', 'cargo.exe'))).toBe('завершится процесс cargo.exe');
    expect(waitLabel(wait('window_text', 'setup.exe: Готово'))).toBe('в окне появится: setup.exe: Готово');
    expect(waitLabel(wait('window_text_gone', 'setup.exe: Установка'))).toBe('из окна пропадёт: setup.exe: Установка');
  });
  it('shows only the subject of a kind it does not know', () => {
    expect(waitLabel(wait('moon_rises', 'луна'))).toBe('луна');
  });
});

describe('minutesLeft', () => {
  it('rounds up and stops at zero', () => {
    expect(minutesLeft(1000, 1000_000 - 61_000)).toBe(2);
    expect(minutesLeft(1000, 1000_000 - 60_000)).toBe(1);
    expect(minutesLeft(1000, 1000_000 - 1)).toBe(1);
    expect(minutesLeft(1000, 1000_000)).toBe(0);
    expect(minutesLeft(1000, 2000_000)).toBe(0);
  });
});

describe('the question before a scenario is saved', () => {
  const head = 'Пауза 1 ч, не больше 3 раз в день. Когда: тебя нет за компьютером 15 мин. Что делать: ';
  const base = ISLAND_SHAPES.awaiting_confirm;

  it('is shown whole only for a scenario by event', () => {
    expect(showsWhole('trigger_create')).toBe(true);
    for (const tool of ['run_command', 'wait_for', 'trigger_delete', 'command_save']) {
      expect(showsWhole(tool), tool).toBe(false);
    }
    expect(isMessageKey('events.command_changed')).toBe(true);
  });

  it('keeps the usual island for a one-line scenario and for every other question', () => {
    expect(confirmShape('Пауза 1 мин, не больше 20 раз в день.', true)).toEqual(base);
    expect(confirmShape('я'.repeat(2000), false)).toEqual(base);
  });

  it('gives an instruction of 300 characters a line for every part of it', () => {
    const target = `${head}${'я'.repeat(300)}.`;
    const lines = targetLines(target);
    expect(lines).toBeGreaterThan(3);
    expect(lines).toBeLessThanOrEqual(CONFIRM_MAX_LINES);
    const shape = confirmShape(target, true);
    // The island is taller by the lines the usual one has no room for.
    expect(shape.height).toBeGreaterThan(base.height + (lines - 3) * CONFIRM_LINE_PX);
    expect({ width: shape.width, radius: shape.radius }).toEqual({ width: base.width, radius: base.radius });
    // One more line of text makes it one line taller.
    const longer = confirmShape(`${target}${'я'.repeat(62)}`, true);
    expect(longer.height - shape.height).toBeGreaterThanOrEqual(Math.floor(CONFIRM_LINE_PX));
  });

  it('stops growing at the most lines the card shows without scrolling', () => {
    const most = confirmShape('я'.repeat(5000), true).height;
    expect(confirmShape('я'.repeat(9000), true).height).toBe(most);
    expect(most).toBeGreaterThan(base.height + (CONFIRM_MAX_LINES - 3) * CONFIRM_LINE_PX);
    // The island still fits the canvas the chat uses.
    expect(most).toBeLessThan(500);
  });
});
