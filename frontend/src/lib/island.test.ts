import { describe, expect, it } from 'vitest';
import {
  CHAT_NARROW,
  CHAT_WIDE,
  CONFIRM_LINES,
  CONFIRM_NOTE,
  confirmLines,
  ISLAND_SHAPES,
  SETTINGS_SHAPE,
  confirmShape,
  confirmTargetHeight,
  islandRect,
  noticeOf,
  foldsAway,
  opensChat,
  reducePaused,
  reduceState,
  shapeFor,
  stateLabel,
  unionRect,
  viewShape,
} from './island';
import { isMessageKey } from './i18n';

describe('islandRect', () => {
  it('centres the island on the canvas', () => {
    expect(islandRect('idle', 1000)).toEqual({ x: 382, y: 14, w: 236, h: 44 });
    expect(islandRect('awaiting_confirm', 1000)).toEqual({ x: 200, y: 14, w: 600, h: 268 });
  });
  it('falls back to idle for unknown state', () => {
    expect(islandRect('bogus', 1000)).toEqual({ x: 382, y: 14, w: 236, h: 44 });
  });
});

describe('unionRect', () => {
  it('covers both rectangles', () => {
    expect(unionRect({ x: 382, y: 14, w: 236, h: 44 }, { x: 200, y: 14, w: 600, h: 268 })).toEqual({
      x: 200,
      y: 14,
      w: 600,
      h: 268,
    });
    expect(unionRect({ x: 0, y: 0, w: 10, h: 10 }, { x: 20, y: 5, w: 10, h: 10 })).toEqual({
      x: 0,
      y: 0,
      w: 30,
      h: 15,
    });
  });
});

describe('shapeFor', () => {
  it('returns the spec sizes', () => {
    expect(shapeFor('idle')).toEqual({ width: 236, height: 44, radius: 22 });
    expect(shapeFor('listening')).toEqual({ width: 340, height: 52, radius: 26 });
    expect(shapeFor('thinking')).toEqual({ width: 380, height: 52, radius: 26 });
    expect(shapeFor('speaking')).toEqual({ width: 620, height: 78, radius: 30 });
    expect(shapeFor('awaiting_confirm')).toEqual({ width: 600, height: 268, radius: 30 });
  });
  it('falls back to idle for unknown values', () => {
    expect(shapeFor('bogus')).toEqual(ISLAND_SHAPES.idle);
    expect(shapeFor(undefined)).toEqual(ISLAND_SHAPES.idle);
  });
});

describe('stateLabel', () => {
  it('has a text for every state', () => {
    for (const state of Object.keys(ISLAND_SHAPES)) {
      expect(isMessageKey(stateLabel(state as keyof typeof ISLAND_SHAPES)), state).toBe(true);
    }
  });
});

describe('viewShape', () => {
  it('sizes match the design', () => {
    expect(viewShape('chat', 'thinking', true)).toEqual({ width: 860, height: 500, radius: 34 });
    expect(viewShape('chat', 'thinking', false)).toEqual({ width: 600, height: 500, radius: 34 });
    expect(viewShape('settings', 'idle', false)).toEqual({ width: 860, height: 500, radius: 34 });
    expect([CHAT_WIDE, CHAT_NARROW, SETTINGS_SHAPE].every((s) => s.height === 500)).toBe(true);
  });
  it('island follows the assistant state', () => {
    expect(viewShape('island', 'speaking', true)).toEqual(ISLAND_SHAPES.speaking);
  });
  it('every shape fits the 1000 x 600 canvas below the top margin', () => {
    for (const s of [...Object.values(ISLAND_SHAPES), CHAT_WIDE, SETTINGS_SHAPE]) {
      expect(s.width).toBeLessThanOrEqual(1000 - 2 * 20);
      expect(14 + s.height).toBeLessThanOrEqual(600 - 20);
    }
  });
});

describe('confirmShape', () => {
  const recording = ['Микрофон: записывается', 'Звук компьютера: записывается', 'Длина: до 30 минут', 'Экран: монитор 1, 2560×1440', 'Файл: C:\\v\\a.mp4'];

  it('a target of one or two lines keeps the size from the design', () => {
    expect(confirmShape('Get-ChildItem')).toBe(ISLAND_SHAPES.awaiting_confirm);
    expect(confirmShape('a\nb')).toBe(ISLAND_SHAPES.awaiting_confirm);
    // A long command is one line that wraps: the card scrolls it, as before.
    expect(confirmShape('x'.repeat(4000))).toBe(ISLAND_SHAPES.awaiting_confirm);
    expect(confirmLines('x'.repeat(4000))).toBe(1);
  });

  it('grows by a line for every line of the target from the third', () => {
    expect(confirmShape(recording.slice(0, 3).join('\n'))).toEqual({ width: 600, height: 290, radius: 30 });
    expect(confirmShape(recording.slice(0, 4).join('\n'))).toEqual({ width: 600, height: 308, radius: 30 });
    expect(confirmShape(recording.join('\n'))).toEqual({ width: 600, height: 326, radius: 30 });
    expect(confirmLines(recording.join('\n'))).toBe(5);
  });

  it('counts the wraps of a long line: the title of a window does not hide the lines below it', () => {
    const title = 'Очень длинное название вкладки, которое страница написала сама и…';
    const window = [
      'Микрофон: записывается',
      'Звук компьютера: записывается',
      'Длина: до 30 минут',
      `Окно: «${title}» (chrome.exe), 1920×1040`,
      'Файл: C:\\Users\\me\\Videos\\Mika\\Запись экрана 2026-11-14 15-30-05.mp4',
    ].join('\n');
    // The window line takes two lines of the card, the file one.
    expect(confirmLines(window)).toBe(6);
    expect(confirmShape(window)).toEqual({ width: 600, height: 344, radius: 30 });
    // The box of the target is as tall as its lines.
    expect(confirmTargetHeight(6)).toBe(127);
    expect(confirmTargetHeight(5)).toBeLessThanOrEqual(110);
  });

  it('stops growing after eight lines and still fits the canvas', () => {
    const tall = confirmShape(Array(40).fill('строка').join('\n'));
    expect(CONFIRM_LINES).toBe(8);
    expect(tall).toEqual({ width: 600, height: 380, radius: 30 });
    expect(confirmShape(['a', 'x'.repeat(4000)].join('\n'))).toEqual(tall);
    expect(14 + tall.height).toBeLessThanOrEqual(600 - 20);
  });

  it('is taller by the line that says the description is still in Russian', () => {
    const base = ISLAND_SHAPES.awaiting_confirm;
    // Without the line nothing changes, and the usual island is the very object of the design.
    expect(confirmShape('Get-ChildItem', false, false)).toBe(base);
    expect(confirmShape('Get-ChildItem', false, true)).toEqual({ ...base, height: base.height + CONFIRM_NOTE });
    // A screen recording: the file line keeps its place under the note.
    expect(confirmShape(recording.join('\n'), false, true)).toEqual({ width: 600, height: 326 + CONFIRM_NOTE, radius: 30 });
    expect(confirmShape(recording.slice(0, 3).join('\n'), false, true).height).toBe(290 + CONFIRM_NOTE);
    // A scenario by event, shown whole: one line and the longest one.
    expect(confirmShape('Пауза 1 мин.', true, true).height).toBeGreaterThanOrEqual(240 + 19 + CONFIRM_NOTE);
    const long = 'я'.repeat(9000);
    expect(confirmShape(long, true, true).height).toBe(confirmShape(long, true).height + CONFIRM_NOTE);
    // The tallest of both kinds still fits the canvas.
    const tallest = Math.max(confirmShape(long, true, true).height, confirmShape(Array(40).fill('строка').join('\n'), false, true).height);
    expect(14 + tallest).toBeLessThanOrEqual(600 - 20);
  });
});

describe('reducePaused', () => {
  it('follows pause_changed', () => {
    expect(reducePaused(false, { type: 'pause_changed', paused: true })).toBe(true);
    expect(reducePaused(true, { type: 'pause_changed', paused: false })).toBe(false);
  });
  it('ignores other events and malformed payloads', () => {
    expect(reducePaused(true, { type: 'state_changed', from: 'idle', to: 'thinking' })).toBe(true);
    expect(reducePaused(true, { type: 'pause_changed', paused: 'no' })).toBe(true);
    expect(reducePaused(false, null)).toBe(false);
  });
  it('the paused line of the island is a text of the locales', () => {
    expect(isMessageKey('face.paused')).toBe(true);
  });
});

describe('reduceState', () => {
  it('follows state_changed', () => {
    expect(reduceState('idle', { type: 'state_changed', from: 'idle', to: 'listening' })).toBe('listening');
  });
  it('ignores other events', () => {
    expect(reduceState('thinking', { type: 'assistant_text', text: 'x' })).toBe('thinking');
  });
  it('ignores malformed payloads', () => {
    expect(reduceState('thinking', { type: 'state_changed', to: 'bogus' })).toBe('thinking');
    expect(reduceState('thinking', null)).toBe('thinking');
    expect(reduceState('thinking', 'state_changed')).toBe('thinking');
  });
});

describe('opensChat', () => {
  const at = (hit: boolean) => ({ closest: () => (hit ? {} : null) });

  it('opens on a click on the island body', () => {
    expect(opensChat(at(false))).toBe(true);
  });

  it('ignores clicks that started on a control inside the island', () => {
    expect(opensChat(at(true))).toBe(false);
  });

  it('treats a missing target as a click on the body', () => {
    expect(opensChat(null)).toBe(true);
  });
});

describe('noticeOf', () => {
  it('shows a fired reminder, but not a scheduled task', () => {
    expect(noticeOf({ type: 'reminder_fired', id: 1, text: 'чайник', kind: 'remind', late_s: 0 })).toBe('чайник');
    expect(noticeOf({ type: 'reminder_fired', id: 2, text: 'сделай', kind: 'task', late_s: 0 })).toBeNull();
  });

  it('shows her lines about a meeting, a break and the end of focus', () => {
    expect(noticeOf({ type: 'remark', text: 'Через 10 мин: Созвон', kind: 'calendar' })).toBe('Через 10 мин: Созвон');
    expect(noticeOf({ type: 'remark', text: 'Пора размяться', kind: 'break' })).toBe('Пора размяться');
    expect(noticeOf({ type: 'remark', text: 'Фокус закончился. За это время ничего не пришло.', kind: 'focus' })).toBe(
      'Фокус закончился. За это время ничего не пришло.',
    );
  });

  it('shows her line about a background job that ended', () => {
    expect(noticeOf({ type: 'remark', text: 'Готово: Записать экран', kind: 'job' })).toBe('Готово: Записать экран');
    expect(noticeOf({ type: 'remark', text: 'Не получилось: Сжать «a.mp4». FFmpeg завершился с ошибкой:\nConversion failed!', kind: 'job' })).toBe(
      'Не получилось: Сжать «a.mp4». FFmpeg завершился с ошибкой.',
    );
  });

  it('shows only the head of the focus list', () => {
    const text = 'Фокус закончился. За это время пришло 2:\n• Telegram — Аня: привет\n• Почта — счёт';
    expect(noticeOf({ type: 'remark', text, kind: 'focus' })).toBe('Фокус закончился. За это время пришло 2.');
  });

  it('keeps greetings and the late-night line in the chat', () => {
    expect(noticeOf({ type: 'remark', text: 'Уже поздно', kind: 'life' })).toBeNull();
    expect(noticeOf({ type: 'remark', text: '  ', kind: 'calendar' })).toBeNull();
    expect(noticeOf({ type: 'assistant_delta', text: 'привет' })).toBeNull();
    expect(noticeOf(null)).toBeNull();
  });
});

describe('foldsAway', () => {
  const quiet = { view: 'chat' as const, windowed: false, away: true, state: 'idle' as const, asking: false };

  it('folds the open chat of the island once it lost focus and nothing goes on', () => {
    expect(foldsAway(quiet)).toBe(true);
  });

  it('keeps the chat while it has focus or never lost it', () => {
    expect(foldsAway({ ...quiet, away: false })).toBe(false);
  });

  it('keeps the chat while she listens, thinks, speaks or waits for an answer', () => {
    for (const state of ['listening', 'thinking', 'speaking', 'awaiting_confirm'] as const) {
      expect(foldsAway({ ...quiet, state })).toBe(false);
    }
    expect(foldsAway({ ...quiet, asking: true })).toBe(false);
  });

  it('keeps the ordinary window, the settings and the welcome', () => {
    expect(foldsAway({ ...quiet, windowed: true })).toBe(false);
    expect(foldsAway({ ...quiet, view: 'settings' })).toBe(false);
    expect(foldsAway({ ...quiet, view: 'welcome' })).toBe(false);
    expect(foldsAway({ ...quiet, view: 'island' })).toBe(false);
  });
});
