import { describe, expect, it } from 'vitest';
import { comboFromEvent, dictationFrom, hotkeyNoticeOf, screenAskFrom, screenAskGone, screenAskLands, showsNotice } from './hotkeys';
import ru from '../locales/ru.json';

const key = (code: string, mods: Partial<Record<'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey', boolean>> = {}) => ({
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  code,
  ...mods,
});

describe('comboFromEvent', () => {
  it('writes the modifiers and the key code', () => {
    expect(comboFromEvent(key('KeyD', { ctrlKey: true, altKey: true }))).toBe('Ctrl+Alt+KeyD');
    expect(comboFromEvent(key('Digit7', { altKey: true, shiftKey: true }))).toBe('Alt+Shift+Digit7');
    expect(comboFromEvent(key('ArrowUp', { ctrlKey: true, metaKey: true }))).toBe('Ctrl+Win+ArrowUp');
  });

  it('keeps the modifiers in one order', () => {
    expect(comboFromEvent(key('KeyS', { metaKey: true, shiftKey: true, altKey: true, ctrlKey: true }))).toBe('Ctrl+Alt+Shift+Win+KeyS');
  });

  it('waits for a key while only modifiers are down', () => {
    for (const code of ['ControlLeft', 'ControlRight', 'AltLeft', 'AltRight', 'ShiftLeft', 'ShiftRight', 'MetaLeft', 'MetaRight', 'OSLeft', 'OSRight']) {
      expect(comboFromEvent(key(code, { ctrlKey: true, altKey: true })), code).toBeNull();
    }
    expect(comboFromEvent(key('', { ctrlKey: true }))).toBeNull();
  });

  it('takes a key without modifiers as it is', () => {
    expect(comboFromEvent(key('F9'))).toBe('F9');
  });
});

describe('dictationFrom', () => {
  it('reads the three phases', () => {
    expect(dictationFrom({ action: 'dictation', phase: 'listening', app: 'winword.exe' })).toEqual({ phase: 'listening', app: 'winword.exe' });
    expect(dictationFrom({ action: 'dictation', phase: 'recognising', app: 'winword.exe' })).toEqual({ phase: 'recognising', app: 'winword.exe' });
    expect(dictationFrom({ action: 'dictation', phase: 'typing' })).toEqual({ phase: 'typing', app: '' });
  });

  it('ends on idle and on a phase it does not know', () => {
    expect(dictationFrom({ action: 'dictation', phase: 'idle', app: '' })).toBeNull();
    expect(dictationFrom({ action: 'dictation', phase: 'dancing' })).toBeNull();
    expect(dictationFrom({ action: 'dictation' })).toBeNull();
  });

  it('leaves other messages alone', () => {
    expect(dictationFrom({ action: 'open_chat' })).toBeUndefined();
    expect(dictationFrom({ action: 'screen_ask', app: 'excel.exe', title: 'x' })).toBeUndefined();
    expect(dictationFrom(null)).toBeUndefined();
    expect(dictationFrom('dictation')).toBeUndefined();
    expect(dictationFrom(undefined)).toBeUndefined();
  });

  it('has a text for every phase shown in the island', () => {
    const strings = ru as Record<string, string>;
    for (const k of ['dictation.island', 'dictation.recognising', 'dictation.typing']) expect(strings[k], k).toBeTruthy();
  });
});

describe('screenAskFrom', () => {
  it('reads a full payload', () => {
    expect(screenAskFrom({ action: 'screen_ask', app: 'excel.exe', title: 'Report.xlsx - Excel', width: 1600, height: 900 })).toEqual({
      app: 'excel.exe',
      title: 'Report.xlsx - Excel',
      width: 1600,
      height: 900,
    });
  });

  it('fills what a picture can do without', () => {
    expect(screenAskFrom({ action: 'screen_ask', title: 'Notes' })).toEqual({ app: '', title: 'Notes', width: 0, height: 0 });
  });

  it('wants a title', () => {
    expect(screenAskFrom({ action: 'screen_ask', app: 'excel.exe', width: 1600, height: 900 })).toBeNull();
    expect(screenAskFrom({ action: 'screen_ask', app: 'excel.exe', title: 7 })).toBeNull();
  });

  it('ignores other messages', () => {
    expect(screenAskFrom({ action: 'selection', text: 'hello', title: 'x' })).toBeNull();
    expect(screenAskFrom({ action: 'dictation', phase: 'listening' })).toBeNull();
    expect(screenAskFrom(null)).toBeNull();
  });
});

describe('hotkey texts', () => {
  it('has a name for every action and a text for every refusal', () => {
    const strings = ru as Record<string, string>;
    for (const id of ['chat', 'listen', 'selection', 'dictation', 'screen_ask']) expect(strings[`hotkeys.action.${id}`], id).toBeTruthy();
    for (const code of ['no_key', 'unknown_key', 'modifier', 'reserved', 'same', 'taken']) expect(strings[`hotkeys.err_${code}`], code).toBeTruthy();
    for (const code of ['no_window', 'own_window', 'blind', 'password', 'keys_held', 'failed']) expect(strings[`dictation.err_${code}`], code).toBeTruthy();
    for (const code of ['no_window', 'own_window', 'blind', 'screen_off', 'failed']) expect(strings[`screen_ask.err_${code}`], code).toBeTruthy();
  });
});

describe('screenAskGone', () => {
  it('knows the message that the waiting picture was thrown away', () => {
    expect(screenAskGone({ action: 'screen_ask', gone: true })).toBe(true);
    // A new picture is not a goodbye, and it is not a picture when it says goodbye.
    expect(screenAskGone({ action: 'screen_ask', app: 'excel.exe', title: 'Отчёт', width: 1, height: 1 })).toBe(false);
    expect(screenAskFrom({ action: 'screen_ask', gone: true })).toBeNull();
    expect(screenAskGone({ action: 'dictation', gone: true })).toBe(false);
    expect(screenAskGone({ action: 'screen_ask', gone: 'yes' })).toBe(false);
    expect(screenAskGone(null)).toBe(false);
  });
});

describe('screenAskLands', () => {
  it('brings the chat forward from the island and from the settings', () => {
    expect(screenAskLands('island', false)).toBe('chat');
    expect(screenAskLands('settings', false)).toBe('chat');
    expect(screenAskLands('chat', false)).toBe('chat');
    expect(screenAskLands('chat', true)).toBe('chat');
  });

  it('does not keep a picture whose card nobody would see', () => {
    expect(screenAskLands('welcome', false)).toBe('drop');
    expect(screenAskLands('welcome', true)).toBe('drop');
    // The panel opens the setup of a provider, not the chat.
    expect(screenAskLands('island', true)).toBe('drop');
    expect(screenAskLands('settings', true)).toBe('drop');
  });
});

describe('notices of hotkey actions', () => {
  it('knows her lines about dictation and the screen question', () => {
    expect(hotkeyNoticeOf({ type: 'remark', kind: 'dictation', text: 'Диктовка подождёт' })).toBe('Диктовка подождёт');
    expect(hotkeyNoticeOf({ type: 'remark', kind: 'screen_ask', text: 'Не вижу окна' })).toBe('Не вижу окна');
    expect(hotkeyNoticeOf({ type: 'remark', kind: 'break', text: 'Пора размяться' })).toBeNull();
    expect(hotkeyNoticeOf({ type: 'remark', kind: 'dictation', text: '  ' })).toBeNull();
    expect(hotkeyNoticeOf({ type: 'assistant_text', text: 'привет' })).toBeNull();
    expect(hotkeyNoticeOf(null)).toBeNull();
  });

  it('shows such a line while she is busy, and any other only when idle', () => {
    expect(showsNotice('idle', 'строка', false)).toBe(true);
    expect(showsNotice('idle', 'строка', true)).toBe(true);
    expect(showsNotice('thinking', 'строка', false)).toBe(false);
    expect(showsNotice('speaking', 'строка', false)).toBe(false);
    expect(showsNotice('thinking', 'строка', true)).toBe(true);
    expect(showsNotice('speaking', 'строка', true)).toBe(true);
    // A recording and an open question keep the island.
    expect(showsNotice('listening', 'строка', true)).toBe(false);
    expect(showsNotice('awaiting_confirm', 'строка', true)).toBe(false);
    expect(showsNotice('thinking', '', true)).toBe(false);
  });
});

describe('texts that follow the hotkeys', () => {
  const texts = ru as Record<string, string>;

  it('has a line for a hotkey that waits for its master switch', () => {
    for (const id of ['dictation', 'screen_ask']) {
      expect(texts[`hotkeys.status_needs.${id}`], id).toBeTruthy();
    }
    for (const key of ['dictation.err_unsure', 'screen_ask.err_busy', 'screen_ask.err_setup']) {
      expect(texts[key], key).toBeTruthy();
    }
  });

  it('names no fixed combination where the user can change it', () => {
    // These lines were written when the combinations could not change.
    for (const key of ['island.games_sub', 'welcome.mic_sub']) {
      expect(texts[key], key).not.toMatch(/Ctrl\+|Alt\+/);
    }
  });
});
