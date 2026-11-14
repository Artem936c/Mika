import { afterEach, describe, expect, it } from 'vitest';
import en from '../locales/en.json';
import ru from '../locales/ru.json';
import { isMessageKey, t } from './i18n';
import { LANGS, dateLocale, isLang, lang, nativeName, pickText, setLang } from './locale.svelte';

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

  it('names the longest Telegram voice message in minutes', () => {
    const text = t('ext.tg_voice_in_sub', { minutes: 2 });
    expect(text).toContain('2 минут');
    expect(text).not.toContain('{');
  });
});

describe('ru.json', () => {
  it('every message is non-empty', () => {
    for (const [key, text] of Object.entries(ru)) {
      expect(text.trim(), key).not.toBe('');
    }
  });
});

const ruTexts: Record<string, string> = ru;
const enTexts: Record<string, string> = en;
const has = (table: object, key: string) => Object.prototype.hasOwnProperty.call(table, key);
const placeholders = (text: string) => [...new Set([...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]))].sort();

describe('en.json', () => {
  // After other work adds keys to ru.json this test lists what en.json still lacks.
  it('en.json has the same keys as ru.json, in the same order', () => {
    const ruKeys = Object.keys(ru);
    const enKeys = Object.keys(en);
    expect({
      'missing in en.json': ruKeys.filter((key) => !has(en, key)),
      'not in ru.json': enKeys.filter((key) => !has(ru, key)),
    }).toEqual({ 'missing in en.json': [], 'not in ru.json': [] });
    expect(enKeys).toEqual(ruKeys);
  });

  it('en.json has the same placeholders', () => {
    for (const key of Object.keys(ru).filter((k) => has(en, k))) {
      expect(placeholders(enTexts[key]), key).toEqual(placeholders(ruTexts[key]));
    }
  });

  it('variant keys match', () => {
    const variantKeys = (table: Record<string, string>) => Object.keys(table).filter((key) => table[key].includes('|'));
    expect(variantKeys(enTexts)).toEqual(variantKeys(ruTexts));
    expect(variantKeys(ruTexts)).toContain('rhythm.break');
    for (const key of variantKeys(ruTexts)) {
      expect(enTexts[key].split('|').length, key).toBe(ruTexts[key].split('|').length);
      for (const table of [ruTexts, enTexts]) {
        // Any variant may be the one shown, so each holds every placeholder of the key.
        for (const variant of table[key].split('|')) {
          expect(placeholders(variant), `${key}: ${variant}`).toEqual(placeholders(table[key]));
        }
      }
    }
  });

  it('en.json has no cyrillic', () => {
    const offenders = Object.entries(en)
      .filter(([, text]) => /[Ѐ-ӿ]/.test(text))
      .map(([key]) => key);
    expect(offenders).toEqual([]);
  });

  it('every message is non-empty', () => {
    for (const [key, text] of Object.entries(en)) {
      expect(text.trim(), key).not.toBe('');
    }
  });
});

describe('language', () => {
  afterEach(() => setLang('ru'));

  it('t follows the language', () => {
    expect(lang()).toBe('ru');
    expect(t('tray.quit')).toBe('Выход');
    setLang('en');
    expect(lang()).toBe('en');
    expect(t('tray.quit')).toBe('Quit');
    const text = t('chat.setup', { name: 'Mika' });
    expect(text).toBe('For Mika to answer, set a provider and a key.');
    expect(text).not.toMatch(/\{\w+\}/);
    setLang('ru');
    expect(t('tray.quit')).toBe('Выход');
  });

  it('a text missing in one table comes from the fallback', () => {
    expect(pickText({ a: 'en' }, { a: 'ru' }, 'a')).toBe('en');
    expect(pickText({}, { a: 'x' }, 'a')).toBe('x');
    expect(pickText({}, {}, 'a')).toBeUndefined();
    expect(pickText({}, {}, 'toString')).toBeUndefined();
  });

  it('each language names itself', () => {
    expect(nativeName('ru')).toMatch(/[а-яё]/i);
    expect(nativeName('en')).toBe('English');
    expect(dateLocale()).toBe('ru-RU');
    setLang('en');
    expect(dateLocale()).toBe('en-GB');
  });

  it('only the two languages are languages', () => {
    expect([...LANGS]).toEqual(['ru', 'en']);
    expect(LANGS.every(isLang)).toBe(true);
    for (const value of ['de', 'EN', '', null, undefined, 1]) {
      expect(isLang(value), String(value)).toBe(false);
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

  // `t('key')` for a text with `{name}` shows the braces on screen.
  it('no text with a placeholder is asked for without its values', () => {
    const files = import.meta.glob('../**/*.{svelte,ts}', { query: '?raw', import: 'default', eager: true }) as Record<
      string,
      string
    >;
    const checked = Object.entries(files).filter(([path]) => !path.includes('/locales/') && !path.endsWith('.test.ts'));
    expect(checked.length).toBeGreaterThan(10);
    const bare = /\bt\(\s*'([\w.]+)'\s*\)/g;
    const offenders = checked.flatMap(([path, text]) =>
      [...text.matchAll(bare)]
        .map((m) => m[1])
        .filter((key) => has(ru, key) && placeholders(`${ruTexts[key]} ${enTexts[key] ?? ''}`).length > 0)
        .map((key) => `${path}: ${key}`),
    );
    expect(offenders).toEqual([]);
  });
});
