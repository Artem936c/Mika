import { describe, expect, it } from 'vitest';
import { BUBBLE_CHARS, bubbleFor, lingerMs } from './bubble';

describe('bubbleFor', () => {
  it('is empty when idle', () => {
    expect(bubbleFor('idle', 'старый ответ', '', null)).toBeNull();
  });

  it('shows the reply tail when speaking', () => {
    expect(bubbleFor('speaking', 'Готово, сделала тише.', '', null)).toEqual({ text: 'Готово, сделала тише.' });
    const long = 'слово '.repeat(60);
    const b = bubbleFor('speaking', long, '', null);
    expect(b && 'text' in b && b.text.length).toBeLessThanOrEqual(BUBBLE_CHARS + 1);
    expect(bubbleFor('speaking', '', '', null)).toBeNull();
  });

  it('shows the action when confirming, whatever the state', () => {
    expect(bubbleFor('awaiting_confirm', '', '', 'Перезагрузить компьютер')).toEqual({ text: 'Перезагрузить компьютер' });
    expect(bubbleFor('thinking', '', 'шаг', 'Перезагрузить компьютер')).toEqual({ text: 'Перезагрузить компьютер' });
  });

  it('names the running step when thinking', () => {
    expect(bubbleFor('thinking', '', 'Узнать погоду', null)).toEqual({ text: 'Узнать погоду' });
    expect(bubbleFor('thinking', '', '', null)).toEqual({ key: 'bubble.thinking' });
    expect(bubbleFor('listening', '', '', null)).toEqual({ key: 'bubble.listening' });
  });
});

describe('lingerMs', () => {
  it('grows with the text between 4 and 12 seconds', () => {
    expect(lingerMs('ок')).toBe(4000);
    expect(lingerMs('я'.repeat(100))).toBe(6000);
    expect(lingerMs('я'.repeat(1000))).toBe(12000);
  });
});
