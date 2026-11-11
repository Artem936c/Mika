import { describe, expect, it } from 'vitest';
import ru from '../locales/ru.json';
import { isMessageKey, t } from './i18n';

describe('t', () => {
  it('interpolates params', () => {
    expect(t('chat.setup', { name: 'Айри' })).toBe('Чтобы Айри отвечала, укажи провайдера и ключ.');
    expect(t('chat.volume', { was: 60, level: 40 })).toBe('Громкость 60 → 40 %');
  });

  it('leaves a placeholder without a value as is', () => {
    expect(t('chat.setup', {})).toContain('{name}');
  });

  it('knows which strings are keys', () => {
    expect(isMessageKey('chat.send')).toBe(true);
    expect(isMessageKey('toString')).toBe(false);
  });
});

describe('ru.json', () => {
  it('every message is non-empty', () => {
    for (const [key, text] of Object.entries(ru)) {
      expect(text.trim(), key).not.toBe('');
    }
  });
});

describe('sources', () => {
  // CLAUDE.md: interface texts live in locale files, not in code.
  it('no cyrillic outside locales', () => {
    const files = import.meta.glob('../**/*.{svelte,ts}', { query: '?raw', import: 'default', eager: true }) as Record<
      string,
      string
    >;
    const checked = Object.entries(files).filter(
      ([path]) => !path.includes('/locales/') && !path.includes('/dev/') && !path.endsWith('.test.ts'),
    );
    expect(checked.length).toBeGreaterThan(10);
    const offenders = checked.filter(([, text]) => /[а-яё]/i.test(text)).map(([path]) => path);
    expect(offenders).toEqual([]);
  });
});
