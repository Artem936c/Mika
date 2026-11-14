// Interface texts. Every string shown to the user lives in `locales/ru.json` and
// `locales/en.json`; `locale.svelte.ts` holds the language of the window.

import ru from '../locales/ru.json';
import { dateLocale, lookup, type MessageKey } from './locale.svelte';

export type { MessageKey };
export { dateLocale };

/** `ru.json` is the reference set of keys. */
export function isMessageKey(key: string): key is MessageKey {
  return Object.prototype.hasOwnProperty.call(ru, key);
}

/** Text for `key` in the interface language with `{name}` placeholders filled from `params`. */
export function t(key: MessageKey, params?: Record<string, string | number>): string {
  const text = lookup(key) ?? key;
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => (name in params ? String(params[name]) : whole));
}
