// What the living model of this window reacts to (decision 0040): her
// state, a tool at work, the user's sending, the cursor (the desktop
// window tells clicks on her by it), and gestures asked of her. One per window; the scene reads it every frame.

import { listen } from '@tauri-apps/api/event';
import { isAssistantState, type AssistantState } from '../island';
import type { Emotion } from '../avatar';
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
  /** How many gestures were asked of her: the built-in pictures hop on each. */
  gestures: number;
}

const live = $state<Live>({
  state: 'idle',
  working: false,
  sentAt: -Infinity,
  cursor: null,
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

/** The part of the living model under a point of the window; null without one or past her. */
export function locate(clientX: number, clientY: number): TouchPart | null {
  return shown?.touch(clientX, clientY) ?? null;
}

/** Her body answers a click on `part`, after the face the click brought when known. */
export function react(part: TouchPart, emotion?: Emotion) {
  shown?.react(part, emotion);
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
  } catch {
    // Outside Tauri and without a mock nothing happens.
  }
}
