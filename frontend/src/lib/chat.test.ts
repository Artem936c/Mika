import { describe, expect, it } from 'vitest';
import { CHAT_SHAPE, fromHistory, reduceChat, tailWords, viewShape, type ChatItem } from './chat';
import { ISLAND_SHAPES } from './island';

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

describe('viewShape', () => {
  it('open chat uses the chat shape', () => {
    expect(viewShape('chat', 'thinking')).toEqual(CHAT_SHAPE);
  });
  it('island follows the assistant state', () => {
    expect(viewShape('island', 'speaking')).toEqual(ISLAND_SHAPES.speaking);
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
