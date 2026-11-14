// Interface language of this window and the texts of both languages.
// The language is Svelte state: every `t()` in a template, a `$derived` or an
// `$effect` follows it without a change at the call site.

import en from '../locales/en.json';
import ru from '../locales/ru.json';

export type MessageKey = keyof typeof ru;
export type Lang = 'ru' | 'en';
export const LANGS: readonly Lang[] = ['ru', 'en'];

// A key of ru.json that en.json lacks is a type error on this line (svelte-check).
const tables: Record<Lang, Record<MessageKey, string>> = { ru, en };
const current = $state<{ lang: Lang }>({ lang: 'ru' });

export function isLang(value: unknown): value is Lang {
  return value === 'ru' || value === 'en';
}

export function lang(): Lang {
  return current.lang;
}

export function setLang(next: Lang): void {
  current.lang = next;
  if (typeof document !== 'undefined') document.documentElement.lang = next;
}

/** Text for `key` from `table`, else from `fallback`. Own properties only. */
export function pickText(table: Record<string, string>, fallback: Record<string, string>, key: string): string | undefined {
  if (Object.prototype.hasOwnProperty.call(table, key)) return table[key];
  if (Object.prototype.hasOwnProperty.call(fallback, key)) return fallback[key];
  return undefined;
}

/** Text for `key` in the current language; a key that language lacks comes in Russian. */
export function lookup(key: string): string | undefined {
  return pickText(tables[current.lang], tables.ru, key);
}

/** The name of a language written in that language. */
export function nativeName(l: Lang): string {
  return tables[l]['lang.name'];
}

/** Locale for dates and times: 24-hour clock and the day before the month in both. */
export function dateLocale(): string {
  return current.lang === 'en' ? 'en-GB' : 'ru-RU';
}
