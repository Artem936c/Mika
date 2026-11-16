// What the living model of this window reacts to (decision 0040): her
// state, a tool at work, the user's typing and sending, the cursor, and
// gestures asked of her. One per window; the scene reads it every frame.

import { listen } from '@tauri-apps/api/event';
import { isAssistantState, type AssistantState } from '../island';
import type { Scene } from './scene';
import type { TouchPart } from './touch';

interface Live {
  state: AssistantState;
  /** A tool is at work. */
  working: boolean;
  /** `performance.now()` of the last message the user sent. */
  sentAt: number;
  /** The cursor in logical pixels of the window, and when it last moved. */
  cursor: { x: number; y: number; movedAt: number } | null;
  /** Where the user types, in pixels of the window, and when. */
  typing: { x: number; y: number; at: number } | null;
  /** How many gestures were asked of her: the built-in pictures hop on each. */
  gestures: number;
}

const live = $state<Live>({
  state: 'idle',
  working: false,
  sentAt: -Infinity,
  cursor: null,
  typing: null,
  gestures: 0,
});

export { live };

/** Takes one event of the bus. */
export function onLiveEvent(payload: unknown, now = performance.now()) {
  const e = payload as {
    type?: string;
    to?: unknown;
    name?: unknown;
    motion?: unknown;
  };
  switch (e?.type) {
    case 'state_changed':
      if (isAssistantState(e.to)) live.state = e.to;
      // A reply that ended leaves no tool at work behind.
      if (e.to === 'idle') live.working = false;
      break;
    case 'tool_started':
      live.working = true;
      break;
    case 'tool_finished':
      live.working = false;
      break;
    case 'user_text':
      live.sentAt = now;
      break;
    case 'avatar_gesture':
      if (typeof e.name !== 'string') break;
      live.gestures += 1;
      shown?.perform({
        name: e.name,
        motion: typeof e.motion === 'string' ? e.motion : null,
      });
      break;
  }
}

/** The living scene shown in this window now, if any, and the view that holds it. */
let shown: Scene | null = null;
let holder: HTMLElement | null = null;

/** The view at `host` shows `scene` now. */
export function setShownScene(scene: Scene, host: HTMLElement) {
  shown = scene;
  holder = host;
}

/** The view at `host` lets the scene go; a view that came in since keeps it. */
export function releaseShownScene(host: HTMLElement) {
  if (holder !== host) return;
  shown = null;
  holder = null;
}

/** Views of this window that want the cursor. */
let wanting = 0;

/** One more view wants the cursor (+1) or one less (-1): the new state when it changes, else null. */
export function cursorWanted(delta: 1 | -1): boolean | null {
  const before = wanting > 0;
  wanting = Math.max(0, wanting + delta);
  const now = wanting > 0;
  return now === before ? null : now;
}

/** The part of the living model under a point of the window; null without one or past her. */
export function locate(clientX: number, clientY: number): TouchPart | null {
  return shown?.touch(clientX, clientY) ?? null;
}

/** Her body answers a click on `part`. */
export function react(part: TouchPart) {
  shown?.react(part);
}

/** Asks how opaque the living model is under a point; false without one. The answer comes after her next frame. */
export function probeAt(clientX: number, clientY: number, answer: (alpha: number) => void): boolean {
  if (!shown) return false;
  shown.probe(clientX, clientY, answer);
  return true;
}

/** The window moves: the model leans and her hair swings. */
export function pushScene(vx: number, vy: number) {
  shown?.push(vx, vy);
}

/** The user types at `x`, `y` (pixels of this window). */
export function noteTyping(x: number, y: number, now = performance.now()) {
  live.typing = { x, y, at: now };
}

let started = false;

/** Called once by the root of a window. */
export async function startLive(): Promise<void> {
  if (started) return;
  started = true;
  try {
    await listen('mika://event', (e) => onLiveEvent(e.payload));
    await listen<{ x: number; y: number }>('mika://cursor', (e) => {
      const { x, y } = e.payload ?? {};
      if (typeof x === 'number' && typeof y === 'number') live.cursor = { x, y, movedAt: performance.now() };
    });
    await listen<{ action?: string; x?: number; y?: number }>('mika://ui', (e) => {
      const p = e.payload;
      if (p?.action === 'typing' && typeof p.x === 'number' && typeof p.y === 'number') noteTyping(p.x, p.y);
    });
  } catch {
    // Outside Tauri and without a mock nothing happens.
  }
}
