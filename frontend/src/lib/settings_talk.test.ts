import { describe, expect, it } from 'vitest';
import { changedByTalk } from './settings_talk';

describe('changedByTalk', () => {
  it('is true only for a setting changed by conversation', () => {
    expect(changedByTalk({ action: 'settings_changed', by: 'talk', key: 'voice.volume' })).toBe(true);
    // A page of the settings changed it: the page already shows the value.
    expect(changedByTalk({ action: 'settings_changed' })).toBe(false);
    expect(changedByTalk({ action: 'open_chat', by: 'talk' })).toBe(false);
    expect(changedByTalk(null)).toBe(false);
    expect(changedByTalk('settings_changed')).toBe(false);
  });
});
