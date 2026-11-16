// Where she looks and how far her head, eyes and body turn for it
// (decision 0040). Angles in degrees: yaw to her left (the right of the
// screen) is positive, pitch up is positive. Pure functions, no DOM.

import type { BodyState } from './body';

export type GazeSource = 'reaction' | 'typing' | 'state' | 'cursor' | 'emotion' | 'wander';

/** What there is to look at now; the first in the order of the design wins. */
export function pickGaze(c: { reaction?: boolean; typing?: boolean; state?: boolean; cursor?: boolean; emotion?: boolean }): GazeSource {
  if (c.reaction) return 'reaction';
  if (c.typing) return 'typing';
  if (c.state) return 'state';
  if (c.cursor) return 'cursor';
  if (c.emotion) return 'emotion';
  return 'wander';
}

/** Where she looks while busy: up and aside thinking, aside at work, at you otherwise. */
export const STATE_GAZE: Record<BodyState, [number, number]> = {
  thinking: [-15, 12],
  working: [20, -5],
  confirm: [0, 0],
  listening: [0, 0],
};

/** Head turns at most this far aside and up or down. */
const HEAD_YAW = 35;
const HEAD_PITCH = 20;
/** Eyes turn at most this far past the head. */
const EYES = 15;
/** Past this the body turns too, by a share of the rest. */
const BODY_FROM = 25;
const BODY_SHARE = 0.4;

const clamp = (v: number, limit: number) => Math.max(-limit, Math.min(limit, v)) + 0;

/** Turn to a point of the window from her eyes at `eye`, both in pixels; `depth` is how far the viewer sits, pixels. */
export function angleTo(point: { x: number; y: number }, eye: { x: number; y: number }, depth: number): { yaw: number; pitch: number } {
  const d = Math.max(1, depth);
  return {
    yaw: (Math.atan2(point.x - eye.x, d) * 180) / Math.PI,
    pitch: (Math.atan2(eye.y - point.y, d) * 180) / Math.PI,
  };
}

/** How a turn of `yaw` and `pitch` is shared between the head, the eyes and the body. */
export function aimAt(
  yaw: number,
  pitch: number,
): {
  headYaw: number;
  headPitch: number;
  eyeYaw: number;
  eyePitch: number;
  bodyYaw: number;
} {
  const headYaw = clamp(yaw, HEAD_YAW);
  const headPitch = clamp(pitch, HEAD_PITCH);
  const over = Math.abs(yaw) - BODY_FROM;
  return {
    headYaw,
    headPitch,
    eyeYaw: clamp(yaw - headYaw, EYES),
    eyePitch: clamp(pitch - headPitch, EYES),
    bodyYaw: over > 0 ? Math.sign(yaw) * over * BODY_SHARE + 0 : 0,
  };
}

/** A cursor that has not moved this long is not looked at any more, ms. */
export const CURSOR_INTEREST_MS = 5000;

/** Whether a cursor that last moved at `movedAt` still holds her eyes at `now`. */
export function cursorInterest(movedAt: number, now: number): boolean {
  return now - movedAt <= CURSOR_INTEREST_MS;
}
