import { describe, expect, it } from 'vitest';
import {
  BLINK,
  EMOTION_ORDER,
  EYE_LAYERS,
  MOUTH_LAYERS,
  blinkPause,
  canBlink,
  canTalk,
  frameUrl,
  isEmotion,
  nextMouth,
  reduceEmotion,
} from './avatar';
import { isMessageKey } from './i18n';

describe('frames', () => {
  it('every emotion and layer has its own picture', () => {
    const urls = [...EMOTION_ORDER, ...EYE_LAYERS, ...MOUTH_LAYERS].map(frameUrl);
    for (const u of urls) expect(u).not.toBe('');
    expect(new Set(urls).size).toBe(urls.length);
  });
});

describe('blink and lips', () => {
  it('blink closes the eyes and opens them again', () => {
    expect(BLINK.map(([l]) => l)).toContain('eyes-shut');
    expect(BLINK[BLINK.length - 1][0]).toBeNull();
    expect(canBlink('neutral')).toBe(true);
    expect(canBlink('joy')).toBe(false);
  });

  it('pause between blinks stays in a lively range', () => {
    expect(blinkPause(0)).toBeGreaterThanOrEqual(2000);
    expect(blinkPause(0.999)).toBeLessThanOrEqual(6000);
  });

  it('the mouth never keeps one shape twice in a row', () => {
    let m = nextMouth(null, 0);
    for (let i = 0; i < 50; i++) {
      const n = nextMouth(m, (i * 0.37) % 1);
      expect(n).not.toBe(m);
      m = n;
    }
    expect(nextMouth('mouth-oo', 0.9999)).not.toBe('mouth-oo');
  });

  it('lips move only where the mouth layers fit', () => {
    expect(canTalk('neutral')).toBe(true);
    expect(canTalk('sad')).toBe(false);
  });
});

describe('emotions', () => {
  it('ten slots, each with a name and a reason in the locale file', () => {
    expect(EMOTION_ORDER.length).toBe(10);
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
