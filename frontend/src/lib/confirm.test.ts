import { describe, expect, it } from 'vitest';
import { alwaysTitle, confirmHead, reducePending, secondsLeft, untranslated, type Pending } from './confirm';
import { isMessageKey } from './i18n';

const assessment = { level: 'medium', summary: 'Открыть', target: 'notepad', reason: 'запуск', scope: 'notepad' };
const req = { type: 'confirm_requested', id: 7, tool: 'open_app', assessment, allow_always: true, timeout_s: 60 };

describe('reducePending', () => {
  it('confirm_requested opens', () => {
    expect(reducePending(null, req, 1000)).toEqual({
      id: 7,
      tool: 'open_app',
      assessment,
      allowAlways: true,
      deadline: 61_000,
    });
  });
  it('high risk never allows always even if the event says so', () => {
    const p = reducePending(null, { ...req, assessment: { ...assessment, level: 'high' } }, 0);
    expect(p?.allowAlways).toBe(false);
  });
  it('confirm_resolved with same id closes', () => {
    const p = reducePending(null, req, 0);
    expect(reducePending(p, { type: 'confirm_resolved', id: 7, answer: 'once', by: 'button' }, 0)).toBeNull();
  });
  it('other id is ignored', () => {
    const p = reducePending(null, req, 0);
    expect(reducePending(p, { type: 'confirm_resolved', id: 8 }, 0)).toBe(p);
  });
  it('malformed ignored', () => {
    expect(reducePending(null, { type: 'confirm_requested', id: 'x' }, 0)).toBeNull();
    expect(reducePending(null, { type: 'confirm_requested', id: 1, tool: 't', assessment: { level: 'huge' } }, 0)).toBeNull();
    expect(reducePending(null, null, 0)).toBeNull();
  });
});

describe('secondsLeft', () => {
  it('counts down and stops at 0', () => {
    const p = reducePending(null, req, 0) as Pending;
    expect(secondsLeft(p, 0)).toBe(60);
    expect(secondsLeft(p, 59_001)).toBe(1);
    expect(secondsLeft(p, 61_000)).toBe(0);
  });
});

describe('card texts', () => {
  const pending = (level: 'low' | 'medium' | 'high', allowAlways: boolean): Pending => ({
    id: 1,
    tool: 't',
    assessment: { level, summary: 's', target: 't', reason: 'r', scope: '*' },
    allowAlways,
    deadline: 0,
  });

  it('every risk level has a heading', () => {
    for (const level of ['low', 'medium', 'high'] as const) {
      expect(isMessageKey(confirmHead(level)), level).toBe(true);
    }
  });

  it('explains why "always" is not offered', () => {
    expect(alwaysTitle(pending('low', true))).toBe('confirm.always_title');
    expect(alwaysTitle(pending('high', false))).toBe('confirm.always_high');
    expect(alwaysTitle(pending('medium', false))).toBe('confirm.always_wide');
  });

  it('says that a Russian description is not translated, in the English interface only', () => {
    const russian = reducePending(null, req, 0) as Pending;
    expect(untranslated(russian, 'en')).toBe(true);
    expect(untranslated(russian, 'ru')).toBe(false);
    // The reason alone is enough: the user reads it before deciding.
    const mixed: Pending = { ...russian, assessment: { ...assessment, level: 'medium', summary: 'Open', reason: 'запуск' } };
    expect(untranslated(mixed, 'en')).toBe(true);
    // Nothing to warn about once the texts are English; a Russian path in the target is not a description.
    const english: Pending = { ...russian, assessment: { ...assessment, level: 'medium', summary: 'Open', target: 'Блокнот', reason: 'starts a program' } };
    expect(untranslated(english, 'en')).toBe(false);
    expect(isMessageKey('confirm.untranslated')).toBe(true);
  });
});
