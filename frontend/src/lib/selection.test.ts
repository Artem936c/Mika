import { describe, expect, it } from 'vitest';
import { PASTABLE, SELECTION_ACTIONS, selectionFrom } from './selection';
import ru from '../locales/ru.json';

describe('selection', () => {
  it('reads the shortcut payload and ignores others', () => {
    expect(selectionFrom({ action: 'selection', text: 'hello', app: 'word.exe' })).toEqual({ text: 'hello', app: 'word.exe' });
    expect(selectionFrom({ action: 'selection', text: '  ' })).toBeNull();
    expect(selectionFrom({ action: 'open_chat' })).toBeNull();
    expect(selectionFrom(null)).toBeNull();
  });

  it('has a label for every button and pastes only rewrites', () => {
    const strings = ru as Record<string, string>;
    for (const a of SELECTION_ACTIONS) expect(strings[`selection.${a}`]).toBeTruthy();
    expect(PASTABLE.has('explain')).toBe(false);
    expect(PASTABLE.has('translate')).toBe(true);
  });
});
