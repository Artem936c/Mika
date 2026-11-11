// Interface texts. Every string shown to the user lives in `locales/ru.json`.

import ru from '../locales/ru.json';

export type MessageKey = keyof typeof ru;

const messages: Record<string, string> = ru;

export function isMessageKey(key: string): key is MessageKey {
  return Object.prototype.hasOwnProperty.call(messages, key);
}

/** Text for `key` with `{name}` placeholders filled from `params`. */
export function t(key: MessageKey, params?: Record<string, string | number>): string {
  const text = messages[key] ?? key;
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => (name in params ? String(params[name]) : whole));
}
