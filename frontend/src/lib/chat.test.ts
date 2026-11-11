import { describe, expect, it } from 'vitest';
import {
  cardData,
  formatSeconds,
  fromHistory,
  inlineMarkup,
  reduceChat,
  speakWords,
  tailWords,
  toolLabel,
  weekdayShort,
  type ChatItem,
} from './chat';
import { isMessageKey } from './i18n';

const run = (events: unknown[], start: ChatItem[] = []) => events.reduce<ChatItem[]>(reduceChat, start);

describe('reduceChat', () => {
  it('user_text appends user item', () => {
    expect(run([{ type: 'user_text', text: 'привет' }])).toEqual([{ kind: 'user', text: 'привет' }]);
  });

  it('deltas join into one streaming assistant item', () => {
    expect(
      run([
        { type: 'assistant_delta', text: 'При' },
        { type: 'assistant_delta', text: 'вет' },
      ]),
    ).toEqual([{ kind: 'assistant', text: 'Привет', pending: true }]);
  });

  it('assistant_text finalizes the streaming item', () => {
    expect(
      run([
        { type: 'assistant_delta', text: 'Ок' },
        { type: 'assistant_text', text: 'Ок' },
      ]),
    ).toEqual([{ kind: 'assistant', text: 'Ок', pending: false }]);
  });

  it('assistant_text without deltas appends the whole reply', () => {
    expect(run([{ type: 'assistant_text', text: 'Ок' }])).toEqual([{ kind: 'assistant', text: 'Ок' }]);
  });

  it('tool_started then tool_finished updates one tool row with ms', () => {
    expect(
      run([
        { type: 'tool_started', id: 'c1', name: 'get_time', summary: 'Узнать время' },
        { type: 'tool_finished', id: 'c1', name: 'get_time', ok: true, output: '12:00', elapsed_ms: 4 },
      ]),
    ).toEqual([{ kind: 'tool', id: 'c1', name: 'get_time', text: 'Узнать время', ok: true, ms: 4, pending: false }]);
  });

  it('tool_finished without start adds a failed row', () => {
    expect(run([{ type: 'tool_finished', id: 'c9', name: 'x', ok: false, output: 'нет', elapsed_ms: 0 }])).toEqual([
      { kind: 'tool', id: 'c9', name: 'x', text: 'нет', ok: false, ms: 0 },
    ]);
  });

  it('error appends error and finalizes streaming', () => {
    expect(
      run([
        { type: 'assistant_delta', text: 'Нача' },
        { type: 'error', message: 'обрыв' },
      ]),
    ).toEqual([
      { kind: 'assistant', text: 'Нача', pending: false },
      { kind: 'error', text: 'обрыв' },
    ]);
  });

  it('malformed events are ignored', () => {
    const start: ChatItem[] = [{ kind: 'user', text: 'a' }];
    expect(run([null, 'x', { type: 'user_text' }, { type: 'assistant_delta', text: 5 }, { type: 'bogus' }], start)).toEqual(start);
  });
});

describe('cardData', () => {
  const weather = {
    kind: 'weather',
    city: 'Пермь',
    temp: 1,
    feels: -3,
    wind: 6,
    text: 'дождь',
    days: [{ date: '2026-11-12', min: -4, max: 0, text: 'снег' }],
  };

  it('accepts weather and volume', () => {
    expect(cardData(weather)).toEqual(weather);
    expect(cardData({ kind: 'volume', level: 40, was: 60 })).toEqual({ kind: 'volume', level: 40, was: 60 });
  });

  it('rejects unknown kinds and broken shapes', () => {
    expect(cardData(undefined)).toBeUndefined();
    expect(cardData({ kind: 'stock', price: 1 })).toBeUndefined();
    expect(cardData({ kind: 'volume', level: '40', was: 60 })).toBeUndefined();
    expect(cardData({ ...weather, temp: null })).toBeUndefined();
    expect(cardData({ ...weather, days: [{ date: 5 }] })).toBeUndefined();
    expect(cardData({ ...weather, days: 'no' })).toBeUndefined();
  });

  it('tool_finished keeps the card data of a successful tool', () => {
    const items = run([
      { type: 'tool_started', id: 'c1', name: 'volume', summary: 'Громкость 40%' },
      { type: 'tool_finished', id: 'c1', name: 'volume', ok: true, output: 'x', elapsed_ms: 5, data: { kind: 'volume', level: 40, was: 60 } },
      { type: 'tool_started', id: 'c2', name: 'volume', summary: 'Громкость 10%' },
      { type: 'tool_finished', id: 'c2', name: 'volume', ok: false, output: 'x', elapsed_ms: 5, data: { kind: 'volume', level: 10, was: 40 } },
      { type: 'tool_started', id: 'c3', name: 'get_time', summary: 'Время' },
      { type: 'tool_finished', id: 'c3', name: 'get_time', ok: true, output: 'x', elapsed_ms: 5, data: { kind: 'bogus' } },
    ]);
    expect(items.map((it) => it.data)).toEqual([{ kind: 'volume', level: 40, was: 60 }, undefined, undefined]);
  });
});

describe('formatSeconds', () => {
  it('uses a comma and fewer digits for longer times', () => {
    expect(formatSeconds(400)).toBe('0,4');
    expect(formatSeconds(3849)).toBe('3,8');
    expect(formatSeconds(19090)).toBe('19');
    expect(formatSeconds(27)).toBe('0,03');
    expect(formatSeconds(0)).toBe('0');
  });
});

describe('speakWords', () => {
  it('numbers words from the start of the reply', () => {
    expect(speakWords('  раз два  три ', 100)).toEqual([
      { i: 0, word: 'раз' },
      { i: 1, word: 'два' },
      { i: 2, word: 'три' },
    ]);
  });
  it('keeps only the tail that fits, with the same numbers', () => {
    expect(speakWords('один два три четыре', 10)).toEqual([
      { i: 2, word: 'три' },
      { i: 3, word: 'четыре' },
    ]);
    expect(speakWords('', 10)).toEqual([]);
  });
});

describe('toolLabel', () => {
  it('names a group for known tools and falls back for others', () => {
    expect(toolLabel('get_weather')).toBe('tool.weather');
    expect(toolLabel('run_shell')).toBe('tool.shell');
    expect(toolLabel('brand_new_tool')).toBe('tool.other');
    for (const name of ['open_app', 'find_files', 'note_create', 'rdp_open', 'volume', 'web_search', 'get_time']) {
      expect(isMessageKey(toolLabel(name)), name).toBe(true);
      expect(toolLabel(name)).not.toBe('tool.other');
    }
  });
});

describe('weekdayShort', () => {
  it('gives a capitalised short weekday', () => {
    expect(weekdayShort('2026-11-12')).toBe('Чт');
    expect(weekdayShort('nonsense')).toBe('');
  });
});

describe('tailWords', () => {
  it('keeps short text', () => {
    expect(tailWords(' a  b ', 10)).toBe('a b');
  });
  it('cuts at a word boundary from the end', () => {
    expect(tailWords('один два три четыре', 10)).toBe('…четыре');
  });
});

describe('fromHistory', () => {
  it('maps user and assistant entries and skips junk', () => {
    expect(fromHistory([{ role: 'user', text: 'a' }, { role: 'tool', text: 'x' }, { role: 'assistant', text: 'b' }, 3])).toEqual([
      { kind: 'user', text: 'a' },
      { kind: 'assistant', text: 'b' },
    ]);
    expect(fromHistory(null)).toEqual([]);
  });
});

describe('inlineMarkup', () => {
  it('plain text is one text segment', () => {
    expect(inlineMarkup('Привет')).toEqual([{ kind: 'text', text: 'Привет' }]);
  });

  it('splits inline code and bold out of the text', () => {
    expect(inlineMarkup('Создала `text.txt` в **Загрузках**.')).toEqual([
      { kind: 'text', text: 'Создала ' },
      { kind: 'code', text: 'text.txt' },
      { kind: 'text', text: ' в ' },
      { kind: 'bold', text: 'Загрузках' },
      { kind: 'text', text: '.' },
    ]);
  });

  it('leaves an unclosed marker as text while the reply streams', () => {
    expect(inlineMarkup('файл `text.t')).toEqual([{ kind: 'text', text: 'файл `text.t' }]);
  });
});
