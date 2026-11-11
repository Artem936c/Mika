import { describe, expect, it } from 'vitest';
import { EMOTION_ORDER, EMOTIONS, avatarSvg, isEmotion, reduceEmotion } from './avatar';
import { isMessageKey } from './i18n';

describe('avatarSvg', () => {
  it('has a view box for head and full', () => {
    expect(avatarSvg('head', 'neutral')).toContain('viewBox="96 92 208 208"');
    expect(avatarSvg('full', 'neutral')).toContain('viewBox="0 0 400 520"');
  });

  it('every emotion renders a distinct face', () => {
    const faces = EMOTION_ORDER.map((e) => avatarSvg('head', e));
    expect(new Set(faces).size).toBe(10);
    for (const face of faces) {
      expect(face).not.toContain('undefined');
      expect(face).not.toContain('NaN');
    }
  });

  it('unknown emotion falls back to neutral', () => {
    expect(avatarSvg('head', 'rage')).toBe(avatarSvg('head', 'neutral'));
    expect(avatarSvg('head', undefined)).toBe(avatarSvg('head', 'neutral'));
  });

  it('refers only to gradients that AvatarDefs provides', () => {
    const ids = [...avatarSvg('full', 'shy').matchAll(/url\(#([\w-]+)\)/g)].map((m) => m[1]);
    const known = ['mk-hair', 'mk-tail', 'mk-back', 'mk-skin', 'mk-iris', 'mk-collar', 'mk-face', 'mk-eye-l', 'mk-eye-r'];
    expect(ids.length).toBeGreaterThan(5);
    for (const id of ids) expect(known).toContain(id);
  });
});

describe('emotions', () => {
  it('ten slots, each with a name and a reason in the locale file', () => {
    expect(Object.keys(EMOTIONS).sort()).toEqual([...EMOTION_ORDER].sort());
    for (const e of EMOTION_ORDER) {
      expect(isMessageKey(`emotion.${e}`), e).toBe(true);
      expect(isMessageKey(`emotion.why.${e}`), e).toBe(true);
    }
  });

  it('isEmotion rejects other values', () => {
    expect(isEmotion('joy')).toBe(true);
    expect(isEmotion('happy')).toBe(false);
    expect(isEmotion(null)).toBe(false);
  });

  it('only emotion_changed moves the emotion', () => {
    expect(reduceEmotion('neutral', { type: 'emotion_changed', emotion: 'worry' })).toBe('worry');
    expect(reduceEmotion('joy', { type: 'emotion_changed', emotion: 'nope' })).toBe('joy');
    expect(reduceEmotion('joy', { type: 'emotion_cue', emotion: 'sad' })).toBe('joy');
    expect(reduceEmotion('joy', null)).toBe('joy');
  });
});
