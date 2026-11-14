// Scenarios by event and "wait and tell" on the settings page. Pure functions.

import { isMessageKey, t } from './i18n';

export interface TriggerAction {
  kind: 'command' | 'instruction';
  text: string;
}

export interface TriggerItem {
  id: number;
  name: string;
  /** One of the ten event ids, `audio_connected` … `user_back`. */
  event: string;
  /** Part of a device name or a process name; empty when the event takes none. */
  filter: string;
  idle_min: number;
  action: TriggerAction;
  enabled: boolean;
  cooldown_s: number;
  max_per_day: number;
  /** Unix seconds of the last run, 0 for never. */
  fired_at: number;
  fired_day: string;
  fired_count: number;
  fired_today: number;
  /** The saved command the scenario runs was deleted. */
  command_missing: boolean;
  command_changed: boolean;
  /** What the saved command says now; absent for an instruction and for a command that was deleted. */
  command_text?: string;
}

export interface WaitItem {
  id: number;
  /** `file_appears`, `file_ready`, `process_exits`, `window_text` or `window_text_gone`. */
  kind: string;
  what: string;
  /** Unix seconds. */
  since: number;
  until: number;
}

export interface EventsView {
  enabled: boolean;
  max_per_hour: number;
  triggers: TriggerItem[];
  waits: WaitItem[];
}

/** The event of a scenario in words; an event this build does not know shows its id. */
export function whenLabel(tr: Pick<TriggerItem, 'event' | 'filter' | 'idle_min'>): string {
  const key = `events.when.${tr.event}`;
  if (!isMessageKey(key)) return tr.event;
  const text = t(key, { min: tr.idle_min });
  return tr.filter ? t('events.when_filter', { when: text, filter: tr.filter }) : text;
}

/** What a scenario does. */
export function actionLabel(a: TriggerAction): string {
  return a.kind === 'command' ? t('events.action_command', { name: a.text }) : t('events.action_instruction', { text: a.text });
}

/** The pause between two runs: seconds under two minutes, minutes under two hours, then hours. */
export function pauseLabel(seconds: number): string {
  if (seconds < 120) return t('events.pause_s', { n: Math.round(seconds) });
  if (seconds < 7200) return t('events.pause_min', { n: Math.round(seconds / 60) });
  return t('events.pause_h', { n: Math.round(seconds / 3600) });
}

/** What a watch waits for; a kind this build does not know shows only its subject. */
export function waitLabel(w: WaitItem): string {
  const key = `events.wait.${w.kind}`;
  return isMessageKey(key) ? t(key, { what: w.what }) : w.what;
}

/** Whole minutes until `until` (unix seconds), rounded up, never below zero. */
export function minutesLeft(until: number, nowMs: number): number {
  return Math.max(0, Math.ceil((until * 1000 - nowMs) / 60_000));
}
