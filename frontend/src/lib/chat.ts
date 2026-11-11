// Chat items folded from bus events. Pure functions.

import { shapeFor, type AssistantState, type IslandShape } from './island';

export interface ChatItem {
  kind: 'user' | 'assistant' | 'tool' | 'error';
  text: string;
  /** Tool call id for tool rows. */
  id?: string;
  /** Tool name for tool rows. */
  name?: string;
  ok?: boolean;
  ms?: number;
  /** Assistant text still arriving, or tool still running. */
  pending?: boolean;
}

export type View = 'island' | 'chat';

export const CHAT_SHAPE: IslandShape = { width: 600, height: 500, radius: 28 };

/** Island shape for the window view and assistant state. The open chat wins, except the confirm card. */
export function viewShape(view: View, state: AssistantState): IslandShape {
  if (view === 'chat') return CHAT_SHAPE;
  return shapeFor(state);
}

function obj(e: unknown): Record<string, unknown> | null {
  return typeof e === 'object' && e !== null ? (e as Record<string, unknown>) : null;
}

function str(v: unknown): string | null {
  return typeof v === 'string' ? v : null;
}

function finishStreaming(items: ChatItem[]): ChatItem[] {
  return items.map((it) => (it.kind === 'assistant' && it.pending ? { ...it, pending: false } : it));
}

export function reduceChat(items: ChatItem[], event: unknown): ChatItem[] {
  const e = obj(event);
  if (!e) return items;
  switch (e.type) {
    case 'user_text': {
      const text = str(e.text);
      return text === null ? items : [...finishStreaming(items), { kind: 'user', text }];
    }
    case 'assistant_delta': {
      const text = str(e.text);
      if (text === null) return items;
      const last = items[items.length - 1];
      if (last && last.kind === 'assistant' && last.pending) {
        return [...items.slice(0, -1), { ...last, text: last.text + text }];
      }
      return [...items, { kind: 'assistant', text, pending: true }];
    }
    case 'assistant_text': {
      const text = str(e.text);
      if (text === null) return items;
      const idx = items.findIndex((it) => it.kind === 'assistant' && it.pending);
      if (idx >= 0) return finishStreaming(items);
      return text ? [...items, { kind: 'assistant', text }] : items;
    }
    case 'tool_started': {
      const id = str(e.id);
      const name = str(e.name);
      if (id === null || name === null) return items;
      return [...finishStreaming(items), { kind: 'tool', id, name, text: str(e.summary) ?? name, pending: true }];
    }
    case 'tool_finished': {
      const id = str(e.id);
      const name = str(e.name);
      if (id === null || name === null) return items;
      const ok = e.ok === true;
      const ms = typeof e.elapsed_ms === 'number' ? e.elapsed_ms : undefined;
      const idx = items.findIndex((it) => it.kind === 'tool' && it.id === id);
      if (idx < 0) {
        return [...items, { kind: 'tool', id, name, text: str(e.output) ?? name, ok, ms }];
      }
      const next = items.slice();
      next[idx] = { ...next[idx], ok, ms, pending: false, text: next[idx].text };
      return next;
    }
    case 'error': {
      const message = str(e.message);
      return message === null ? items : [...finishStreaming(items), { kind: 'error', text: message }];
    }
    default:
      return items;
  }
}

/** Last words of the reply for the "speaking" island. */
export function tailWords(text: string, maxChars: number): string {
  const t = text.replace(/\s+/g, ' ').trim();
  if (t.length <= maxChars) return t;
  const cut = t.slice(t.length - maxChars);
  const space = cut.indexOf(' ');
  return '…' + (space >= 0 ? cut.slice(space + 1) : cut);
}

export interface HistoryItem {
  role: 'user' | 'assistant';
  text: string;
}

export function fromHistory(history: unknown): ChatItem[] {
  if (!Array.isArray(history)) return [];
  return history
    .map(obj)
    .filter((h): h is Record<string, unknown> => !!h && (h.role === 'user' || h.role === 'assistant') && typeof h.text === 'string')
    .map((h) => ({ kind: h.role as 'user' | 'assistant', text: h.text as string }));
}
