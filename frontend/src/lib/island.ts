// Island shapes and event folding. Pure functions, no Tauri calls.

import type { MessageKey } from './i18n';

export type AssistantState = 'idle' | 'listening' | 'thinking' | 'speaking' | 'awaiting_confirm';

export interface IslandShape {
  width: number;
  height: number;
  radius: number;
}

/** Sizes from the design, section 10. */
export const ISLAND_SHAPES: Record<AssistantState, IslandShape> = {
  idle: { width: 236, height: 44, radius: 22 },
  listening: { width: 340, height: 52, radius: 26 },
  thinking: { width: 380, height: 52, radius: 26 },
  speaking: { width: 620, height: 78, radius: 30 },
  awaiting_confirm: { width: 600, height: 268, radius: 30 },
};

/** Open chat with the avatar column, and without it. */
export const CHAT_WIDE: IslandShape = { width: 860, height: 500, radius: 34 };
export const CHAT_NARROW: IslandShape = { width: 600, height: 500, radius: 34 };
export const SETTINGS_SHAPE: IslandShape = { width: 860, height: 500, radius: 34 };
/** Width of the avatar column in the wide chat. */
export const AVATAR_COLUMN = 290;

export type View = 'island' | 'chat' | 'settings' | 'welcome';

export function isAssistantState(value: unknown): value is AssistantState {
  return typeof value === 'string' && Object.prototype.hasOwnProperty.call(ISLAND_SHAPES, value);
}

export function stateLabel(state: AssistantState): MessageKey {
  return `state.${state}`;
}

export function shapeFor(state: unknown): IslandShape {
  return isAssistantState(state) ? ISLAND_SHAPES[state] : ISLAND_SHAPES.idle;
}

/** Island shape for the open panel and the assistant state. An open panel wins over the state. */
export function viewShape(view: View, state: AssistantState, wide: boolean): IslandShape {
  if (view === 'settings' || view === 'welcome') return SETTINGS_SHAPE;
  if (view === 'chat') return wide ? CHAT_WIDE : CHAT_NARROW;
  return shapeFor(state);
}

export function reduceState(current: AssistantState, event: unknown): AssistantState {
  if (typeof event !== 'object' || event === null) return current;
  const e = event as { type?: unknown; to?: unknown };
  if (e.type === 'state_changed' && isAssistantState(e.to)) return e.to;
  return current;
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export const ISLAND_TOP = 14;

export function rectForShape(shape: IslandShape, canvasWidth: number): Rect {
  return { x: (canvasWidth - shape.width) / 2, y: ISLAND_TOP, w: shape.width, h: shape.height };
}

export function islandRect(state: unknown, canvasWidth: number): Rect {
  return rectForShape(shapeFor(state), canvasWidth);
}

export function unionRect(a: Rect, b: Rect): Rect {
  const x = Math.min(a.x, b.x);
  const y = Math.min(a.y, b.y);
  return {
    x,
    y,
    w: Math.max(a.x + a.w, b.x + b.w) - x,
    h: Math.max(a.y + a.h, b.y + b.h) - y,
  };
}

/**
 * Text that unfolds the island and the avatar bubble for a moment: a fired
 * reminder, and her own line about a break, the end of focus or a meeting soon.
 * They must be seen with replies not spoken. A scheduled task shows up as its
 * own message; greetings and the late-night line (`life`) stay in the chat.
 * Of a line with a list (what came during focus) only the head is shown: the
 * list itself is in the chat.
 */
export function noticeOf(event: unknown): string | null {
  if (typeof event !== 'object' || event === null) return null;
  const e = event as { type?: unknown; text?: unknown; kind?: unknown };
  if (typeof e.text !== 'string' || !e.text.trim()) return null;
  if (e.type === 'reminder_fired' && e.kind !== 'task') return e.text;
  if (e.type === 'remark' && e.kind !== 'life') {
    const head = e.text.trim().split('\n')[0].trim();
    return head.replace(/:$/, '.');
  }
  return null;
}

/** Elements inside the island that handle their own clicks. */
const CONTROLS = 'button, a, input, textarea, select, label, [role="button"]';

/**
 * Whether a click on the island should open the chat. A click on a button inside
 * the island bubbles up too; by then the button may already have closed the chat.
 */
export function opensChat(target: { closest(selector: string): unknown } | null): boolean {
  return !target?.closest(CONTROLS);
}
