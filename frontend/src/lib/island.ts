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

/**
 * Height of the island question without the lines of its target, px: the
 * heading, the summary, a reason of two lines, the buttons, the hint, the
 * paddings and the frame of the target box. Measured in the preview (235) with
 * a few pixels to spare.
 */
const CONFIRM_REST = 240;
/** Most lines a question that is shown whole can take. */
export const CONFIRM_MAX_LINES = 11;
/** Characters in one line of the target at the island width, counted with room to spare. */
const CONFIRM_LINE_CHARS = 62;
/** Height of one line of the target, px: 12.5px text at 1.45. */
export const CONFIRM_LINE_PX = 18.125;

/** How many lines the target of a question takes in the island. */
export function targetLines(target: string): number {
  return target
    .split('\n')
    .reduce((sum, line) => sum + Math.max(1, Math.ceil(line.length / CONFIRM_LINE_CHARS)), 0);
}

/** The confirm card shows this many lines of the target without scrolling; the island grows from the third one. */
export const CONFIRM_LINES = 8;
/** Height of one line of the target on the confirm card. */
const CONFIRM_LINE = 18;
/** What the size from the design lacks to show two lines in full. */
const CONFIRM_LACK = 4;
/**
 * Characters of the target that fit in one line of the card on the island (534 px
 * of 12.5 px monospace, 7.3 px a character, less a small reserve). A longer line wraps.
 */
const CONFIRM_COLS = 70;

/**
 * How many lines the target of a question takes on the card, at most `CONFIRM_LINES`.
 * A target written in lines counts each of them with its wraps: the title of a window
 * or a long path takes two or three. A target without line breaks is a command: it is
 * one line, however long, and the card scrolls it.
 */
export function confirmLines(target: string): number {
  const lines = target.split('\n');
  if (lines.length < 2) return 1;
  const drawn = lines.reduce((sum, line) => sum + Math.max(1, Math.ceil([...line].length / CONFIRM_COLS)), 0);
  return Math.min(drawn, CONFIRM_LINES);
}

/** Height of the box that shows `lines` lines of the target in full: the lines, the padding and the frame. */
export function confirmTargetHeight(lines: number): number {
  return Math.ceil(lines * 18.125) + 18;
}

/**
 * Height of the line that stands above the summary in the English interface
 * while the description itself is still Russian (`untranslated` in confirm.ts),
 * px: 12 px text in one line with its margins of 8 and -4.
 */
export const CONFIRM_NOTE = 21;

/**
 * The island with a question in it. A target written in lines (a screen
 * recording names the microphone, the sound, the length, the screen and the file)
 * makes it taller, so that none of them hides below a scroll bar. A question that
 * must be read whole (`whole`, see `showsWhole`) makes the island taller by its
 * own, larger measure instead of scrolling its text. With `note` the card has
 * one more line above the summary: the island is taller by that line, or the
 * box of the target would give up a line of its own to it.
 */
export function confirmShape(target: string, whole = false, note = false): IslandShape {
  const base = ISLAND_SHAPES.awaiting_confirm;
  const extra = note ? CONFIRM_NOTE : 0;
  let height = base.height + extra;
  if (whole) {
    const lines = Math.min(targetLines(target), CONFIRM_MAX_LINES);
    height = Math.max(base.height, Math.ceil(CONFIRM_REST + extra + lines * CONFIRM_LINE_PX));
  } else {
    const lines = confirmLines(target);
    if (lines > 2) height += CONFIRM_LACK + (lines - 2) * CONFIRM_LINE;
  }
  return height === base.height ? base : { ...base, height };
}

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
