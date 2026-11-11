// Island shapes and event folding. Pure functions, no Tauri calls.

export type AssistantState = 'idle' | 'listening' | 'thinking' | 'speaking' | 'awaiting_confirm';

export interface IslandShape {
  width: number;
  height: number;
  radius: number;
}

export const ISLAND_SHAPES: Record<AssistantState, IslandShape> = {
  idle: { width: 236, height: 44, radius: 22 },
  listening: { width: 340, height: 52, radius: 26 },
  thinking: { width: 380, height: 52, radius: 26 },
  speaking: { width: 620, height: 78, radius: 30 },
  awaiting_confirm: { width: 600, height: 268, radius: 30 },
};

export const STATE_LABELS: Record<AssistantState, string> = {
  idle: 'Жду',
  listening: 'Слушаю',
  thinking: 'Думаю',
  speaking: 'Отвечаю',
  awaiting_confirm: 'Нужно подтверждение',
};

export type MikaEvent =
  | { type: 'state_changed'; from: AssistantState; to: AssistantState }
  | { type: 'user_text'; text: string }
  | { type: 'assistant_text'; text: string }
  | { type: 'error'; message: string };

export function isAssistantState(value: unknown): value is AssistantState {
  return typeof value === 'string' && Object.prototype.hasOwnProperty.call(ISLAND_SHAPES, value);
}

export function shapeFor(state: unknown): IslandShape {
  return isAssistantState(state) ? ISLAND_SHAPES[state] : ISLAND_SHAPES.idle;
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
